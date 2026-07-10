// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The poll loop, its decision state, and the daemon lifecycle.
//!
//! [`Daemon`] is generic over its four seams — [`RosterPoller`],
//! [`CredentialStore`], [`AccountStash`] and [`Clock`] — so the whole loop runs
//! hermetically against in-memory fakes in tests: no live quota, no keychain, no
//! real time, no signals, no socket. The current-thread runtime (see `main`) is
//! what lets the seams stay free of `Send` bounds.
//!
//! ## One tick ([`Daemon::tick`])
//!
//! 1. **Identify the active account.** Resolved once and cached, updated on each
//!    swap — see [`Daemon::resolve_active`]. `None` (un-identifiable) → poll-only,
//!    never swap.
//! 2. **Poll ONE account** (issue #80). Each tick polls a single account — the next
//!    entry in a staggered schedule that interleaves the active account before each
//!    enabled non-active peer (issue #366) — through the canonical credential when it is the
//!    active account (freshest token) or its stash otherwise. Spreading a cycle's N
//!    polls across N sub-intervals (≈`poll_secs / N` apart) keeps each request in its
//!    own rate-limit window: the usage endpoint is source-scoped and serves ~one
//!    request per short window, so the former poll-of-all BURST had all-but-one
//!    `429`-fail at the CDN edge. The polled account's reading updates its slot in the
//!    carried `last_readings`; a failed poll clears it. A `429` / `5xx` backs off
//!    only THAT account's next poll (issue #293, revising #76): the `429` is
//!    per-account (each token resolves to its own Anthropic org, so the throttle
//!    buckets are independent), so the throttled account is skipped until its
//!    back-off window elapses while the active account keeps polling.
//! 3. **Decide and swap** on the LAST-KNOWN reading of each account (issue #80) — no
//!    longer a single-instant poll-of-all, so one account's number may be ~a cycle
//!    older than another's. If the active account's SESSION usage is at/above the
//!    session swap-away trigger, OR its WEEKLY usage is at/above the separate
//!    (typically higher) weekly trigger — each drawn this cycle from its own
//!    timing strategy and clamped to range (issues #38, #41) — pick the viable
//!    target whose weekly quota resets soonest (issue #37, [`pick_target`]) and run
//!    the out-of-band [`swap::swap`]. A per-cycle jittered post-swap cooldown (issue
//!    #10) refuses a re-swap until it has elapsed, bounding oscillation between two
//!    near-exhausted accounts. Until the first cycle has polled every account once,
//!    the swap-away decision HOLDS (warm-up): acting on a partial reading set could
//!    swap to a suboptimal target or declare a spurious all-exhausted (issue #80).
//!
//! The session trigger, the weekly trigger (#41), the cooldown, and the
//! poll interval are each a
//! [`Strategy`] (base + optional jitter, issue #38): a fresh value is drawn and
//! clamped to the parameter's range every cycle through the [`SplitMix64`] seam,
//! so polling/swaps decorrelate across accounts and cycles instead of running in
//! lockstep. The seam is seeded from entropy in production and from a fixed seed
//! in tests (`Daemon::with_seed`), keeping the draws deterministic under test.
//!
//! ## Lifecycle (the run loop, [`run_loop()`])
//!
//! - **Single-instance lock** ([`InstanceLock`]) — a kernel advisory `flock` held
//!   for the process lifetime; a second `run` exits `3`.
//! - **Reconcile-on-start** ([`Daemon::reconcile_on_start`]) — heal a crash /
//!   third-writer `oauthAccount`↔canonical mismatch before the first poll.
//! - **Control socket** ([`UnixControl`]) — a `0600` Unix-domain socket serving
//!   newline-delimited JSON `status`, carrying handles + percentages only, never a
//!   token (issue #15).
//! - **Graceful shutdown** ([`Shutdown`]) — SIGINT / SIGTERM is observed only
//!   *between* ticks, so an in-flight swap always runs to completion (#6 is
//!   no-half-swap): complete-or-abort, never a torn swap.
//!
//! The forward-looking `next_swap` candidate shown by `status` (the account the
//! daemon would rotate to next, #88) is computed here, and every swap /
//! all-exhausted / token-rejection / lock-wait is recorded to the structured event
//! log (#9, via
//! [`crate::observability`]). Oscillation is prevented at its source by the always-on
//! session gate (a session-saturated account is never a swap target, mirroring the
//! weekly-exhaustion exclusion); the post-swap cooldown (#10) additionally PACES swaps
//! — a re-swap is refused until the per-cycle jittered cooldown has elapsed — and the
//! swap-target `target_max_usage` reserve (#398) is a default-on ceiling on top: a
//! PROACTIVE swap only lands on an account whose session usage is *below* it, so the
//! target keeps runway. The reserve is a hard filter on that path — if nothing sits
//! below it, holding is the correct answer. An EMERGENCY swap (the active credential is
//! dead or quarantined) drops the reserve entirely: any live account beats a dead one,
//! and honouring it there would strand the daemon on the corpse. When no target
//! survives the filters there is no viable target ([`TickAction::NoViableTarget`],
//! #11): the loop enters the all-exhausted terminal state — it HOLDS (no swap, so no
//! thrash) and emits a single edge-triggered `all_exhausted` event naming the
//! least-bad account, its `cause=` (session-blocked vs weekly-exhausted) and, when
//! known, the `resets_at=` of the relief that unblocks it.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use tokio::signal::unix::{signal, Signal, SignalKind};

use crate::claude_state;
use crate::config::{Account, Config, Tunables, DEFAULT_REFRESH_SYSTEMIC_FAILURE_N};
// The daemon↔refresh_tick boundary contract (issue #202) lives in its own leaf module so
// `refresh_tick` can depend on it without depending on the whole daemon. Re-exported under
// `crate::daemon::*` so every existing `daemon::Clock` / `daemon::RealClock` caller is unchanged.
// (`SweepOutcome` is named only in test code here — the run loop consumes it by inference — so
// it is imported test-scoped inside `mod tests`, not at module scope.)
pub(crate) use crate::contract::{Clock, RealClock, RefreshObservation, RefreshTicker};
use crate::error::{Error, Result};
use crate::keychain::{
    CanonicalChange, CanonicalWatch, Credential, CredentialStore, RealCredentialStore,
};
use crate::observability::{
    BackoffClass, CaptureEventOutcome, CredentialHealth, DecisionClass, Diagnostic, DiagnosticLog,
    Event, EventLog, KeepWarmTrigger, PollClass, RefreshEventOutcome, SwapReason,
};
use crate::refresh::{RefreshOutcome, RefreshReport};
use crate::refresh_tick::{refresh_event_outcome, RealRefreshEngine, RefreshEngine};
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};
use crate::swap::{self, SwapDecision, SwapLock, SWAP_LOCK_MAX_WAIT};
use crate::systemic_refresh::{SweepHealth, SystemicRefreshHealth};
use crate::timing::{Jitter, Rng, SplitMix64, Strategy};
use crate::usage::{CurlTransport, PolledReading, RealUsageSource, Usage, UsageSource};
use crate::usage_store::{append_sample, compact_and_roll, RetentionPolicy, Sample};

// Per-concern submodules split off from this file along its responsibility seams (issue
// #203, the #195 decomposition after #202 untied the contract cycle). Each is re-exported
// under `crate::daemon::*` below, so relocating them is source-compatible for every existing
// consumer (cli / use_account / poke / capture) and for the in-module test suite, which
// reaches them through `use super::*`. `daemon` retains only the poll-loop decision core
// (the [`Daemon`] state machine) and its wiring.
mod peer_auth;

pub(crate) use peer_auth::peer_is_same_user;
// `is_same_user` / `peer_euid` are exercised only by the in-module peer-auth tests
// (production reaches them through `peer_is_same_user`); re-export test-scoped so
// `mod tests`' `use super::*` resolves them unmodified while a non-test build sees no
// unused re-export.
#[cfg(test)]
pub(crate) use peer_auth::{is_same_user, peer_euid};

mod snapshot;

pub(crate) use snapshot::{
    credential_health, refresh_health_view, to_pct, versioned_status_response, AccountReading,
    AccountStatusLine, NextSwap, NextSwapReason, SchemaVersion, StatusResponse, StatusSnapshot,
    VersionedStatus, STATUS_SCHEMA_VERSION,
};
// `status_response` (the payload projection) and `RefreshHealth` are named only by the in-module
// tests — production reaches the wire through `versioned_status_response` (issue #164) and builds
// the health view through `refresh_health_view` without naming the type. Re-export test-scoped so
// `use super::*` resolves them while a non-test build sees no unused re-export.
#[cfg(test)]
pub(crate) use snapshot::{status_response, RefreshHealth};

mod socket;

pub(crate) use socket::{
    notify_restored, notify_roster_reload, request_swap, write_capture_ack, write_swap_ack,
    CaptureAck, CaptureCommand, CaptureRejection, Control, ControlSignal, ControlYield, SwapAck,
    SwapCommand, SwapRejection, UnixControl, SWAP_ACK_WRITE_TIMEOUT,
};
// `serve_control` / `control_reply` / `MAX_CONTROL_LINE_BYTES` are exercised only by the
// in-module socket tests (production reaches them through `UnixControl::serve`); `serve_watch` /
// `parse_watch_frame` / `WatchFrame` / `ServeOutcome` are the issue-#165 watch surface the same
// tests drive (`serve_watch` is reached in production through `UnixControl::serve`, but
// `parse_watch_frame` / `WatchFrame` have no in-tree client yet). Re-export test-scoped so
// `use super::*` resolves them while a non-test build sees no unused re-export.
#[cfg(test)]
pub(crate) use socket::{
    control_reply, encode_heartbeat_frame, encode_snapshot_frame, parse_watch_frame, serve_control,
    serve_watch, ServeOutcome, WatchFrame, MAX_CONTROL_LINE_BYTES,
};

mod run_loop;

pub(crate) use run_loop::run_loop;
// `swap_report` / `unrecoverable_report` are exercised only by the in-module run-loop tests
// (production calls them inside `run_loop`); re-export test-scoped so `use super::*` resolves them
// while a non-test build sees no unused re-export.
#[cfg(test)]
pub(crate) use run_loop::{swap_report, unrecoverable_report};

/// Per-cycle clamp bounds for the swap-away trigger draw, in PERCENT — mirrors
/// config's `session_trigger` range so a jittered draw can never escape it.
const TRIGGER_PCT_LO: f64 = 50.0;
const TRIGGER_PCT_HI: f64 = 99.0;
/// Per-cycle clamp bounds for the WEEKLY swap-away trigger draw, in PERCENT
/// (issue #41) — mirrors config's `weekly_trigger` range. Its own constants
/// (numerically equal to the session bounds today) so the two triggers stay
/// independently bounded.
const WEEKLY_TRIGGER_PCT_LO: f64 = 50.0;
const WEEKLY_TRIGGER_PCT_HI: f64 = 99.0;
/// Per-cycle clamp bounds for the cooldown draw, in seconds. The LOW bound is the
/// non-zero swap-cooldown floor ([`crate::config::COOLDOWN_SECS_FLOOR`], issue #272),
/// which the config `cooldown_secs` range mirrors (`floor..=3600`). Clamping every
/// draw to the floor is what makes it NON-BYPASSABLE: even a wide configured jitter
/// spread can never pull a cycle's cooldown below the floor, so swap pacing cannot be
/// disabled to zero via jitter any more than via the base tunable.
const COOLDOWN_SECS_LO: f64 = crate::config::COOLDOWN_SECS_FLOOR as f64;
const COOLDOWN_SECS_HI: f64 = 3600.0;
/// Per-cycle clamp bounds for the poll-interval draw, in seconds (config range).
const POLL_SECS_LO: f64 = 5.0;
const POLL_SECS_HI: f64 = 3600.0;

/// First back-off after a cycle finds the keychain LOCKED (issue #13) — short, so
/// a brief lock (the operator mid-unlock) is recovered from within a second.
const LOCK_BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Ceiling for the locked-keychain back-off (issue #13). The wait doubles each
/// locked cycle from [`LOCK_BACKOFF_BASE`] but never exceeds this, settling at one
/// read attempt per minute — prompt to resume on unlock, yet not a busy-spin on a
/// keychain that stays locked. The daemon NEVER auto-unlocks or prompts; a locked
/// keychain is the operator's to open (a non-interactive read just fails, exit 36).
const LOCK_BACKOFF_CAP: Duration = Duration::from_secs(60);

/// Largest exponent applied to the rate-limit / transient poll back-off (issue
/// #76). A backed-off cycle widens the wait to `interval × 2^min(streak, this)`;
/// clamping the exponent keeps the intermediate finite, while [`POLL_BACKOFF_CAP`]
/// is the actual ceiling. `6` (×64) is past the cap for any realistic interval, so
/// it is a safety bound, not the operative limit.
const POLL_BACKOFF_MAX_SHIFT: u32 = 6;
/// Ceiling on the rate-limit / transient poll back-off (issue #76). Under sustained
/// `429` / `5xx` the effective poll spacing grows exponentially but settles here —
/// one poll per hour, gentle on a throttling endpoint without going fully dark. A
/// server-advised `Retry-After` is honoured as a MINIMUM but is itself clamped to this
/// ceiling (issue #294), so this is the absolute maximum any single account's
/// poll-backoff window can reach.
const POLL_BACKOFF_CAP: Duration = Duration::from_secs(3600);
/// Upper bound (seconds) on the jittered start-up delay (issue #76). Before its
/// FIRST poll the daemon waits a uniform `[0, this)` so that repeated restarts of
/// the same config — and the N accounts within a cycle — do not synchronize an
/// immediate burst of usage requests. Small enough to stay responsive on launch.
const STARTUP_DELAY_CAP: f64 = 30.0;

/// Shutdown seam: resolves when a graceful stop has been requested. Behind a seam
/// so the loop's stop path is driven deterministically in tests (a real
/// implementation waits on SIGINT / SIGTERM).
pub(crate) trait Shutdown {
    /// Resolve when a graceful shutdown has been requested.
    async fn requested(&mut self);
}

/// Real shutdown: resolves on the first SIGINT or SIGTERM.
pub(crate) struct RealShutdown {
    sigint: Signal,
    sigterm: Signal,
}

impl RealShutdown {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            sigint: signal(SignalKind::interrupt())?,
            sigterm: signal(SignalKind::terminate())?,
        })
    }
}

impl Shutdown for RealShutdown {
    async fn requested(&mut self) {
        tokio::select! {
            _ = self.sigint.recv() => {}
            _ = self.sigterm.recv() => {}
        }
    }
}

/// Per-account usage seam: poll one roster account, routing the active account
/// through the canonical credential and every other through its stash. The test
/// fake (`FakeRosterPoller`) returns scripted per-account readings.
pub(crate) trait RosterPoller {
    /// Poll `account`'s usage. `active` selects the token source: the canonical
    /// keychain item for the active account (whose token is the freshest), or the
    /// account's stash for any other. Returns the full [`PolledReading`] — the
    /// swap-decision [`Usage`] plus the sample-only `severity` — from a single API
    /// call; the caller projects to `Usage` for the decision and records the sample
    /// from the same reading (issue #156, no extra call).
    async fn poll(&self, account: &Account, active: bool) -> Result<PolledReading>;
}

/// Production poller: build a [`CurlTransport`]-backed [`RealUsageSource`] per
/// call — over the canonical store for the active account, or a stash-backed
/// [`StashCredentialStore`] for any other. Stateless: the consecutive-401 streak
/// that drives dead-credential detection lives in the daemon's per-account health
/// state (issue #42), not in this per-poll source.
pub(crate) struct RealRosterPoller {
    stash: RealAccountStash,
}

impl RealRosterPoller {
    pub(crate) fn new() -> Self {
        Self {
            stash: RealAccountStash::new(),
        }
    }
}

impl RosterPoller for RealRosterPoller {
    async fn poll(&self, account: &Account, active: bool) -> Result<PolledReading> {
        if active {
            // The active account's token refreshes in place, so the canonical
            // item is the freshest bearer — poll through it.
            RealUsageSource::new(CurlTransport::new(RealCredentialStore::new()))
                .usage()
                .await
        } else {
            // A non-active account is polled with its stashed token — the seam #5
            // anticipated: `CurlTransport` is generic over `CredentialStore`.
            RealUsageSource::new(CurlTransport::new(StashCredentialStore {
                stash: &self.stash,
                service: account.stash(),
            }))
            .usage()
            .await
        }
    }
}

/// A read-only [`CredentialStore`] whose token comes from a per-account stash —
/// the adapter that lets the usage poller read a non-active account through the
/// same transport seam as the active one.
struct StashCredentialStore<'a, S> {
    stash: &'a S,
    service: String,
}

impl<S: AccountStash> CredentialStore for StashCredentialStore<'_, S> {
    async fn read(&self) -> Result<Credential> {
        Ok(self.stash.read(&self.service).await?.credential)
    }

    async fn write(&self, _credential: &Credential) -> Result<()> {
        // Polling never writes the canonical item through a stash adapter; the
        // swap engine writes the canonical item directly.
        Err(Error::Unimplemented(
            "stash-backed credential store is read-only",
        ))
    }
}

/// The provider tag stamped on every usage sample (issue #156). Sessiometer manages
/// Claude accounts only today; the [`Sample`] schema keeps a provider field so a
/// future multi-provider store stays distinguishable, so the collector names it.
const USAGE_PROVIDER: &str = "claude";

/// Record ONE usage sample for a poll (issue #156), fail-open, to the INJECTED store path.
///
/// The poll-loop entry point. `samples_path` is `Some` when production wired the real
/// `crate::paths::usage_samples()` path via `Daemon::with_usage_samples` — then delegate to
/// [`append_sample_for_poll`] with the poll reading + wall clock. When `None` — the
/// hermetic-test default — record NOTHING, so ticking a `FakeDaemon` never writes to the
/// developer's real store (issue #315). The path is injected, never resolved inline, so the
/// collector cannot reach the real support dir from a test. A store-WRITE failure is still
/// swallowed inside [`append_sample_for_poll`] — sampling telemetry must never break the
/// poll/swap loop.
fn record_usage_sample(
    samples_path: Option<&std::path::Path>,
    account_label: &str,
    polled: &Result<PolledReading>,
    now: i64,
) {
    let Some(samples_path) = samples_path else {
        return; // collector not wired (hermetic-test default) → write nothing (issue #315)
    };
    append_sample_for_poll(samples_path, account_label, polled, now);
}

/// Append one usage [`Sample`] for a poll to `samples_path`, fail-open (issue #156).
///
/// The collector's testable core — an injected `now` and path make it hermetic:
///
/// - **Piggyback, no extra call**: builds the sample from `polled`, the reading the
///   swap poll already fetched, never issuing its own usage request.
/// - **Gap honesty**: a FAILED poll (`Err` — API error, 401, offline) records
///   NOTHING. A gap is absent, never a fabricated zero/healthy sample (issue #157
///   reads a missing sample as UNKNOWN, never zero).
/// - **Redaction-clean**: `acct` is the account's redacted handle (`label`), never
///   an email or token — the store's invariant (issue #15).
/// - **Fail-open**: a store-write error (disk full, permission, torn write) is logged
///   and swallowed; it must never break the poll/swap loop or crash the daemon. The
///   credential rotation/health path is unaffected by a sampling failure.
///
/// `spend` is a forward slot in the [`Sample`] schema with no producer in the current
/// usage response, so it is wired to `None` until one exists.
fn append_sample_for_poll(
    samples_path: &std::path::Path,
    account_label: &str,
    polled: &Result<PolledReading>,
    now: i64,
) {
    let Ok(reading) = polled else {
        return; // gap: a poll with no reading records no sample
    };
    let sample = Sample::new(
        now,
        USAGE_PROVIDER,
        account_label,
        reading.usage.session,
        reading.usage.weekly,
    )
    .with_resets(
        reading.usage.session_resets_at,
        reading.usage.weekly_resets_at,
    )
    .with_severity(reading.severity.clone())
    .with_spend(None);
    if let Err(err) = append_sample(samples_path, &sample) {
        eprintln!("sessiometer: usage-sample write skipped: {err}");
    }
}

/// Per-account usage-gap tracking for the rate-limited `usage_gap` event (issue #161). An
/// entry exists only while an account is in a gap streak (no reading since `since`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GapState {
    /// Epoch second the current gap streak began — the event's fixed `since`.
    since: i64,
    /// Epoch second the last `usage_gap` for this streak was emitted — the rate-limit anchor.
    last_emitted: i64,
}

/// The cadences bounding the stats-store maintenance layer (issue #161): how often a gapping
/// account re-emits `usage_gap`, and how often the daemon runs `compact_and_roll`.
#[derive(Clone, Copy, Debug)]
struct StatsCadence {
    /// Minimum seconds between `usage_gap` re-emissions for one account while it keeps gapping.
    gap_reemit_secs: i64,
    /// Minimum seconds between `compact_and_roll` passes.
    roll_cadence_secs: i64,
}

/// Minimum seconds between `usage_gap` re-emits for one account (issue #161): a persistently
/// gapping account logs at most hourly, not once per failed poll.
const GAP_REEMIT_MIN_SECS: i64 = 3_600;
/// Minimum seconds between `compact_and_roll` passes (issue #161): compaction runs at most
/// hourly, bounding raw-file churn — a roll only folds data when a whole day ages out anyway.
const STATS_ROLL_CADENCE_SECS: i64 = 3_600;
/// The production stats cadences (issue #161). The pure [`stats_events_for_poll`] core takes
/// these as a parameter so a hermetic test can drive tighter windows.
const STATS_CADENCE: StatsCadence = StatsCadence {
    gap_reemit_secs: GAP_REEMIT_MIN_SECS,
    roll_cadence_secs: STATS_ROLL_CADENCE_SECS,
};

/// The two on-disk paths of the usage-stats store (issue #161), bundled so the maintenance
/// core takes one argument for them rather than two.
struct StorePaths<'a> {
    /// The append-only raw-sample JSONL (`crate::paths::usage_samples`).
    samples: &'a Path,
    /// The rolled-aggregate document (`crate::paths::usage_rollup`).
    rollup: &'a Path,
}

/// The carried usage-stats maintenance state (issue #161): per-account gap streaks plus the
/// last compaction time. One cohesive unit in [`DecisionState`], mutated only when the stats
/// seam is wired ([`Daemon::with_stats`]).
#[derive(Default)]
struct StatsState {
    /// Per-account gap streaks keyed by handle (see [`GapState`]) — an entry exists only while
    /// an account is gapping, so a roster reload never has to resize it.
    gap_state: BTreeMap<String, GapState>,
    /// Epoch second of the last `compact_and_roll` pass, or `None` until the first — the
    /// cadence anchor bounding compaction to at most one pass per roll window.
    last_roll: Option<i64>,
}

/// The pure, hermetic core of the usage-stats maintenance layer (issue #161): given a poll's
/// gap-or-reading outcome, the store paths, the retention policy, and the carried gap +
/// roll-cadence state, produce the redacted events to emit. Mutates only the two pieces of
/// carried state it is handed (`gap_state`, `last_roll`); everything else is by-value / `&`.
///
/// Two independent, fail-open effects (a store error is swallowed — telemetry must never break
/// the poll loop):
///
/// - **Gap** (`is_gap`): a no-reading poll recorded no sample (#156's gap-honesty), and MAY
///   surface a rate-limited `usage_gap`. The first poll of a streak emits (`since = now`);
///   later gapping polls re-emit only once `cadence.gap_reemit_secs` has elapsed, `since` fixed
///   at the streak start. A reading clears the account's streak.
/// - **Rollup**: at most once per `cadence.roll_cadence_secs`, run `compact_and_roll` under
///   `policy`; when a pass folds ≥1 raw sample, emit a `usage_rollup`.
///
/// Every emitted event is redaction-clean: `usage_gap` carries only the account HANDLE +
/// timestamps, `usage_rollup` only integers — never a token or email (the #15 guarantee).
fn stats_events_for_poll(
    paths: &StorePaths,
    account_label: &str,
    is_gap: bool,
    now: i64,
    policy: &RetentionPolicy,
    state: &mut StatsState,
    cadence: &StatsCadence,
) -> Vec<Event> {
    let mut events = Vec::new();

    // Gap tracking + rate-limited emission.
    if is_gap {
        match state.gap_state.get_mut(account_label) {
            Some(streak) => {
                // The streak continues: re-emit only once the rate-limit window has passed,
                // keeping `since` fixed at the streak start.
                if now - streak.last_emitted >= cadence.gap_reemit_secs {
                    streak.last_emitted = now;
                    events.push(Event::UsageGap {
                        account: account_label.to_owned(),
                        since: streak.since,
                    });
                }
            }
            None => {
                // A new streak: record it and emit the first gap immediately.
                state.gap_state.insert(
                    account_label.to_owned(),
                    GapState {
                        since: now,
                        last_emitted: now,
                    },
                );
                events.push(Event::UsageGap {
                    account: account_label.to_owned(),
                    since: now,
                });
            }
        }
    } else {
        // A reading resumed → the streak (if any) is over.
        state.gap_state.remove(account_label);
    }

    // Cadence-gated compaction + rollup emission. Store-global (independent of this poll's
    // account), so it runs on every poll subject only to the cadence.
    let roll_due = match state.last_roll {
        None => true,
        Some(prev) => now - prev >= cadence.roll_cadence_secs,
    };
    if roll_due {
        // Bound attempts to one per cadence window regardless of outcome (fail-open): a
        // persistent store error then retries at the cadence, never once per poll.
        state.last_roll = Some(now);
        if let Ok(summary) = compact_and_roll(paths.samples, paths.rollup, now, policy) {
            if summary.raw_lines > 0 {
                events.push(Event::UsageRollup {
                    rolled_through: summary.rolled_through_ts,
                    raw_lines: summary.raw_lines,
                });
            }
        }
    }

    events
}

/// The external-login watch cadence (issue #140): how often the run loop probes the canonical
/// credential item for an OUT-OF-BAND change (a manual `claude /login`), DECOUPLED from the
/// usage-poll cadence (`poll_secs`, default 300 s). The probe is a LOCAL keychain read — no
/// network, no rate-limit — so a short cadence is cheap; 15 s bounds the worst-case
/// active-account re-auth latency to seconds instead of a full poll interval. A named constant
/// (not config) keeps issue #140 scoped to the reactivity change; operator-tunable config is a
/// deliberate future option. Chosen over event-driven keychain watching (kqueue / FSEvents /
/// `Sec*` callbacks): the macOS keychain DB is fragile to watch and would add substantial
/// unsafe / FFI surface for little gain over a cheap local poll on the established idle-seam
/// pattern (#105).
const EXTERNAL_LOGIN_WATCH_SECS: u64 = 15;

/// External-login watch seam (issue #140): the run loop drives this from its idle path to
/// notice a manual `claude /login` (or any out-of-band canonical rewrite) on the ACTIVE
/// account FASTER than the usage-poll cadence. Distinct from [`RefreshTicker`] (#105, the
/// periodic parked-account refresh) and [`CanonicalWatch`] (the per-tick change classifier):
/// this is purely a shorter-cadence TRIGGER — it reads the canonical and, when it differs from
/// the daemon's last-committed baseline ([`Daemon::canonical_baseline`]), the run loop breaks
/// the idle to re-tick so the existing [`Daemon::reconcile_canonical_change`] does the
/// authoritative re-stash / re-resolve / surface. It NEVER mutates daemon state itself.
///
/// The seam owns its OWN [`CredentialStore`] because the daemon's is borrowed by the idle
/// `wait` future; both read the SAME canonical item in production. Wholly inert when a hermetic
/// test wires the no-op watch: [`until_due`](ExternalLoginWatch::until_due) never resolves, so
/// the arm never wins the idle select and the loop behaves exactly as before #140.
pub(crate) trait ExternalLoginWatch {
    /// Resolve when the next canonical probe is due (the watch's own cadence). MUST never
    /// resolve when disabled, so it never wins the idle select. Re-armable: the run loop awaits
    /// it afresh each idle iteration.
    async fn until_due(&mut self);
    /// Read the canonical credential item via the watch's OWN store. `None` on ANY
    /// unreadable / locked / absent read — a probe that cannot read simply detects nothing and
    /// the run loop keeps idling (fail-safe: detection never stalls or crashes the loop).
    async fn read_canonical(&mut self) -> Option<Credential>;
}

/// Production external-login watch (issue #140): a short-cadence LOCAL probe of the canonical
/// item over a [`RealCredentialStore`]. Always-on — the probe is a cheap local keychain read
/// with no network / rate-limit cost and a strictly better active-account re-auth latency, so
/// there is no feature gate; a hermetic test that must NOT probe wires the inert no-op watch
/// instead. Its own store is a second [`RealCredentialStore`] (stateless, resolves the same
/// canonical item as the daemon's) so the idle `wait`'s `&mut Daemon` borrow is untouched.
pub(crate) struct ExternalLoginWatcher<C> {
    store: C,
}

impl<C> ExternalLoginWatcher<C> {
    pub(crate) fn new(store: C) -> Self {
        Self { store }
    }
}

impl<C: CredentialStore> ExternalLoginWatch for ExternalLoginWatcher<C> {
    async fn until_due(&mut self) {
        tokio::time::sleep(Duration::from_secs(EXTERNAL_LOGIN_WATCH_SECS)).await;
    }

    async fn read_canonical(&mut self) -> Option<Credential> {
        // Best-effort: a locked / not-found / transient keychain read yields `None`, so the run
        // loop detects nothing this probe and keeps idling — a detection failure must never
        // break the poll/swap loop (mirrors #156's fail-open collector, #162's fail-safe
        // refresh).
        self.store.read().await.ok()
    }
}

/// Per-account refresh seam the POLL path uses to revive an expired-but-refreshable
/// access token BEFORE a usage 401 counts toward the #42 dead-credential streak (issue
/// #162). Distinct from [`RefreshTicker`] (the periodic parked-account sweep, #105): this
/// is a single, on-demand, one-account refresh composed into the poll→streak seam that a
/// 401 previously fell straight through.
///
/// Carried as an OPTIONAL [`Daemon`] field (`Option<Box<dyn PollRefresh>>`, like
/// `swap_lock_path`) rather than a 5th generic seam: the retry re-polls through the
/// account's EXISTING [`RosterPoller`], so only the refresh needs injecting, and the boxed
/// option leaves every hermetic-test `Daemon::new` site — and `tick`'s many call sites —
/// untouched (a scoped change that composes with the queued #140 daemon work). `None` (the
/// default) is the pre-#162 behaviour: a 401 flows straight to the streak. Production wires
/// the #102 engine ([`RealRefreshEngine`]); the seam tests wire a scripted fake.
///
/// A hand-desugared `async fn` (a boxed future) so the trait is `dyn`-compatible; the
/// current-thread runtime keeps the returned future free of a `Send` bound.
pub(crate) trait PollRefresh {
    /// Run ONE isolated refresh cycle for `account` (the #102 engine), yielding the
    /// classified [`RefreshReport`] so the caller can distinguish a revived / still-alive
    /// token from a `Dead` one (the refresh token cleared in place).
    fn refresh<'a>(
        &'a self,
        account: &'a Account,
    ) -> Pin<Box<dyn Future<Output = Result<RefreshReport>> + 'a>>;
}

impl PollRefresh for RealRefreshEngine {
    fn refresh<'a>(
        &'a self,
        account: &'a Account,
    ) -> Pin<Box<dyn Future<Output = Result<RefreshReport>> + 'a>> {
        // Reuse the SAME #102 engine the periodic tick drives — the poll path and the
        // sweep now compose over one refresh implementation (issue #162 root cause: they
        // were scoped as separate issues and never composed).
        Box::pin(RefreshEngine::refresh(self, account))
    }
}

/// The in-place ACTIVE-account keep-warm seam (issue #282) — the FOURTH refresh mechanism.
/// Given the active account and its current CANONICAL blob it mints a fresh token by driving
/// `claude` through the isolated back-dating spawn (there is no first-party OAuth exchange —
/// a fresh token comes only from Claude Code, see [`crate::refresh`]) and RETURNS it, so the
/// DAEMON promotes it to the canonical `Claude Code-credentials` item (atomic `-U`, under the
/// swap lock, baseline-committed). It never writes the canonical item itself, keeping the
/// daemon the single canonical writer (ADR-0003). Distinct from [`PollRefresh`] (the
/// #253-excluded isolated engine that writes the STASH): this is the ONE refresh path that
/// legitimately targets the active account, because its result lands where a live session reads.
///
/// Carried as an OPTIONAL [`Daemon`] field (`Option<Box<dyn KeepWarm>>`, like `poll_refresh`)
/// so every hermetic-test `Daemon::new` site is untouched; `None` (the default) is the pre-#282
/// behaviour. A hand-desugared `async fn` (a boxed future) so the trait is `dyn`-compatible; the
/// current-thread runtime keeps the returned future free of a `Send` bound.
pub(crate) trait KeepWarm {
    /// Mint a fresh token for `account` from its `canonical` blob and return it for the daemon
    /// to promote to the canonical item. `Ok((report, Some(credential)))` ONLY on a real refresh
    /// ([`RefreshOutcome::Refreshed`]); `(report, None)` for `NoChange` / `Dead` / `Error` (the
    /// daemon then leaves the canonical item untouched — a `Dead` outcome flows to the #42
    /// streak). `Err` is a could-not-run (spawn / FS) failure the daemon treats fail-safe.
    fn keep_warm<'a>(
        &'a self,
        account: &'a Account,
        canonical: &'a Credential,
    ) -> Pin<Box<dyn Future<Output = Result<KeepWarmMint>> + 'a>>;
}

/// The keep-warm mint result: the classified [`RefreshReport`] plus the fresh credential the
/// daemon promotes to the canonical item — `Some` ONLY on a real [`RefreshOutcome::Refreshed`],
/// `None` for `NoChange` / `Dead` / `Error`. Aliased so the `dyn`-compatible boxed-future
/// signatures on the [`KeepWarm`] trait stay readable (`clippy::type_complexity`); the same tuple
/// [`crate::refresh::keep_warm_cycle`] returns.
type KeepWarmMint = (RefreshReport, Option<Credential>);

/// The production [`KeepWarm`]: mints via [`crate::refresh::keep_warm_account`], which reuses
/// the #102 isolated back-dating spawn on a COPY of the canonical blob and hands the fresh
/// token back. Holds the `[refresh].claude_bin` OVERRIDE (issue #375), NOT a resolved path:
/// like the periodic tick's [`RealRefreshEngine`] it resolves `claude` PER CYCLE at the spawn
/// site via [`resolve_binary`](Self::resolve_binary), so a symlink / `$PATH` / version change
/// after the daemon started is picked up on the next keep-warm with no restart. The ephemeral
/// isolated dir + keychain are derived per-call from the account uuid.
pub(crate) struct RealKeepWarmEngine {
    claude_bin: Option<PathBuf>,
}

impl RealKeepWarmEngine {
    pub(crate) fn new(claude_bin: Option<PathBuf>) -> Self {
        Self { claude_bin }
    }

    /// Resolve the `claude` binary to spawn THIS keep-warm cycle (issue #375) via the UNCHANGED
    /// policy ([`crate::paths::claude_binary_with_override`]: `[refresh].claude_bin` →
    /// `$CLAUDE_BIN` → `$PATH`) — only the timing moved to per-cycle; which binary is chosen is
    /// identical to before (no canonicalization, no validation — a wrapper symlink spawns as-is).
    /// A failure surfaces as the mint's `Err`, which the daemon treats non-fatally: the canonical
    /// item is left untouched and the mint is retried next cycle.
    fn resolve_binary(&self) -> Result<PathBuf> {
        crate::paths::claude_binary_with_override(self.claude_bin.as_deref())
    }
}

impl KeepWarm for RealKeepWarmEngine {
    fn keep_warm<'a>(
        &'a self,
        account: &'a Account,
        canonical: &'a Credential,
    ) -> Pin<Box<dyn Future<Output = Result<KeepWarmMint>> + 'a>> {
        // Resolve THIS cycle (issue #375), not from a frozen field, then own the non-borrowed
        // inputs so the future needs only `canonical`'s lifetime. A resolution failure is carried
        // into the future as the `Err` the daemon handles fail-safe (canonical left untouched).
        let resolved = self.resolve_binary();
        let uuid = account.account_uuid.clone();
        Box::pin(async move {
            let binary = resolved?;
            crate::refresh::keep_warm_account(canonical.expose(), &uuid, binary).await
        })
    }
}

/// A held single-instance lock: a kernel advisory `flock(LOCK_EX|LOCK_NB)` on the
/// native-local `daemon.lock`. The file is held open for the process lifetime —
/// the kernel releases the lock on death (or on drop), so there is no stale-PID
/// reaping. A second `run` cannot acquire it and gets [`Error::AlreadyRunning`]
/// (process exit `3`).
pub(crate) struct InstanceLock {
    // Held open purely to keep the lock; dropping it (or the process dying)
    // releases it.
    _file: File,
}

impl InstanceLock {
    /// Acquire the lock at `path`, creating the file `0600` if needed.
    /// [`Error::AlreadyRunning`] if another instance already holds it.
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        // Raw `flock` FFI, kept un-wrapped by ADR-0004: kept raw rather than
        // adding a `rustix` production dependency; the std wheel
        // (`File::try_lock`, stable 1.89) is the planned replacement once MSRV
        // reaches 1.89 (see #257).
        // SAFETY: `flock` takes a valid open fd (owned by `file`, which outlives
        // the call) and the two flag constants; it has no other preconditions.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Self { _file: file });
        }
        let err = std::io::Error::last_os_error();
        // EWOULDBLOCK (== EAGAIN) means another instance holds the lock; anything
        // else is a genuine I/O failure.
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Err(Error::AlreadyRunning)
        } else {
            Err(Error::Io(err))
        }
    }

    /// Probe whether the single-instance lock at `path` is currently held by a LIVE daemon,
    /// WITHOUT disturbing it (issue #396) — the lock-fallback half of the `daemon status`
    /// liveness projection (socket-primary, lock-fallback). A non-blocking
    /// `flock(LOCK_EX|LOCK_NB)` over a fresh read-only open (a separate open file description,
    /// so it contends with the daemon's held lock exactly as a second `run` would):
    /// - `EWOULDBLOCK` ⇒ another process holds it — a daemon is alive even if its control
    ///   socket is not answering yet (the honest startup / wedged case; NOT "not running").
    /// - a successful acquire ⇒ no live holder; the lock is released the instant `file` drops
    ///   at the end of this scope — nothing is started, stopped, or signalled.
    /// - an absent lock file ⇒ the daemon has never created it ⇒ not running.
    ///
    /// Read-only by construction (the `daemon status` AC: no process is started/stopped/
    /// signalled). Kept beside [`Self::acquire`] so the raw `flock` FFI stays localized
    /// (ADR-0004).
    ///
    /// Note the one inherent tradeoff: probing a FREE lock necessarily acquires it for the
    /// ~microseconds until `file` drops — `flock` has no test-without-acquire mode, so this
    /// acquire-then-release is the canonical liveness-probe shape. It is benign here because
    /// the caller runs this ONLY as the socket-primary fallback (a real startup already holds
    /// the lock, so the probe fails to acquire and never contends); the sole residual race is
    /// a `run` whose own `acquire` lands in that microsecond window and self-refuses (exit 3),
    /// which is vanishingly unlikely and self-correcting on retry.
    pub(crate) fn is_held(path: &Path) -> Result<bool> {
        use std::os::unix::io::AsRawFd;

        let file = match OpenOptions::new().read(true).open(path) {
            Ok(file) => file,
            // No lock file at all ⇒ the daemon has never created it ⇒ not held.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(Error::Io(err)),
        };
        // SAFETY: as in `acquire` — a valid open fd (owned by `file`, which outlives the
        // call) plus the two flag constants; `flock` has no other preconditions.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            // Acquired ⇒ no live holder; `file` drops at the end of this scope, releasing the
            // lock at once.
            return Ok(false);
        }
        let err = std::io::Error::last_os_error();
        // EWOULDBLOCK (== EAGAIN): another instance holds the lock — a live daemon.
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(true)
        } else {
            Err(Error::Io(err))
        }
    }
}

/// Current wall-clock as epoch SECONDS, the unit the #119 credential rollup and wire use
/// (`access_expires_at`, like `session_resets_at`). `0` on the pre-1970 impossible case —
/// a best-effort, never-panic read, mirroring [`crate::observability`]'s log timestamps.
/// NOT the [`Clock`] seam (which is monotonic, for poll intervals): the rollup needs a
/// wall instant to compare against a stored `expiresAt`. Used only on the DISPLAY /
/// event-emission path (never a swap decision), so a direct read keeps the deterministic
/// decision logic on the injectable clock while the rollup logic stays a pure function of
/// an explicit `now_secs` (unit-tested directly).
fn wall_clock_now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Current wall-clock as epoch MILLISECONDS — the unit CC's credential `expiresAt` uses (issue
/// #282): the keep-warm near-expiry gate compares the active canonical token's ms `expiresAt`
/// against this. The ms sibling of [`wall_clock_now_secs`]; `0` on the pre-1970 impossible case.
/// Off the swap-DECISION path (only the proactive keep-warm horizon reads it), so a direct wall
/// read keeps the deterministic decision logic on the injectable [`Clock`] while the horizon
/// stays a pure function of an explicit `now_ms` — [`Daemon::keep_active_warm`] takes `now_ms` as
/// a parameter so the gate is unit-tested without wall-clock flakiness.
fn wall_clock_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Whether `canonical` carries a NON-empty refresh token — the one CC can exchange for a fresh
/// access token (issue #282). `Some(empty)` (CC cleared the RT in place → a dead credential) and
/// `None` (the field is absent) both mean "nothing to refresh", so a keep-warm mint would be a
/// doomed `claude -p` spawn: the keep-warm short-circuits on `false` and lets the #42 death streak
/// advance to the emergency swap (invariant 4 — a truly-dead active credential still quarantines).
/// Reuses [`crate::refresh::refresh_token`], the one audited extractor, so the emptiness rule lives
/// in a single place; the token bytes are only emptiness-checked here, never logged (the #15
/// single-surface guarantee).
fn has_live_refresh_token(canonical: &Credential) -> bool {
    matches!(crate::refresh::refresh_token(canonical.expose()), Some(rt) if !rt.is_empty())
}

/// A deterministic per-account keep-warm stagger offset in `[0, cadence)` seconds (issue #282),
/// ADDED to the near-expiry horizon so each account's proactive mint fires at a DIFFERENT phase of
/// the shared ~8h token TTL. Without it a roster logged in together would all reach the same
/// `remaining <= cadence` point at once and re-warm (or fail to) in lockstep — one shared failure
/// domain. Derived from the account uuid so it is STABLE across restarts: a wall-clock- or
/// entropy-seeded offset would move every launch and slowly re-correlate the roster. Reuses the
/// same [`SplitMix64`] decorrelation PRNG as the poll jitter (no new dependency — `cargo deny`
/// stays green), seeded from the uuid's SHA-256 so distinct accounts draw distinct, well-distributed
/// offsets. A pure function of `(account_uuid, cadence)`, so the de-correlation AC is unit-tested
/// directly.
fn keep_warm_stagger_secs(account_uuid: &str, cadence: Duration) -> u64 {
    let cadence_secs = cadence.as_secs();
    // A zero cadence has no window to stagger within (and would scale to a degenerate 0).
    if cadence_secs == 0 {
        return 0;
    }
    // Seed from the first 64 bits of the uuid's SHA-256 hex digest — a stable, well-distributed
    // per-account seed (the digest is ASCII hex; 16 chars = 8 bytes = one u64).
    let digest = crate::sha256::sha256_hex(account_uuid.as_bytes());
    let seed = u64::from_str_radix(&digest[..16], 16).unwrap_or(0);
    // Scale a single `next_unit()` draw in `[0, 1)` — the same decorrelation primitive the poll
    // jitter draws from — into `[0, cadence_secs)`, uniform across the window.
    (SplitMix64::new(seed).next_unit() * cadence_secs as f64) as u64
}

/// Fold an access-token `expiresAt` from CC's native epoch MILLISECONDS to the epoch
/// SECONDS the credential rollup and the `status` wire speak (issue #141 must-carry: the
/// blob is ms, the wire/rollup are s — a missed conversion misfires the operator clock by
/// 1000×). Integer division truncates any sub-second remainder, immaterial for a
/// token-lifetime clock and matching the refresh fold's `ms / 1000`
/// ([`Daemon::apply_refresh_observation`]). A pure function so the boundary is unit-tested.
fn millis_to_secs(ms: i64) -> i64 {
    ms / 1000
}

/// What the loop decided to do this cycle — logged, and asserted on in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TickAction {
    /// Active account is below the swap-away trigger — stay put.
    Held,
    /// Swapped the active credential from roster index `from` to `to`.
    Swapped { from: usize, to: usize },
    /// EMERGENCY-swapped from a confirmed-DEAD active account `from` to `to`, the
    /// soonest-reset viable target (issue #42) — bypassing the swap-away trigger and
    /// the cooldown. Distinct from [`Swapped`](Self::Swapped) so a forced
    /// dead-credential escape is visible in tests and outcomes.
    EmergencySwapped { from: usize, to: usize },
    /// The active account's credential is DEAD (quarantined, #42) but no other
    /// account is a viable swap target — the daemon holds on the dead active, unable
    /// to escape. The `credential_dead` signal already fired on the death transition,
    /// so this state is silent (no repeat-spam). The dead-credential cousin of
    /// [`NoViableTarget`](Self::NoViableTarget).
    ActiveDeadNoTarget,
    /// Active is over the trigger but no other account is a viable target: every
    /// other account is weekly-exhausted (or, with the opt-in target-max-usage
    /// enabled, all over it). The all-exhausted terminal state (#11) — the loop
    /// holds and emits one edge-triggered `all_exhausted` signal, never swapping.
    NoViableTarget,
    /// The active account could not be identified — poll-only, no swap.
    SkippedActiveUnknown,
    /// The active account's reading was unavailable this cycle (transient / 401 /
    /// unreadable) — never swap on missing data.
    SkippedActiveUnavailable,
    /// Over the trigger but within the post-swap cooldown — the re-swap is
    /// refused to bound oscillation (issue #10).
    SkippedCooldown,
    /// A swap was attempted but the engine returned an error; #6 is no-half-swap,
    /// so the state is coherent and the loop retries next cycle.
    SwapFailed,
    /// The keychain was LOCKED when this cycle went to read the canonical
    /// credential (issue #13). All work is deferred — no resolve, no poll, no swap
    /// — and the loop backs off (the wait is carried in
    /// [`TickOutcome::next_wait`]). The daemon never auto-unlocks or prompts.
    KeychainLocked,
}

impl TickAction {
    /// The operator-facing [`DecisionClass`] this action renders as on the diagnostic
    /// channel (issue #77). Total and 1:1 over the variants; the swap participants of
    /// [`Swapped`](Self::Swapped) / [`EmergencySwapped`](Self::EmergencySwapped) are
    /// intentionally dropped (the decision line is a pure label — the handles ride the
    /// event log's `swap` line and the foreground echo).
    fn decision_class(self) -> DecisionClass {
        match self {
            TickAction::Held => DecisionClass::Hold,
            TickAction::Swapped { .. } => DecisionClass::Swap,
            TickAction::EmergencySwapped { .. } => DecisionClass::EmergencySwap,
            TickAction::ActiveDeadNoTarget => DecisionClass::ActiveDeadNoTarget,
            TickAction::NoViableTarget => DecisionClass::AllExhausted,
            TickAction::SkippedActiveUnknown => DecisionClass::SkipActiveUnknown,
            TickAction::SkippedActiveUnavailable => DecisionClass::SkipActiveUnavailable,
            TickAction::SkippedCooldown => DecisionClass::SkipCooldown,
            TickAction::SwapFailed => DecisionClass::SwapFailed,
            TickAction::KeychainLocked => DecisionClass::KeychainLocked,
        }
    }
}

/// The result of one poll iteration.
#[derive(Debug)]
pub(crate) struct TickOutcome {
    /// What the loop decided to do.
    pub(crate) action: TickAction,
    /// The structured log events this cycle generated (issue #9): the
    /// poll-outcome events (401 / keychain-locked / 403) in roster order, then the
    /// decision event (swap / all-exhausted) if any. `run_loop` emits each to the
    /// event log; a Hold or a skip generates none.
    pub(crate) events: Vec<Event>,
    /// The operator-facing diagnostics this cycle generated (issue #77), in the
    /// order they are emitted: one [`Diagnostic::Poll`] per polled account (in
    /// roster order), then — on the edge — a [`Diagnostic::AllExhaustedCleared`]
    /// when this cycle LEFT the all-exhausted state, and finally the per-tick
    /// [`Diagnostic::Tick`] decision (with any back-off). Unlike `events`, EVERY
    /// tick produces some (a Hold still logs its poll outcomes + decision), so
    /// `run_loop`'s [`DiagnosticLog`] — not this vec — applies the verbosity gate.
    /// Produced unconditionally so the #15 redaction meter scans them on every
    /// cycle, in quiet mode too.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// The per-account readings this cycle, for the control socket (`status`).
    pub(crate) snapshot: StatusSnapshot,
    /// How long the run loop should wait before the next tick. `None` = the normal
    /// jittered poll interval (issue #38); `Some(d)` = the locked-keychain back-off
    /// (issue #13), imposed while the keychain stays locked and NOTHING can be polled.
    /// The rate-limit / transient back-off is NO LONGER a whole-loop wait (issue #293):
    /// it is scoped per-account and applied by skipping the throttled account's own poll
    /// (see [`Daemon::note_account_backoff`]), so it never widens this loop-level wait.
    pub(crate) next_wait: Option<Duration>,
}

/// When the loop last performed a swap. Drives the post-swap cooldown floor (its
/// `at`); the forward-looking `status` candidate is computed fresh from readings
/// (#88's `next_swap`), so this record no longer feeds the display.
#[derive(Debug, Clone)]
struct LastSwap {
    /// When the swap completed — monotonic, so it is the cooldown floor.
    /// Process-local: never serialized directly (an [`Instant`] is meaningless across
    /// the socket).
    at: Instant,
}

/// Per-account health carried ACROSS ticks — the dead-credential lifecycle state
/// (issue #42), indexed by roster position. Daemon-level (not per-poll) because the
/// 401 streak and the recovery probe must accumulate across ticks: a per-poll
/// counter is rebuilt every cycle and never observes a streak (the prerequisite the
/// issue fixed). Sized to the roster in [`Daemon::new`].
// `pub(crate)` (fields stay private) so the snapshot submodule's re-exported
// `refresh_health_view(&AccountHealth)` does not expose a more-private type (issue #203);
// the health state itself remains daemon-owned and its fields daemon-private.
#[derive(Default, Clone)]
pub(crate) struct AccountHealth {
    /// Consecutive non-scope 401s on this account's stored token. Incremented on a
    /// 401, reset to 0 on ANY non-401 outcome (success, 403, transient, locked). The
    /// `consecutive=` field of a `monitor_401` event while still healthy; reaching
    /// `monitor_401_n` QUARANTINES the account ([`quarantined`](Self::quarantined)).
    consec_401: u32,
    /// Whether this account is QUARANTINED — its stored ACCESS token was rejected
    /// (`monitor_401_n` 401s in a row), so the daemon stops polling and selecting it for
    /// the rotation until it recovers. NON-TERMINAL (issue #427): the remedy is a refresh
    /// (`poke` / a restart), not necessarily a re-login — a modern `status` surfaces it as
    /// the `Degraded` rollup (issue #42). The edge that fires the [`Event::CredentialDead`]
    /// / [`Event::CredentialRestored`] signals exactly once per transition.
    quarantined: bool,
    /// Consecutive successful recovery probes on a quarantined account that recovers
    /// WITHOUT a re-login (issue #42). A re-login un-quarantines on the spot (the #13
    /// re-stash now clears the flag directly, issue #107), so this counter advances
    /// only on the spontaneous-revival path: a dead ACTIVE account with no viable swap
    /// target stays active and is re-probed, and if its OWN token starts answering
    /// again, reaching `monitor_recovery_m` consecutive `Live` polls un-quarantines it.
    /// Reset to 0 on any non-success (so the M successes must be consecutive) AND when
    /// the account leaves the active slot mid-recovery via a manual `use` swap (issue
    /// #108) — a non-active account is never polled, so an un-reset probe would freeze
    /// here below `monitor_recovery_m` forever; see
    /// [`deactivate_recovery_probe`](Daemon::deactivate_recovery_probe).
    recovery_successes: u32,
    /// The last-observed stored access-token `expiresAt` as epoch SECONDS (issue #119),
    /// folded from a refresh sweep's [`RefreshObservation`] (the engine reads it in MS;
    /// converted at the fold). `None` until the refresh engine has observed this account
    /// (e.g. `[refresh]` is off). The rollup's `Stale` (expired) input + the wire clock.
    access_expires_at: Option<i64>,
    /// The access-token `expiresAt` as epoch SECONDS observed on the POLL path (issue
    /// #141) — read from the SAME credential the usage poll used (the canonical item for
    /// the active account, the per-account stash for any other), converted MS→s at the
    /// read boundary by [`millis_to_secs`]. `None` until this account has been polled.
    ///
    /// DISPLAY-ONLY, and deliberately DISTINCT from [`access_expires_at`](Self::access_expires_at):
    /// it is the `--json` clock's fallback so `status` surfaces an expiry even with
    /// `[refresh]` off (the refresh engine, the only prior writer, is off by default), but
    /// it NEVER feeds [`credential_health`]. Routing an idle account's naturally-lapsed
    /// stashed expiry into the rollup's `access_expires_at <= now → Stale` branch would
    /// fire a false-🟠 for every idle account (CC refreshes only its own ACTIVE token); the
    /// rollup gains a positive-liveness signal and consumes the poll clock under #137. The
    /// wire prefers the refresh-sourced value and falls back to this (see [`Daemon::snapshot`]).
    poll_expires_at: Option<i64>,
    /// The last-observed refresh classification (issue #119): drives the rollup's `Dead`
    /// (a cleared refresh token) check and the `--json` `last_ok` projection. `None` until
    /// a refresh has been observed. Stored as the full enum (not the reduced `last_ok`)
    /// because the rollup must distinguish a terminal `Dead` from a transient `Error`.
    last_refresh_outcome: Option<RefreshEventOutcome>,
    /// Whether CC rotated the refresh-token value on the last observed refresh (issue #119,
    /// the AC-3 durability signal). `None` until a refresh has been observed.
    refresh_token_rotated: Option<bool>,
    /// Consecutive refresh FAILURES (`Dead` / `Error` outcomes) carried across sweeps (issue
    /// #119), reset to 0 by the next alive refresh — the rollup's `AtRisk` input.
    consecutive_refresh_failures: u32,
    /// The previously-emitted credential-health rollup verdict (issue #119), for
    /// edge-triggered transition events: the run loop diffs the freshly-computed rollup
    /// against this and emits one [`Event::CredentialHealth`] per CHANGE. `None` means
    /// UNSEEDED — the first computation seeds it WITHOUT emitting (there is no prior state
    /// to transition from), so a fresh daemon does not log a startup storm.
    last_health: Option<CredentialHealth>,
    /// Edge-trigger latch for the unrecoverable-death signal (issue #261): set when a
    /// quarantined account's isolated #106-sweep refresh returns `Dead` and the daemon
    /// emits [`Event::CredentialUnrecoverable`], and CLEARED when the account
    /// (re-)quarantines (the single set site of [`quarantined`](Self::quarantined) in
    /// [`note_poll_outcome`](Daemon::note_poll_outcome)). So the signal fires exactly ONCE
    /// per quarantine episode — not once per sweep re-probe of a still-dead token — and
    /// fires afresh if the account recovers and dies again. Mirrors the daemon-level
    /// [`DecisionState::signaled_all_exhausted`] idiom, but PER-ACCOUNT (a dead refresh
    /// token is an account-scoped fact). Keyed on this latch, NOT on
    /// [`last_refresh_outcome`](Self::last_refresh_outcome): that field flaps `Dead`↔`Error`
    /// on a transient sweep failure and survives an un-quarantine, so keying the edge off it
    /// would both double-fire and miss-fire.
    unrecoverable_signaled: bool,
    /// When the daemon last ATTEMPTED an in-place keep-warm of this account (issue #282),
    /// on the monotonic [`Clock`] — set by BOTH the proactive and the reactive keep-warm
    /// paths (`keep_warm_and_promote`). The proactive path throttles off it to at most one
    /// mint per keep-warm cadence, so a persistently no-op mint (e.g. CC declines to refresh)
    /// cannot spawn `claude -p` every tick while the token sits in the near-expiry window.
    /// `None` until the first attempt; the reactive path (a live-session 401) is instead
    /// throttled once-per-episode by `consec_401 == 0`, so it ignores this field but still
    /// stamps it (a reactive mint suppresses a redundant proactive mint the same tick).
    last_keep_warm_attempt: Option<Instant>,
    /// Consecutive throttled polls on THIS account that backed off (issue #293, the
    /// per-account revision of #76's endpoint-global `DecisionState::poll_backoff_streak`).
    /// A `429` (rate-limited) or `5xx` / network transient advances it; any other poll
    /// outcome resets it to 0. Drives the exponential widening of this account's back-off
    /// window: the wait is the jittered poll interval times `2^min(streak,
    /// POLL_BACKOFF_MAX_SHIFT)`, capped at [`POLL_BACKOFF_CAP`] (never below a server
    /// `Retry-After`). Per-account so one account's throttle never widens another's cadence
    /// — see [`note_account_backoff`](Daemon::note_account_backoff).
    poll_backoff_streak: u32,
    /// The monotonic-[`Clock`] instant before which this account's poll is SKIPPED (issue
    /// #293): armed to `now + widened` on a throttled poll, cleared on any non-throttling
    /// outcome. While [`Daemon::tick`] sees `now < poll_backoff_until` it skips this
    /// account's poll (no usage request, carries its last reading) — so a `429` on one
    /// account never silences the active account (the daemon's core job). `None` until the
    /// first throttle; on the same monotonic clock as
    /// [`last_keep_warm_attempt`](Self::last_keep_warm_attempt).
    poll_backoff_until: Option<Instant>,
}

/// Per-loop decision state carried across polls.
#[derive(Default)]
struct DecisionState {
    /// 1-based count of polls taken.
    ticks: u64,
    /// Roster index of the active account, resolved once and updated on each
    /// swap. `None` until first resolved (then the loop polls but never swaps).
    active: Option<usize>,
    /// The last swap performed, or `None` until the first. Drives the post-swap
    /// cooldown (#10): a re-swap is refused until this cycle's jittered `cooldown` has
    /// elapsed since this swap, PACING swaps — it does not by itself prevent a ping-pong
    /// with a persistent cause; the always-on session gate in `pick_target` is what
    /// prevents two session-saturated accounts from oscillating. (The forward-looking
    /// `status` candidate is #88's `next_swap`,
    /// computed fresh from readings — not this record.)
    last_swap: Option<LastSwap>,
    /// Per-account health carried across ticks (issue #42), indexed by roster
    /// position: the consecutive-401 streak (feeding the `monitor_401` log event and
    /// the dead-credential threshold), the quarantine flag, and the recovery-probe
    /// count. Sized to the roster in [`Daemon::new`]. See [`AccountHealth`].
    health: Vec<AccountHealth>,
    /// Edge-trigger guard for the all-exhausted signal (issue #11): set when an
    /// `all_exhausted` event is emitted, and cleared by [`Daemon::tick`] on any
    /// cycle that is NOT the no-viable-target state. So the signal fires exactly
    /// ONCE per all-exhausted episode — not once per poll while every account
    /// stays exhausted — and fires afresh if the state clears and is re-entered.
    signaled_all_exhausted: bool,
    /// The out-of-band canonical-change detector (issue #13 re-auth re-stash):
    /// tracks the last *committed* canonical credential so a rewrite by something
    /// other than the daemon — a `claude /login` re-auth, or a silent in-place
    /// token refresh — is detected and the owning account's stash refreshed. The
    /// daemon's OWN canonical writes (a swap) are committed into it so they are not
    /// re-detected as external. The *type* lives in [`crate::keychain`] so the
    /// dead-credential path (#42) reuses it; the daemon owns this instance.
    canonical_watch: CanonicalWatch,
    /// Current locked-keychain back-off (issue #13): `None` while the keychain is
    /// readable, `Some(d)` while locked — grown from [`LOCK_BACKOFF_BASE`] toward
    /// [`LOCK_BACKOFF_CAP`] each locked cycle and returned as
    /// [`TickOutcome::next_wait`]. Reset to `None` on the first readable cycle, so
    /// a later lock episode starts the climb afresh.
    lock_backoff: Option<Duration>,
    /// Edge-trigger guard for the keychain-locked signal (issue #13): set when a
    /// `keychain_locked_wait` event is emitted, cleared on the first readable
    /// cycle. So the signal fires exactly ONCE per lock episode — not once per
    /// backed-off retry while the keychain stays locked — mirroring
    /// `signaled_all_exhausted`.
    signaled_keychain_locked: bool,
    /// Last-known usage reading per roster account (issue #80), indexed by roster
    /// position. The daemon polls ONE account per tick (staggered, the active
    /// interleaved before each peer — #366), so a decision is taken on the most recent
    /// reading of EACH account rather than a single-instant poll-of-all — one account's
    /// number may be ~a cycle older than another's. `None` until an account is first
    /// polled (or after a poll fails).
    /// Sized to the roster in [`Daemon::new`]. The decision/snapshot view masks an
    /// out-of-rotation (disabled / quarantined) non-active account back to `None`
    /// ([`decision_readings`](Daemon::decision_readings)), so stale carried data can
    /// never leak into [`pick_target`].
    last_readings: Vec<Option<Usage>>,
    /// The staggered poll schedule for the CURRENT cycle (issues #80, #366): the roster
    /// indices to poll, in order — the active account INTERLEAVED before each enabled,
    /// non-quarantined non-active peer (`[active, p1, active, p2, …]`, issue #366), so
    /// the active is re-observed roughly every second tick without raising the poll rate
    /// (see [`build_poll_schedule`](Daemon::build_poll_schedule)). One entry is consumed
    /// per tick; when [`poll_pos`](Self::poll_pos) reaches its end the schedule is
    /// rebuilt for the next cycle (re-resolving active and re-reading rotation
    /// membership). Empty only for a degenerate roster (no active and nothing enabled),
    /// in which case a tick polls nothing.
    poll_schedule: Vec<usize>,
    /// Cursor into [`poll_schedule`](Self::poll_schedule): the position to poll this
    /// tick. Advances by one per tick and triggers a schedule rebuild on wrap, so the
    /// daemon walks active → spare → active → spare → … one account per sub-interval
    /// (the active interleaved between peers, issue #366) instead of bursting all at
    /// once (issue #80).
    poll_pos: usize,
    /// Whether each roster account has been polled at least once this run (issue #80),
    /// indexed by roster position. Drives the warm-up latch below; sized to the roster
    /// in [`Daemon::new`].
    polled_once: Vec<bool>,
    /// Warm-up latch (issue #80): `false` until every account in the FIRST cycle's
    /// schedule has been polled once, then latched `true` for the run. While `false`
    /// the swap-away decision HOLDS — no swap and no `all_exhausted` signal — because
    /// the carried readings are still partial: acting on them could swap to a
    /// suboptimal target or declare a spurious all-exhausted when an unpolled account
    /// might still be viable. Once warmed up, [`decide_action`](Daemon::decide_action)
    /// runs normally on the full last-known set.
    warmed_up: bool,
    /// The usage-stats store maintenance state (issue #161): per-account gap streaks (for the
    /// rate-limited `usage_gap` event) plus the last `compact_and_roll` time (the roll-cadence
    /// anchor). Populated ONLY when the stats seam is wired ([`Daemon::with_stats`]); otherwise
    /// it stays at its empty default and the collector's roll/gap layer is inert. See
    /// [`StatsState`].
    stats_state: StatsState,
    /// Daemon-level SYSTEMIC refresh-failure detector (issue #378): the consecutive all-eligible-
    /// account refresh-error sweep streak plus the once-per-episode edge latch. Fed one classified
    /// sweep at a time via [`Daemon::note_systemic_refresh`] (post-idle, like the sweep's #106
    /// restores / #119 observations), it edge-triggers a distinct `refresh_systemic_failure` when
    /// the streak crosses [`Daemon::systemic_failure_n`] and clears on the first working sweep — a
    /// mechanism-down signal distinct from the per-account `at_risk` rollup. `Default` (healthy,
    /// no streak) is the fresh-daemon state. See [`SystemicRefreshHealth`].
    systemic_refresh: SystemicRefreshHealth,
}

/// The poll loop, generic over its four injectable seams.
pub(crate) struct Daemon<P, C, S, K> {
    roster: Vec<Account>,
    poller: P,
    store: C,
    stash: S,
    clock: K,
    claude_json: PathBuf,
    /// Per-cycle swap-away trigger strategy (issue #38): drawn + clamped to
    /// `50..=99` percent each cycle, then `/100` for the swap decision. Replaces
    /// the former fixed `session_trigger` fraction.
    trigger_strategy: Strategy,
    /// Per-cycle WEEKLY swap-away trigger strategy (issue #41): drawn + clamped to
    /// `50..=99` percent each cycle, then `/100` for the swap decision — the
    /// weekly-dimension counterpart of `trigger_strategy`, independent of it.
    weekly_trigger_strategy: Strategy,
    /// Base WEEKLY-exhaustion threshold as a fraction (`weekly_trigger / 100`),
    /// un-jittered — the SAME value the `use` pre-swap gate treats as "weekly
    /// exhausted" (issue #11/#37). Distinct from `weekly_trigger_strategy` (the
    /// per-cycle JITTERED swap-decision draw): the snapshot's `weekly_exhausted`
    /// verdict (issue #72) must be deterministic and match the user-facing
    /// viability rule, so it keys off this base, not a per-cycle draw.
    weekly_trigger_base: f64,
    /// Base SESSION swap-away threshold as a fraction (`session_trigger / 100`),
    /// un-jittered — the session-dimension counterpart of [`Self::weekly_trigger_base`].
    /// The always-on session anti-thrash gate in [`pick_target`] keys off this on the
    /// deterministic display paths ([`Self::next_swap`], [`Self::refresh_exclusions`]),
    /// so the "next swap" candidate never flickers with per-cycle session-trigger
    /// jitter; the live swap path (`decide_action`) uses the per-cycle drawn trigger.
    session_trigger_base: f64,
    /// Default-on swap-target session reserve (issue #398) as a fraction
    /// (`target_max_usage / 100`), always valued. The PROACTIVE swap path passes it as
    /// `Some(..)` to [`pick_target`] — only swap TO an account whose session usage is
    /// below it — layering a STRICTER reserve on the always-on session gate
    /// (`session < session_trigger`, which prevents oscillation on its own). The
    /// EMERGENCY path ([`Self::emergency_swap`]) passes `None` instead: when the active
    /// credential is DEAD, liveness beats the reserve. Supersedes #10's opt-in `None`
    /// default — the config `target_max_usage` is now always set.
    target_max_usage: f64,
    /// Per-cycle post-swap cooldown strategy (issue #38; the #10 seam — see
    /// [`DecisionState`]): drawn + clamped to `COOLDOWN_SECS_LO..=3600` s each cycle
    /// — the low bound is the non-zero swap-cooldown floor (#272), so jitter can never
    /// draw a sub-floor cooldown. Replaces the former fixed `cooldown` duration.
    cooldown_strategy: Strategy,
    /// Base post-swap cooldown (issue #10) as a fixed [`Duration`] (`cooldown_secs`),
    /// UN-jittered — the stable window the socket `swap` command (issue #167) gates a
    /// manual swap on, so a non-`force` `use` routed THROUGH the daemon refuses inside
    /// the cooldown exactly as the standalone `use` path does (which derives the same
    /// window from `config.tunables.cooldown_secs`). Distinct from `cooldown_strategy`
    /// (the per-cycle JITTERED draw the auto-swap path uses), mirroring how
    /// `weekly_trigger_base` is the stable verdict distinct from `weekly_trigger_strategy`.
    cooldown_base: Duration,
    /// Per-cycle poll-interval strategy (issue #38): drawn + clamped to
    /// `5..=3600` s each loop iteration by
    /// [`next_poll_interval`](Self::next_poll_interval).
    poll_strategy: Strategy,
    /// Jitter RNG seam — process entropy in production, a fixed seed in tests
    /// (`with_seed`) so per-cycle draws are deterministic.
    rng: SplitMix64,
    /// Consecutive non-scope 401s before an account's stored credential is treated
    /// as DEAD and quarantined (issue #42; config `monitor_401_n`, `1..=20`).
    monitor_401_n: u8,
    /// Consecutive successful recovery probes before a quarantined account is
    /// restored to the rotation (issue #42; config `monitor_recovery_m`, `1..=20`).
    monitor_recovery_m: u8,
    /// The single-writer swap lock path (issue #64), or `None` to swap WITHOUT the
    /// cross-process lock. `None` is the hermetic-test default — a single-process
    /// test has no second writer to serialize against, so taking a real `flock`
    /// would only couple every swap test to a real file. Production wires the real
    /// `swap.lock` via [`with_swap_lock`](Self::with_swap_lock); when `Some`, every
    /// swap routes through [`swap::swap_locked`] and a contended acquire defers the
    /// swap (fail-closed) rather than risk a torn write.
    swap_lock_path: Option<PathBuf>,
    /// The `config.toml` path re-read on a runtime roster-reload (issue #139), or
    /// `None` to disable the reload (the hermetic-test default — a test with no
    /// on-disk config wires the pure [`reconcile_roster`](Self::reconcile_roster)
    /// directly instead). Production sets the real [`crate::paths::config_file`] via
    /// [`with_config_path`](Self::with_config_path); when `None`, an inbound
    /// `roster-reload` signal is a logged best-effort no-op (there is nothing to read).
    config_path: Option<PathBuf>,
    /// The poll-path refresh-then-retry seam (issue #162), or `None` to disable it (the
    /// hermetic-test default AND a `[refresh]`-off daemon — a 401 then flows straight to
    /// the #42 streak exactly as before). Production wires the #102 engine
    /// ([`RealRefreshEngine`]) via [`with_refresh_engine`](Self::with_refresh_engine), wired
    /// on the same `[refresh].enabled` switch as the periodic tick (issue #375: `claude` is
    /// resolved per-cycle at the spawn site, not gated on a startup resolution); when `Some`,
    /// a usage 401 attempts one isolated refresh + re-poll before it can quarantine the account.
    poll_refresh: Option<Box<dyn PollRefresh>>,
    /// The usage-stats store maintenance seam (issue #161), or `None` to disable it (the
    /// hermetic-test default — a test with no on-disk store wires nothing, so the collector's
    /// roll/gap emission is wholly inert and the ~existing `tick` tests are unaffected).
    /// Production wires the config-derived [`RetentionPolicy`] via
    /// [`with_stats`](Self::with_stats); when `Some`, each poll runs the cadence-gated
    /// `compact_and_roll` (emitting a redacted `usage_rollup` when a pass folds samples) and
    /// records a rate-limited redacted `usage_gap` on a no-reading poll. The append-per-poll
    /// collector (#156) runs independently, gated on its own `usage_samples_path` seam — this
    /// `stats` seam adds only the roll + gap-event layer.
    stats: Option<RetentionPolicy>,
    /// The per-poll usage-sample store path (issue #156/#315), or `None` to disable the
    /// collector (the hermetic-test default — a test with no on-disk store wires nothing, so
    /// ticking a `FakeDaemon` writes NO sample and the developer's real store stays untouched).
    /// Production wires the real `crate::paths::usage_samples()` path via
    /// [`with_usage_samples`](Self::with_usage_samples); when `Some`, each successful poll
    /// appends one redacted `Sample` to it. The path is INJECTED, never resolved inline, so a
    /// test cannot reach the real support dir (issue #315) — the same reason `NativeHistoryStore`
    /// holds its paths so a test can point one at a temp dir.
    usage_samples_path: Option<PathBuf>,
    /// Whether the operator turned the periodic isolated-refresh tick ON in config
    /// (`[refresh].enabled`, issue #105). Since #375 this CONFIG value IS the tick's effective
    /// switch (the `claude` binary is resolved per-cycle, not gated at startup). Carried onto the
    /// display snapshot so the
    /// thin `status` client can surface the issue-#138 discoverability advisory: with the tick
    /// OFF, non-active accounts get no maintenance and their credentials can silently lapse.
    /// `false` by default (the opt-in default, #105); production sets the real config value via
    /// [`with_refresh_enabled`](Self::with_refresh_enabled). Purely a display signal — never a
    /// swap/poll decision input.
    refresh_enabled: bool,
    /// The in-place ACTIVE-account keep-warm seam (issue #282), or `None` to disable it (the
    /// hermetic-test default AND a `[refresh]`-off daemon — the active account then simply
    /// lapses at expiry exactly as before). This is the FOURTH refresh mechanism: unlike the
    /// #253-excluded isolated engine ([`poll_refresh`](Self::poll_refresh) / the parked sweep),
    /// which writes only the STASH, this mints a fresh token and the daemon PROMOTES it to the
    /// canonical `Claude Code-credentials` item a live session reads — proactively before the
    /// active token's expiry, and as a reactive backstop on an active 401. Wired by
    /// [`with_keep_warm_engine`](Self::with_keep_warm_engine) on the SAME `[refresh].enabled`
    /// switch as the periodic tick (issue #375: `claude` is resolved per-cycle at the spawn
    /// site, not gated on a startup resolution).
    keep_warm: Option<Box<dyn KeepWarm>>,
    /// The keep-warm near-expiry horizon AND the proactive per-account attempt throttle
    /// (issue #282), sourced from `[refresh].cadence_secs` — the SAME dual-purpose knob the
    /// parked sweep uses for its own near-expiry horizon (no second config knob). A per-account
    /// stagger offset in `[0, cadence)` is added on top for de-correlation. Only read when
    /// [`keep_warm`](Self::keep_warm) is wired; the `new` default is an inert placeholder.
    keep_warm_cadence: Duration,
    /// The systemic refresh-failure threshold (issue #378): consecutive all-eligible-account
    /// refresh-error sweeps before the daemon surfaces a mechanism-down signal. Sourced from
    /// `[refresh].systemic_failure_n` (`1..=100`) via
    /// [`with_systemic_failure_n`](Self::with_systemic_failure_n); `new`'s default is the config
    /// default (an inert placeholder for hermetic tests, which drive the detector directly). Read
    /// only by [`note_systemic_refresh`](Self::note_systemic_refresh).
    systemic_failure_n: u32,
    state: DecisionState,
}

impl<P, C, S, K> Daemon<P, C, S, K>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
{
    pub(crate) fn new(
        roster: Vec<Account>,
        poller: P,
        store: C,
        stash: S,
        clock: K,
        claude_json: PathBuf,
        tunables: &Tunables,
    ) -> Self {
        // Per-account health carried across ticks (issue #42), one slot per account.
        let health = vec![AccountHealth::default(); roster.len()];
        // Carried last-known reading + warm-up tracking per account (issue #80), one
        // slot per account, sized to the roster like `health`.
        let last_readings = vec![None; roster.len()];
        let polled_once = vec![false; roster.len()];
        Self {
            roster,
            poller,
            store,
            stash,
            clock,
            claude_json,
            trigger_strategy: tunables.trigger_strategy,
            weekly_trigger_strategy: tunables.weekly_trigger_strategy,
            // The un-jittered base the `use` gate uses for "weekly exhausted" — the
            // deterministic threshold the `status` `weekly_exhausted` verdict keys
            // off (issue #72), NOT the per-cycle jittered swap-decision draw.
            weekly_trigger_base: f64::from(tunables.weekly_trigger) / 100.0,
            session_trigger_base: f64::from(tunables.session_trigger) / 100.0,
            target_max_usage: f64::from(tunables.target_max_usage) / 100.0,
            cooldown_strategy: tunables.cooldown_strategy,
            // The un-jittered cooldown window the socket `swap` command gates a manual
            // swap on (issue #167) — the same base `config.tunables.cooldown_secs` the
            // standalone `use` path uses, so routing through the daemon does not shift
            // the cooldown behavior.
            cooldown_base: Duration::from_secs(tunables.cooldown_secs),
            poll_strategy: tunables.poll_strategy,
            rng: SplitMix64::from_entropy(),
            monitor_401_n: tunables.monitor_401_n,
            monitor_recovery_m: tunables.monitor_recovery_m,
            // No cross-process swap lock by default; production opts in via
            // `with_swap_lock`. See the field's docs for why tests stay lock-free.
            swap_lock_path: None,
            // No runtime roster-reload by default (issue #139); production opts in via
            // `with_config_path`. A hermetic test drives `reconcile_roster` directly.
            config_path: None,
            // No poll-path refresh-then-retry by default (issue #162); production opts in
            // via `with_refresh_engine`. Left unset, a 401 flows straight to the streak.
            poll_refresh: None,
            // No usage-stats store maintenance by default (issue #161); production opts in via
            // `with_stats`. Left unset, the collector's roll/gap-event layer is inert.
            stats: None,
            // No per-poll usage-sample collector by default (issue #156/#315); production opts
            // in via `with_usage_samples`. Left unset, ticking writes NO sample, so `cargo test`
            // never touches the developer's real store — the isolation the injected path buys.
            usage_samples_path: None,
            // The periodic-refresh tick defaults OFF (opt-in, #105); production sets the real
            // `config.refresh.enabled` via `with_refresh_enabled`. Left false, the #138 advisory
            // stays inert (it also requires an unhealthy non-active account to fire).
            refresh_enabled: false,
            // No active-account keep-warm by default (issue #282); production opts in via
            // `with_keep_warm_engine` on the same `[refresh].enabled` switch as the periodic tick.
            // Left unset, the active account lapses at expiry exactly as before. The cadence is an
            // inert placeholder until the engine is wired (it is read only when `keep_warm` is).
            keep_warm: None,
            keep_warm_cadence: Duration::from_secs(3600),
            // The #378 systemic-failure threshold defaults to the config default (opt-in wiring
            // via `with_systemic_failure_n`); hermetic tests that exercise the detector pass the
            // threshold directly to `SystemicRefreshHealth::note`, so this placeholder only sets
            // the value for a `note_systemic_refresh` call an integration test drives at defaults.
            systemic_failure_n: DEFAULT_REFRESH_SYSTEMIC_FAILURE_N,
            state: DecisionState {
                health,
                last_readings,
                polled_once,
                ..DecisionState::default()
            },
        }
    }

    /// Wire the single-writer swap lock (issue #64): every swap then acquires the
    /// `flock` at `path` (blocking, bounded, fail-closed) so the daemon and a manual
    /// `use` swap can never interleave into a split state. Production sets the real
    /// `paths::swap_lock()`; a test may point it at a throwaway file. Builder-style
    /// to mirror `with_seed` and keep `new`'s 7 args stable.
    pub(crate) fn with_swap_lock(mut self, path: PathBuf) -> Self {
        self.swap_lock_path = Some(path);
        self
    }

    /// Record whether the operator enabled the periodic isolated-refresh tick
    /// (`[refresh].enabled`, issue #105) so the display snapshot can carry it to the `status`
    /// client for the issue-#138 discoverability advisory. Builder-style to keep `new`'s arg
    /// list stable, mirroring `with_swap_lock` / `with_config_path`; the hermetic-test default
    /// (`false`, set in `new`) leaves the advisory inert.
    pub(crate) fn with_refresh_enabled(mut self, enabled: bool) -> Self {
        self.refresh_enabled = enabled;
        self
    }

    /// Wire the systemic refresh-failure threshold (`[refresh].systemic_failure_n`, issue #378):
    /// the consecutive all-eligible-account refresh-error sweep count at which
    /// [`note_systemic_refresh`](Self::note_systemic_refresh) surfaces a mechanism-down signal.
    /// Production sets the real `config.refresh.systemic_failure_n`; the `new` default (the config
    /// default) leaves hermetic tests unaffected. Builder-style to keep `new`'s arg list stable,
    /// mirroring `with_refresh_enabled` / `with_swap_lock`.
    pub(crate) fn with_systemic_failure_n(mut self, n: u32) -> Self {
        self.systemic_failure_n = n;
        self
    }

    /// Wire the `config.toml` path re-read on a runtime roster-reload (issue #139):
    /// an inbound `roster-reload` control signal then re-reads this file and
    /// reconciles the in-memory roster. Production sets the real
    /// [`crate::paths::config_file`]; a test may point it at a throwaway file it rewrites
    /// mid-run to drive the reload end-to-end. Builder-style to mirror `with_swap_lock`
    /// / `with_seed` and keep `new`'s args stable.
    pub(crate) fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Wire the usage-stats store maintenance seam (issue #161): the daemon then runs the
    /// cadence-gated `compact_and_roll` under `policy` after each poll — emitting a redacted
    /// `usage_rollup` when a pass folds aged samples — and records a rate-limited redacted
    /// `usage_gap` on a no-reading poll. `policy` is the config-derived
    /// [`StatsConfig::retention_policy`](crate::config::StatsConfig::retention_policy).
    /// Builder-style to mirror `with_swap_lock` / `with_config_path` and keep `new`'s args
    /// stable; a hermetic test drives the pure [`stats_events_for_poll`] core directly instead.
    pub(crate) fn with_stats(mut self, policy: RetentionPolicy) -> Self {
        self.stats = Some(policy);
        self
    }

    /// Wire the per-poll usage-sample store path (issue #156/#315): the daemon then appends one
    /// redacted `Sample` to `path` after each successful poll. Production sets the real
    /// `crate::paths::usage_samples()` path; left unset, the collector is inert so a hermetic
    /// `FakeDaemon` tick writes nothing to the real support dir. The path is INJECTED rather than
    /// resolved inline precisely so a test cannot reach the real store (issue #315).
    /// Builder-style to mirror `with_stats` / `with_swap_lock` and keep `new`'s args stable.
    pub(crate) fn with_usage_samples(mut self, path: PathBuf) -> Self {
        self.usage_samples_path = Some(path);
        self
    }

    /// The stats-store maintenance layer wired into [`tick`](Self::tick) (issue #161): a thin
    /// adapter over the pure [`stats_events_for_poll`] core. Resolve the real store paths, run
    /// the core under the wired policy + carried gap/roll state, and append any redacted events
    /// to this tick's batch. A no-op when the stats seam is unset (`with_stats` not called) or a
    /// store path is unavailable — fail-open, exactly like [`record_usage_sample`]: sampling
    /// telemetry never breaks the poll/swap loop. `i` is the polled roster index; `is_gap` is
    /// whether that poll yielded no reading.
    fn maintain_stats_store(&mut self, i: usize, is_gap: bool, now: i64, events: &mut Vec<Event>) {
        let Some(policy) = &self.stats else {
            return; // stats seam not wired (hermetic-test default) → inert
        };
        let samples_path = match crate::paths::usage_samples() {
            Ok(path) => path,
            Err(_) => return,
        };
        let rollup_path = match crate::paths::usage_rollup() {
            Ok(path) => path,
            Err(_) => return,
        };
        let produced = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            &self.roster[i].label,
            is_gap,
            now,
            policy,
            &mut self.state.stats_state,
            &STATS_CADENCE,
        );
        events.extend(produced);
    }

    /// Wire the poll-path refresh-then-retry seam (issue #162): on a usage 401 the daemon
    /// then attempts one isolated refresh (the #102 engine) + a single re-poll BEFORE the
    /// 401 counts toward the #42 death streak, so a merely-expired access token is revived
    /// instead of quarantining a healthy account. Production wires [`RealRefreshEngine`]
    /// (on the same `[refresh].enabled` switch as the periodic tick — the `claude` binary is
    /// resolved per-cycle at the spawn site, issue #375); left unset, a test / feature-off daemon
    /// behaves exactly as before. Builder-style to mirror `with_swap_lock` / `with_config_path`
    /// and keep `new`'s args stable.
    pub(crate) fn with_refresh_engine(mut self, engine: Box<dyn PollRefresh>) -> Self {
        self.poll_refresh = Some(engine);
        self
    }

    /// Wire the in-place ACTIVE-account keep-warm seam (issue #282): the daemon then refreshes
    /// the active account's CANONICAL token in place — proactively before it nears expiry, and
    /// as a reactive backstop on an active 401 — so the overnight false-death cascade never
    /// starts. `cadence` (from `[refresh].cadence_secs`) is both the near-expiry horizon and the
    /// proactive per-account throttle. Production wires [`RealKeepWarmEngine`] on the SAME
    /// `[refresh].enabled` switch as the periodic tick (the `claude` binary is resolved per-cycle,
    /// issue #375); left unset, the active account lapses at expiry exactly as before. Builder-style
    /// to mirror `with_refresh_engine` and keep `new`'s args stable.
    pub(crate) fn with_keep_warm_engine(
        mut self,
        engine: Box<dyn KeepWarm>,
        cadence: Duration,
    ) -> Self {
        self.keep_warm = Some(engine);
        self.keep_warm_cadence = cadence;
        self
    }

    /// Replace the jitter RNG with a deterministically-seeded one — the test seam
    /// for reproducible per-cycle draws (issue #38 AC).
    #[cfg(test)]
    pub(crate) fn with_seed(mut self, seed: u64) -> Self {
        self.rng = SplitMix64::new(seed);
        self
    }

    /// Reconcile `~/.claude.json` to the canonical credential on startup.
    ///
    /// Heals the post-swap crash window: a swap writes the incoming token to the
    /// canonical item (the commit) and then co-writes `~/.claude.json` (best
    /// effort); a crash in between leaves the canonical holding the incoming token
    /// while the display still shows the outgoing account. Here we find the roster
    /// account whose stash matches the canonical token and, if the display
    /// disagrees, co-write that account's `oauthAccount`. When the canonical
    /// matches no stash — the active account's token has merely drifted (refreshed
    /// in place) on a normal restart, or it belongs to an un-captured account —
    /// `~/.claude.json` is left untouched (there is nothing to heal). Best-effort
    /// and idempotent.
    ///
    /// This IS the issue #13 process-death-mid-swap recovery: the swap commits the
    /// canonical token before co-writing the display, so a crash in that window
    /// leaves the keychain authoritative and the display stale — exactly the
    /// mismatch healed here on the next start. No separate mechanism is needed; the
    /// keychain-first ordering plus this reconcile make a torn swap self-healing.
    pub(crate) async fn reconcile_on_start(&self) -> Result<()> {
        let canonical = self.store.read().await?;
        for account in &self.roster {
            let Ok(stashed) = self.stash.read(&account.stash()).await else {
                continue;
            };
            if !stashed.credential.matches(&canonical) {
                continue;
            }
            // The canonical belongs to this account; ensure the display agrees.
            let displayed = claude_state::read_oauth_account_from(&self.claude_json)
                .ok()
                .map(|o| o.account_uuid().to_owned());
            if displayed.as_deref() != Some(stashed.oauth_account.account_uuid()) {
                claude_state::write_oauth_account(&self.claude_json, &stashed.oauth_account)?;
            }
            return Ok(());
        }
        // No stash matched the canonical token — leave ~/.claude.json untouched.
        Ok(())
    }

    /// Identify the active account: the roster index whose credential the
    /// canonical keychain item currently holds.
    ///
    /// Delegates to [`resolve_account_for`](Self::resolve_account_for) when the
    /// canonical is readable (token-match, then the `~/.claude.json` display
    /// fallback); when the canonical is unreadable (locked / not-found) it uses the
    /// displayed identity alone — the same json signal, the only one available
    /// without a token to match. `None` if neither resolves; the caller then polls
    /// but never swaps.
    async fn resolve_active(&self) -> Option<usize> {
        match self.store.read().await {
            Ok(canonical) => self.resolve_account_for(&canonical).await,
            // Canonical unreadable (locked / not-found): the displayed identity is the
            // only signal left — the same display-only fallback the shared resolver's
            // step 2 uses. The daemon degrades to it here rather than swap blindly.
            Err(_) => crate::active::resolve_via_display(&self.roster, &self.claude_json),
        }
    }

    /// Run one poll iteration: resolve the active account, poll every roster
    /// account, then decide and (if warranted) swap.
    pub(crate) async fn tick(&mut self) -> TickOutcome {
        self.state.ticks += 1;
        let at = self.clock.now();
        let mut events: Vec<Event> = Vec::new();
        // The operator-facing diagnostics this cycle (issue #77), produced
        // unconditionally — `run_loop`'s `DiagnosticLog` applies the verbosity gate.
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Read the canonical credential ONCE at the top of the cycle. It drives
        // three things, all from this single read: lock detection (defer the whole
        // cycle and back off, #13), re-auth re-stash detection (the canonical
        // changed out-of-band, #13), and the active resolution below. A locked
        // keychain is the one outcome that short-circuits the entire tick.
        let canonical = match self.store.read().await {
            Err(Error::KeychainLocked { .. }) => return self.locked_tick(),
            Ok(canonical) => {
                // Readable: clear any lock back-off and re-arm the edge-triggered
                // lock signal, then heal an out-of-band canonical change (#13).
                self.state.lock_backoff = None;
                self.state.signaled_keychain_locked = false;
                self.reconcile_canonical_change(&canonical, &mut events)
                    .await;
                Some(canonical)
            }
            Err(_) => {
                // Unreadable for a non-lock reason (not-found / transient): no
                // change-detection is possible, but it is not a lock — clear the
                // back-off and fall through to poll (the loop never swaps on missing
                // data, so an unknown active simply holds).
                self.state.lock_backoff = None;
                self.state.signaled_keychain_locked = false;
                None
            }
        };

        // Resolve the active account once; cached, updated on each swap, and dropped
        // by a re-auth re-stash above so it is re-resolved here. Reuse the canonical
        // already read above (the "read ONCE" intent) rather than re-reading it; only
        // the non-lock unreadable case falls back to the display-only resolve, whose
        // own store read has just failed anyway.
        if self.state.active.is_none() {
            self.state.active = match &canonical {
                Some(canonical) => self.resolve_account_for(canonical).await,
                None => self.resolve_active().await,
            };
        }
        let active = self.state.active;

        // Poll ONE account this tick — the next entry in the staggered schedule
        // (issue #80): the active account interleaved before each enabled,
        // non-quarantined peer (#366), one account per sub-interval, so each poll lands
        // in its own rate-limit window instead of a single back-to-back burst (most of
        // which the source-scoped usage endpoint `429`s at the CDN edge). The polled
        // account's reading replaces its slot in the carried `last_readings`; every
        // OTHER slot keeps its most-recent value, so the decision below is taken on
        // last-known-per-account readings (one account's number may be ~a cycle older
        // than another's). The poll OUTCOME still feeds the event log (issue #9: a 401
        // / 403 each emits a line) and the diagnostic channel (issue #77). The active
        // account is polled through the canonical credential (freshest token); a
        // disabled / quarantined active is still polled (swap-AWAY trigger, dead-active
        // re-probe), never a disabled / quarantined non-active. A locked keychain is
        // handled at top-of-tick, not here (see `locked_tick`).
        let poll_idx = self.next_poll_index(active);
        // The per-account rate-limit / transient back-off this tick imposed (issue #293,
        // the per-account revision of #76): the widened wait the account polled this tick
        // earned by a `429` / `5xx`, plus the raw server `Retry-After` source label (issue
        // #295), surfaced on the diagnostic tick line below. `None` when this tick polled
        // cleanly, skipped an account still inside its back-off window, or polled nothing.
        let mut this_tick_backoff: Option<TickBackoff> = None;
        // Issue #293: the `429` is PER-ACCOUNT (each token resolves to its own Anthropic
        // org, so the throttle buckets are independent), so a throttled account backs off
        // ONLY its own next poll — the active account and every other account keep their
        // normal cadence. `filter` drops the scheduled index while that account is still
        // inside its back-off window, so the whole poll body below is SKIPPED (no usage
        // request, no `Diagnostic::Poll`, `last_readings` carried untouched); the schedule
        // cursor already advanced in `next_poll_index`, so the slot is consumed and the
        // account is re-attempted once the window elapses. Transient (`5xx` / network) is
        // scoped the same way — see `note_account_backoff`.
        if let Some(i) = poll_idx.filter(|&i| !self.account_backing_off(i)) {
            let polled = self.poller.poll(&self.roster[i], active == Some(i)).await;
            // Record ONE usage sample for this poll (issue #156): piggyback the
            // reading just fetched (no extra usage-API call), recording nothing on a
            // gap and swallowing any store error. The store path is INJECTED (issue #315):
            // `None` in the hermetic-test default, so ticking a `FakeDaemon` writes nothing —
            // off the swap-decision path, so a sampling failure never perturbs the loop below.
            record_usage_sample(
                self.usage_samples_path.as_deref(),
                &self.roster[i].label,
                &polled,
                wall_clock_now_secs(),
            );
            // Usage-stats store maintenance (issue #161): a cadence-gated `compact_and_roll`
            // (emitting a redacted `usage_rollup` when a pass folds aged samples) plus a
            // rate-limited redacted `usage_gap` on a no-reading poll. Inert unless `with_stats`
            // wired the retention policy; off the swap-decision path, so a store failure is
            // swallowed and never perturbs the loop below.
            self.maintain_stats_store(i, polled.is_err(), wall_clock_now_secs(), &mut events);
            // Project to the lean `Usage` the decision path consumes; the sample-only
            // `severity` does not travel past here.
            let mut result = polled.map(|reading| reading.usage);
            // Issue #162: a usage 401 is USUALLY a merely-expired access token, not a dead
            // credential. Before it counts toward the #42 death streak, attempt ONE isolated
            // refresh (the #102 engine) + a single re-poll — but only on the FIRST 401 of a
            // streak episode and never for an already-quarantined account (see
            // `should_refresh_retry`), so a persistently-401 account triggers at most one
            // refresh per episode (no `claude -p` storm). A re-poll that CLEARS keeps the
            // account healthy (the false-death this fixes); a 401 that SURVIVES a fresh token
            // — or a refresh that reports the token DEAD — is the genuine dead signal and
            // flows on to `note_poll_outcome` unchanged. The seam is inert (unset) unless
            // `with_refresh_engine` wired it, so every other path behaves exactly as before.
            if self.should_refresh_retry(i, &result) {
                result = self.refresh_retry(i, &mut events).await;
            } else if self.should_keep_warm_retry(i, &result) {
                // Issue #282 REACTIVE backstop: the ACTIVE account's first usage-401. The #162
                // path above EXCLUDES the active account (it writes the stash, #253); this mints
                // and PROMOTES to the canonical a live session reads, then re-polls through it —
                // reviving an expired-but-refreshable active token in place BEFORE the 401 counts
                // toward the #42 death streak. Mutually exclusive with `should_refresh_retry` on
                // `i` (active vs parked), so a 401 takes exactly one refresh path. A truly-dead
                // credential (empty RT / a `Dead` mint) still returns the 401 → the streak
                // advances to the emergency swap (invariant 4).
                result = self
                    .keep_warm_retry(i, canonical.as_ref(), &mut events)
                    .await;
            }
            self.note_poll_outcome(i, &result, &mut events);
            diagnostics.push(Diagnostic::Poll {
                account: self.roster[i].label.clone(),
                outcome: diag_poll_class(&result),
            });
            // Fold the outcome into account `i`'s OWN back-off (issue #293): a `429` / `5xx`
            // advances its per-account streak and arms its back-off window; any other
            // outcome clears both. The returned widened wait rides the diagnostic tick line;
            // the durable back-off ENTER / EXIT events (issue #399) ride `events`.
            this_tick_backoff = self.note_account_backoff(i, &result, &mut events);
            // Durable per-account usage VELOCITY (issue #399): the signed percent delta between
            // this reading and the account's previous one, so the durable log carries how fast each
            // account is climbing (the gated #368 adaptive-trigger measurement). Both readings must
            // be present — the account's FIRST reading, or a reading after a throttle / failure
            // (which clears the slot below to `None`), has nothing to diff — and the account must
            // have measurably MOVED (a non-zero rounded delta in either dimension), mirroring
            // `usage_rollup`'s no-op silence so a flat idle account stays quiet on the always-on log.
            if let (Some(prev), Ok(next)) = (self.state.last_readings[i].as_ref(), result.as_ref())
            {
                let (session_delta_pct, weekly_delta_pct) = usage_velocity(prev, next);
                if session_delta_pct != 0 || weekly_delta_pct != 0 {
                    events.push(Event::UsageVelocity {
                        account: self.roster[i].account_uuid.clone(),
                        session_delta_pct,
                        weekly_delta_pct,
                    });
                }
            }
            self.state.last_readings[i] = result.ok();
            // Populate the DISPLAY expiry clock (issue #141) from the SAME credential this
            // poll used — kept DISTINCT from the refresh-sourced `access_expires_at` the
            // rollup reads, so `status --json` surfaces the access-token expiry with
            // `[refresh]` off without firing a false-🟠 Stale for an idle lapsed token (the
            // rollup's positive-liveness consumption of this clock lands under #137).
            let poll_expiry = self
                .read_poll_expires_at(&self.roster[i], active == Some(i))
                .await;
            self.state.health[i].poll_expires_at = poll_expiry;
            self.note_polled(i);
        }

        // Issue #282 PROACTIVE keep-warm: BEFORE deciding, if the active token is within its
        // (staggered) near-expiry horizon, mint a fresh token in place and promote it to the
        // canonical item — so the overnight false-death cascade never starts. Serialized here,
        // inside `tick`, ahead of `decide_action`; inert unless the keep-warm seam is wired. The
        // wall-clock `now_ms` is read here (off the swap-decision path) and passed in, so the
        // near-expiry gate stays a pure, deterministically-testable function of an explicit clock.
        self.keep_active_warm(active, canonical.as_ref(), wall_clock_now_ms(), &mut events)
            .await;

        // Decide on the carried last-known readings, masking an out-of-rotation
        // (disabled / quarantined) non-active account back to `None` so its stale
        // carried value can never become a swap target (issue #80).
        let readings = self.decision_readings(active);
        let action = self.decide_action(at, active, &readings, &mut events).await;
        // Edge-trigger the all-exhausted signal (issue #11): clear the guard
        // whenever this cycle is NOT the no-viable-target state, so a later
        // re-entry signals afresh. `decide_action` sets the guard (and emits once)
        // while in the state; this is the matching reset on the way out.
        if !matches!(action, TickAction::NoViableTarget) {
            // Diagnostic LEAVE edge (issue #77): the guard is still set from the
            // prior episode here (it has NOT been cleared yet), so a set guard on a
            // non-exhausted cycle means we are LEAVING the state — emit the marker
            // BEFORE the reset below. The symmetric partner of the event log's
            // edge-triggered `all_exhausted` ENTER, so a stale reading is
            // distinguishable from a current one.
            if self.state.signaled_all_exhausted {
                diagnostics.push(Diagnostic::AllExhaustedCleared);
            }
            self.state.signaled_all_exhausted = false;
        }
        // The rate-limit / transient back-off is PER-ACCOUNT now (issue #293): it is
        // applied by skipping the throttled account's own poll above (`account_backing_off`
        // / `note_account_backoff`), NOT by widening the WHOLE loop's wait — so the active
        // account and every other account keep polling on their normal cadence. `next_wait`
        // therefore stays `None` in the normal tick path; only the locked-keychain tick
        // (#13, `locked_tick`) still returns a whole-loop wait.
        let next_wait: Option<Duration> = None;
        // The per-tick decision diagnostic (issue #77), with any back-off this tick
        // imposed — the decision class names what the loop did (swap / hold / skip /
        // all_exhausted / …); a `None` back-off omits the field.
        diagnostics.push(Diagnostic::Tick {
            decision: action.decision_class(),
            // The back-off imposed on the account polled THIS tick (issue #293, per-account);
            // `None` on a clean poll, a skipped (already-backing-off) account, or a no-poll tick.
            backoff_secs: this_tick_backoff.map(|b| b.wait.as_secs()),
            // The wait's SOURCE (issue #295): the raw server `Retry-After` when one drove (or
            // floored) the wait, `None` when it was the self-capped exponential.
            retry_after_secs: this_tick_backoff
                .and_then(|b| b.retry_after)
                .map(|ra| ra.as_secs()),
        });
        // Snapshot from the POST-swap active index (`self.state.active`), NOT the local
        // `active` captured at top-of-tick (`let active = self.state.active`): when
        // `decide_action` above performed a swap it advanced `self.state.active` (via
        // `record_swap`), so the stale local `Copy` would mark the swapped-AWAY account as
        // `*` over the control socket for one whole poll interval, until the next tick
        // rebuilt the snapshot (#117). `readings` stays keyed on the pre-swap `active`, which
        // is consistent: a quarantined swapped-away account already carries `None` either way
        // (a quarantined active holding a reading returns `Held`, never swaps), so the only
        // actual masking difference is a DISABLED (parked) swapped-away account — its last
        // reading is shown rather than masked to `None`, harmless, and `next_swap` excludes it
        // from targeting via the enabled-filter regardless. The sibling `locked_tick` already
        // snapshots from `self.state.active`. The wall clock (epoch seconds) for the #119
        // credential rollup is read here (display-only, off the deterministic decision path).
        let snapshot = self.snapshot(self.state.active, &readings, wall_clock_now_secs());
        TickOutcome {
            action,
            events,
            diagnostics,
            snapshot,
            next_wait,
        }
    }

    /// The roster index to poll THIS tick — the next entry in the staggered schedule
    /// (issue #80) — advancing the cursor and rebuilding the schedule at the start of
    /// each cycle. The schedule interleaves the active account before each enabled,
    /// non-quarantined non-active peer (see [`build_poll_schedule`](Self::build_poll_schedule),
    /// issue #366); consuming one entry per tick keeps each poll `poll_secs/N` apart (the
    /// divisor is [`rotation_len`](Self::rotation_len) = N, unchanged by the interleave).
    /// `None` only for a degenerate roster whose schedule is empty (no active and
    /// nothing enabled) — that tick polls nothing and simply decides + waits.
    fn next_poll_index(&mut self, active: Option<usize>) -> Option<usize> {
        // Start of a cycle: rebuild the schedule from the freshly-resolved active and
        // current rotation membership (a swap or an enable/disable since last cycle is
        // picked up here, at the cycle boundary).
        if self.state.poll_pos >= self.state.poll_schedule.len() {
            self.state.poll_schedule = self.build_poll_schedule(active);
            self.state.poll_pos = 0;
        }
        let idx = self.state.poll_schedule.get(self.state.poll_pos).copied();
        self.state.poll_pos += 1;
        idx
    }

    /// Build this cycle's poll schedule: the active account INTERLEAVED before each
    /// peer — `[active, p1, active, p2, …, active, p_{N-1}]` (issue #366) — rather than
    /// once at the head (the original #80 shape, `[active, p1, p2, …]`). The active
    /// account is the only one that can reach its ceiling WHILE active (hence the only
    /// one whose swap-AWAY trigger is time-sensitive), so it is re-observed every ~2
    /// sub-intervals (≈`2·poll_secs/N`) instead of once per full sweep. The peers are
    /// every enabled (#36), non-quarantined (#42) non-active account in roster order,
    /// each still appearing ONCE — so a peer re-observes every `2·poll_secs·(N-1)/N`,
    /// which is `< 2·poll_secs` for all N (peers are only swap targets, ranked by weekly
    /// reset, so relaxing their cadence is fine). This 1:1 interleave IS the cap the
    /// acceptance asks for: inserting the active more than once per peer would push a
    /// peer's re-observation past `2·poll_secs`.
    ///
    /// Rate-neutral (load-bearing, #366): interleaving only lengthens the schedule
    /// VECTOR. The tick divisor is [`rotation_len`](Self::rotation_len) = N — the count
    /// of DISTINCT rotation accounts, taken from the roster, NOT the schedule length —
    /// so [`next_subinterval`](Self::next_subinterval) keeps ticks `poll_secs/N` apart —
    /// the per-tick spacing, the aggregate request rate, and the `poll_secs/N` per-source
    /// FLOOR are all unchanged (the active's re-observation deliberately tightens to
    /// `2·poll_secs/N`, still 2× slower than that floor). No new timer / async task
    /// / concurrent poller (one would fire outside the stagger and re-open the #80/#293
    /// burst); the change is purely this vector plus leaving `rotation_len` at N.
    ///
    /// The active account is always included even when disabled / quarantined (its
    /// swap-AWAY trigger must still fire and a dead active is re-probed), exactly as the
    /// former poll-all loop did; a disabled / quarantined non-active is excluded (never a
    /// swap target, and polling its dead token would waste a `curl`). Degenerate rosters:
    /// with no active, the schedule is just the peers in order (nothing to interleave);
    /// with an active but no peers, it is the active alone (still must be polled).
    fn build_poll_schedule(&self, active: Option<usize>) -> Vec<usize> {
        // Upper bound on length: the active (≤ 1) interleaved before each of ≤ roster
        // peers, so ≤ 2·roster.len().
        let mut schedule = Vec::with_capacity(2 * self.roster.len());
        for i in 0..self.roster.len() {
            if active == Some(i) {
                continue; // the active account is interleaved below, never listed as a peer
            }
            if self.roster[i].enabled && !self.state.health[i].quarantined {
                if let Some(a) = active {
                    schedule.push(a); // re-observe the active account before each peer (#366)
                }
                schedule.push(i);
            }
        }
        // Every push above is peer-driven, so an empty schedule here means the active
        // account had NO peers to interleave against — it still must be polled (its
        // swap-away trigger / dead-active re-probe), so schedule it alone.
        if let Some(a) = active {
            if schedule.is_empty() {
                schedule.push(a);
            }
        }
        schedule
    }

    /// Record that account `i` was polled this run and latch the warm-up flag (issue
    /// #80) once every account in the current schedule has been polled at least once —
    /// i.e. the first full cycle is complete and the carried readings are no longer
    /// partial. Until then the swap-away decision HOLDS (see
    /// [`decide_action`](Self::decide_action)).
    fn note_polled(&mut self, i: usize) {
        self.state.polled_once[i] = true;
        if !self.state.warmed_up
            && self
                .state
                .poll_schedule
                .iter()
                .all(|&j| self.state.polled_once[j])
        {
            self.state.warmed_up = true;
        }
    }

    /// The per-account readings the decision and snapshot operate on (issue #80): the
    /// carried last-known reading for the active account and every enabled,
    /// non-quarantined account, but `None` for a disabled (#36) / quarantined (#42)
    /// NON-active account. The mask mirrors the former poll-all loop (which pushed
    /// `None` for a skipped account), so a stale carried reading for an account that
    /// has since left the rotation can never be selected by [`pick_target`] — and the
    /// snapshot keeps showing such an account as unavailable, not at a stale number.
    fn decision_readings(&self, active: Option<usize>) -> Vec<Option<Usage>> {
        (0..self.roster.len())
            .map(|i| {
                if active == Some(i)
                    || (self.roster[i].enabled && !self.state.health[i].quarantined)
                {
                    self.state.last_readings[i]
                } else {
                    None
                }
            })
            .collect()
    }

    /// The number of DISTINCT accounts in the current poll rotation (issue #80): the
    /// active account plus every enabled, non-quarantined non-active account. This is the
    /// divisor that spreads a cycle's polls across the interval (see
    /// [`next_subinterval`](Self::next_subinterval)) — deliberately the count of distinct
    /// rotation accounts (N), NOT the length of [`poll_schedule`](DecisionState::poll_schedule),
    /// which the #366 active-interleave makes ~2N. Keeping the divisor at N is what holds
    /// the per-tick spacing at `poll_secs/N` (and thus the aggregate request rate, and
    /// the `poll_secs/N` per-source floor), unchanged by the interleave. At least 0;
    /// callers clamp to ≥ 1.
    fn rotation_len(&self) -> usize {
        (0..self.roster.len())
            .filter(|&i| {
                self.state.active == Some(i)
                    || (self.roster[i].enabled && !self.state.health[i].quarantined)
            })
            .count()
    }

    /// The keychain was LOCKED when this cycle went to read the canonical
    /// credential (issue #13). Defer ALL work — no resolve, no poll, no swap — and
    /// back off so the daemon does not hammer a locked keychain. The back-off grows
    /// exponentially from [`LOCK_BACKOFF_BASE`], doubling each consecutive locked
    /// cycle up to [`LOCK_BACKOFF_CAP`]. The `keychain_locked_wait` event is
    /// edge-triggered: emitted ONCE when the lock is first observed (guarded by
    /// `signaled_keychain_locked`), not every backed-off retry. The daemon NEVER
    /// auto-unlocks or prompts — a locked keychain is the operator's to open; a
    /// non-interactive read just fails (exit 36), and the daemon waits it out.
    fn locked_tick(&mut self) -> TickOutcome {
        let mut events = Vec::new();
        if !self.state.signaled_keychain_locked {
            events.push(Event::KeychainLockedWait);
            self.state.signaled_keychain_locked = true;
        }
        // Grow the back-off: first locked cycle waits BASE, each subsequent one
        // doubles up to CAP. Stored so the next locked cycle continues the climb.
        let backoff = match self.state.lock_backoff {
            None => LOCK_BACKOFF_BASE,
            Some(prev) => (prev * 2).min(LOCK_BACKOFF_CAP),
        };
        self.state.lock_backoff = Some(backoff);
        // Build an all-absent snapshot so the control socket keeps answering while
        // locked: every reading is unavailable (the keychain is unreadable), but
        // `status` still lists the roster rather than going dark. With no readings the
        // next-swap candidate reads `awaiting usage data` for any live spare (#88) — an
        // honest "no swap right now", since a lock merely defers the data behind it.
        let readings = vec![None; self.roster.len()];
        // A locked tick reads no credentials and changes no health state, so the #119
        // rollup it projects is the carried last-known verdict (the wall clock only
        // governs the access-token expiry crossover, harmless to evaluate here).
        let snapshot = self.snapshot(self.state.active, &readings, wall_clock_now_secs());
        // Diagnostic (issue #77): a locked tick polls NOTHING (it short-circuits
        // before the poll loop), so there are no per-poll lines — just the decision
        // line naming the deferral and the back-off wait it imposed.
        let diagnostics = vec![Diagnostic::Tick {
            decision: TickAction::KeychainLocked.decision_class(),
            backoff_secs: Some(backoff.as_secs()),
            // A keychain-lock back-off (#13) is self-imposed — no server is in the loop, so
            // there is no `Retry-After` source to label (issue #295).
            retry_after_secs: None,
        }];
        TickOutcome {
            action: TickAction::KeychainLocked,
            events,
            diagnostics,
            snapshot,
            next_wait: Some(backoff),
        }
    }

    /// Detect and heal an OUT-OF-BAND canonical change (issue #13 re-auth re-stash):
    /// the operator ran `claude /login` (or the active token silently refreshed in
    /// place), rewriting the canonical credential underneath the daemon. Classify
    /// the freshly-read `canonical` against the watch; on a `Changed` verdict, find
    /// the account it now belongs to and refresh that account's stash to the new
    /// token — so a later swap AWAY and back restores the re-authenticated
    /// credential, not the stale stashed one.
    ///
    /// The watch's two-step protocol (classify, then commit) makes this re-fire
    /// safe: the baseline advances only once the re-stash SUCCEEDS, so a failure
    /// (e.g. the keychain locks mid-write) leaves the change to be re-detected and
    /// retried next cycle. After a successful re-stash the cached active index is
    /// dropped so it is re-resolved against the new canonical (a `/login` may have
    /// switched to a different account).
    ///
    /// If the re-stashed account was QUARANTINED (issue #107), the re-login also
    /// un-quarantines it immediately and emits [`Event::CredentialRestored`] — a
    /// just-re-authenticated credential is live, so it must not linger in
    /// `needs re-login` for `monitor_recovery_m` more polls. The slower
    /// M-consecutive-live-poll recovery in [`note_poll_outcome`](Self::note_poll_outcome)
    /// stays for the spontaneous-revival path (no re-login).
    async fn reconcile_canonical_change(
        &mut self,
        canonical: &Credential,
        events: &mut Vec<Event>,
    ) {
        match self.state.canonical_watch.classify(canonical) {
            // First observation this run: prime the baseline, detect nothing.
            CanonicalChange::Primed => self.state.canonical_watch.commit(canonical),
            // No out-of-band write since we last looked.
            CanonicalChange::Unchanged => {}
            CanonicalChange::Changed => match self.resolve_account_for(canonical).await {
                Some(idx) => {
                    if self.restash_account(idx, canonical).await {
                        events.push(Event::ReStash {
                            account: self.roster[idx].label.clone(),
                        });
                        // A re-login of a quarantined account un-quarantines it ON THE
                        // SPOT (issue #107): a just-re-authenticated canonical IS a live
                        // credential, so stranding it in `needs re-login` for
                        // `monitor_recovery_m` more polls would make the durable status
                        // lie for ~a poll interval. Edge-triggered: clear the flag, drop
                        // any in-flight recovery probe, and emit `CredentialRestored`
                        // exactly once on the dead→alive transition. If the new token is
                        // somehow dead after all, the normal `monitor_401_n` path
                        // re-quarantines it. The M-consecutive-live-poll recovery in
                        // `note_poll_outcome` remains for the spontaneous-revival path (a
                        // dead ACTIVE account whose own token answers again WITHOUT a
                        // re-login).
                        if self.state.health[idx].quarantined {
                            self.state.health[idx].quarantined = false;
                            self.state.health[idx].recovery_successes = 0;
                            events.push(Event::CredentialRestored {
                                account: self.roster[idx].label.clone(),
                            });
                        }
                        // If this out-of-band change swapped AWAY from a DIFFERENT
                        // account that was mid-recovery, drop its now-frozen recovery
                        // probe (issue #108) — the daemon-notices-it-itself fallback to
                        // the `adopt_manual_swap` seam. `idx` is the swap-TO account (the
                        // new active, re-resolved below); `deactivate_recovery_probe`
                        // skips it as `next` and acts only on a departing `prev`.
                        let prev_active = self.state.active;
                        self.deactivate_recovery_probe(prev_active, Some(idx));
                        // Handled: advance the baseline so this write is not
                        // re-detected, and drop the cached active so it is
                        // re-resolved against the new canonical below.
                        self.state.canonical_watch.commit(canonical);
                        self.state.active = None;
                    }
                    // else: the re-stash failed (e.g. a locked keychain) — do NOT
                    // commit; leave the change to re-fire and catch up next cycle.
                }
                None => {
                    // The new canonical maps to no roster account: an UN-CAPTURED login
                    // (issue #140 scope decision). SURFACE it, do NOT auto-onboard — the
                    // daemon cannot isolate this shared-item token or attribute its identity;
                    // that is the managed `sessiometer login` (#132/#134/#135) path's job. The
                    // event prompts the operator to run it. Edge-triggered by the commit below:
                    // the next `classify` sees this same blob as `Unchanged`, so it fires ONCE
                    // per distinct un-captured login, not every watch cycle. Best-effort
                    // identity: the displayed `accountUuid` when readable (a redacted, non-PII
                    // handle, like #135's post-harvest `Login` account), else omitted.
                    let account_uuid = claude_state::read_oauth_account_from(&self.claude_json)
                        .ok()
                        .map(|oauth| oauth.account_uuid().to_owned());
                    events.push(Event::UncapturedLogin { account_uuid });
                    // Committed so we do not re-surface it every cycle; nothing to re-stash.
                    self.state.canonical_watch.commit(canonical);
                    // Drop the cached active too (issue #208), mirroring the Some-branch
                    // above: the canonical now resolves to NO roster account, so a
                    // surviving stale index would make `status` show a false `*` on the
                    // now-inactive account and let `decide_action` act on a phantom
                    // active. Cleared here, the top-of-tick re-resolution finds no stash
                    // or display match and re-resolves to `None`, so `decide_action`
                    // routes to the safe `SkippedActiveUnknown` path.
                    self.state.active = None;
                }
            },
        }
    }

    /// The canonical credential the daemon last COMMITTED to its [`CanonicalWatch`] — the
    /// baseline the external-login watch (issue #140) compares a fresh idle-time read against
    /// to detect an out-of-band `claude /login`. Snapshotted before the idle block (like the
    /// refresh exclusions) so the watch arm can distinguish an external write from the daemon's
    /// own last-committed state WITHOUT borrowing `&mut self` mid-idle. `None` until the first
    /// tick primes the watch.
    pub(crate) fn canonical_baseline(&self) -> Option<Credential> {
        self.state.canonical_watch.baseline()
    }

    /// Identify which roster account the given `canonical` credential belongs to — a
    /// thin `&self` adapter over the shared token-first resolver
    /// [`crate::active::resolve_account_for`] (canonical token byte-match, then the
    /// `~/.claude.json` display fallback). Extracted so the manual `use` swap resolves
    /// the active account the SAME way (issue #207); called here by
    /// [`resolve_active`](Self::resolve_active) and the re-auth re-stash path (#13).
    async fn resolve_account_for(&self, canonical: &Credential) -> Option<usize> {
        crate::active::resolve_account_for(&self.roster, &self.stash, &self.claude_json, canonical)
            .await
    }

    /// Refresh account `idx`'s stash to the new `canonical` token (issue #13 re-auth
    /// re-stash), PRESERVING its `oauthAccount` identity half. The identity is taken
    /// from the existing stash if present; otherwise from `~/.claude.json` — but
    /// only when the displayed identity actually belongs to account `idx` (its
    /// `accountUuid` matches the roster entry), so a wrong identity is never stapled
    /// onto the refreshed token. Returns `false` (re-stash not performed) when no
    /// usable identity is available or the stash write fails — the caller then
    /// leaves the change to re-fire rather than committing the baseline.
    async fn restash_account(&self, idx: usize, canonical: &Credential) -> bool {
        let account = &self.roster[idx];
        // Prefer the identity already stashed for this account: it is authoritative
        // and does not depend on the best-effort display file.
        let oauth_account = if let Ok(existing) = self.stash.read(&account.stash()).await {
            existing.oauth_account
        } else if let Ok(displayed) = claude_state::read_oauth_account_from(&self.claude_json) {
            // No existing stash: fall back to the displayed identity, but only if it
            // is THIS account's — never staple a different account's identity on.
            if account.account_uuid != displayed.account_uuid() {
                return false;
            }
            displayed
        } else {
            return false;
        };
        let refreshed = StashedAccount {
            credential: canonical.clone(),
            oauth_account,
        };
        self.stash.write(&account.stash(), &refreshed).await.is_ok()
    }

    /// Fold account `i`'s poll `result` into its per-account health (issue #42) and
    /// push any poll-outcome event. Classifies the result into a [`PollOutcome`]:
    ///
    /// - **Unauthorized** (401): increment the consecutive-401 streak and reset any
    ///   recovery probe. While the account is still healthy, emit `monitor_401` with
    ///   the climbing count; the Nth consecutive (`monitor_401_n`) QUARANTINES it (a
    ///   dead credential) and emits [`Event::CredentialDead`] — edge-triggered, ONCE
    ///   on the transition. Once quarantined, further 401s are silent (no spam): the
    ///   dead state is a durable status, not a repeated log line.
    /// - **Live** (success): reset the streak. If the account is quarantined, this is
    ///   a recovery probe — count consecutive successes and, at `monitor_recovery_m`,
    ///   un-quarantine it and emit [`Event::CredentialRestored`] (edge-triggered,
    ///   ONCE). This M-poll path is now the SPONTANEOUS-REVIVAL case only: a re-login
    ///   un-quarantines immediately in
    ///   [`reconcile_canonical_change`](Self::reconcile_canonical_change) (issue #107),
    ///   so the account reaching here is a dead ACTIVE one with no viable swap target
    ///   (it stays active and is re-probed) whose OWN token starts answering again
    ///   WITHOUT a re-login. That is intended — a token returning success M times in a
    ///   row is a working credential, and leaving such an account stranded in
    ///   `needs re-login` would make the durable status lie.
    /// - **ScopeMissing** (403): reset the streak — a 403 token authenticates, so it
    ///   is NOT dead — and emit `usage_scope_fail` (#5). Resets any recovery probe.
    /// - **Transient** (5xx / network / 429 / other 4xx / locked / unreadable): reset
    ///   the streak silently — no liveness signal either way — and reset any recovery
    ///   probe (only a `Live` poll advances recovery). A locked keychain is
    ///   process-global and signaled once at top-of-tick (#13), never here.
    fn note_poll_outcome(&mut self, i: usize, result: &Result<Usage>, events: &mut Vec<Event>) {
        match classify_poll(result) {
            PollOutcome::Unauthorized => {
                let consecutive = self.state.health[i].consec_401.saturating_add(1);
                self.state.health[i].consec_401 = consecutive;
                // A 401 breaks any in-progress recovery probe.
                self.state.health[i].recovery_successes = 0;
                // Already dead → stay silent: the durable status carries the dead
                // state; CredentialDead already fired on the transition (no spam).
                if self.state.health[i].quarantined {
                    return;
                }
                events.push(Event::Monitor401 {
                    account: self.roster[i].label.clone(),
                    consecutive,
                });
                // The Nth consecutive non-scope 401 declares the credential DEAD.
                if consecutive >= u32::from(self.monitor_401_n) {
                    self.state.health[i].quarantined = true;
                    // Open a fresh unrecoverable-death episode (issue #261): this account
                    // may later be confirmed unrecoverable by a dead sweep-refresh, and the
                    // #261 latch must be armed for THIS quarantine, having been left set by
                    // any prior episode. Reset here, the single quarantine-SET site.
                    self.state.health[i].unrecoverable_signaled = false;
                    events.push(Event::CredentialDead {
                        account: self.roster[i].label.clone(),
                    });
                }
            }
            PollOutcome::Live => {
                self.state.health[i].consec_401 = 0;
                if self.state.health[i].quarantined {
                    let m = self.state.health[i].recovery_successes.saturating_add(1);
                    self.state.health[i].recovery_successes = m;
                    if m >= u32::from(self.monitor_recovery_m) {
                        self.state.health[i].quarantined = false;
                        self.state.health[i].recovery_successes = 0;
                        events.push(Event::CredentialRestored {
                            account: self.roster[i].label.clone(),
                        });
                    }
                }
            }
            PollOutcome::ScopeMissing => {
                self.state.health[i].consec_401 = 0;
                self.state.health[i].recovery_successes = 0;
                events.push(Event::UsageScopeFail {
                    account: self.roster[i].label.clone(),
                });
            }
            PollOutcome::Transient => {
                self.state.health[i].consec_401 = 0;
                self.state.health[i].recovery_successes = 0;
            }
        }
    }

    /// Whether poll `i`'s outcome warrants a #162 refresh-then-retry, evaluated on the
    /// PRE-fold state (before [`note_poll_outcome`](Self::note_poll_outcome) advances the
    /// streak):
    ///
    /// - a refresh seam is wired ([`with_refresh_engine`](Self::with_refresh_engine)),
    /// - the poll was a 401 ([`PollOutcome::Unauthorized`]),
    /// - the account is not already quarantined (a dead account is left to the #106 sweep /
    ///   an operator re-login — never re-refreshed on every re-probe poll),
    /// - this is the FIRST 401 of the current streak episode (`consec_401 == 0`), and
    /// - the account is NOT the ACTIVE one (`state.active != Some(i)`, issue #253).
    ///
    /// The `consec_401 == 0` condition is the once-per-episode guard (AC-4, no refresh storm): a
    /// refresh spawns `claude -p` under the swap lock (seconds), so a persistently-401 account
    /// must refresh at most once per streak — the first 401 attempts the revive; the rest of the
    /// episode advances the streak directly.
    ///
    /// The active-account exclusion (issue #253) upholds the #102 engine's Caller contract
    /// ("refresh PARKED accounts only", `refresh.rs`): the isolated refresh performs a real OAuth
    /// exchange that ROTATES the refresh token server-side, but CAS-writes the fresh token only to
    /// the account's STASH — the canonical keychain item every live session reads keeps the old,
    /// now-invalidated token. Refreshing the active account would therefore break concurrent live
    /// sessions AND mask the account healthy (its stash re-poll succeeds), stranding the fresh
    /// token where no recovery path promotes it. `state.active` is resolved token-first at
    /// top-of-tick (#207) — the same authoritative signal [`refresh_exclusions`](Self::refresh_exclusions)
    /// (#105) and #250/`poke` exclude on. A still-active account's 401 instead advances the #42
    /// streak toward an operator re-login, exactly how a dead active account is already handled.
    fn should_refresh_retry(&self, i: usize, result: &Result<Usage>) -> bool {
        self.poll_refresh.is_some()
            && matches!(classify_poll(result), PollOutcome::Unauthorized)
            && !self.state.health[i].quarantined
            && self.state.health[i].consec_401 == 0
            && self.state.active != Some(i)
    }

    /// Attempt one isolated refresh of account `i` (the #102 engine) and a single re-poll,
    /// returning the outcome [`note_poll_outcome`](Self::note_poll_outcome) then folds into
    /// the streak (issue #162). Only called when [`should_refresh_retry`](Self::should_refresh_retry)
    /// holds, so `poll_refresh` is `Some` and — per that guard's active-account exclusion
    /// (issue #253) — `i` is always a PARKED account, never the active one.
    ///
    /// - Refresh reports **`Dead`** (the refresh token was cleared in place, `refresh.rs`) →
    ///   a genuine death: skip the re-poll and let the 401 stand so the streak advances.
    /// - Refresh ran otherwise (refreshed / no-change / even an engine error report) → the
    ///   account's STASH may now bear a fresh token, so re-poll THROUGH THE STASH
    ///   (`active = false`). The re-poll is a liveness probe against the parked account's stash
    ///   that never touches the live canonical credential. (This path deliberately does NOT run
    ///   for the active account: `should_refresh_retry` excludes it, because the harm is the
    ///   `engine.refresh` server-side token rotation one step EARLIER — which the re-poll cannot
    ///   undo — not the re-poll itself, issue #253.)
    /// - The refresh itself **errors** → "could not revive"; fail-safe by letting the 401
    ///   stand. A refresh failure never crashes the poll loop.
    ///
    /// Every firing pushes ONE durable [`Event::PollRefresh`] onto `events` (issue #255): the
    /// isolated-refresh ACTION the durable log previously lacked — until now only the DOWNSTREAM
    /// poll outcome reached it (via [`note_poll_outcome`](Self::note_poll_outcome)), so the log
    /// showed a `CredentialDead` edge but not the poll-refresh that preceded it.
    async fn refresh_retry(&self, i: usize, events: &mut Vec<Event>) -> Result<Usage> {
        let refreshed = match self.poll_refresh.as_ref() {
            Some(engine) => engine.refresh(&self.roster[i]).await,
            // Unreachable given the `should_refresh_retry` guard; treat as could-not-revive.
            None => return Err(Error::UsageUnauthorized),
        };
        // Durably record the isolated poll-refresh ACTION (issue #255): the fact it fired, its
        // target PARKED account (redacted handle), and the classified outcome — the forensic
        // trail the transient `diag=` line alone did not leave. Emitted for EVERY firing, BEFORE
        // the Dead / re-poll split below, so a genuine-death `Dead` is evented as surely as a
        // revive. A completed cycle maps through the shared `refresh_event_outcome` (the same
        // vocabulary the periodic #106 `event=refresh` uses); an engine that could not even run
        // (`Err`) is an `Error` outcome — mirroring `refresh_tick`'s `error_refresh_event`.
        events.push(Event::PollRefresh {
            account: self.roster[i].label.clone(),
            outcome: match &refreshed {
                Ok(report) => refresh_event_outcome(report),
                Err(_) => RefreshEventOutcome::Error,
            },
            // The AC-3 rotation flag on the poll path (issue #279): the completed cycle's
            // own signal; an engine that could not even run (`Err`) renders `false`.
            refresh_token_rotated: match &refreshed {
                Ok(report) => report.refresh_token_rotated,
                Err(_) => false,
            },
        });
        match refreshed {
            // The refresh token was cleared in place → genuinely dead: let the 401 stand.
            Ok(report) if report.outcome == RefreshOutcome::Dead => Err(Error::UsageUnauthorized),
            // A fresh token may now be stashed → probe liveness through the stash.
            Ok(_) => self
                .poller
                .poll(&self.roster[i], false)
                .await
                .map(|reading| reading.usage),
            // Could not revive (spawn / read-back failure) → fail-safe: the 401 stands.
            Err(_) => Err(Error::UsageUnauthorized),
        }
    }

    /// Whether the active poll `i`'s outcome warrants a #282 REACTIVE keep-warm backstop,
    /// evaluated on the PRE-fold state (before [`note_poll_outcome`](Self::note_poll_outcome)
    /// advances the streak) — the ACTIVE-account counterpart of
    /// [`should_refresh_retry`](Self::should_refresh_retry):
    ///
    /// - a keep-warm seam is wired ([`with_keep_warm_engine`](Self::with_keep_warm_engine)),
    /// - the poll was a 401 ([`PollOutcome::Unauthorized`]),
    /// - the account is not already quarantined (a dead account is left to the #42 streak /
    ///   emergency swap — never re-warmed on every re-probe poll),
    /// - this is the FIRST 401 of the current streak episode (`consec_401 == 0`), and
    /// - the account IS the ACTIVE one (`state.active == Some(i)`).
    ///
    /// The last clause is the EXACT complement of `should_refresh_retry`'s active-EXCLUSION
    /// (issue #253): the #162 isolated engine writes the STASH, so it must never touch the active
    /// account; this keep-warm mints and PROMOTES to the canonical item a live session reads, so
    /// it is the ONE path that legitimately targets the active account. The two are therefore
    /// mutually exclusive on `i` and wired as an `if / else if` in [`tick`](Self::tick), so a 401
    /// takes exactly one refresh path. `consec_401 == 0` is the same once-per-episode storm guard:
    /// the first active 401 attempts the in-place revive; the rest of the episode advances the
    /// streak directly toward the #42 emergency swap.
    fn should_keep_warm_retry(&self, i: usize, result: &Result<Usage>) -> bool {
        self.keep_warm.is_some()
            && matches!(classify_poll(result), PollOutcome::Unauthorized)
            && !self.state.health[i].quarantined
            && self.state.health[i].consec_401 == 0
            && self.state.active == Some(i)
    }

    /// The REACTIVE keep-warm backstop (issue #282): on the active account's FIRST usage-401,
    /// mint a fresh token in place and PROMOTE it to the canonical item, then re-poll the active
    /// account THROUGH the (now-fresh) canonical — the reading
    /// [`note_poll_outcome`](Self::note_poll_outcome) then folds into the streak. Only called when
    /// [`should_keep_warm_retry`](Self::should_keep_warm_retry) holds, so `keep_warm` is `Some` and
    /// `i` is the ACTIVE account.
    ///
    /// - A successful promote + a re-poll that CLEARS → `Ok(usage)` resets the streak (the
    ///   false-death this fixes: an expired-but-refreshable active token is revived in place before
    ///   it counts toward the #42 quarantine).
    /// - No promote (`NoChange` / a dead-or-absent refresh token / an engine error / a swap that
    ///   raced the mint) → the 401 STANDS (`Err(UsageUnauthorized)`), so the streak advances toward
    ///   quarantine → the #42 emergency swap. This is invariant 4: a truly-dead active credential
    ///   still quarantines and the escape to a live spare is preserved.
    /// - A re-poll that 401s AGAIN even after a fresh token → the 401 stands (a genuine problem the
    ///   fresh token did not fix); the streak advances.
    ///
    /// `canonical` is the blob read once at top-of-tick; `None` (unreadable) → nothing to mint
    /// from, fail-safe to the 401. The mint never crashes the poll loop (a spawn / FS failure is
    /// an `Err` the keep-warm engine swallows into a no-promote).
    async fn keep_warm_retry(
        &mut self,
        i: usize,
        canonical: Option<&Credential>,
        events: &mut Vec<Event>,
    ) -> Result<Usage> {
        // No readable canonical blob → cannot mint; fail-safe, let the 401 stand.
        let Some(canonical) = canonical else {
            return Err(Error::UsageUnauthorized);
        };
        if self
            .keep_warm_and_promote(i, canonical, KeepWarmTrigger::Reactive, events)
            .await
        {
            // The canonical now holds a fresh token → re-poll the ACTIVE account through it.
            self.poller
                .poll(&self.roster[i], true)
                .await
                .map(|reading| reading.usage)
        } else {
            // No fresh token promoted → the 401 stands so the #42 streak advances (invariant 4).
            Err(Error::UsageUnauthorized)
        }
    }

    /// The PROACTIVE keep-warm (issue #282): when the active token is within its (staggered)
    /// near-expiry horizon, mint a fresh token in place and PROMOTE it to the canonical item —
    /// BEFORE any 401 — so a live session always reads a warm token and the overnight false-death
    /// cascade never starts. Serialized into [`tick`](Self::tick) just before
    /// [`decide_action`](Self::decide_action). Inert (an immediate return) unless the keep-warm
    /// seam is wired.
    ///
    /// Gates, in order (each a cheap check before the expensive `claude -p` mint):
    /// - the seam is wired, an active account resolved, and its canonical blob is readable;
    /// - the active account is NOT quarantined (a dead account is the streak's job, not re-warmed);
    /// - the token is within `[refresh].cadence_secs + `[`keep_warm_stagger_secs`]` of expiry (the
    ///   per-account stagger de-correlates the roster's mints across the shared ~8h TTL); and
    /// - the proactive per-account throttle has elapsed (`last_keep_warm_attempt`), so a persistently
    ///   no-op mint (CC declines to refresh) cannot spawn `claude -p` every tick in the window.
    ///
    /// `now_ms` is the wall-clock epoch-ms the horizon compares the token's `expiresAt` against,
    /// taken as a parameter (not read inside) so the gate is unit-tested deterministically. Unlike
    /// the reactive path there is NO re-poll: a proactive promote simply leaves a fresh token for
    /// the NEXT tick's poll to read.
    async fn keep_active_warm(
        &mut self,
        active: Option<usize>,
        canonical: Option<&Credential>,
        now_ms: i64,
        events: &mut Vec<Event>,
    ) {
        if self.keep_warm.is_none() {
            return;
        }
        let (Some(i), Some(canonical)) = (active, canonical) else {
            return;
        };
        // A quarantined active account is a dead credential the #42 streak / emergency swap owns —
        // never re-warmed every tick (invariant 4; mirrors `should_keep_warm_retry`'s guard).
        if self.state.health[i].quarantined {
            return;
        }
        // Near-expiry gate: fire only inside the token's staggered horizon. An unreadable expiry
        // → skip (no basis to decide); a far-from-expiry token → skip (nothing to warm yet).
        let Some(expires_at_ms) = crate::refresh::expires_at(canonical.expose()) else {
            return;
        };
        let stagger = keep_warm_stagger_secs(&self.roster[i].account_uuid, self.keep_warm_cadence);
        let horizon_ms = i64::try_from(self.keep_warm_cadence.as_secs().saturating_add(stagger))
            .unwrap_or(i64::MAX)
            .saturating_mul(1000);
        if expires_at_ms.saturating_sub(now_ms) > horizon_ms {
            return;
        }
        // Proactive throttle: at most one mint per keep-warm cadence (the reactive path ignores
        // this — it is once-per-episode-gated by `consec_401 == 0` — but a reactive mint still
        // stamps `last_keep_warm_attempt`, so it suppresses a redundant proactive mint the same
        // window).
        let now = self.clock.now();
        if let Some(last) = self.state.health[i].last_keep_warm_attempt {
            if now.saturating_duration_since(last) < self.keep_warm_cadence {
                return;
            }
        }
        // The proactive path discards the promote result: it warms the canonical for the NEXT
        // poll to read, it does not re-poll now.
        let _ = self
            .keep_warm_and_promote(i, canonical, KeepWarmTrigger::Proactive, events)
            .await;
    }

    /// Mint a fresh token for the active account `i` from its `canonical` blob and, on a real
    /// refresh, PROMOTE it to the canonical item (issue #282) — the shared core of the proactive
    /// and reactive paths. Returns whether a fresh token was actually promoted to canonical.
    ///
    /// Steps: (1) short-circuit a dead/absent refresh token — CC has nothing to exchange, so skip
    /// the doomed spawn and report no-promote (the caller lets the #42 streak advance: invariant
    /// 4); (2) stamp `last_keep_warm_attempt` (the proactive throttle + reactive-suppresses-proactive
    /// signal) BEFORE the mint, so even a could-not-run attempt counts; (3) drive the keep-warm
    /// engine to mint; (4) push ONE durable [`Event::KeepWarm`] recording the action (mirrors
    /// [`refresh_retry`](Self::refresh_retry)'s `PollRefresh` event); (5) promote ONLY a real mint
    /// ([`RefreshOutcome::Refreshed`] → `Some(credential)`) via
    /// [`promote_canonical`](Self::promote_canonical); every other outcome
    /// (`NoChange` / `Dead` / `Error` / could-not-run) leaves the canonical item untouched.
    async fn keep_warm_and_promote(
        &mut self,
        i: usize,
        canonical: &Credential,
        trigger: KeepWarmTrigger,
        events: &mut Vec<Event>,
    ) -> bool {
        // A dead (empty) or absent refresh token cannot be revived by ANY mint — skip the doomed
        // `claude -p` spawn and report no-promote so the caller lets the #42 streak advance to
        // quarantine → emergency swap (invariant 4). Only a NON-empty RT is worth minting.
        if !has_live_refresh_token(canonical) {
            return false;
        }
        // Stamp the attempt up front so BOTH the proactive throttle and the
        // reactive-suppresses-a-same-window-proactive signal count even a mint that cannot run.
        self.state.health[i].last_keep_warm_attempt = Some(self.clock.now());
        let minted = match self.keep_warm.as_ref() {
            Some(engine) => engine.keep_warm(&self.roster[i], canonical).await,
            // Unreachable given the callers' `keep_warm.is_some()` gate; treat as no-promote.
            None => return false,
        };
        // Durably record the keep-warm ACTION (issue #282), for EVERY firing — the forensic trail
        // mirroring `refresh_retry`'s `PollRefresh`. A completed cycle maps through the shared
        // `refresh_event_outcome`; a cycle that could not even run (`Err`) is an `Error` outcome.
        events.push(Event::KeepWarm {
            account: self.roster[i].label.clone(),
            trigger,
            outcome: match &minted {
                Ok((report, _)) => refresh_event_outcome(report),
                Err(_) => RefreshEventOutcome::Error,
            },
            refresh_token_rotated: match &minted {
                Ok((report, _)) => report.refresh_token_rotated,
                Err(_) => false,
            },
        });
        // Promote ONLY a real mint; NoChange / Dead / Error / could-not-run leave canonical as-is.
        match minted {
            Ok((_, Some(cred))) => self.promote_canonical(i, &cred).await.unwrap_or(false),
            _ => false,
        }
    }

    /// Promote a freshly-minted `cred` to the canonical `Claude Code-credentials` item for the
    /// active account `i` (issue #282), serialized against the swap engine (ADR-0003 no-torn-swap).
    /// Returns whether the canonical was actually written (`Ok(false)` = a deliberate abort, not an
    /// error).
    ///
    /// The mint's `claude -p` spawn ran WITHOUT the swap lock (holding it across a multi-second
    /// spawn would stall every swap), so a concurrent `use` / auto swap could have moved the active
    /// account meanwhile. Under the SAME single-writer swap lock the swap engine holds, this
    /// RE-READS the canonical and confirms it still resolves to account `i` BEFORE overwriting:
    /// promoting a now-stale, account-`i`-derived token would CLOBBER that operator swap, so a
    /// changed active identity ABORTS with zero writes (the minted token is simply discarded; the
    /// #13/#42 recovery path reclaims a stranded credential if it ever matters). On the happy path
    /// the write is the keychain's atomic `add-generic-password -U` (a live session's next read
    /// sees the fresh token whole, never torn), then the canonical-watch baseline is committed so
    /// the #140 external-login watch and the next-tick #13 reconcile do NOT misfire on the daemon's
    /// OWN write. A contended lock exhausting its bounded wait fails closed (`Err(SwapLockBusy)`) —
    /// the caller treats it as no-promote, exactly like any other swap-lock refusal.
    async fn promote_canonical(&mut self, i: usize, cred: &Credential) -> Result<bool> {
        // Serialize against the swap engine: take the SAME single-writer swap lock `use` / auto
        // swaps hold (when one is configured — hermetic tests run lock-free, no second writer).
        let _guard = match self.swap_lock_path.as_deref() {
            Some(path) => Some(SwapLock::acquire(path, SWAP_LOCK_MAX_WAIT).await?),
            None => None,
        };
        // Re-read UNDER THE LOCK and confirm account `i` is still active before overwriting — the
        // mint ran unlocked, so a swap may have raced it. A changed / unreadable canonical aborts
        // with zero writes rather than clobber a concurrent swap.
        let still_active = match self.store.read().await {
            Ok(current) => self.resolve_account_for(&current).await == Some(i),
            Err(_) => false,
        };
        if !still_active {
            return Ok(false);
        }
        // Atomic canonical write; the live session's next read sees the fresh token whole.
        self.store.write(cred).await?;
        // Baseline-commit so #140 / the #13 reconcile do not misfire on the daemon's own write.
        self.state.canonical_watch.commit(cred);
        Ok(true)
    }

    /// The per-roster-index enabled (in-rotation, issue #36) mask `pick_target`
    /// consumes — a disabled account is never a viable swap target. Rebuilt per call
    /// (the roster is small); shared by the normal and the #42 emergency swap path.
    fn enabled_mask(&self) -> Vec<bool> {
        self.roster.iter().map(|account| account.enabled).collect()
    }

    /// Decide what to do about the active account this cycle, performing the swap
    /// if one is warranted. Returns the per-cycle verdict.
    async fn decide_action(
        &mut self,
        at: Instant,
        active: Option<usize>,
        readings: &[Option<Usage>],
        events: &mut Vec<Event>,
    ) -> TickAction {
        // No identifiable active account → poll-only (never swap on an unknown
        // active account: it is missing data about WHO to swap away from).
        let Some(active_idx) = active else {
            return TickAction::SkippedActiveUnknown;
        };
        // The active account's credential is DEAD (quarantined, #42) — distinct from
        // a transient skip below. Two sub-cases, by whether it polled this cycle:
        if self.state.health[active_idx].quarantined {
            match readings[active_idx] {
                // Still failing (no reading) → the live session is blocked. Escape it
                // with an emergency swap, bypassing the swap-away trigger AND cooldown.
                None => return self.emergency_swap(at, active_idx, readings, events).await,
                // Polling live again → the credential is spontaneously reviving (a
                // re-login would already have un-quarantined it upstream in
                // reconcile_canonical_change, #107; note_poll_outcome counts these live
                // polls toward the M-poll restore). Hold: never swap away mid-recovery,
                // never emergency-swap one that now works.
                Some(_) => return TickAction::Held,
            }
        }
        // The active account's own reading is unavailable (transient / a 401 below the
        // dead threshold / unreadable) → skip; never swap on missing data.
        let Some(active_usage) = readings[active_idx] else {
            return TickAction::SkippedActiveUnavailable;
        };
        // Draw this cycle's swap-away triggers (issues #38, #41): each jittered +
        // clamped to 50..=99 percent, then to a fraction for the decision. The
        // session and weekly triggers are independent — swap when EITHER dimension
        // reaches its own; below BOTH → hold. Both are drawn every cycle (a fixed
        // strategy consumes no RNG), keeping the per-cycle draw order deterministic.
        let session_trigger =
            self.trigger_strategy
                .draw(&mut self.rng, TRIGGER_PCT_LO, TRIGGER_PCT_HI)
                / 100.0;
        let weekly_trigger = self.weekly_trigger_strategy.draw(
            &mut self.rng,
            WEEKLY_TRIGGER_PCT_LO,
            WEEKLY_TRIGGER_PCT_HI,
        ) / 100.0;
        if swap::decide(&active_usage, session_trigger, weekly_trigger) == SwapDecision::Hold {
            return TickAction::Held;
        }
        // Over the trigger — but until the staggered loop has polled every account in
        // the rotation at least once (issue #80 warm-up), the carried readings are
        // still partial: a swap now could pick a suboptimal target (a soonest-reset
        // account not yet polled, #37) or declare a spurious `all_exhausted` when an
        // unpolled account is in fact viable (#11). HOLD until the first cycle
        // completes, then decide on the full last-known set. (Emergency swaps away from
        // a confirmed-dead active are NOT gated — they take a separate path above and
        // self-correct by retrying as targets become known.)
        if !self.state.warmed_up {
            return TickAction::Held;
        }
        // Over the trigger. Cooldown (#10): refuse a re-swap until this cycle's
        // (jittered) cooldown has elapsed since the last swap. This PACES swaps; it
        // cannot by itself stop a ping-pong with a persistent cause — the always-on
        // session gate in `pick_target` is what prevents two session-saturated accounts
        // from oscillating (it excludes each as a target of the other).
        let cooldown = Duration::from_secs_f64(self.cooldown_strategy.draw(
            &mut self.rng,
            COOLDOWN_SECS_LO,
            COOLDOWN_SECS_HI,
        ));
        if let Some(last) = &self.state.last_swap {
            if at.saturating_duration_since(last.at) < cooldown {
                return TickAction::SkippedCooldown;
            }
        }
        // Pick the viable target whose weekly quota resets soonest (issue #37). A
        // disabled (parked) account is not viable (issue #36), and a weekly-exhausted
        // account is not viable (#11) — so when every ENABLED other account is
        // weekly-exhausted this returns `None`. A disabled account, even with weekly
        // headroom, never counts, so it cannot hold the daemon out of the
        // all-exhausted terminal state (#11).
        let Some(target_idx) = pick_target(
            active_idx,
            readings,
            &self.enabled_mask(),
            Some(self.target_max_usage),
            session_trigger,
            weekly_trigger,
        ) else {
            // No viable target — every other account is weekly-exhausted, session-
            // saturated (over the always-on session gate), or over the default-on floor.
            // The all-exhausted TERMINAL state (issue #11): HOLD, do NOT swap (swapping
            // among exhausted accounts only thrashes), and emit ONE edge-triggered
            // signal naming the least-bad account and WHY relief is blocked
            // (`cause=session|weekly`), so the operator knows when relief arrives. When
            // the block is session-wide (a weekly-viable account held out only by
            // session), relief arrives at the sooner SESSION reset and the hint keys off
            // it (issue #398, the acknowledged follow-up); otherwise it is the weekly
            // reset (#11). The active account is left exactly as is. The signal is
            // edge-triggered: emit only on ENTERING the state, so the payload is computed
            // once per episode, not every poll while it holds.
            if !self.state.signaled_all_exhausted {
                let session_ceiling = session_trigger.min(self.target_max_usage);
                let (cause, hold_idx, resets_at) = all_exhausted_relief(
                    active_idx,
                    readings,
                    &self.enabled_mask(),
                    session_ceiling,
                    weekly_trigger,
                );
                events.push(Event::AllExhausted {
                    hold: self.roster[hold_idx].label.clone(),
                    cause,
                    resets_at,
                });
                self.state.signaled_all_exhausted = true;
            }
            return TickAction::NoViableTarget;
        };
        // Run the out-of-band swap, serialized by the single-writer lock (#64). #6
        // is no-half-swap: an error (including a contended lock that fails closed)
        // leaves the canonical item and both stashes coherent, so we simply retry
        // next cycle.
        let outgoing = self.roster[active_idx].stash();
        let incoming = self.roster[target_idx].stash();
        match self.locked_swap(&outgoing, &incoming).await {
            Ok(_report) => {
                self.record_swap(target_idx, &incoming, at).await;
                // Log the swap (issue #9). `swap::decide` returns only a binary
                // verdict, so the reason is re-derived here from the active reading:
                // session-first when BOTH dimensions are over their (this-cycle)
                // triggers. `session_pct` reuses `to_pct` so the log agrees with the
                // percentage `status` shows for the same reading.
                let reason = if active_usage.session >= session_trigger {
                    SwapReason::Session
                } else {
                    SwapReason::Weekly
                };
                events.push(Event::Swap {
                    from: self.roster[active_idx].label.clone(),
                    to: self.roster[target_idx].label.clone(),
                    reason,
                    session_pct: to_pct(active_usage.session),
                });
                TickAction::Swapped {
                    from: active_idx,
                    to: target_idx,
                }
            }
            Err(_) => TickAction::SwapFailed,
        }
    }

    /// Record a completed swap to `target_idx` (its incoming stash named `incoming`):
    /// update the cached active index, the post-swap cooldown floor + `status`
    /// display (#8), and prime the canonical watch with the token just promoted, so
    /// this OWN write is not re-detected as an out-of-band change next cycle (#13).
    /// Read the token back from the incoming stash (which still holds it) rather than
    /// re-reading the canonical: if a third writer changed the canonical after our
    /// write, committing the token we INTENDED leaves that change to be detected and
    /// re-stashed next cycle, instead of silently adopting the intruder. Shared by
    /// the normal swap and the emergency swap (#42).
    async fn record_swap(&mut self, target_idx: usize, incoming: &str, at: Instant) {
        self.state.active = Some(target_idx);
        self.state.last_swap = Some(LastSwap { at });
        if let Ok(incoming_stashed) = self.stash.read(incoming).await {
            self.state
                .canonical_watch
                .commit(&incoming_stashed.credential);
        }
    }

    /// Run one out-of-band swap, serialized by the single-writer swap lock (issue
    /// #64) when one is configured ([`with_swap_lock`](Self::with_swap_lock)). The
    /// lock makes the daemon's swap and a concurrent manual `use` swap mutually
    /// exclusive over the two-step write. A contended acquire that exhausts its
    /// bounded wait fails closed (`Err(SwapLockBusy)`) — the caller treats it like
    /// any other swap failure ([`TickAction::SwapFailed`]) and retries next cycle,
    /// never a torn write. With no lock configured (hermetic tests) the swap runs
    /// unlocked — there is no second writer in-process to serialize against.
    async fn locked_swap(&self, outgoing: &str, incoming: &str) -> Result<swap::SwapReport> {
        swap::swap_locked(
            self.swap_lock_path
                .as_deref()
                .map(|path| (path, swap::SWAP_LOCK_MAX_WAIT)),
            &self.store,
            &self.stash,
            outgoing,
            incoming,
            &self.claude_json,
        )
        .await
    }

    /// Drop an in-flight recovery probe on an account that just LEFT the active slot
    /// via a manual `use` swap (issue #108). The spontaneous-revival recovery in
    /// [`note_poll_outcome`](Self::note_poll_outcome) advances ONLY while an account is
    /// ACTIVE: a quarantined NON-active account is excluded from the poll schedule
    /// ([`build_poll_schedule`](Self::build_poll_schedule)) — the deliberately-tested
    /// `a_dead_spare_is_never_polled_so_it_cannot_spuriously_recover` invariant. So when
    /// the operator swaps AWAY from an account mid-recovery (`quarantined &&
    /// recovery_successes > 0`), its probe would otherwise FREEZE below
    /// `monitor_recovery_m` forever — a phantom partial-progress counter that can never
    /// complete, leaving the account durably `needs re-login` while LOOKING mid-recovery.
    /// Reset the probe to 0 so the state is HONEST: a non-active quarantined account is
    /// simply a dead spare like any other, with no recovery in flight.
    ///
    /// This does NOT poll or otherwise recover the de-activated account — by design a
    /// non-active quarantined account recovers only by becoming ACTIVE again (the
    /// operator `use`s it back, or the daemon emergency-swaps to it as the last viable
    /// target, and the M-poll probe then runs from scratch) OR by a re-login, which
    /// un-quarantines on the spot in
    /// [`reconcile_canonical_change`](Self::reconcile_canonical_change) (issue #107).
    /// Those are the two recovery paths a dead spare has always had; #108 only removes
    /// the misleading frozen counter, it does not add a third path. The AC1 split is
    /// therefore: a *refreshed* (byte-changed) valid credential recovers via the #107
    /// canonical-change path the moment it becomes canonical; this reset governs the
    /// *same-bytes* spontaneous-revival case, where there is NO canonical change to react
    /// to and re-probing a never-changed dead token would just re-confirm it dead (why a
    /// "re-probe the stash" mechanism would not have helped this case).
    ///
    /// Invoked on BOTH manual-swap active transitions so the invariant lives at the
    /// transition, not at one caller: [`adopt_manual_swap`](Self::adopt_manual_swap) (the
    /// control-socket path, which commits the canonical-watch baseline and so is NOT
    /// re-observed by `reconcile_canonical_change`) and
    /// [`reconcile_canonical_change`](Self::reconcile_canonical_change) (the
    /// daemon-notices-it-itself fallback). The daemon's OWN swaps cannot trigger it: an
    /// auto-swap away from a recovering active account is HELD
    /// ([`decide_action`](Self::decide_action)), and an emergency swap fires only when the
    /// active reading is absent — a non-live poll that already reset the probe. The
    /// `recovery_successes > 0` guard makes it a safe no-op on any other transition (a
    /// healthy account leaving the slot, or the same account staying active).
    fn deactivate_recovery_probe(&mut self, prev: Option<usize>, next: Option<usize>) {
        let Some(prev) = prev else { return };
        // The SAME account staying active (an in-place refresh / re-login of it) is not
        // a swap-away — leave any probe untouched (a re-login is handled upstream).
        if next == Some(prev) {
            return;
        }
        let health = &mut self.state.health[prev];
        if health.quarantined && health.recovery_successes > 0 {
            health.recovery_successes = 0;
        }
    }

    /// Adopt a manual `use` swap signalled over the control socket (issue #64).
    ///
    /// `use` rewrote the canonical credential out-of-band and then notified us; this
    /// records it as the latest swap so the EXISTING post-swap cooldown (#10)
    /// applies — the very next poll therefore HOLDS on the operator's choice instead
    /// of immediately reverting it, and normal policy resumes once the cooldown
    /// window elapses (a cooldown, never a permanent pin). The active account is
    /// re-resolved from the AUTHORITATIVE canonical item, not from the message — the
    /// signal carries no target — so an out-of-order or duplicate notification
    /// cannot corrupt state; at worst it re-arms a cooldown. Mirrors
    /// [`record_swap`](Self::record_swap): update active, arm the cooldown + `status`
    /// display, and prime the canonical watch so this manual write is not later
    /// re-detected as an out-of-band change (#13).
    async fn adopt_manual_swap(&mut self) {
        let at = self.clock.now();
        // Re-resolve active from the canonical item and prime the watch. A locked /
        // unreadable keychain leaves active to the next tick's own resolve, but the
        // cooldown is armed regardless below — the load-bearing manual-hold effect.
        if let Ok(canonical) = self.store.read().await {
            let prev_active = self.state.active;
            let next_active = self.resolve_account_for(&canonical).await;
            self.state.active = next_active;
            // If this manual swap moved AWAY from an account that was mid-recovery, drop
            // its now-frozen recovery probe so its dead-spare state is honest (issue
            // #108). This is the load-bearing seam: `adopt_manual_swap` commits the
            // canonical-watch baseline below, so `reconcile_canonical_change` will see
            // this write as `Unchanged` and never re-observe it.
            self.deactivate_recovery_probe(prev_active, next_active);
            self.state.canonical_watch.commit(&canonical);
        }
        // Record it as the latest swap: arms the cooldown (#10). The cooldown arming
        // is what makes a manual choice stick, so it happens even when the active
        // account could not be resolved just now.
        self.state.last_swap = Some(LastSwap { at });
    }

    /// Perform a socket `swap` command (issue #167) against the daemon's live state, returning the
    /// redacted [`SwapAck`] plus any durable [`Event`] to log — WITHOUT touching the socket, so the
    /// re-validation + write are unit-testable apart from the ack I/O (the run loop writes the ack
    /// via [`write_swap_ack`]). Runs where `&mut Daemon` is available (the
    /// run loop's post-idle), because the ack must reflect the REAL outcome (accepted /
    /// rejected-with-reason).
    ///
    /// The daemon re-validates the target's viability from its OWN state — it resolves the handle
    /// against its current roster and reads `quarantined` / weekly-exhaustion / cooldown from its
    /// health + readings — NEVER a client hint (a client-side "greyed out" is UX only). `force` is
    /// POLICY-only ([`swap_command_verdict`]): it bypasses those gates, but it can never manufacture
    /// an outgoing account and never reaches the SAFETY aborts, which live in the swap engine below
    /// — the write goes through the SAME single-writer swap lock (issue #64) the auto-swaps use, so
    /// a contended lock fails closed ([`SwapRejection::SwapLockBusy`]), and the engine's step-1 read
    /// aborts on a LOCKED keychain ([`SwapRejection::KeychainLocked`]) even under `force`. The
    /// single-owner guard is upheld by CONSTRUCTION: the daemon (holding the single-instance lock)
    /// is the SOLE writer, and this command routes the write THROUGH it rather than spawning a
    /// second one.
    ///
    /// On a completed swap it mirrors [`record_swap`](Self::record_swap): caches active, arms the
    /// post-swap cooldown (#10) so the next poll HOLDS on the operator's choice, primes the
    /// canonical watch, drops a now-frozen recovery probe on the account swapped AWAY from (issue
    /// #108, the same transition invariant [`adopt_manual_swap`](Self::adopt_manual_swap) upholds
    /// for the standalone-`use` path), and returns the SAME `Event::Swap` (`Manual` / `Forced`) the
    /// standalone `use` emits — so the cooldown derived from the durable log agrees whichever path
    /// wrote.
    async fn perform_socket_swap(&mut self, command: &SwapCommand) -> (SwapAck, Option<Event>) {
        // 1. Resolve the target handle (label OR uuid) against the CURRENT roster — the daemon's
        //    OWN resolution, never a client-provided index; it never guesses (issue #17).
        let target_idx = match crate::use_account::resolve_target(&self.roster, &command.target) {
            Ok(idx) => idx,
            Err(Error::UseTargetAmbiguous { .. }) => {
                return (
                    SwapAck::Rejected {
                        reason: SwapRejection::AmbiguousTarget,
                    },
                    None,
                )
            }
            // Not found (or any other resolve failure) → unknown target.
            Err(_) => {
                return (
                    SwapAck::Rejected {
                        reason: SwapRejection::UnknownTarget,
                    },
                    None,
                )
            }
        };

        // 2. Re-validate viability from the daemon's LIVE state (health + last readings + the
        //    un-jittered weekly threshold + the in-memory last-swap), never a client hint. The
        //    `weekly_exhausted` computation is EXACTLY the snapshot's per-account verdict
        //    (`weekly >= weekly_trigger_base`) — same data source AND formula, so the ack agrees with
        //    what `status` shows. This is the daemon's LAST-KNOWN reading (≤ one poll interval old),
        //    NOT the fresh poll the daemon-DOWN `use` runs: a target that crossed the threshold since
        //    the last poll can be accepted here. The divergence is bounded and self-correcting — the
        //    very next tick polls the now-active target and swaps away if it is truly exhausted — and
        //    it deliberately keeps this re-validation off the network so it never blocks the
        //    single-thread run loop (ADR-0001). `force` overrides it either way.
        let now = self.clock.now();
        let quarantined = self.state.health[target_idx].quarantined;
        let weekly_exhausted = self.state.last_readings[target_idx]
            .is_some_and(|usage| usage.weekly >= self.weekly_trigger_base);
        let in_cooldown = self
            .state
            .last_swap
            .as_ref()
            .is_some_and(|last| now.saturating_duration_since(last.at) < self.cooldown_base);

        match swap_command_verdict(
            target_idx,
            self.state.active,
            quarantined,
            weekly_exhausted,
            in_cooldown,
            command.force,
        ) {
            SwapVerdict::AlreadyActive => (
                SwapAck::AlreadyActive {
                    to: self.roster[target_idx].label.clone(),
                },
                None,
            ),
            SwapVerdict::Reject(reason) => (SwapAck::Rejected { reason }, None),
            // The verdict returns `Swap` only when an active account exists (it rejects
            // `NoActiveAccount` otherwise); re-match defensively rather than `expect` on the
            // long-running daemon path.
            SwapVerdict::Swap => match self.state.active {
                None => (
                    SwapAck::Rejected {
                        reason: SwapRejection::NoActiveAccount,
                    },
                    None,
                ),
                Some(active_idx) => {
                    let outgoing = self.roster[active_idx].stash();
                    let incoming = self.roster[target_idx].stash();
                    // The SAME lock-wrapped engine (#64) the auto-swaps use: #6 is no-half-swap, so
                    // an error (a contended lock that fails closed, a locked keychain) leaves the
                    // canonical item and both stashes coherent — ZERO writes — and becomes a
                    // redacted rejection.
                    match self.locked_swap(&outgoing, &incoming).await {
                        Ok(_report) => {
                            let prev_active = self.state.active;
                            // Mirror the auto-swap tail: cache active, arm the cooldown, prime the
                            // canonical watch (so this write is not re-detected as out-of-band #13).
                            self.record_swap(target_idx, &incoming, now).await;
                            // An operator-driven swap can move AWAY from a mid-recovery account
                            // (unlike the auto-swap, which HOLDS on one): drop its now-frozen probe
                            // so its dead-spare state is honest (issue #108).
                            self.deactivate_recovery_probe(prev_active, Some(target_idx));
                            let reason = if command.force {
                                SwapReason::Forced
                            } else {
                                SwapReason::Manual
                            };
                            let from = self.roster[active_idx].label.clone();
                            let to = self.roster[target_idx].label.clone();
                            // The SAME durable `Event::Swap` the standalone `use` emits (issue #9),
                            // so the log-derived cooldown agrees whichever path wrote. `session_pct`
                            // = 0: a manual/forced swap is not session-triggered (the reason
                            // distinguishes it). Non-secret handles only (issue #15).
                            let event = Event::Swap {
                                from: from.clone(),
                                to: to.clone(),
                                reason,
                                session_pct: 0,
                            };
                            (SwapAck::Accepted { from, to }, Some(event))
                        }
                        Err(err) => (
                            SwapAck::Rejected {
                                reason: classify_swap_failure(&err),
                            },
                            None,
                        ),
                    }
                }
            },
        }
    }

    /// Perform a socket `capture` command (issue #359) against the daemon's live state, returning
    /// the redacted [`CaptureAck`] plus the durable [`Event::Capture`] to log — WITHOUT touching the
    /// socket, so the read + stash + reconcile are unit-testable apart from the ack I/O (the run
    /// loop writes the ack via [`write_capture_ack`]). Runs where `&mut Daemon` is available (the run
    /// loop's post-idle), because the ack must reflect the REAL outcome (captured / refreshed /
    /// rejected). The daemon-routed sibling of [`perform_socket_swap`](Self::perform_socket_swap),
    /// mirroring it 1:1.
    ///
    /// The daemon does ALL the credential work itself — the client never touches a credential (the
    /// panel-originates-no-seam invariant, REQ-MBR-C-005). It reuses the #357
    /// [`capture_locked`](crate::capture::capture_locked) primitive with its OWN seams (the same
    /// `store` / `stash` / `claude_json` the swaps use), so the identity read → token read → stash
    /// write run under the SAME single-writer swap lock the auto-swaps use: a contended acquire fails
    /// closed ([`CaptureRejection::SwapLockBusy`]) BEFORE any read, and a LOCKED keychain aborts the
    /// token read ([`CaptureRejection::KeychainLocked`]) — the same safety aborts `force` can never
    /// bypass on the swap path. Capture is canonical-READ-ONLY: it never writes the canonical
    /// keychain item or `~/.claude.json`, only a per-account stash + a roster row, so a mid-write
    /// crash cannot corrupt the active account or the live session.
    ///
    /// After the locked stash lands, the new roster is persisted to the wired `config_path` (the
    /// authoritative on-disk `config.toml`, OUTSIDE the lock — a swap never contends on it,
    /// stash-before-roster like the standalone `capture`) and the in-memory rotation is reconciled to
    /// it ([`reconcile_roster`](Self::reconcile_roster), the SAME core the #139 roster-reload
    /// drives): an already-rostered active account is an idempotent REFRESH (its per-account state
    /// preserved, NO duplicate row), and a newly-captured one joins the live rotation without a
    /// restart. The daemon-`None` `config_path` (the hermetic-test default with no reload wired)
    /// fails closed — the capture cannot be persisted, so nothing is stashed-then-lost.
    async fn perform_socket_capture(
        &mut self,
        command: &CaptureCommand,
    ) -> (CaptureAck, Option<Event>) {
        // The authoritative on-disk roster path — required to persist the capture. Production always
        // wires it (`with_config_path`); with none wired the daemon cannot persist the new roster, so
        // fail closed BEFORE any read rather than land a stash + in-memory row the next restart loses.
        let Some(config_path) = self.config_path.clone() else {
            return self.capture_failure(command, CaptureRejection::Failed);
        };
        // Load the existing roster to plan against (absent → a first capture, malformed → a failure)
        // — read BEFORE the lock, exactly like the standalone `capture`'s `load_existing`.
        let existing = match Config::load_path(&config_path) {
            Ok(config) => Some(config),
            Err(Error::ConfigNotFound { .. }) => None,
            Err(err) => return self.capture_failure(command, classify_capture_failure(&err)),
        };
        // Reuse the #357 primitive with the daemon's OWN seams + swap lock. `None` lock is the
        // hermetic-test default (no second in-process writer to serialize against); production threads
        // the real `swap.lock` path, so a concurrent auto-swap cannot interleave with the two reads.
        let lock = self
            .swap_lock_path
            .as_deref()
            .map(|path| (path, SWAP_LOCK_MAX_WAIT));
        match crate::capture::capture_locked(
            lock,
            &self.store,
            &self.stash,
            &self.claude_json,
            existing,
            command.label.as_deref(),
        )
        .await
        {
            Ok(report) => {
                // Persist the new roster OUTSIDE the lock (a swap never contends on `config.toml`),
                // stash-before-roster. A save failure leaves an inert ORPHAN stash, never a partial
                // roster — report it `Failed` (the stash landed, but the roster row did not).
                if let Err(err) = report.config.save_to(&config_path) {
                    return self.capture_failure(command, classify_capture_failure(&err));
                }
                let crate::capture::CaptureReport {
                    config,
                    outcome,
                    label,
                    count,
                } = report;
                // Reconcile the in-memory rotation to the freshly-written roster (the SAME core the
                // #139 roster-reload drives): an already-rostered active account keeps its per-account
                // state (the idempotent refresh, no duplicate row), a new one joins with default state
                // and becomes a swap target once it has a reading.
                self.reconcile_roster(config.roster);
                // The durable audit line (best-effort logged by the run loop): the resolved roster
                // LABEL handle + the outcome token — non-secret by construction (#15).
                let event = Event::Capture {
                    account: Some(label.clone()),
                    outcome: capture_event_outcome(outcome),
                };
                let ack = match outcome {
                    crate::capture::CaptureOutcome::Captured => {
                        CaptureAck::Captured { label, count }
                    }
                    crate::capture::CaptureOutcome::Refreshed => {
                        CaptureAck::Refreshed { label, count }
                    }
                };
                (ack, Some(event))
            }
            Err(err) => self.capture_failure(command, classify_capture_failure(&err)),
        }
    }

    /// Build the redacted `(CaptureAck, Event)` for a REFUSED capture (issue #359): the bare machine
    /// `reason` on the ack, and the SAME reason folded onto the event's outcome axis. The event's
    /// handle is the operator's label HINT (the only handle a pre-stash failure has — the daemon
    /// never read an identity), or `None` when none was given, so the audit line still names WHY the
    /// capture failed without ever carrying a secret. A pure builder (no `&mut self` mutation), so
    /// the mapping is unit-testable and a refusal is a true no-op on the daemon's state.
    fn capture_failure(
        &self,
        command: &CaptureCommand,
        reason: CaptureRejection,
    ) -> (CaptureAck, Option<Event>) {
        let event = Event::Capture {
            account: command.label.clone(),
            outcome: capture_event_outcome_rejected(reason),
        };
        (CaptureAck::Rejected { reason }, Some(event))
    }

    /// Adopt a runtime roster-reload signalled over the control socket (issue #139).
    ///
    /// A roster write (`capture` / `login` / `remove`) committed a NEW `config.toml`
    /// on disk and notified us; re-read that authoritative file and reconcile the
    /// in-memory roster to it via [`reconcile_roster`](Self::reconcile_roster).
    /// BEST-EFFORT by contract, mirroring [`adopt_manual_swap`](Self::adopt_manual_swap):
    /// the on-disk file is authoritative, so a read failure — a malformed or briefly
    /// absent file — leaves the current in-memory roster INTACT and is logged, never
    /// fatal. A torn/partial read cannot occur: `Config::save` writes a temp file and
    /// `rename`s it over `config.toml` atomically, so this read observes either the
    /// whole old or the whole new file (issue #139 acceptance). A `None` `config_path`
    /// (the hermetic-test default) is a silent no-op.
    ///
    /// No lock is taken: the run loop drives `tick`, the control serve, and this
    /// adoption on a SINGLE task, so no daemon swap can interleave with the reconcile;
    /// and `config.toml` is written only by the CLI verbs (never by a daemon swap,
    /// which touches the keychain + `~/.claude.json`), so the re-read races nothing the
    /// daemon itself writes.
    async fn adopt_roster_reload(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return; // reload disabled (no config path wired) — nothing to do.
        };
        match Config::load_path(&path) {
            Ok(config) => self.reconcile_roster(config.roster),
            // Best-effort: keep the current in-memory roster on any read/parse failure
            // (a transient absent file, or a malformed edit) rather than dropping the
            // rotation. The next reload notification re-attempts.
            Err(err) => eprintln!("sessiometer: roster-reload skipped: {err}"),
        }
    }

    /// Reconcile the in-memory roster (and its per-account decision state) to
    /// `new_roster` — the pure core of the runtime roster-reload (issue #139),
    /// hermetically testable with no I/O.
    ///
    /// Accounts are matched by the immutable `account_uuid` (never by roster position,
    /// which shifts as accounts are added/removed):
    ///   - an account present in BOTH keeps its carried per-account state — health
    ///     (#42 quarantine/recovery streaks), the last-known usage reading (#80) and
    ///     the `polled_once` warm-up flag — so a `capture`/`login`/`remove` of ANOTHER
    ///     account never resets a healthy account's decision state;
    ///   - an account NEW on disk (an onboard, or a relogin of one never rostered) is
    ///     appended with DEFAULT state (unpolled, no reading, healthy) — it joins the
    ///     rotation and is polled on subsequent ticks, becoming a swap target only once
    ///     it has a reading;
    ///   - an account GONE from disk (a `remove`) is dropped along with its state.
    ///
    /// The active account is re-resolved by `account_uuid`: it keeps its (possibly
    /// shifted) new index when it persists, or becomes `None` when it was removed — the
    /// next [`tick`](Self::tick) then re-resolves active from the canonical credential,
    /// or polls-without-swapping if the active account is no longer rostered. The
    /// staggered poll schedule (#80) is reset (its entries were OLD roster indices);
    /// [`next_poll_index`](Self::next_poll_index) rebuilds it at the next cycle start.
    /// The warm-up latch (#80) is left as-is: once warmed up, a freshly-onboarded
    /// unpolled account is simply not yet a swap target (it has no reading), so it need
    /// not re-gate the whole rotation. State NOT indexed by roster position — the
    /// cooldown (#10), the canonical watch (#13), the tick counter — is deliberately
    /// untouched: a roster change is not a swap and must not re-arm or clear them.
    fn reconcile_roster(&mut self, new_roster: Vec<Account>) {
        // Capture the active account's identity from the CURRENT roster before it is
        // replaced, so active can be re-resolved by uuid against the new roster.
        let active_uuid = self
            .state
            .active
            .and_then(|i| self.roster.get(i))
            .map(|account| account.account_uuid.clone());

        // Re-key each account's carried decision state by uuid: preserve it for an
        // account that persists, default it for a newly-onboarded one. (Rosters are a
        // handful of accounts, so the per-account `position` scan is inconsequential.)
        let mut health = Vec::with_capacity(new_roster.len());
        let mut last_readings = Vec::with_capacity(new_roster.len());
        let mut polled_once = Vec::with_capacity(new_roster.len());
        for account in &new_roster {
            match self
                .roster
                .iter()
                .position(|old| old.account_uuid == account.account_uuid)
            {
                Some(old_idx) => {
                    health.push(self.state.health[old_idx].clone());
                    last_readings.push(self.state.last_readings[old_idx]);
                    polled_once.push(self.state.polled_once[old_idx]);
                }
                None => {
                    health.push(AccountHealth::default());
                    last_readings.push(None);
                    polled_once.push(false);
                }
            }
        }

        // Re-resolve active by uuid: kept (its new index) if it persists, else `None`.
        let active = active_uuid.and_then(|uuid| {
            new_roster
                .iter()
                .position(|account| account.account_uuid == uuid)
        });

        // Commit the reconciled roster + parallel state together, so no tick ever
        // observes a roster/state length mismatch.
        self.roster = new_roster;
        self.state.health = health;
        self.state.last_readings = last_readings;
        self.state.polled_once = polled_once;
        self.state.active = active;
        // The schedule held OLD roster indices; clear it so `next_poll_index` rebuilds a
        // fresh one (the active interleaved before each enabled non-quarantined peer,
        // #366) at the next cycle start.
        self.state.poll_schedule.clear();
        self.state.poll_pos = 0;
    }

    /// Emergency-swap away from a confirmed-DEAD active account (issue #42): the live
    /// session is blocked, so rotate to the soonest-reset viable target IMMEDIATELY —
    /// bypassing the swap-away trigger and the post-swap cooldown that gate a normal
    /// swap. Thrash-safe by construction: it fires ONLY on a quarantined active
    /// account, and a quarantined account is never itself a viable target (it is
    /// skipped in polling, so its reading is absent), so there is no ping-pong.
    /// `pick_target` (the #37 soonest-reset rule) still excludes disabled and
    /// weekly-exhausted accounts, but its always-on session gate is bypassed here
    /// (`f64::INFINITY`) — liveness beats session headroom when the active is dead;
    /// with no viable target the daemon holds on the dead
    /// active ([`TickAction::ActiveDeadNoTarget`]) — the `CredentialDead` signal
    /// already fired, so this stuck state is silent (no repeat-spam).
    async fn emergency_swap(
        &mut self,
        at: Instant,
        active_idx: usize,
        readings: &[Option<Usage>],
        events: &mut Vec<Event>,
    ) -> TickAction {
        // The weekly-exhaustion viability filter for `pick_target` — drawn like the
        // normal path (a fixed strategy consumes no RNG). The session swap-away
        // trigger and the cooldown are deliberately NOT consulted: an emergency swap
        // bypasses both (the active credential is dead, not merely over a trigger).
        let weekly_trigger = self.weekly_trigger_strategy.draw(
            &mut self.rng,
            WEEKLY_TRIGGER_PCT_LO,
            WEEKLY_TRIGGER_PCT_HI,
        ) / 100.0;
        let Some(target_idx) = pick_target(
            active_idx,
            readings,
            &self.enabled_mask(),
            // Drop the target-max-usage reserve on the emergency path (issue #398): the
            // active credential is DEAD, so liveness beats the reserve — escape to ANY
            // live account even if it is over the floor. Without this, a default-on floor
            // (#398) plus every live account at/above it would strand the daemon on the
            // dead active (`ActiveDeadNoTarget`) — a self-DoS. The dead active is
            // quarantined (never a viable target), so this cannot ping-pong; once a
            // session-fresh target exists the normal path's session gate moves off any
            // saturated account cleanly.
            None,
            // Also bypass the always-on session gate here (same liveness rationale):
            // escape even to an account over the session trigger.
            f64::INFINITY,
            weekly_trigger,
        ) else {
            return TickAction::ActiveDeadNoTarget;
        };
        // #6 is no-half-swap: an error (including a fail-closed contended swap lock,
        // #64) leaves the canonical item and both stashes coherent — the dead active
        // stays quarantined and the emergency swap retries next cycle.
        let outgoing = self.roster[active_idx].stash();
        let incoming = self.roster[target_idx].stash();
        match self.locked_swap(&outgoing, &incoming).await {
            Ok(_report) => {
                self.record_swap(target_idx, &incoming, at).await;
                events.push(Event::EmergencySwap {
                    from: self.roster[active_idx].label.clone(),
                    to: self.roster[target_idx].label.clone(),
                });
                TickAction::EmergencySwapped {
                    from: active_idx,
                    to: target_idx,
                }
            }
            Err(_) => TickAction::SwapFailed,
        }
    }

    /// The forward-looking next-swap candidate for the `status` display (issue #88):
    /// who [`pick_target`] would choose right now, or why there is no candidate. THE
    /// candidate is computed daemon-side — the CLI never re-derives the selection rule
    /// (it cannot: the wire carries only rounded percents, not the raw `Usage` /
    /// `target_max_usage` / triggers `pick_target` consumes). Uses the BASE (un-jittered)
    /// session and weekly triggers ([`Self::session_trigger_base`],
    /// [`Self::weekly_trigger_base`]) — the same thresholds the snapshot's per-account
    /// exhaustion flags key off — so the candidate and the displayed exhaustion state
    /// can never disagree, and the candidate does not flicker with the per-cycle
    /// swap-decision jitter.
    ///
    /// `None` only when there is no active account to swap FROM (no anchor). Otherwise
    /// the three cases mirror `pick_target`'s verdict: a viable [`NextSwap::Target`]; a
    /// [`NextSwap::NoViableTarget`] when readings are in hand but none qualifies (or no
    /// other enabled, non-quarantined account exists at all); and
    /// [`NextSwap::AwaitingData`] for the post-restart moment when such an account exists
    /// but none has a reading yet — the distinction #88 exists to draw.
    fn next_swap(&self, active: Option<usize>, readings: &[Option<Usage>]) -> Option<NextSwap> {
        let active_idx = active?;
        let enabled = self.enabled_mask();
        if let Some((target, reason)) = pick_target_with_reason(
            active_idx,
            readings,
            &enabled,
            Some(self.target_max_usage),
            self.session_trigger_base,
            self.weekly_trigger_base,
        ) {
            // Carry the daemon's own selection rationale (issue #393) alongside the label, so the
            // panel + `sessiometer status` render the reason the daemon actually used rather than a
            // client-re-derived (and, on the superseded "most headroom" axis, wrong) one.
            return Some(NextSwap::Target {
                to: self.roster[target].label.clone(),
                reason: Some(reason),
            });
        }
        // No viable target. Distinguish the post-restart "no readings yet" case (some
        // other enabled account exists, but none has been polled) from a genuine "all
        // exhausted / none enabled" verdict — drawing that line is the point of #88. A
        // QUARANTINED account (#42) is excluded from this tally: `decision_readings`
        // masks its reading to `None`, but a dead credential is NOT "data on the way"
        // (it needs a re-login), so counting it would mislabel an all-dead-spares roster
        // as `awaiting usage data` instead of the truthful `no viable target`.
        //
        // `all_unpolled` (EVERY live other unpolled), not "any unpolled", is deliberate:
        // in steady state a single transient `None` (one account's poll blipped) must not
        // flip the footer back to `awaiting usage data` while the others still hold
        // readings — so AwaitingData demands ALL live others be unpolled. The cost is a
        // brief, self-correcting `no viable target` during staggered warm-up (#80) once
        // the first spare is polled but a later one has not; acceptable, as the real swap
        // is itself warm-up-gated.
        let mut any_other_enabled = false;
        let mut all_unpolled = true;
        for (i, reading) in readings.iter().enumerate() {
            if i != active_idx && enabled[i] && !self.state.health[i].quarantined {
                any_other_enabled = true;
                all_unpolled &= reading.is_none();
            }
        }
        Some(if any_other_enabled && all_unpolled {
            NextSwap::AwaitingData
        } else {
            NextSwap::NoViableTarget
        })
    }

    /// The roster `account_uuid`s the periodic refresh tick (issue #105) must NOT refresh —
    /// the inputs to the engine's "parked accounts only" Caller contract, computed daemon-side
    /// from the authoritative swap state (the tick has none of its own):
    ///
    ///   - the **active** account (the live session's credential — never touch it), and
    ///   - the **imminent swap target** ([`pick_target`]'s current choice, the same account
    ///     `next_swap` shows): a swap that promotes it reads its stash WITHOUT rewriting it
    ///     (#6), so the engine's CAS re-stash cannot observe the promotion (#102) — exclude it
    ///     ahead of time. The mid-swap window itself is covered by the swap lock the engine
    ///     holds; this excludes only the *predictable* targets.
    ///
    /// Quarantined (dead) accounts are NO LONGER excluded (issue #106 reverses #105's
    /// "futile to refresh" exclusion): a dead credential may still be REFRESHABLE — its
    /// refresh token can work even after its access token began failing — so refreshing it is
    /// exactly the RESTORE path. They are supplied separately by [`refresh_quarantined`](Self::refresh_quarantined),
    /// which the tick uses to bypass the near-expiry filter and to report a recovered account
    /// for un-quarantine. The bounded cost — one wasted isolated-spawn per cadence for a
    /// TRULY-dead account, until an operator re-login — is accepted to close the gap where a
    /// less-recently-used parked account silently stays unusable.
    ///
    /// Returns owned uuids so the run loop can hand them to the tick without borrowing the
    /// daemon across the idle wait.
    fn refresh_exclusions(&self) -> Vec<String> {
        let mut excluded = Vec::new();
        if let Some(active) = self.state.active {
            excluded.push(self.roster[active].account_uuid.clone());
            // The imminent swap target from the latest carried readings — the same selection
            // `next_swap` surfaces. `pick_target` already excludes the active account.
            let readings = self.decision_readings(Some(active));
            let enabled = self.enabled_mask();
            if let Some(target) = pick_target(
                active,
                &readings,
                &enabled,
                Some(self.target_max_usage),
                self.session_trigger_base,
                self.weekly_trigger_base,
            ) {
                excluded.push(self.roster[target].account_uuid.clone());
            }
        }
        excluded
    }

    /// The roster `account_uuid`s the daemon currently holds QUARANTINED ("needs re-login",
    /// issue #42) — handed to the refresh tick so it can attempt the RESTORE path (#106):
    /// refresh them even when not near expiry (a server-revoked token may sit far from its
    /// stored timestamp expiry) and report a successful one for un-quarantine. An account
    /// here that is ALSO in [`refresh_exclusions`](Self::refresh_exclusions) (a dead ACTIVE
    /// account) is still skipped — the tick checks exclusion first (the engine Caller contract
    /// wins), so the active credential is never touched.
    ///
    /// Owned uuids, like [`refresh_exclusions`](Self::refresh_exclusions), so the run loop
    /// need not borrow the daemon across the idle wait.
    fn refresh_quarantined(&self) -> Vec<String> {
        self.roster
            .iter()
            .enumerate()
            .filter(|(i, _)| self.state.health[*i].quarantined)
            .map(|(_, account)| account.account_uuid.clone())
            .collect()
    }

    /// Apply one RESTORE the refresh tick reported (issue #106): un-quarantine the account
    /// with `uuid` whose isolated refresh succeeded, returning the edge-triggered
    /// [`Event::CredentialRestored`] for the run loop to log — or `None` if the account is
    /// no longer quarantined (a concurrent re-login already restored it, #107) or the uuid is
    /// unknown. Pairs the health flip with its event in the daemon, exactly as the #42 poll
    /// and #107 re-login recovery paths do; the tick only signals which accounts recovered.
    fn apply_refresh_restore(&mut self, uuid: &str) -> Option<Event> {
        let idx = self.roster.iter().position(|a| a.account_uuid == uuid)?;
        if !self.state.health[idx].quarantined {
            return None;
        }
        self.state.health[idx].quarantined = false;
        self.state.health[idx].recovery_successes = 0;
        Some(Event::CredentialRestored {
            account: self.roster[idx].label.clone(),
        })
    }

    /// Fold one [`RefreshObservation`] the refresh sweep reported (issue #119) into the
    /// owning account's carried health state — the credential clocks the `status` rollup
    /// projects. The engine's `expiresAt` is MS (CC's native unit); it is converted to the
    /// epoch SECONDS the rollup and wire use HERE, at the fold boundary. A `None` uuid (an
    /// account the daemon no longer holds) is ignored, mirroring [`Self::apply_refresh_restore`].
    ///
    /// The expiry clock updates on EVERY observation (refreshed or read-only). The
    /// refresh-health fields update only when the sweep actually refreshed the account
    /// (`observation.refresh` is `Some`): a `Dead` / `Error` outcome advances the
    /// consecutive-failure streak; any alive outcome resets it — so the streak the rollup's
    /// `AtRisk` keys off counts only CONSECUTIVE failures.
    ///
    /// Returns [`Event::CredentialUnrecoverable`] on the ONE observation that first confirms a
    /// QUARANTINED account's refresh token is dead — the sweep's isolated refresh came back
    /// `Dead`, so no automated path revives it and only an operator `claude /login` can (issue
    /// #261). Gated by the sticky per-account [`AccountHealth::unrecoverable_signaled`] latch so
    /// the caller emits the operator signal exactly once per quarantine episode, never per sweep
    /// re-probe. Every other observation returns `None` (mirroring [`Self::apply_refresh_restore`]'s
    /// `Option<Event>` shape, so the caller emits uniformly).
    fn apply_refresh_observation(&mut self, observation: &RefreshObservation) -> Option<Event> {
        let idx = self
            .roster
            .iter()
            .position(|a| a.account_uuid == observation.account_uuid)?;
        let health = &mut self.state.health[idx];
        // ms → s at the boundary; the rollup/wire are uniform epoch seconds.
        health.access_expires_at = observation.expires_at_ms.map(|ms| ms / 1000);
        let delta = observation.refresh?;
        health.last_refresh_outcome = Some(delta.outcome);
        health.refresh_token_rotated = Some(delta.token_rotated);
        match delta.outcome {
            RefreshEventOutcome::Dead | RefreshEventOutcome::Error => {
                health.consecutive_refresh_failures =
                    health.consecutive_refresh_failures.saturating_add(1);
            }
            RefreshEventOutcome::Refreshed
            | RefreshEventOutcome::RefreshedNotReStashed
            | RefreshEventOutcome::NoChange => {
                health.consecutive_refresh_failures = 0;
            }
        }
        // Issue #261: a QUARANTINED account whose isolated sweep-refresh returns `Dead` is
        // confirmed unrecoverable. Fire the operator signal once per quarantine episode — the
        // latch (reset on re-quarantine in `note_poll_outcome`) suppresses the re-probe repeats
        // and the `Dead`↔`Error` flap. Keyed on the latch, deliberately NOT on the prior
        // `last_refresh_outcome`, which is orthogonal to the quarantine lifecycle.
        let signal = delta.outcome == RefreshEventOutcome::Dead
            && health.quarantined
            && !health.unrecoverable_signaled;
        if signal {
            health.unrecoverable_signaled = true;
        }
        // The `&mut health` borrow ends at its last use above (NLL); read the label off `roster`.
        signal.then(|| Event::CredentialUnrecoverable {
            account: self.roster[idx].label.clone(),
        })
    }

    /// Fold one sweep's [`SweepHealth`] classification into the daemon-level systemic-refresh
    /// detector (issue #378), returning the edge-triggered [`Event`] to emit at an episode
    /// boundary — [`Event::RefreshSystemicFailure`] on the streak crossing
    /// [`systemic_failure_n`](Self::systemic_failure_n), [`Event::RefreshSystemicRecovered`] on
    /// recovery — or `None` on a neutral / mid-episode sweep.
    ///
    /// Driven from the run loop AFTER the idle borrow drops (like the #106 restores + #119
    /// observations), once PER SWEEP: the classification is captured per sweep in
    /// `idle_until_next_tick` so multiple sweeps in one idle period (a low cadence under a long
    /// poll interval) each advance the streak individually rather than merging into one. The
    /// per-account observation fold ([`apply_refresh_observation`](Self::apply_refresh_observation))
    /// updates the `at_risk` rollup independently — this is the orthogonal MECHANISM-level signal.
    fn note_systemic_refresh(&mut self, health: SweepHealth) -> Option<Event> {
        self.state
            .systemic_refresh
            .note(health, self.systemic_failure_n)
    }

    /// Read the just-polled account's stored access-token expiry (epoch SECONDS, issue
    /// #141) — the DISPLAY clock the poll path feeds into [`AccountHealth::poll_expires_at`],
    /// so `status --json` surfaces an expiry even with `[refresh]` off. Reads the SAME
    /// credential the usage poll used: the CANONICAL item for the active account (its token
    /// refreshes in place there, the freshest expiry), the per-account STASH otherwise —
    /// mirroring [`RealRosterPoller::poll`]. Reuses the non-secret
    /// [`crate::refresh::expires_at`] / [`crate::refresh::stored_expires_at`] extractors
    /// (only the `i64` is pulled, never the token) and converts MS→s at this boundary. A
    /// best-effort clock, never a gate: `None` when the credential is unreadable (a locked
    /// keychain, an absent stash), which just leaves the wire field null this cycle.
    async fn read_poll_expires_at(&self, account: &Account, active: bool) -> Option<i64> {
        let expires_at_ms = if active {
            self.store
                .read()
                .await
                .ok()
                .and_then(|credential| crate::refresh::expires_at(credential.expose()))
        } else {
            crate::refresh::stored_expires_at(&self.stash, &account.stash()).await
        };
        expires_at_ms.map(millis_to_secs)
    }

    /// Recompute every account's 5-state credential-health rollup (issue #119) against
    /// `now_secs` and emit one [`Event::CredentialHealth`] per account whose verdict CHANGED
    /// since the last call — the edge-triggered health timeline the issue's AC-3 requires
    /// ("exactly one redacted event per transition"). The very first computation per account
    /// SEEDS [`AccountHealth::last_health`] WITHOUT emitting (no prior state to transition
    /// from), so a fresh daemon never logs a startup storm.
    ///
    /// Driven from the run loop AFTER folding the sweep's restores + observations, so a
    /// transition reflects both the quarantine machinery (#42, updated in `tick`) and the
    /// refresh clocks (#119, updated post-idle). Independent of — and complementary to — the
    /// #42 [`Event::CredentialDead`] / [`Event::CredentialRestored`] edges: those signal the
    /// quarantine sub-state and drive recovery, while this is the operator-facing rollup edge
    /// (it also captures the Healthy↔Stale↔AtRisk transitions #42 never sees, and a
    /// refresh-detected death the 401 path never quarantines).
    fn note_health_transitions(&mut self, now_secs: i64) -> Vec<Event> {
        let mut events = Vec::new();
        // The same masked, in-rotation readings the display snapshot uses (keyed on the
        // current active), so the edge-triggered event verdict matches what `status` shows:
        // a `Some` entry is this account's positive-liveness signal (a successful poll),
        // `None` a failed poll / out-of-rotation account → the #137 `Unknown` input.
        let readings = self.decision_readings(self.state.active);
        for (i, reading) in readings.iter().enumerate() {
            let health = &self.state.health[i];
            let verdict = credential_health(
                health.quarantined,
                health.last_refresh_outcome,
                health.consecutive_refresh_failures,
                health.access_expires_at,
                reading.is_some(),
                now_secs,
            );
            // Emit only on a CHANGE from a SEEDED baseline; the first observation (None)
            // seeds silently.
            if let Some(prev) = self.state.health[i].last_health {
                if prev != verdict {
                    events.push(Event::CredentialHealth {
                        account: self.roster[i].label.clone(),
                        state: verdict,
                    });
                }
            }
            self.state.health[i].last_health = Some(verdict);
        }
        events
    }

    /// Build the non-secret per-account snapshot for the event log and the socket.
    fn snapshot(
        &self,
        active: Option<usize>,
        readings: &[Option<Usage>],
        now_secs: i64,
    ) -> StatusSnapshot {
        StatusSnapshot {
            accounts: self
                .roster
                .iter()
                .enumerate()
                .map(|(i, account)| {
                    let health = &self.state.health[i];
                    AccountReading {
                        label: account.label.clone(),
                        active: active == Some(i),
                        enabled: account.enabled,
                        quarantined: health.quarantined,
                        // Mid-recovery iff dead AND its credential is currently answering
                        // again (issue #109) — a refinement of `quarantined`, so `status`
                        // can soften `needs re-login` to `recovering` for a healing account.
                        recovering: health.quarantined && health.recovery_successes > 0,
                        // The daemon's own viability verdict, deterministic (base, not
                        // jittered, trigger) so the displayed "resets in" matches when
                        // `use` would accept the account again (issue #72).
                        weekly_exhausted: readings[i]
                            .is_some_and(|usage| usage.weekly >= self.weekly_trigger_base),
                        usage: readings[i],
                        // The credential clocks + the daemon-computed 5-state rollup (issue
                        // #119), projected from this account's carried health state. The
                        // rollup is computed HERE (daemon-side) against `now_secs`; the thin
                        // client just renders the verdict's glyph + the raw clocks. The wire
                        // clock prefers the refresh-sourced expiry and falls back to the
                        // poll-sourced one (issue #141) so it is populated with `[refresh]`
                        // off; the rollup below still reads ONLY the refresh-sourced field,
                        // so a lapsed idle poll clock never fires a false-🟠 Stale (see #137).
                        access_expires_at: health.access_expires_at.or(health.poll_expires_at),
                        refresh_health: refresh_health_view(health),
                        // #137: a `Some` reading is this account's positive-liveness signal (a
                        // successful poll); without one (and no refresh telemetry / expiry) the
                        // rollup is `Unknown`, never a false 🟢. The poll-sourced clock above is
                        // display-only and deliberately NOT fed here (set even on a failed poll).
                        health: credential_health(
                            health.quarantined,
                            health.last_refresh_outcome,
                            health.consecutive_refresh_failures,
                            health.access_expires_at,
                            readings[i].is_some(),
                            now_secs,
                        ),
                    }
                })
                .collect(),
            // The forward-looking next-swap candidate (issue #88), computed from the
            // same raw readings; sourced from a label only, so no token/email can
            // reach it (issue #15).
            next_swap: self.next_swap(active, readings),
            // The config `[refresh].enabled` (#105), carried to the client for the #138
            // advisory — the CONFIG value, so the advisory keys off what the operator set
            // (AC-2: "suppressed when [refresh] is enabled").
            refresh_enabled: self.refresh_enabled,
            // The snapshot's freshness stamp for the frozen wire contract (issue #164): the SAME
            // `now_secs` the #119 health rollup reads above, so one wall-clock read backs the
            // whole cycle and the client's live-vs-stale check agrees with the rollup's clock.
            generated_at: now_secs,
            // The daemon-level systemic refresh-health indicator (issue #378): `Some(n)` while a
            // systemic-failure episode is active (n consecutive all-account error sweeps), `None`
            // when healthy — surfaced by `status` so the mechanism-down state is visible without
            // waiting for an account to die. A COUNT only (#15).
            systemic_refresh: self.state.systemic_refresh.status(),
        }
    }

    /// Draw this cycle's FULL poll interval from the poll strategy (issue #38),
    /// clamped to the valid `5..=3600` s range. The fixed (no-jitter) case
    /// returns the base verbatim; deterministic under a seeded RNG. This is the
    /// per-account cadence (how often any one account is re-polled) and the base of
    /// the rate-limit back-off (issue #76); the staggered loop spreads it across the
    /// rotation via [`next_subinterval`](Self::next_subinterval).
    pub(crate) fn next_poll_interval(&mut self) -> Duration {
        Duration::from_secs_f64(
            self.poll_strategy
                .draw(&mut self.rng, POLL_SECS_LO, POLL_SECS_HI),
        )
    }

    /// The wait between two consecutive single-account polls (issue #80): the full
    /// jittered interval divided by the rotation SIZE (the distinct-account count N, see
    /// [`rotation_len`](Self::rotation_len)), so consecutive polls stay ~`poll_secs / N`
    /// apart (≈40–45 s for a typical roster) — the per-source FLOOR the stagger exists to
    /// enforce (no account is polled faster than this). The #366 active-interleave
    /// lengthens the schedule (a full sweep of it now
    /// spans ~`2·poll_secs`, re-observing the active every ~2 sub-intervals) but does NOT
    /// touch this divisor, so the per-tick spacing is unchanged. Each sub-interval draws a
    /// fresh full interval (inheriting the #38 jitter decorrelation) before dividing. The
    /// divisor is clamped to ≥ 1 so a single-account roster simply waits the whole
    /// interval — there is nothing to stagger and no burst is possible.
    fn next_subinterval(&mut self) -> Duration {
        let interval = self.next_poll_interval();
        let len = self.rotation_len().max(1) as u32;
        interval / len
    }

    /// Sleep until the next single-account poll is due — a freshly drawn, jittered
    /// sub-interval (issues #38, #80) handed to the [`Clock`] seam.
    pub(crate) async fn wait_for_next_poll(&mut self) {
        let interval = self.next_subinterval();
        self.clock.tick(interval).await;
    }

    /// Sleep until the next tick is due. `next_wait` is the just-finished tick's
    /// requested wait: `None` → the normal jittered poll interval (issue #38);
    /// `Some(d)` → the locked-keychain back-off (issue #13). The rate-limit / transient
    /// back-off no longer rides this wait (issue #293): it is per-account, applied by
    /// skipping the throttled account inside `tick`. Behind the [`Clock`] seam, so tests
    /// drive both paths deterministically.
    pub(crate) async fn wait_after_tick(&mut self, next_wait: Option<Duration>) {
        match next_wait {
            Some(backoff) => self.clock.tick(backoff).await,
            None => self.wait_for_next_poll().await,
        }
    }

    /// Whether account `i` is inside its per-account rate-limit / transient back-off
    /// window (issue #293): it earned a `429` / `5xx` and the widened wait it armed has
    /// not yet elapsed on the monotonic [`Clock`]. While `true`, [`tick`](Self::tick)
    /// SKIPS that account's poll this cycle — so the throttle stays scoped to the one
    /// account and the active account (the daemon's core job) keeps polling. `false` once
    /// the window elapses, or after any non-throttling poll clears it
    /// ([`note_account_backoff`](Self::note_account_backoff)).
    fn account_backing_off(&self, i: usize) -> bool {
        self.state.health[i]
            .poll_backoff_until
            .is_some_and(|until| self.clock.now() < until)
    }

    /// Fold account `i`'s poll outcome into its OWN rate-limit / transient back-off (issue
    /// #293, the per-account revision of #76's endpoint-global model). A `429`
    /// (rate-limited) or a `5xx` / network transient advances the account's exponential
    /// streak and arms its back-off window (its next poll is skipped until the window
    /// elapses on the monotonic [`Clock`]); ANY other outcome (success / 401 / 403) clears
    /// the account's streak and window. Returns the [`TickBackoff`] it imposed — the
    /// effective wait plus the raw server `Retry-After` source label (issue #295), both for
    /// the diagnostic tick line — or `None` when the outcome was not a throttle.
    ///
    /// The base is this cycle's freshly-drawn, jittered poll interval — inheriting the #38
    /// decorrelation — times `2^min(streak, POLL_BACKOFF_MAX_SHIFT)`, clamped to
    /// [`POLL_BACKOFF_CAP`]. The first throttled poll already earns ~2× the interval, so
    /// the account's re-poll spacing is WIDER than the fixed cadence — #76's core ask, now
    /// per-account. A server-advised `Retry-After` is honoured as a MINIMUM (the wait is
    /// never shorter than it) but clamped to [`POLL_BACKOFF_CAP`] as a MAXIMUM, so a
    /// pathological value cannot dark the account past the exponential ceiling (issue #294).
    ///
    /// Scoping BOTH the `429` and the transient per-account is deliberate (issue #293): the
    /// `429` is per-Anthropic-org (independent buckets), and under a genuine endpoint outage
    /// every account fails its OWN poll and arms its OWN window anyway — so one per-account
    /// path is the simplest correct design and needs no separate global case.
    fn note_account_backoff(
        &mut self,
        i: usize,
        result: &Result<Usage>,
        events: &mut Vec<Event>,
    ) -> Option<TickBackoff> {
        // The account UUID is the durable identity for the #399 events (never the free-form,
        // PII-capable `label`, #15). Cloned up front so the borrow does not tangle with the
        // `&mut self.state.health[i]` below.
        let account_uuid = self.roster[i].account_uuid.clone();
        let Some(signal) = backoff_signal(result) else {
            let health = &mut self.state.health[i];
            // Edge-triggered EXIT (issue #399): a non-throttling poll (success / 401 / 403) that
            // CLEARED an actually-armed window emits a durable `usage_backoff_cleared`, bracketing
            // the episode's span. A plain clean poll with no armed window stays silent (mirroring
            // `usage_rollup`'s no-op silence), so the exit is a true edge, not a per-clean-poll line.
            let was_backing_off = health.poll_backoff_until.is_some();
            health.poll_backoff_streak = 0;
            health.poll_backoff_until = None;
            if was_backing_off {
                events.push(Event::UsageBackoffCleared {
                    account: account_uuid,
                });
            }
            return None;
        };
        let streak = self.state.health[i].poll_backoff_streak.saturating_add(1);
        self.state.health[i].poll_backoff_streak = streak;
        let shift = streak.min(POLL_BACKOFF_MAX_SHIFT);
        let widened = self
            .next_poll_interval()
            .checked_mul(1u32 << shift)
            .unwrap_or(POLL_BACKOFF_CAP)
            .min(POLL_BACKOFF_CAP);
        // Clamp to `POLL_BACKOFF_CAP` as a MAXIMUM (issue #294) — bounds a pathological or buggy
        // `Retry-After` (e.g. `86400`). `widened` is already ≤ the cap, so this bites only the
        // `Retry-After` arm.
        let wait = match signal.retry_after {
            Some(ra) => widened.max(ra),
            None => widened,
        }
        .min(POLL_BACKOFF_CAP);
        // `wait` is bounded to `POLL_BACKOFF_CAP` above, so adding it to the monotonic
        // instant cannot overflow — the value is bounded at the source (issue #294), which
        // supersedes #293's `checked_add` guard against an unbounded `Retry-After`.
        self.state.health[i].poll_backoff_until = Some(self.clock.now() + wait);
        // Durable ENTER (issue #399): make the previously stderr-only 429 / back-off signal durable
        // — the account UUID, the throttle class (`429` vs transient), the running streak, the RAW
        // server `Retry-After` (pre-cap #295), and the effective armed window. Emitted on EACH
        // throttled poll (the streak climbs, the window widens), so the durable log shows the
        // WIDENING across the episode — the residual-late-swap signal a single first-throttle line
        // would hide. So a back-off on the ACTIVE account (which blinds the very account being
        // consumed) is diagnosable from `sessiometer.log` alone, without the stderr channel.
        events.push(Event::UsageBackoff {
            account: account_uuid,
            class: signal.class,
            consecutive: streak,
            retry_after_secs: signal.retry_after.map(|ra| ra.as_secs()),
            backoff_secs: wait.as_secs(),
        });
        // Carry the RAW server `Retry-After` (pre-cap) alongside the effective `wait` so the
        // diagnostic tick line can LABEL the wait's source (issue #295): a `Some` marks a
        // server-advised floor, a `None` marks the self-capped exponential. Pre-cap keeps a
        // pathological value the #294 clamp bit visible (`wait` ≪ `retry_after`).
        Some(TickBackoff {
            wait,
            retry_after: signal.retry_after,
        })
    }

    /// Draw the jittered start-up delay (issue #76): a uniform `[0,
    /// STARTUP_DELAY_CAP)` wait taken ONCE, before the first poll, so repeated
    /// restarts of the same config — and the N accounts polled within a cycle — do
    /// not synchronize an immediate burst of usage requests. Deterministic under the
    /// seeded RNG, like [`next_poll_interval`](Self::next_poll_interval), so it is
    /// unit-testable without a wall clock.
    pub(crate) fn startup_delay(&mut self) -> Duration {
        // base = spread = CAP/2 makes the draw `CAP/2 + (2u-1)*CAP/2 = CAP*u` for the
        // unit draw u in [0, 1) — i.e. uniform [0, CAP). The `draw`'s own clamp to
        // [0, CAP] is then purely defensive: the raw value is already in range.
        let strategy = Strategy {
            base: STARTUP_DELAY_CAP / 2.0,
            jitter: Jitter::Uniform {
                spread: STARTUP_DELAY_CAP / 2.0,
            },
        };
        Duration::from_secs_f64(strategy.draw(&mut self.rng, 0.0, STARTUP_DELAY_CAP))
    }
}

/// The health-relevant classification of ONE account's poll this tick — the typed
/// poll outcome (issue #42) the per-account health state machine consumes. Derived
/// from the poll `Result` by [`classify_poll`]; distinct from the raw HTTP taxonomy
/// (`usage`'s status classes) in that it folds every non-liveness-bearing error into
/// one `Transient` class and separates the two liveness signals — `Live` (the
/// credential works) from `Unauthorized` (the token was rejected). "Dead" and
/// "exhausted" are not single-poll outcomes: death is the ACCUMULATION of
/// `Unauthorized` across ticks (the per-account 401 streak reaching `monitor_401_n`),
/// and exhaustion is derived from a `Live` reading's usage against the swap triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollOutcome {
    /// A successful usage reading — the credential is alive. Resets the death streak;
    /// for a quarantined account, advances the recovery probe.
    Live,
    /// HTTP 401 — the stored token was rejected. Advances the consecutive-401 death
    /// streak; the Nth (`monitor_401_n`) quarantines the account.
    Unauthorized,
    /// HTTP 403 — the token authenticated but lacks the usage scope (a non-interactive
    /// setup token). NON-dead (it authenticated), surfaced distinctly (#5).
    ScopeMissing,
    /// Any other failure (5xx / network / 429 / other 4xx / keychain-locked /
    /// unreadable token / unparseable body): no liveness signal — neither advances
    /// nor, by itself, distinguishes death. Resets the death streak (a 401 streak
    /// must be unbroken).
    Transient,
}

/// Classify a poll `Result` into its [`PollOutcome`] — the typed poll outcome the
/// dead-credential health state machine consumes (issue #42). Pure: the single place
/// the HTTP error taxonomy is mapped onto the liveness/death axis, so the policy is
/// testable in isolation and `note_poll_outcome` stays a state-transition.
fn classify_poll(result: &Result<Usage>) -> PollOutcome {
    match result {
        Ok(_) => PollOutcome::Live,
        Err(Error::UsageUnauthorized) => PollOutcome::Unauthorized,
        Err(Error::UsageScopeMissing) => PollOutcome::ScopeMissing,
        Err(_) => PollOutcome::Transient,
    }
}

/// Classify a poll `Result` into its operator-facing [`PollClass`] for the diagnostic
/// channel (issue #77). Distinct from [`classify_poll`] in ONE place: a `429`
/// (rate-limited) is its OWN class here, where the dead-credential machine folds it
/// into `Transient` — an operator debugging a throttling storm needs to see
/// `rate_limited` rather than a generic transient (the very signal #77 surfaces). The
/// `5xx` / network / unreadable / unparseable remainder is `Transient`.
fn diag_poll_class(result: &Result<Usage>) -> PollClass {
    match result {
        Ok(_) => PollClass::Live,
        Err(Error::UsageUnauthorized) => PollClass::Unauthorized,
        Err(Error::UsageScopeMissing) => PollClass::Scope,
        Err(Error::UsageRateLimited { .. }) => PollClass::RateLimited,
        Err(_) => PollClass::Transient,
    }
}

/// A poll outcome that asks the loop to back off (issue #76): a `429`
/// (rate-limited) or a `5xx` / network transient. Carries the throttle `class` (issue
/// #399, so the durable back-off event can tell a `429` from a transient) and the
/// server-advised `Retry-After` the response supplied, if any.
struct BackoffSignal {
    class: BackoffClass,
    retry_after: Option<Duration>,
}

/// The back-off one throttled poll imposed this tick (issue #293/#294), for the diagnostic
/// tick line. The output sibling of [`BackoffSignal`] (the input): `wait` is the effective
/// window armed on the account — `max(self-capped exponential, server Retry-After)`, clamped
/// to [`POLL_BACKOFF_CAP`] — which the line renders as `backoff_secs`. `retry_after` is the
/// RAW server-advised `Retry-After` the response supplied (issue #295), BEFORE that clamp,
/// or `None` when the server sent none — the source label that tells a server-advised wait
/// from the daemon's self-capped exponential. Pre-cap on purpose: a pathological value the
/// #294 cap bit stays visible (`wait` = 3600 s beside `retry_after` = 86400 s), rather than
/// collapsing into an unplaceable `backoff_secs=3600`.
#[derive(Debug, Clone, Copy)]
struct TickBackoff {
    wait: Duration,
    retry_after: Option<Duration>,
}

/// Classify a poll `Result` for the rate-limit / transient back-off (issue #76):
/// `Some` when it is a back-off outcome (`429` or `5xx` / network), carrying any
/// `Retry-After`; `None` otherwise. A success, a `401`, a `403`, or any other error
/// does NOT, by itself, widen the poll spacing. Deliberately separate from
/// [`classify_poll`] (which feeds the #42 dead-credential health machine): back-off
/// is orthogonal — a `429` both resets the 401 streak (via `classify_poll`'s
/// `Transient`) AND asks the loop to slow down (here).
fn backoff_signal(result: &Result<Usage>) -> Option<BackoffSignal> {
    match result {
        // The `class` (issue #399) distinguishes the two back-off outcomes so the durable
        // `usage_backoff` event can carry which one armed the window — a `429` is the
        // rate-limit the "429 count" acceptance counts, a `5xx` / network the transient.
        Err(Error::UsageRateLimited { retry_after, .. }) => Some(BackoffSignal {
            class: BackoffClass::RateLimited,
            retry_after: *retry_after,
        }),
        Err(Error::UsageTransient { retry_after, .. }) => Some(BackoffSignal {
            class: BackoffClass::Transient,
            retry_after: *retry_after,
        }),
        _ => None,
    }
}

/// The per-account usage VELOCITY between two consecutive readings (issue #399): the signed
/// change in each dimension as a rounded percent, `to_pct(next) - to_pct(prev)`. Reuses
/// [`to_pct`] so the delta agrees with the percents the swap line (`session_pct`) and `status`
/// show for the same reading; a difference of two `0..=100` percents lands in `-100..=100`, well
/// inside `i16`. Positive ⇒ usage climbing; negative ⇒ a window reset dropped the reading. Pure,
/// so the quantization is unit-tested without a daemon.
fn usage_velocity(prev: &Usage, next: &Usage) -> (i16, i16) {
    let session = i16::from(to_pct(next.session)) - i16::from(to_pct(prev.session));
    let weekly = i16::from(to_pct(next.weekly)) - i16::from(to_pct(prev.weekly));
    (session, weekly)
}

/// Pick the viable swap target whose weekly window resets SOONEST (issue #37):
/// among accounts other than `active` that are enabled (issue #36), whose reading
/// is available, that are NOT session-saturated (session usage below
/// `session_trigger`) and NOT weekly-exhausted (weekly usage below `weekly_trigger`,
/// issue #11) — and, when the opt-in `floor` is `Some`, whose session usage is
/// below it too (#10) — the one with the earliest weekly `resets_at`. An account with a
/// known reset is preferred over one without (an unknown reset sorts last); an
/// exact tie — or an all-unknown field — keeps the earliest roster index. `None`
/// when none qualifies: with every enabled other account weekly-exhausted that is
/// the all-exhausted terminal state (#11). `enabled` is indexed by roster position,
/// parallel to `readings`.
///
/// Soonest-reset (issue #37) SUPERSEDES the former most-weekly-headroom rule.
/// Swapping TO the account whose quota refills first burns an allowance that is
/// about to reset anyway and preserves the longer-runway account, raising total
/// roster utilization. It also UNIFIES normal selection with the #11 terminal hold,
/// which already holds on the soonest-`resets_at` account
/// ([`soonest_weekly_reset`]) — so, when resets are known, the daemon prefers the
/// same least-time-to-relief account whether or not a viable target exists. The two
/// differ deliberately only on the degenerate `None` case: this fn keeps an
/// unknown-reset account as a last-resort eligible target (selection must pick
/// SOMETHING viable), whereas [`soonest_weekly_reset`] excludes `None` outright (the
/// hold then omits a timestamp). The viability FILTER is unchanged; only the choice
/// AMONG viable accounts changed.
///
/// Three exclusions are load-bearing; two are the symmetric anti-thrash guards.
/// The weekly-exhaustion exclusion (#11): a target at/above its weekly trigger
/// would re-trip [`swap::decide`]'s weekly dimension next cycle and thrash, so it
/// can never be a useful destination — excluding it is what turns "all enabled
/// accounts weekly-exhausted" into a no-viable-target verdict instead of a swap.
/// The session-saturation exclusion is its exact mirror on the OTHER trigger
/// dimension: [`swap::decide`] swaps away on `session >= session_trigger` OR
/// `weekly >= weekly_trigger`, so a target at/above EITHER trigger re-trips next
/// cycle. Guarding only weekly left a session-saturated but weekly-viable account
/// eligible, and the soonest-reset rule — anti-correlated with session headroom,
/// since the account nearest its weekly reset is the most-cycled one — would pick
/// exactly such a target, producing an indefinite session ping-pong between the two
/// soonest-reset accounts. The `session < session_trigger` filter closes that: the
/// acquire predicate is now at least as strict as the negation of the release
/// predicate on BOTH dimensions. It is unconditional, distinct from `floor` — a
/// STRICTER reserve layered on top (effective ceiling `min(session_trigger, floor)`)
/// which the PROACTIVE caller passes (default 80, #398) and the EMERGENCY caller
/// drops (`None`) so a dead active always escapes. The disabled exclusion (#36): a parked account
/// is never a destination even with ample headroom, and — being excluded here
/// rather than relying on its (skipped) poll — it can never hold the daemon out of
/// the #11 terminal state.
fn pick_target(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    floor: Option<f64>,
    session_trigger: f64,
    weekly_trigger: f64,
) -> Option<usize> {
    // The index-only projection for the callers that need no rationale (the swap decision, the
    // refresh exclusions). [`pick_target_with_reason`] is the single source of selection truth;
    // this drops the reason so those call sites (and their tests) stay unchanged.
    pick_target_with_reason(
        active,
        readings,
        enabled,
        floor,
        session_trigger,
        weekly_trigger,
    )
    .map(|(i, _)| i)
}

/// Like [`pick_target`], but also returns WHY the winner was chosen ([`NextSwapReason`], issue
/// #393) — the rationale [`Daemon::next_swap`] carries on the wire so the panel + `sessiometer
/// status` render the ONE reason the daemon actually used. Selection is IDENTICAL to
/// [`pick_target`] (same viability filters, same #37 soonest-weekly-reset `min_by_key`); this
/// variant merely RETAINS the sort axis instead of discarding it. Kept as the shared core (rather
/// than re-deriving the reason in `next_swap`) so the filter set can never drift between the
/// selection and its stated reason.
fn pick_target_with_reason(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    floor: Option<f64>,
    session_trigger: f64,
    weekly_trigger: f64,
) -> Option<(usize, NextSwapReason)> {
    // The viable set — the same exclusions `pick_target` applies (issue #11/#36/#37, the
    // always-on session anti-thrash gate, the opt-in `floor`), collected so its CARDINALITY can
    // distinguish a genuine soonest-reset win from a sole-candidate default.
    let viable: Vec<(usize, Usage)> = readings
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != active)
        .filter(|&(i, _)| enabled[i])
        .filter_map(|(i, reading)| reading.map(|usage| (i, usage)))
        .filter(|&(_, usage)| usage.weekly < weekly_trigger)
        // Always-on session anti-thrash gate: exclude a target at/above the session
        // trigger — it would immediately re-trip [`swap::decide`]'s session dimension
        // and thrash (the exact mirror of the weekly filter above). Distinct from the
        // `floor` below, which tightens this ceiling further when the caller passes it.
        .filter(|&(_, usage)| usage.session < session_trigger)
        .filter(|&(_, usage)| floor.is_none_or(|f| usage.session < f))
        .collect();
    let candidate_count = viable.len();
    // Soonest weekly reset (issue #37). The key sorts a known reset ahead of an
    // unknown one (`false` < `true`), then by the reset epoch ascending;
    // `min_by_key` keeps the first of equal keys, so an exact tie — or an
    // all-unknown field — falls to the earliest roster index, matching
    // [`soonest_weekly_reset`]'s tie-break (#11).
    let (idx, usage) =
        viable
            .into_iter()
            .min_by_key(|&(_, usage)| match usage.weekly_resets_at {
                Some(resets_at) => (false, resets_at),
                None => (true, i64::MAX),
            })?;
    // The reason names the axis that ACTUALLY discriminated the winner — never a rule the daemon
    // did not apply (that inversion is the #393 bug itself). Three genuinely distinct states.
    let reason = if candidate_count < 2 {
        // Sole viable target: there was nothing to compare it against, so no axis decided.
        NextSwapReason::OnlyCandidate
    } else if let Some(resets_at) = usage.weekly_resets_at {
        // ≥2 viable and the winner holds the soonest KNOWN reset — issue #37's axis, carried as
        // the very `min_by_key` key that selected it (previously computed, then discarded).
        NextSwapReason::SoonestReset { resets_at }
    } else {
        // ≥2 viable, and the winner reported NO reset — a `Some` would have sorted ahead of it, so
        // NONE did. No reset-time tiebreak existed; the earliest roster index won. Reporting
        // `OnlyCandidate` here would assert "only viable target" while other targets were viable.
        NextSwapReason::RosterOrder
    };
    Some((idx, reason))
}

/// The roster index (and its epoch) of the account whose WEEKLY window resets
/// soonest, among readings that reported a parseable reset (issue #11). The
/// all-exhausted terminal state holds on this least-bad account. Accounts without
/// a known reset are skipped; an exact tie keeps the earliest roster index. `None`
/// when no account reported a reset, leaving the caller to fall back.
fn soonest_weekly_reset(readings: &[Option<Usage>]) -> Option<(usize, i64)> {
    let mut soonest: Option<(usize, i64)> = None;
    for (i, reading) in readings.iter().enumerate() {
        if let Some(at) = reading.as_ref().and_then(|usage| usage.weekly_resets_at) {
            if soonest.is_none_or(|(_, best)| at < best) {
                soonest = Some((i, at));
            }
        }
    }
    soonest
}

/// Classify why [`pick_target`] found no viable target, for the `all_exhausted`
/// relief hint (issue #398): `(cause, hold_idx, resets_at)`.
///
/// A candidate that is weekly-VIABLE (`weekly < weekly_trigger`) but session-blocked
/// (`session >= session_ceiling`, the ceiling being `min(session_trigger, floor)`) is
/// held out ONLY by session — it returns at its SESSION reset, sooner than any weekly
/// reset. If any such candidate exists the block is session-wide: report
/// [`SwapReason::Session`] and key the hint off the soonest such session reset (naming
/// that account). Otherwise every candidate is weekly-exhausted: report
/// [`SwapReason::Weekly`] and fall back to the soonest weekly reset (the #11 default).
/// `resets_at` is `None` when the relevant window has no parseable reset (the
/// forward-compatible "hold, timestamp omitted" case).
fn all_exhausted_relief(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    session_ceiling: f64,
    weekly_trigger: f64,
) -> (SwapReason, usize, Option<i64>) {
    // Soonest SESSION reset among weekly-viable-but-session-blocked candidates, plus a
    // naming fallback (the first such account) for when none reports a parseable reset.
    let mut session_relief: Option<(usize, i64)> = None;
    let mut session_blocked: Option<usize> = None;
    for (i, reading) in readings.iter().enumerate() {
        if i == active || !enabled[i] {
            continue;
        }
        let Some(usage) = reading else { continue };
        if usage.weekly < weekly_trigger && usage.session >= session_ceiling {
            session_blocked.get_or_insert(i);
            if let Some(at) = usage.session_resets_at {
                if session_relief.is_none_or(|(_, best)| at < best) {
                    session_relief = Some((i, at));
                }
            }
        }
    }
    if let Some(fallback) = session_blocked {
        // Session-wide: a weekly-viable account is held out only by session.
        return match session_relief {
            Some((idx, at)) => (SwapReason::Session, idx, Some(at)),
            None => (SwapReason::Session, fallback, None),
        };
    }
    // Weekly-wide: every candidate is weekly-exhausted (the #11 default).
    match soonest_weekly_reset(readings) {
        Some((idx, at)) => (SwapReason::Weekly, idx, Some(at)),
        None => (SwapReason::Weekly, active, None),
    }
}

/// The daemon's own re-validation verdict for a socket `swap` command (issue #167) — the pure
/// core of [`Daemon::perform_socket_swap`], so the "the daemon re-validates the target itself,
/// never the client hint" rule is unit-testable apart from the swap I/O (mirroring the pure
/// [`pick_target`] / [`crate::use_account`] `cooldown_active`).
enum SwapVerdict {
    /// Proceed: swap the active account OFF and the target ON.
    Swap,
    /// The target is ALREADY active — a no-op success (nothing to write), the non-`force`
    /// already-active case.
    AlreadyActive,
    /// Refused, with the redacted wire reason ([`SwapRejection`]).
    Reject(SwapRejection),
}

/// Decide whether a socket `swap` command may proceed, PURELY from the daemon's live facts about
/// the resolved `target` (issue #167). The viability facts (`quarantined`, `weekly_exhausted`,
/// `in_cooldown`) are computed by the caller from the daemon's OWN state — never a client-supplied
/// hint — mirroring how [`control_reply`](socket::control_reply) takes `peer_authenticated` as a plain bool for the
/// same testability reason.
///
/// `force` is POLICY-only: it bypasses the viability + cooldown gates (a warn-and-proceed the
/// standalone `use` also does), but it can NEVER manufacture an outgoing account, and it never
/// reaches the SAFETY aborts — the locked-keychain abort and the single-writer swap lock live in
/// the swap engine below this verdict ([`classify_swap_failure`]), so no verdict here can bypass
/// them. A `force` swap onto the ALREADY-active account is NOT short-circuited (it proceeds as a
/// self-swap — the `use --force <active>` display-repair path); only a NON-`force` already-active
/// request is the no-op.
fn swap_command_verdict(
    target: usize,
    active: Option<usize>,
    quarantined: bool,
    weekly_exhausted: bool,
    in_cooldown: bool,
    force: bool,
) -> SwapVerdict {
    // Already active + NON-force → a no-op success (nothing to write), matching the standalone
    // `use` no-op. A `force` request falls through to the self-swap below (display repair).
    if !force && active == Some(target) {
        return SwapVerdict::AlreadyActive;
    }
    // A normal re-stash swap needs an OUTGOING (active) account to swap away from. With none, the
    // daemon cannot run it — recovery (adopt-target, issue #212) is the standalone `use --force`
    // path, decoupled from this channel per the issue. `force` cannot manufacture an outgoing.
    if active.is_none() {
        return SwapVerdict::Reject(SwapRejection::NoActiveAccount);
    }
    // POLICY gates — each bypassable by `force` (warn-and-proceed at the operator, silent here);
    // NEVER a safety bypass (those live in the engine below this verdict).
    if !force {
        if quarantined {
            return SwapVerdict::Reject(SwapRejection::Quarantined);
        }
        if weekly_exhausted {
            return SwapVerdict::Reject(SwapRejection::WeeklyExhausted);
        }
        if in_cooldown {
            return SwapVerdict::Reject(SwapRejection::Cooldown);
        }
    }
    SwapVerdict::Swap
}

/// Map a swap-engine failure to the redacted wire reason for a `swap` ack (issue #167). The two
/// SAFETY aborts `force` can NEVER bypass get their own codes — a LOCKED keychain
/// ([`Error::KeychainLocked`], the engine's step-1 read aborts even under `force`) and a contended
/// single-writer swap lock ([`Error::SwapLockBusy`], fail-closed) — so the "force cannot bypass the
/// locked-keychain abort" invariant is observable in the ack. A canonical that is GONE
/// ([`Error::CredentialNotFound`], scrubbed since the daemon last resolved active) routes to the
/// recovery signal (adopt-target is the standalone path); everything else is the opaque `Failed`.
fn classify_swap_failure(err: &Error) -> SwapRejection {
    match err {
        Error::KeychainLocked { .. } => SwapRejection::KeychainLocked,
        Error::SwapLockBusy => SwapRejection::SwapLockBusy,
        Error::CredentialNotFound => SwapRejection::NoActiveAccount,
        _ => SwapRejection::Failed,
    }
}

/// Map a capture failure (from the #357 [`capture_locked`](crate::capture::capture_locked)
/// primitive, or a post-stash roster save) to the redacted wire reason for a `capture` ack (issue
/// #359) — the capture counterpart of [`classify_swap_failure`]. The two SAFETY aborts get their own
/// codes: a LOCKED keychain ([`Error::KeychainLocked`], the token read aborts even mid-capture) and
/// a contended single-writer swap lock ([`Error::SwapLockBusy`], fail-closed BEFORE any read). A
/// missing active account — not logged in to Claude Code (an absent / no-`oauthAccount`
/// `~/.claude.json`) or the canonical credential gone — routes to
/// [`CaptureRejection::NoActiveAccount`]; everything else (an I/O error, a roster save failure) is
/// the opaque `Failed`. Secret-free by construction: it inspects only the error's discriminant.
fn classify_capture_failure(err: &Error) -> CaptureRejection {
    match err {
        Error::KeychainLocked { .. } => CaptureRejection::KeychainLocked,
        Error::SwapLockBusy => CaptureRejection::SwapLockBusy,
        // Not logged in (absent `~/.claude.json` or no `oauthAccount` block) or the canonical token
        // is gone — there is no active account to capture.
        Error::ClaudeStateNotFound { .. }
        | Error::OauthAccountMissing
        | Error::CredentialNotFound => CaptureRejection::NoActiveAccount,
        _ => CaptureRejection::Failed,
    }
}

/// Fold a captured/refreshed [`CaptureOutcome`](crate::capture::CaptureOutcome) onto the event's
/// [`CaptureEventOutcome`] axis (issue #359) — the two SUCCESS tokens the durable audit line carries.
fn capture_event_outcome(outcome: crate::capture::CaptureOutcome) -> CaptureEventOutcome {
    match outcome {
        crate::capture::CaptureOutcome::Captured => CaptureEventOutcome::Captured,
        crate::capture::CaptureOutcome::Refreshed => CaptureEventOutcome::Refreshed,
    }
}

/// Fold a redacted [`CaptureRejection`] onto the event's [`CaptureEventOutcome`] axis (issue #359) —
/// the failure tags, so the durable [`Event::Capture`] carries the SAME machine reason the ack does.
fn capture_event_outcome_rejected(reason: CaptureRejection) -> CaptureEventOutcome {
    match reason {
        CaptureRejection::NoActiveAccount => CaptureEventOutcome::NoActiveAccount,
        CaptureRejection::KeychainLocked => CaptureEventOutcome::KeychainLocked,
        CaptureRejection::SwapLockBusy => CaptureEventOutcome::SwapLockBusy,
        CaptureRejection::Failed => CaptureEventOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_state::OauthAccount;
    use crate::config::Tunables;
    // `SweepOutcome` is named only in test code here (the run loop consumes it by
    // inference); import it test-scoped so a non-test build sees no unused import.
    use crate::contract::SweepOutcome;
    use crate::keychain::FakeCredentialStore;
    // `Verbosity` is named only in test code here (the diagnostic SINK gating lives
    // in `cli`); import it test-scoped so a non-test build sees no unused import.
    use crate::observability::{RefreshEventOutcome, Verbosity};
    use crate::stash::{FakeAccountStash, StashedAccount};
    use crate::timing::Jitter;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    // --- Fakes -------------------------------------------------------------

    /// A clock whose `now` starts at construction and advances by `step` on each
    /// `tick` — so a loop's cadence is deterministic and runs in zero real time.
    /// `frozen` makes `tick` a no-op (constant `now`).
    struct FakeClock {
        now: Cell<Instant>,
        step: Duration,
    }

    impl FakeClock {
        fn new(step: Duration) -> Self {
            Self {
                now: Cell::new(Instant::now()),
                step,
            }
        }
        fn frozen() -> Self {
            Self::new(Duration::ZERO)
        }
        fn advance(&self, by: Duration) {
            self.now.set(self.now.get() + by);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }
        // Advances by its own `step`, independent of the daemon's drawn interval,
        // so the existing run-loop/cooldown tests keep their deterministic
        // cadence. The poll-interval jitter (issue #38) is covered directly via
        // `Daemon::next_poll_interval`.
        async fn tick(&self, _interval: Duration) {
            self.now.set(self.now.get() + self.step);
        }
    }

    /// A scripted per-account poll outcome. `Ok` yields a reading; each error
    /// variant drives one of [`Daemon::note_poll_outcome`]'s issue-#9 arms, so the
    /// 401 / keychain-lock / 403 event paths and the 401 streak become testable.
    #[derive(Clone, Copy)]
    enum Scripted {
        Ok(Usage),
        Transient,
        /// A `429` rate-limit carrying an optional server-advised `Retry-After`
        /// (issue #76) — drives the poll back-off path.
        RateLimited(Option<Duration>),
        Unauthorized,
        Locked,
        ScopeMissing,
    }

    /// Scripts each account's poll outcome keyed by `account_uuid`: `ok` yields a
    /// reading, the error builders inject the issue-#9 conditions, and an
    /// unscripted account returns a transient error (unavailable).
    struct FakeRosterPoller {
        readings: HashMap<String, Scripted>,
    }

    impl FakeRosterPoller {
        fn new() -> Self {
            Self {
                readings: HashMap::new(),
            }
        }
        fn ok(mut self, uuid: &str, session: f64, weekly: f64) -> Self {
            self.readings.insert(
                uuid.to_owned(),
                Scripted::Ok(Usage {
                    session,
                    weekly,
                    weekly_resets_at: None,
                    session_resets_at: None,
                }),
            );
            self
        }
        /// Like [`ok`](Self::ok) but with a known weekly `resets_at` (epoch
        /// seconds) — the all-exhausted tests (#11) script which account resets
        /// soonest through this.
        fn ok_resets(
            mut self,
            uuid: &str,
            session: f64,
            weekly: f64,
            weekly_resets_at: i64,
        ) -> Self {
            self.readings.insert(
                uuid.to_owned(),
                Scripted::Ok(Usage {
                    session,
                    weekly,
                    weekly_resets_at: Some(weekly_resets_at),
                    session_resets_at: None,
                }),
            );
            self
        }
        fn failing(mut self, uuid: &str) -> Self {
            self.readings.insert(uuid.to_owned(), Scripted::Transient);
            self
        }
        /// Script a `429` rate-limit, optionally carrying a `Retry-After` (issue
        /// #76) — exercises the poll back-off path.
        fn rate_limited(mut self, uuid: &str, retry_after: Option<Duration>) -> Self {
            self.readings
                .insert(uuid.to_owned(), Scripted::RateLimited(retry_after));
            self
        }
        fn unauthorized(mut self, uuid: &str) -> Self {
            self.readings
                .insert(uuid.to_owned(), Scripted::Unauthorized);
            self
        }
        fn keychain_locked(mut self, uuid: &str) -> Self {
            self.readings.insert(uuid.to_owned(), Scripted::Locked);
            self
        }
        fn scope_missing(mut self, uuid: &str) -> Self {
            self.readings
                .insert(uuid.to_owned(), Scripted::ScopeMissing);
            self
        }
    }

    impl RosterPoller for FakeRosterPoller {
        async fn poll(&self, account: &Account, _active: bool) -> Result<PolledReading> {
            match self.readings.get(&account.account_uuid) {
                // The scripted decision reading, wrapped as a PolledReading with no
                // severity (the collector's severity path is unit-tested directly).
                Some(Scripted::Ok(usage)) => Ok(PolledReading {
                    usage: *usage,
                    severity: None,
                }),
                Some(Scripted::Unauthorized) => Err(Error::UsageUnauthorized),
                Some(Scripted::Locked) => Err(Error::KeychainLocked { op: "read" }),
                Some(Scripted::ScopeMissing) => Err(Error::UsageScopeMissing),
                Some(Scripted::RateLimited(retry_after)) => Err(Error::UsageRateLimited {
                    status: 429,
                    retry_after: *retry_after,
                }),
                // Explicit `Transient` and any unscripted account both land here.
                _ => Err(Error::UsageTransient {
                    status: 0,
                    retry_after: None,
                }),
            }
        }
    }

    /// Resolves on its `stop_at`-th `requested()` call. The run loop polls
    /// `requested()` ONCE at start-up (the issue #76 de-burst shutdown-check, before
    /// the first poll) and then once per idle cycle — so `after(n)` lets the loop run
    /// `n - 1` ticks before stopping. Each run-loop test sizes `stop_at` to
    /// `desired_ticks + 1` accordingly.
    struct FakeShutdown {
        calls: Cell<u32>,
        stop_at: u32,
    }

    impl FakeShutdown {
        fn after(stop_at: u32) -> Self {
            Self {
                calls: Cell::new(0),
                stop_at,
            }
        }
    }

    impl Shutdown for FakeShutdown {
        async fn requested(&mut self) {
            let n = self.calls.get() + 1;
            self.calls.set(n);
            if n >= self.stop_at {
                return;
            }
            std::future::pending::<()>().await;
        }
    }

    /// A control seam that never serves (its future never resolves), so it never
    /// wins the run loop's idle select.
    struct NoControl;

    impl Control for NoControl {
        async fn serve(&self, _snapshot: &StatusSnapshot) -> ControlYield {
            std::future::pending().await
        }
    }

    /// A control seam that records how many snapshots the run loop PUBLISHED to `watch`
    /// subscribers (issue #165), so a test can assert the loop feeds the channel once per tick.
    /// Its `serve` never resolves (like [`NoControl`]), so it never wins the idle select — only the
    /// default-overriding `publish` is exercised. Single-thread interior mutability (`Rc<Cell>`),
    /// matching the other hermetic run-loop fakes (ADR-0001, `!Send`).
    struct RecordingControl {
        published: Rc<Cell<usize>>,
    }

    impl Control for RecordingControl {
        async fn serve(&self, _snapshot: &StatusSnapshot) -> ControlYield {
            std::future::pending().await
        }
        fn publish(&self, _snapshot: &StatusSnapshot) {
            self.published.set(self.published.get() + 1);
        }
    }

    /// The inert [`RefreshTicker`] for the hermetic run-loop tests (issue #105): never due,
    /// sweeps nothing — so the periodic-refresh arm never wins the idle select and these
    /// tests behave exactly as they did before #105. Production wires the real
    /// [`crate::refresh_tick::RefreshTick`] (disabled by default).
    struct NoopRefreshTicker;

    impl RefreshTicker for NoopRefreshTicker {
        fn recovery_pending(&self, _excluded: &[String], _quarantined: &[String]) -> bool {
            false
        }
        async fn until_due(&mut self, _has_recovery_work: bool) {
            std::future::pending::<()>().await;
        }
        async fn sweep(&mut self, _excluded: &[String], _quarantined: &[String]) -> SweepOutcome {
            SweepOutcome::default()
        }
    }

    /// The inert [`ExternalLoginWatch`] for the hermetic run-loop tests (issue #140): never due,
    /// so the external-login-watch arm never wins the idle select and these tests behave exactly
    /// as they did before #140. Production wires the real [`ExternalLoginWatcher`]; the detection
    /// tests wire [`OnceExternalLogin`].
    struct NoopExternalLoginWatch;

    impl ExternalLoginWatch for NoopExternalLoginWatch {
        async fn until_due(&mut self) {
            std::future::pending::<()>().await;
        }
        async fn read_canonical(&mut self) -> Option<Credential> {
            None
        }
    }

    /// Lets the daemon and the external-login watch (issue #140) share ONE in-memory canonical
    /// item — as in production, where both read the SAME keychain item. The daemon takes its
    /// store by value, so an `Rc` clone is how a hermetic test keeps a second handle to write
    /// through (simulating an external `claude /login`) and observe.
    impl CredentialStore for Rc<FakeCredentialStore> {
        async fn read(&self) -> Result<Credential> {
            (**self).read().await
        }
        async fn write(&self, credential: &Credential) -> Result<()> {
            (**self).write(credential).await
        }
    }

    /// An [`ExternalLoginWatch`] that becomes due exactly ONCE — simulating a manual
    /// `claude /login` mid-idle by writing a fresh token to the SHARED canonical store on that
    /// first fire — then never resolves again. The external-login analog of [`OnceRosterReload`]:
    /// drives the live `Idle::ExternalLoginDetected` re-tick wiring (#140) that
    /// [`NoopExternalLoginWatch`] leaves inert. Because the store is shared with the daemon, the
    /// re-tick it triggers reads the fresh token too and re-stashes it — end-to-end proof that an
    /// external change is picked up off the poll cadence.
    struct OnceExternalLogin {
        fired: Cell<bool>,
        store: Rc<FakeCredentialStore>,
        fresh: Vec<u8>,
    }

    impl OnceExternalLogin {
        fn new(store: Rc<FakeCredentialStore>, fresh: &[u8]) -> Self {
            Self {
                fired: Cell::new(false),
                store,
                fresh: fresh.to_vec(),
            }
        }
    }

    impl ExternalLoginWatch for OnceExternalLogin {
        async fn until_due(&mut self) {
            if self.fired.replace(true) {
                std::future::pending::<()>().await;
            } else {
                // Simulate the external `claude /login` rewriting the shared canonical NOW — so
                // both `read_canonical` below and the daemon's re-tick see the fresh token.
                self.store.write(&cred(&self.fresh)).await.unwrap();
            }
        }
        async fn read_canonical(&mut self) -> Option<Credential> {
            self.store.read().await.ok()
        }
    }

    /// An [`ExternalLoginWatch`] that becomes due exactly ONCE and returns a PRELOADED
    /// `read_canonical` result — `Some(unchanged)` for the healthy no-change path, or `None` for
    /// an unreadable/locked probe (the fail-safe path) — recording that the probe was reached.
    /// Touches no store: it proves the run loop's detection arm ran and correctly did NOT re-tick
    /// (no break), without simulating any external write.
    struct ScriptedExternalLogin {
        fired: Cell<bool>,
        result: Option<Credential>,
        probed: Cell<bool>,
    }

    impl ScriptedExternalLogin {
        fn returning(result: Option<Credential>) -> Self {
            Self {
                fired: Cell::new(false),
                result,
                probed: Cell::new(false),
            }
        }
    }

    impl ExternalLoginWatch for ScriptedExternalLogin {
        async fn until_due(&mut self) {
            if self.fired.replace(true) {
                std::future::pending::<()>().await;
            }
        }
        async fn read_canonical(&mut self) -> Option<Credential> {
            self.probed.set(true);
            self.result.clone()
        }
    }

    /// A [`RefreshTicker`] that becomes due exactly ONCE — then never again — and records the
    /// exclusion set each sweep is handed. The periodic-refresh analog of [`OnceManualSwap`]:
    /// it drives the live `until_due → sweep` idle wiring (#105) that [`NoopRefreshTicker`]
    /// deliberately leaves inert, so a run-loop test can prove the sweep runs and receives the
    /// daemon's exclusions.
    struct OnceRefreshTicker {
        fired: Cell<bool>,
        swept: RefCell<Vec<Vec<String>>>,
        swept_quarantined: RefCell<Vec<Vec<String>>>,
        outcome: RefCell<SweepOutcome>,
        /// The `has_recovery_work` flag each `until_due` call was handed (issue #280), in call
        /// order — so a run-loop test can prove the daemon threads the "≥1 quarantined-parked"
        /// signal into the tick's DUE computation (not only into `sweep`).
        due_recovery: RefCell<Vec<bool>>,
    }

    impl OnceRefreshTicker {
        fn new() -> Self {
            Self {
                fired: Cell::new(false),
                swept: RefCell::new(Vec::new()),
                swept_quarantined: RefCell::new(Vec::new()),
                outcome: RefCell::new(SweepOutcome::default()),
                due_recovery: RefCell::new(Vec::new()),
            }
        }
        /// Pre-load the [`SweepOutcome`] the single sweep returns — its refresh events to log
        /// and the restores to apply (issue #106).
        fn returning(outcome: SweepOutcome) -> Self {
            let ticker = Self::new();
            *ticker.outcome.borrow_mut() = outcome;
            ticker
        }
    }

    impl RefreshTicker for OnceRefreshTicker {
        fn recovery_pending(&self, _excluded: &[String], quarantined: &[String]) -> bool {
            // A simple stand-in for the real predicate (the allowlist/exclusion logic is unit-tested
            // on `RefreshTick` itself): any quarantined account counts as restore work here, so the
            // run loop threads a prompt whenever the daemon reports a quarantined-parked account.
            !quarantined.is_empty()
        }
        async fn until_due(&mut self, has_recovery_work: bool) {
            // Record the recovery signal the run loop threaded in (issue #280) before deciding
            // readiness, so a test can assert the first wait saw the quarantined-parked prompt.
            self.due_recovery.borrow_mut().push(has_recovery_work);
            // Ready the first time, pending forever after — so the sweep fires once, then the
            // idle select falls through to `wait`/shutdown on every later iteration.
            if self.fired.replace(true) {
                std::future::pending::<()>().await;
            }
        }
        async fn sweep(&mut self, excluded: &[String], quarantined: &[String]) -> SweepOutcome {
            self.swept.borrow_mut().push(excluded.to_vec());
            self.swept_quarantined
                .borrow_mut()
                .push(quarantined.to_vec());
            std::mem::take(&mut *self.outcome.borrow_mut())
        }
    }

    /// A [`RefreshTicker`] whose sweep WEDGES (never resolves) after becoming due once — to
    /// prove the run loop's nested select lets a shutdown cut an in-flight sweep instead of
    /// deadlocking on a stuck refresh cycle. (`timeout_secs` bounds a real wedge in production;
    /// here the test asserts the shutdown path directly.)
    struct HangingRefreshTicker {
        fired: Cell<bool>,
    }

    impl HangingRefreshTicker {
        fn new() -> Self {
            Self {
                fired: Cell::new(false),
            }
        }
    }

    impl RefreshTicker for HangingRefreshTicker {
        fn recovery_pending(&self, _excluded: &[String], _quarantined: &[String]) -> bool {
            false
        }
        async fn until_due(&mut self, _has_recovery_work: bool) {
            if self.fired.replace(true) {
                std::future::pending::<()>().await;
            }
        }
        async fn sweep(&mut self, _excluded: &[String], _quarantined: &[String]) -> SweepOutcome {
            std::future::pending::<SweepOutcome>().await
        }
    }

    /// A control seam that yields `ManualSwapped` exactly once, then never resolves —
    /// so the run loop adopts the manual hold on its first idle, then idles normally
    /// (to `wait`/shutdown) on every later poll. Drives the live
    /// `Idle::ManualSwapped => adopt_manual_swap` wiring that `NoControl` never does.
    struct OnceManualSwap {
        fired: Cell<bool>,
    }

    impl OnceManualSwap {
        fn new() -> Self {
            Self {
                fired: Cell::new(false),
            }
        }
    }

    impl Control for OnceManualSwap {
        async fn serve(&self, _snapshot: &StatusSnapshot) -> ControlYield {
            if self.fired.replace(true) {
                std::future::pending().await
            } else {
                ControlYield::Signal(Some(ControlSignal::ManualSwapped))
            }
        }
    }

    /// A control seam that yields `RosterReloadRequested` exactly once, then never
    /// resolves (issue #139) — so the run loop reloads the roster on its first idle,
    /// then idles normally on every later poll. The roster-reload analog of
    /// [`OnceManualSwap`]: drives the live `Idle::RosterReloadRequested =>
    /// adopt_roster_reload` wiring that `NoControl` never does.
    struct OnceRosterReload {
        fired: Cell<bool>,
    }

    impl OnceRosterReload {
        fn new() -> Self {
            Self {
                fired: Cell::new(false),
            }
        }
    }

    impl Control for OnceRosterReload {
        async fn serve(&self, _snapshot: &StatusSnapshot) -> ControlYield {
            if self.fired.replace(true) {
                std::future::pending().await
            } else {
                ControlYield::Signal(Some(ControlSignal::RosterReloadRequested))
            }
        }
    }

    /// A control seam that yields `Restored(uuid)` exactly once, then never resolves
    /// (issue #275) — so the run loop un-quarantines the named account on its first idle,
    /// then idles normally on every later poll. The on-demand-restore analog of
    /// [`OnceRosterReload`]: drives the live `Idle::Restored => apply_refresh_restore`
    /// wiring that `NoControl` never does. Holds the target `uuid` and clones it into the
    /// payload each `serve` (which borrows `&self`), mirroring the real socket path where
    /// the uuid arrives in the request line.
    struct OnceRestored {
        uuid: String,
        fired: Cell<bool>,
    }

    impl OnceRestored {
        fn new(uuid: &str) -> Self {
            Self {
                uuid: uuid.to_owned(),
                fired: Cell::new(false),
            }
        }
    }

    impl Control for OnceRestored {
        async fn serve(&self, _snapshot: &StatusSnapshot) -> ControlYield {
            if self.fired.replace(true) {
                std::future::pending().await
            } else {
                ControlYield::Signal(Some(ControlSignal::Restored(self.uuid.clone())))
            }
        }
    }

    /// A control seam that yields `ShutdownRequested` exactly once (issue #397), then never
    /// resolves — so the run loop takes its graceful `Idle::Shutdown` exit on the FIRST idle,
    /// exactly as an authenticated `{"cmd":"shutdown"}` control command (the `daemon stop` path
    /// for an unmanaged daemon) would. The stop analog of [`OnceRosterReload`]: drives the live
    /// `Signal(ShutdownRequested) => break Idle::Shutdown` wiring that `NoControl` never does.
    struct OnceShutdown {
        fired: Cell<bool>,
    }

    impl OnceShutdown {
        fn new() -> Self {
            Self {
                fired: Cell::new(false),
            }
        }
    }

    impl Control for OnceShutdown {
        async fn serve(&self, _snapshot: &StatusSnapshot) -> ControlYield {
            if self.fired.replace(true) {
                std::future::pending().await
            } else {
                ControlYield::Signal(Some(ControlSignal::ShutdownRequested))
            }
        }
    }

    /// A [`Control`] that hands back a single `swap` command (issue #167) on its first serve — the
    /// run-loop counterpart of [`OnceRestored`], carrying a REAL socket end so a test can read the
    /// redacted ack the loop writes back. Pends forever after, so the loop then idles on the tick
    /// timer / shutdown exactly like [`NoControl`].
    struct OnceSwap {
        command: SwapCommand,
        stream: RefCell<Option<tokio::net::UnixStream>>,
        fired: Cell<bool>,
    }

    impl Control for OnceSwap {
        async fn serve(&self, _snapshot: &StatusSnapshot) -> ControlYield {
            if self.fired.replace(true) {
                std::future::pending().await
            } else {
                let stream = self
                    .stream
                    .borrow_mut()
                    .take()
                    .expect("the swap stream is handed back exactly once");
                ControlYield::Swap(stream, self.command.clone())
            }
        }
    }

    /// A [`Control`] that hands back a single `capture` command (issue #359) on its first serve — the
    /// capture counterpart of [`OnceSwap`], carrying a REAL socket end so a test can read the
    /// redacted ack the loop writes back. Pends forever after, so the loop then idles on the tick
    /// timer / shutdown exactly like [`NoControl`].
    struct OnceCapture {
        command: CaptureCommand,
        stream: RefCell<Option<tokio::net::UnixStream>>,
        fired: Cell<bool>,
    }

    impl Control for OnceCapture {
        async fn serve(&self, _snapshot: &StatusSnapshot) -> ControlYield {
            if self.fired.replace(true) {
                std::future::pending().await
            } else {
                let stream = self
                    .stream
                    .borrow_mut()
                    .take()
                    .expect("the capture stream is handed back exactly once");
                ControlYield::Capture(stream, self.command.clone())
            }
        }
    }

    // --- builders ----------------------------------------------------------

    fn account(uuid: &str, label: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            label: label.to_owned(),
            enabled: true,
        }
    }

    /// A roster account that starts parked (issue #36) — for the disable paths.
    fn disabled_account(uuid: &str, label: &str) -> Account {
        Account {
            enabled: false,
            ..account(uuid, label)
        }
    }

    fn tunables(trigger: u8, floor: u8, cooldown: u64) -> Tunables {
        // Weekly trigger fixed high (98) so the existing tests' weekly readings
        // (all well below it) never trip the new weekly path (issue #41): these
        // tests pin the SESSION trigger. A fixed strategy draws no RNG, so the
        // per-cycle draw sequence — and every seeded-jitter test — is unchanged.
        const WEEKLY_TRIGGER: u8 = 98;
        Tunables {
            poll_secs: 60,
            cooldown_secs: cooldown,
            // Most daemon tests set an explicit floor; `tunables_floor_off` sets it
            // inert (== trigger) for the tests that pin the always-on gate instead.
            target_max_usage: floor,
            session_trigger: trigger,
            weekly_trigger: WEEKLY_TRIGGER,
            monitor_401_n: 3,
            monitor_recovery_m: 2,
            // Existing daemon tests exercise the fixed (no-jitter) path: each
            // strategy draws its base verbatim, identical to the pre-#38 scalars.
            poll_strategy: Strategy::fixed(60.0),
            trigger_strategy: Strategy::fixed(f64::from(trigger)),
            weekly_trigger_strategy: Strategy::fixed(f64::from(WEEKLY_TRIGGER)),
            cooldown_strategy: Strategy::fixed(cooldown as f64),
        }
    }

    /// Tunables with the target-max-usage reserve INERT — set to `session_trigger`, so
    /// `pick_target`'s floor filter never tightens beyond the always-on session gate
    /// (config allows `target_max_usage == session_trigger`). Post-#398 the floor is
    /// always-valued, so "no extra tightening" is expressed this way rather than the
    /// removed opt-out; behaviorally identical to the old `None` for target selection.
    /// The tests that use it pin the always-on gate / weekly behavior, not the reserve.
    fn tunables_floor_off(trigger: u8, cooldown: u64) -> Tunables {
        tunables(trigger, trigger, cooldown)
    }

    fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    fn oauth(uuid: &str) -> OauthAccount {
        OauthAccount::from_object_bytes(
            format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{uuid}@example.com"}}"#).as_bytes(),
        )
        .unwrap()
    }

    fn stashed(token: &[u8], uuid: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(token),
            oauth_account: oauth(uuid),
        }
    }

    /// A temp `~/.claude.json` displaying `uuid`. Returns the tempdir guard + path.
    fn claude_json(uuid: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"numStartups":1,"oauthAccount":{{"accountUuid":"{uuid}","emailAddress":"{uuid}@x.com"}}}}"#
            ),
        )
        .unwrap();
        (dir, path)
    }

    fn displayed_uuid(path: &Path) -> Option<String> {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        value["oauthAccount"]["accountUuid"]
            .as_str()
            .map(str::to_owned)
    }

    async fn store_holding(blob: &[u8]) -> FakeCredentialStore {
        let store = FakeCredentialStore::empty();
        store.write(&cred(blob)).await.unwrap();
        store
    }

    async fn stash_with(entries: &[(&str, &[u8], &str)]) -> FakeAccountStash {
        let stash = FakeAccountStash::empty();
        for (service, token, uuid) in entries {
            stash.write(service, &stashed(token, uuid)).await.unwrap();
        }
        stash
    }

    type FakeDaemon = Daemon<FakeRosterPoller, FakeCredentialStore, FakeAccountStash, FakeClock>;

    /// Write a minimal valid `config.toml` at `path` carrying `accounts` as `(uuid,
    /// label)` pairs — the on-disk fixture the runtime roster-reload (#139) re-reads.
    /// Tunables are omitted (they default via `#[serde(default)]`), so this exercises
    /// the exact `Config::load_path` path `adopt_roster_reload` takes. Written in one
    /// `std::fs::write`, so a reader sees a complete file (production's atomic rename
    /// gives the same all-or-nothing guarantee).
    fn write_roster_config(path: &Path, accounts: &[(&str, &str)]) {
        let mut toml = String::new();
        for (uuid, label) in accounts {
            toml.push_str(&format!(
                "[[account]]\naccount_uuid = \"{uuid}\"\nlabel = \"{label}\"\nenabled = true\n\n"
            ));
        }
        std::fs::write(path, toml).unwrap();
    }

    /// Drive the staggered daemon (issue #80) through one full warm-up cycle — one
    /// tick per in-rotation account — and return the LAST tick's outcome. By then
    /// every account has a fresh last-known reading and the warm-up latch is set, so
    /// that tick takes the first real swap-away decision on a FULL reading set: the
    /// staggered-era equivalent of the pre-#80 single poll-all `tick`. The latch is
    /// set inside the tick that polls the last schedule entry (before its
    /// `decide_action`), so the returned outcome already reflects the warmed decision.
    async fn warmed_tick(daemon: &mut FakeDaemon) -> TickOutcome {
        // Warm-up latches when the schedule's last distinct account is first polled. The
        // #366 active-interleave makes the schedule up to ~2·N long (the active re-inserted
        // before each peer), so warm-up can take up to 2·(N-1) ticks — bound the loop by
        // the max schedule length (2·roster + slack) so a misuse on a roster that can
        // NEVER warm up (no identifiable active AND nothing enabled → an empty schedule
        // whose `note_polled` never fires) still fails LOUDLY here instead of hanging the
        // test forever.
        let max_ticks = 2 * daemon.roster.len() + 1;
        for _ in 0..max_ticks {
            let outcome = daemon.tick().await;
            if daemon.state.warmed_up {
                return outcome;
            }
        }
        panic!("warm-up did not complete within {max_ticks} ticks — empty/degenerate schedule?");
    }

    // --- pick_target (pure) ------------------------------------------------

    // A weekly trigger well above every reading in the pick_target tests below, so
    // the weekly-exhaustion exclusion (#11) is a no-op for the ones that pin the
    // floor / selection behavior; the #11 tests use readings at/above it.
    const WK: f64 = 0.98;

    // A session trigger matching the default (`DEFAULT_SESSION_TRIGGER`), for the
    // always-on session anti-thrash gate. The selection/floor tests below keep every
    // viable winner's session below it, so the gate is a no-op for them; the dedicated
    // `pick_target_excludes_session_saturated_accounts` test exercises it.
    const SESS: f64 = 0.95;

    /// An all-enabled flag slice sized to `readings` (issue #36): the pre-#36
    /// pick_target tests pin the floor / selection / weekly-exhaustion behavior with
    /// every account enabled, so the new disabled exclusion is a no-op for them.
    fn all_on(readings: &[Option<Usage>]) -> Vec<bool> {
        vec![true; readings.len()]
    }

    #[test]
    fn pick_target_chooses_the_soonest_reset_among_viable_accounts() {
        // #37: among viable accounts the one whose weekly window resets SOONEST wins,
        // even when it does NOT have the most weekly headroom (the superseded rule).
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100), // soonest overall — but it is active
                session_resets_at: None,
            }), // index 0 = active (excluded)
            Some(Usage {
                session: 0.50,
                weekly: 0.60,                // less headroom than index 2…
                weekly_resets_at: Some(200), // …but resets soonest among viable -> winner
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.20,                // most headroom — would win the OLD rule…
                weekly_resets_at: Some(500), // …but resets latest
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.85,
                weekly: 0.01,
                weekly_resets_at: Some(50), // earliest of all — but session over floor
                session_resets_at: None,
            }), // session over floor -> not viable
        ];
        // Index 1 (reset 200) beats the most-headroom index 2 (reset 500); index 0 is
        // active and index 3 fails the floor, so neither earlier reset is eligible.
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), SESS, WK),
            Some(1)
        );
    }

    #[test]
    fn pick_target_with_reason_reports_soonest_reset_when_two_or_more_qualify() {
        // The #393 rationale for the #37 axis: with ≥2 viable candidates the winner carries
        // `SoonestReset` holding its OWN weekly-reset epoch — the `min_by_key` key `pick_target`
        // formerly computed then discarded before serialization. Index 1 (reset 200) beats the
        // more-headroom index 2 (reset 500), and its reason is the reset it won on.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100),
                session_resets_at: None,
            }), // index 0 = active (excluded)
            Some(Usage {
                session: 0.50,
                weekly: 0.60,
                weekly_resets_at: Some(200),
                session_resets_at: None,
            }), // winner — soonest reset among viable
            Some(Usage {
                session: 0.10,
                weekly: 0.20,
                weekly_resets_at: Some(500),
                session_resets_at: None,
            }), // more headroom, but later reset
        ];
        assert_eq!(
            pick_target_with_reason(0, &readings, &all_on(&readings), Some(0.80), SESS, WK),
            Some((1, NextSwapReason::SoonestReset { resets_at: 200 })),
        );
    }

    #[test]
    fn pick_target_with_reason_reports_only_candidate_for_a_lone_viable_target() {
        // A SOLE qualifying account — index 0 active, index 1 unavailable — so no reset-time
        // comparison applied: the reason is `OnlyCandidate` even though the winner has a known
        // reset, because there was nothing to be "soonest" among.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100),
                session_resets_at: None,
            }), // index 0 = active (excluded)
            None, // unavailable
            Some(Usage {
                session: 0.10,
                weekly: 0.30,
                weekly_resets_at: Some(400),
                session_resets_at: None,
            }), // the only viable candidate
        ];
        assert_eq!(
            pick_target_with_reason(0, &readings, &all_on(&readings), Some(0.80), SESS, WK),
            Some((2, NextSwapReason::OnlyCandidate)),
        );
    }

    #[test]
    fn pick_target_with_reason_reports_roster_order_when_no_candidate_knows_its_reset() {
        // The degenerate case: ≥2 viable candidates but NONE reported a weekly reset, so the
        // winner fell to roster order (`min_by_key`'s unknown-last tie-break) — there is no epoch
        // to carry, so the reason is neither a hollow `SoonestReset` NOR `OnlyCandidate` (index 2
        // was equally viable; claiming "only viable target" would be the #393 bug again). It is
        // `RosterOrder`: the axis that actually decided.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }), // index 0 = active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.20,
                weekly_resets_at: None,
                session_resets_at: None,
            }), // winner by roster order (no reset to compare)
            Some(Usage {
                session: 0.20,
                weekly: 0.30,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target_with_reason(0, &readings, &all_on(&readings), Some(0.80), SESS, WK),
            Some((1, NextSwapReason::RosterOrder)),
        );
    }

    #[test]
    fn pick_target_excludes_the_active_account_and_unavailable_readings() {
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            None, // unavailable
            Some(Usage {
                session: 0.10,
                weekly: 0.30,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), SESS, WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_is_none_when_every_candidate_is_over_the_floor() {
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.90,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.81,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), SESS, WK),
            None
        );
    }

    #[test]
    fn pick_target_breaks_a_reset_tie_by_roster_order() {
        // #37: when two viable accounts share the same weekly reset, the earlier
        // roster index wins — matching soonest_weekly_reset's tie-break (#11). The
        // superseded rule would have picked index 2 here on its lower session.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100),
                session_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.40,
                weekly: 0.20,
                weekly_resets_at: Some(300), // tie -> first of the tie wins
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.20,
                weekly: 0.20,
                weekly_resets_at: Some(300), // tie, lower session (the OLD winner)
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), SESS, WK),
            Some(1)
        );
    }

    #[test]
    fn pick_target_prefers_a_known_reset_over_an_unknown_one() {
        // #37: an account with a known reset is preferred over one whose reset is
        // unknown (None sorts last) — even when the unknown-reset account has an
        // earlier roster index and more weekly headroom.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.05,           // more headroom + earlier index…
                weekly_resets_at: None, // …but no known reset -> sorts last
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.40,
                weekly_resets_at: Some(900), // a known reset -> preferred
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, SESS, WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_falls_back_to_roster_order_when_no_reset_is_known() {
        // #37: with no viable account reporting a weekly reset, selection falls back
        // to the earliest roster index (the all-unknown tie) — NOT to weekly headroom
        // (the superseded rule, which would have picked the lower-weekly index 2).
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.30, // more weekly used, earlier index -> winner
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.05, // most headroom, but no reset and a later index
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, SESS, WK),
            Some(1)
        );
    }

    #[test]
    fn pick_target_excludes_session_saturated_accounts() {
        // The session-dimension mirror of #11's weekly exclusion: an account at/above
        // the SESSION trigger is not a viable target, even with the floor OFF and ample
        // weekly headroom — swapping there re-trips swap::decide's session dimension
        // next cycle and thrashes. This is the fix for the observed fr <-> pelykh.com
        // ping-pong: the soonest-reset account is session-saturated, so a session-fresh
        // account wins despite its later reset (rather than the two saturated accounts
        // flapping between each other).
        let readings = vec![
            Some(Usage {
                session: 0.50,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.95, // at the session trigger -> not viable, however soon it resets
                weekly: 0.10,
                weekly_resets_at: Some(100), // soonest reset, but session-saturated
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.05, // the only session-fresh other
                weekly: 0.20,
                weekly_resets_at: Some(300), // later reset, but viable
                session_resets_at: None,
            }),
        ];
        // Floor OFF: index 1 is still excluded by the always-on session gate (0.95 is
        // NOT < 0.95), so the session-fresh index 2 wins despite its later reset.
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, SESS, WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_floor_tightens_below_the_always_on_session_gate() {
        // The opt-in target_max_usage (#10) is a STRICTER reserve layered on the always-on
        // session gate: with the floor OFF a target need only clear the gate
        // (session < trigger); an enabled floor also excludes accounts that pass the
        // gate but sit at/above the floor. Effective ceiling = min(session_trigger, floor).
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100),
                session_resets_at: None,
            }), // index 0 = active (excluded)
            Some(Usage {
                session: 0.85, // clears the 0.95 gate, so viable with the floor off…
                weekly: 0.10,
                weekly_resets_at: Some(200), // …and the soonest-reset viable target
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.05,
                weekly: 0.60,
                weekly_resets_at: Some(300),
                session_resets_at: None,
            }), // session-fresh, resets later
        ];
        // Floor OFF → index 1 clears the gate (0.85 < 0.95) and wins on soonest reset…
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, SESS, WK),
            Some(1)
        );
        // …whereas an enabled 80% floor excludes index 1 (0.85 >= 0.80) and falls to index 2.
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), SESS, WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_excludes_weekly_exhausted_accounts() {
        // #11: an account at/above the weekly trigger is not a viable target, even
        // with the target-max-usage OFF and ample session headroom — swapping there
        // would only re-trigger and thrash.
        let readings = vec![
            Some(Usage {
                session: 0.50,
                weekly: 0.99,
                weekly_resets_at: None,
                session_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.99, // weekly-exhausted -> not viable despite low session
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.20, // the only non-exhausted other account
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, SESS, WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_is_none_when_every_other_account_is_weekly_exhausted() {
        // #11 core: with the floor off, the ONLY thing that makes all others
        // non-viable is weekly exhaustion — at/above the trigger (inclusive).
        let readings = vec![
            Some(Usage {
                session: 0.50,
                weekly: 0.99,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.98, // exactly at the trigger -> exhausted (>=)
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 1.00,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, SESS, WK),
            None
        );
    }

    #[test]
    fn pick_target_excludes_a_disabled_account_even_when_it_resets_soonest() {
        // #36 × #37: index 1 resets soonest (it would win the new rule) but is
        // disabled, so it is never a target; selection falls to the enabled index 2.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(500),
                session_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.05,
                weekly_resets_at: Some(100), // soonest reset — the would-be winner…
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.30,
                weekly_resets_at: Some(200),
                session_resets_at: None,
            }),
        ];
        let enabled = [true, false, true]; // …but index 1 is parked
        assert_eq!(pick_target(0, &readings, &enabled, None, SESS, WK), Some(2));
    }

    #[test]
    fn pick_target_a_disabled_account_does_not_rescue_an_all_exhausted_roster() {
        // #11 × #36: the only account with weekly headroom is disabled, so the
        // verdict is still no-viable-target — a parked account must not hold the
        // daemon out of the all-exhausted terminal state, however soon it resets.
        let readings = vec![
            Some(Usage {
                session: 0.50,
                weekly: 0.99,
                weekly_resets_at: None,
                session_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.98, // enabled but weekly-exhausted
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.01, // ample headroom + soonest reset — but disabled, so not viable
                weekly_resets_at: Some(100),
                session_resets_at: None,
            }),
        ];
        let enabled = [true, true, false];
        assert_eq!(pick_target(0, &readings, &enabled, None, SESS, WK), None);
    }

    // --- soonest_weekly_reset (pure, #11) ---------------------------------

    #[test]
    fn soonest_weekly_reset_picks_the_earliest_known_timestamp() {
        let readings = vec![
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(300),
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(100), // soonest
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(200),
                session_resets_at: None,
            }),
            None,
        ];
        assert_eq!(soonest_weekly_reset(&readings), Some((1, 100)));
    }

    #[test]
    fn soonest_weekly_reset_ignores_unknowns_and_breaks_ties_to_first() {
        // Accounts without a known reset are skipped; an exact tie keeps the
        // earliest roster index.
        let tie = vec![
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(500), // first of the tie -> winner
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(500),
                session_resets_at: None,
            }),
        ];
        assert_eq!(soonest_weekly_reset(&tie), Some((1, 500)));
        // All-unknown → None (the caller falls back to the active account).
        let none = vec![
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            None,
        ];
        assert_eq!(soonest_weekly_reset(&none), None);
    }

    #[test]
    fn all_exhausted_relief_names_the_soonest_session_reset_among_blocked_spares() {
        // #417 secondary: on the session-wide branch, the relief hint keys off the
        // SOONEST session reset among weekly-viable-but-session-blocked spares
        // (`session_relief.is_none_or(|(_, best)| at < best)`, ADR-0013 Decision 4). The
        // existing session-branch coverage uses `session_resets_at: None` (only the
        // fallback-naming arm fires), so an inverted comparison (`at > best`) or a wrong
        // index would ship green. Here TWO spares qualify with DISTINCT session resets and
        // the later-indexed one resets sooner, so a correct comparison must override the
        // first-seen fallback.
        let session_ceiling = 0.80_f64;
        let weekly_trigger = 0.95_f64;
        let readings = vec![
            // idx 0: the exhausted active account — skipped (active == 0).
            Some(Usage {
                session: 0.99,
                weekly: 0.99,
                weekly_resets_at: Some(500),
                session_resets_at: Some(500),
            }),
            // idx 1: weekly-viable, session-blocked; first seen → the naming fallback.
            Some(Usage {
                session: 0.90,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: Some(300),
            }),
            // idx 2: weekly-viable, session-blocked; resets SOONEST → the winner.
            Some(Usage {
                session: 0.85,
                weekly: 0.20,
                weekly_resets_at: None,
                session_resets_at: Some(150),
            }),
        ];
        let enabled = vec![true, true, true];
        let (cause, hold_idx, resets_at) =
            all_exhausted_relief(0, &readings, &enabled, session_ceiling, weekly_trigger);
        assert_eq!(cause, SwapReason::Session);
        // idx 2 (soonest, 150) wins over idx 1 (first-seen fallback, 300).
        assert_eq!(hold_idx, 2);
        assert_eq!(resets_at, Some(150));
    }

    // --- tick: decision + swap --------------------------------------------

    #[tokio::test]
    async fn tick_swaps_active_over_trigger_to_the_soonest_reset_target() {
        // #37 end-to-end: the active account is over its trigger; among the two viable
        // targets the daemon picks the one that resets SOONEST — even though the other
        // has more weekly headroom (the superseded rule would have picked it).
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        const B_RESET: i64 = 1_782_864_000; // 2026-07-01 — later
        const C_RESET: i64 = 1_782_496_800; // 2026-06-26 — soonest
        let poller = FakeRosterPoller::new()
            .ok_resets("u-A", 0.97, 0.40, 1_782_777_600) // active: over trigger
            .ok_resets("u-B", 0.10, 0.20, B_RESET) // viable, most headroom but resets later
            .ok_resets("u-C", 0.30, 0.50, C_RESET); // viable, resets soonest -> winner
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 2 });
        // The canonical item now holds C's token, and the display shows C…
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"C-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-C"));
        // …and the in-memory active advanced to C, so the next read polls C.
        assert_eq!(daemon.state.active, Some(2));
    }

    #[tokio::test]
    async fn swap_tick_snapshot_marks_the_now_active_account_not_the_swapped_away_one() {
        // #117: the SAME tick that performs a swap must build its `status` snapshot from
        // the POST-swap active index, so the control socket serves the now-active account
        // as `*` immediately — not the swapped-AWAY account for one poll interval. The
        // regression: `Daemon::tick` captured `active` (a `Copy`) BEFORE `decide_action`
        // performed the swap, then built the snapshot from that stale copy, while the
        // sibling `locked_tick` already passed the live `self.state.active`. Drive a real
        // swap (`work` index 0 over the session trigger → the only viable target `spare`
        // index 1) and pin the snapshot's per-account `active` flag to the PHYSICAL
        // outcome — the gap the existing swap tests left (they assert `state.active` and
        // the `~/.claude.json` write, never the snapshot flag the `status` CLI reads).
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40) // active, over the session trigger
            .ok("u-B", 0.05, 0.05); // the only viable target
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = warmed_tick(&mut daemon).await;

        // Precondition: a real swap occurred OFF `work` (0) ONTO `spare` (1).
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        assert_eq!(daemon.state.active, Some(1));

        // The snapshot served THIS tick must already reflect the swap: `*` on `spare`,
        // not on `work`. Pre-fix this asserted the inverse (work active, spare not).
        let work = &outcome.snapshot.accounts[0];
        let spare = &outcome.snapshot.accounts[1];
        assert_eq!(work.label, "work");
        assert_eq!(spare.label, "spare");
        assert!(
            spare.active,
            "the swapped-ONTO account must be active in the swap-tick snapshot",
        );
        assert!(
            !work.active,
            "the swapped-AWAY account must NOT be active in the swap-tick snapshot",
        );
    }

    #[tokio::test]
    async fn tick_excludes_a_disabled_account_from_polling_and_targeting() {
        // #36 end-to-end: the active account is over its trigger; the parked account
        // (index 1) would be an obvious target but is disabled, so the swap goes to
        // the enabled `spare` (index 2) instead — and the parked account is never
        // polled, so its snapshot reading stays absent despite a scripted `ok`.
        let roster = vec![
            account("u-A", "work"),
            disabled_account("u-B", "parked"),
            account("u-C", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40) // active: over trigger
            .ok("u-B", 0.01, 0.01) // parked: would be an obvious target IF polled
            .ok("u-C", 0.30, 0.50); // enabled, viable
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        let outcome = warmed_tick(&mut daemon).await;

        // Swapped to the ENABLED spare, not the parked account.
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 2 });
        assert_eq!(daemon.state.active, Some(2));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-C"));
        // The parked account was skipped by the poll loop: its reading is absent
        // even though the poller was scripted to return one for it.
        let parked = &outcome.snapshot.accounts[1];
        assert_eq!(parked.label, "parked");
        assert!(!parked.enabled, "the snapshot marks it disabled");
        assert!(
            parked.usage.is_none(),
            "a disabled account is not polled, so its reading stays absent"
        );
    }

    #[tokio::test]
    async fn tick_holds_when_active_is_below_the_trigger() {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.30)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // Tick 1 polls the active `work` first (issue #80 stagger), below its trigger.
        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::Held);
        // No swap has happened, and only the active account has been polled this tick
        // (#80 stagger) — so the next-swap candidate is `awaiting usage data` (#88).
        assert_eq!(first.snapshot.next_swap, Some(NextSwap::AwaitingData));
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        // Diagnostic channel (#77): ONE per-poll line this tick (the staggered loop
        // polls a single account per sub-interval), then the per-tick Hold decision
        // with NO back-off — and, not having been in the all-exhausted state, NO leave
        // marker.
        assert_eq!(
            first.diagnostics,
            vec![
                Diagnostic::Poll {
                    account: "work".to_owned(),
                    outcome: PollClass::Live,
                },
                Diagnostic::Tick {
                    decision: DecisionClass::Hold,
                    backoff_secs: None,
                    retry_after_secs: None,
                },
            ],
        );
        // Tick 2 polls the non-active `spare` (next in the round-robin) and still holds
        // — both accounts are below trigger, just observed one sub-interval apart.
        let second = daemon.tick().await;
        assert_eq!(second.action, TickAction::Held);
        assert_eq!(
            second.diagnostics,
            vec![
                Diagnostic::Poll {
                    account: "spare".to_owned(),
                    outcome: PollClass::Live,
                },
                Diagnostic::Tick {
                    decision: DecisionClass::Hold,
                    backoff_secs: None,
                    retry_after_secs: None,
                },
            ],
        );
    }

    #[tokio::test]
    async fn tick_swaps_when_weekly_reaches_its_trigger_while_session_is_below() {
        // AC #2 (the new dimension, issue #41): the active account's SESSION usage
        // is comfortably below its trigger, but its WEEKLY usage has reached the
        // separate weekly trigger → swap to the (only) viable target.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A: session 0.50 (below the 95 session trigger) but weekly 0.98 (at the
        // helper's 98 weekly trigger) → must swap. B is open and session-viable.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.98)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn tick_holds_when_weekly_is_below_its_own_trigger_even_above_the_session_trigger() {
        // Issue #41: weekly is gated by its OWN (higher) trigger, not the session
        // one. Weekly 0.96 sits ABOVE the 0.95 session trigger yet BELOW the 0.98
        // weekly trigger, and session itself (0.50) is below its trigger — so the
        // cycle HOLDS. (Under a single-threshold rule keyed on session_trigger this
        // same reading would have swapped; the separate weekly trigger is exactly
        // what changes that.)
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.96)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0); // session trigger 95, weekly trigger 98

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::Held);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn tick_skips_without_swapping_when_the_active_poll_fails() {
        // Active A's poll fails (transient); B is wide open. Must NOT swap on
        // missing active data.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new().failing("u-A").ok("u-B", 0.05, 0.05);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::SkippedActiveUnavailable);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn tick_skips_when_the_active_account_cannot_be_identified() {
        // Canonical token matches no stash, and ~/.claude.json shows an account
        // not in the roster → active unknown → poll-only, no swap.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"unknown-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-STRANGER");
        let poller = FakeRosterPoller::new().ok("u-A", 0.99, 0.99);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::SkippedActiveUnknown);
        assert_eq!(daemon.state.active, None);
    }

    #[tokio::test]
    async fn tick_resolves_active_via_claude_json_when_the_canonical_token_has_drifted() {
        // Steady state: the active account's token has refreshed in place, so the
        // canonical matches NO stash. The `~/.claude.json` display (u-A, in the
        // roster) is the fallback that still identifies the active account.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-drifted-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-stale-token", "u-A"), // no longer matches canonical
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A below the trigger, so the cycle simply holds — the point is that the
        // active account was resolved at all (via the display, not a stash match).
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.30)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::Held);
        // Resolved to A purely through the `~/.claude.json` fallback branch.
        assert_eq!(daemon.state.active, Some(0));
    }

    // --- locked keychain & re-auth re-stash (issue #13) --------------------

    #[tokio::test]
    async fn a_locked_keychain_defers_the_whole_tick_and_signals_once() {
        // #13: a locked keychain defers the ENTIRE cycle — no resolve, no poll, no
        // swap — emits ONE edge-triggered keychain_locked_wait, and returns a
        // back-off as the next wait. The daemon never auto-unlocks or prompts; the
        // back-off is the whole response. A is set over the session trigger so that,
        // absent the lock, this cycle WOULD swap — proving the lock truly defers it.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);

        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::KeychainLocked);
        // One lock-wait event on the FIRST locked cycle (edge-triggered)…
        assert_eq!(first.events, vec![Event::KeychainLockedWait]);
        // …with the back-off starting at the base.
        assert_eq!(first.next_wait, Some(LOCK_BACKOFF_BASE));
        // The cycle deferred before resolving: no active account, no swap.
        assert_eq!(daemon.state.active, None);
        // status still answers — the roster is listed, every reading absent.
        assert_eq!(first.snapshot.accounts.len(), 2);
        assert!(first.snapshot.accounts.iter().all(|a| a.usage.is_none()));
        // Diagnostic channel (#77): a locked tick polls NOTHING (it short-circuits
        // before the poll loop), so there are NO per-poll lines — just the decision
        // line naming the deferral and the back-off wait it imposed.
        assert_eq!(
            first.diagnostics,
            vec![Diagnostic::Tick {
                decision: DecisionClass::KeychainLocked,
                backoff_secs: Some(LOCK_BACKOFF_BASE.as_secs()),
                retry_after_secs: None,
            }],
        );

        // A second locked cycle is SILENT (edge-triggered) and the back-off grows.
        let second = daemon.tick().await;
        assert_eq!(second.action, TickAction::KeychainLocked);
        assert!(
            second.events.is_empty(),
            "the lock signal is edge-triggered"
        );
        assert_eq!(second.next_wait, Some(LOCK_BACKOFF_BASE * 2));

        // The canonical was never written (no auto-unlock, no swap): once the lock
        // clears, it still holds A's original token.
        daemon.store.set_locked(false);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn the_locked_keychain_back_off_doubles_then_caps() {
        // #13: the deferred-cycle back-off grows exponentially from the base and
        // saturates at the cap, so a long lock settles at one retry per cap-interval
        // rather than spinning or growing without bound.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);
        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(daemon.tick().await.next_wait.unwrap());
        }
        // Doubling from the 1 s base, capped at the 60 s ceiling:
        // 1, 2, 4, 8, 16, 32, then 64→capped 60, then 60.
        assert_eq!(
            waits,
            vec![
                LOCK_BACKOFF_BASE,
                LOCK_BACKOFF_BASE * 2,
                LOCK_BACKOFF_BASE * 4,
                LOCK_BACKOFF_BASE * 8,
                LOCK_BACKOFF_BASE * 16,
                LOCK_BACKOFF_BASE * 32,
                LOCK_BACKOFF_CAP, // 64 s would exceed the cap → clamped
                LOCK_BACKOFF_CAP,
            ]
        );
    }

    #[tokio::test]
    async fn unlocking_the_keychain_resumes_normal_ticks_and_rearms_the_signal() {
        // #13: after a lock episode, the first readable cycle clears the back-off
        // (next_wait None → normal interval) and re-arms the edge-trigger, so a
        // LATER lock episode signals afresh and restarts the back-off at the base.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);
        let locked = daemon.tick().await;
        assert_eq!(locked.action, TickAction::KeychainLocked);
        assert_eq!(locked.events, vec![Event::KeychainLockedWait]);

        // Unlock: the next cycle reads normally, resolves the active account, holds,
        // and restores the normal interval.
        daemon.store.set_locked(false);
        let resumed = daemon.tick().await;
        assert_eq!(resumed.action, TickAction::Held);
        assert_eq!(resumed.next_wait, None);
        assert_eq!(daemon.state.active, Some(0));

        // A second lock episode signals again (the readable cycle re-armed the edge)
        // and the back-off restarts at the base, not where the first episode left off.
        daemon.store.set_locked(true);
        let relocked = daemon.tick().await;
        assert_eq!(relocked.events, vec![Event::KeychainLockedWait]);
        assert_eq!(relocked.next_wait, Some(LOCK_BACKOFF_BASE));
    }

    // --- per-account rate-limit / transient poll back-off (issue #293, revising #76) ---

    /// The `backoff_secs` the tick's decision diagnostic carried (issue #293) — the
    /// per-account back-off imposed on the account polled this tick, in whole seconds.
    /// `None` when the tick polled cleanly, skipped a backing-off account, or polled
    /// nothing.
    fn tick_backoff_secs(outcome: &TickOutcome) -> Option<u64> {
        outcome.diagnostics.iter().find_map(|d| match d {
            Diagnostic::Tick { backoff_secs, .. } => *backoff_secs,
            _ => None,
        })
    }

    /// The `retry_after_secs` the tick's decision diagnostic carried (issue #295) — the RAW
    /// server-advised `Retry-After` (pre-cap) the throttled poll supplied, in whole seconds.
    /// `None` when the server sent none (the wait is the self-capped exponential), or the
    /// tick imposed no back-off at all — the source label that tells the two apart.
    fn tick_retry_after_secs(outcome: &TickOutcome) -> Option<u64> {
        outcome.diagnostics.iter().find_map(|d| match d {
            Diagnostic::Tick {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        })
    }

    /// Whether the tick actually POLLED an account (emitted a `Diagnostic::Poll`) rather
    /// than SKIPPING a backing-off one (issue #293) — the observable that a per-account
    /// back-off window suppressed the poll this tick.
    fn tick_polled(outcome: &TickOutcome) -> bool {
        outcome
            .diagnostics
            .iter()
            .any(|d| matches!(d, Diagnostic::Poll { .. }))
    }

    /// A single-account ('u-A', active) daemon with the fixed 60 s poll interval — the
    /// seam the per-account poll back-off tests read (issue #293). Frozen clock (no jitter,
    /// so the back-off is `60 s × 2^streak`); a throttled account stays inside its window
    /// until a test `daemon.clock.advance(..)` moves the monotonic clock past the deadline,
    /// so the tests drive the widened re-poll spacing deterministically. Returns the
    /// tempdir guard so the caller keeps the displayed `~/.claude.json` alive.
    async fn rate_limit_daemon(poller: FakeRosterPoller) -> (tempfile::TempDir, FakeDaemon) {
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let daemon = Daemon::new(
            vec![account("u-A", "work")],
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        (dir, daemon)
    }

    #[tokio::test]
    async fn a_sustained_rate_limit_backs_off_instead_of_re_polling_at_the_fixed_interval() {
        // AC (issue #293): a sustained 429 WIDENS the account's effective poll spacing —
        // it is SKIPPED between re-attempts rather than re-polled at the fixed interval —
        // and the loop itself no longer globally backs off (`next_wait` stays `None`). The
        // first throttled poll arms a 2×-interval (120 s) window; each throttled re-poll
        // doubles it.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;

        // Tick 1: A is polled and throttled → arms a 120 s per-account window. The loop
        // does NOT globally back off; the back-off rides the decision diagnostic instead.
        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::SkippedActiveUnavailable);
        assert_eq!(first.next_wait, None);
        // Diagnostic channel (#77): the poll surfaces as the `rate_limited` class — NOT a
        // generic transient — and the per-tick decision carries the per-account back-off in
        // whole seconds. This is exactly the `429` storm the issue says was previously
        // invisible (the event log emits no event for a rate-limited poll).
        assert_eq!(
            first.diagnostics,
            vec![
                Diagnostic::Poll {
                    account: "work".to_owned(),
                    outcome: PollClass::RateLimited,
                },
                Diagnostic::Tick {
                    decision: DecisionClass::SkipActiveUnavailable,
                    backoff_secs: Some(120),
                    // No server `Retry-After` was scripted (issue #295): the 120 s wait is the
                    // self-capped exponential, so the source label is absent.
                    retry_after_secs: None,
                },
            ],
        );

        // Tick 2 (clock unmoved, still inside the 120 s window): A is SKIPPED — no poll, no
        // `Diagnostic::Poll`, no fresh back-off — which IS the widened spacing the AC asks
        // for (backing off instead of re-polling at the fixed interval).
        let skipped = daemon.tick().await;
        assert!(
            !tick_polled(&skipped),
            "the backing-off account must be skipped"
        );
        assert_eq!(skipped.next_wait, None);
        assert_eq!(tick_backoff_secs(&skipped), None);

        // Advancing past each window lets A be re-polled, and each throttled re-poll doubles
        // the window: 240 s, then 480 s.
        daemon.clock.advance(Duration::from_secs(120));
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(240));
        daemon.clock.advance(Duration::from_secs(240));
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(480));
    }

    #[tokio::test]
    async fn the_rate_limit_back_off_doubles_then_caps() {
        // The per-account back-off grows exponentially from the interval and saturates at
        // the cap, so a sustained-429 account settles at one re-poll per hour rather than
        // growing without bound — mirroring the locked-keychain back-off shape. Advancing
        // the clock past each window re-polls the (still-throttled) account, so its streak
        // climbs tick over tick.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        let mut waits = Vec::new();
        for _ in 0..8 {
            let secs = tick_backoff_secs(&daemon.tick().await).unwrap();
            waits.push(Duration::from_secs(secs));
            // Step past the just-armed window so the next tick re-polls (not skips) A.
            daemon.clock.advance(Duration::from_secs(secs));
        }
        // 60 s × 2^streak: 120, 240, 480, 960, 1920, then 3840→capped 3600, then 3600.
        assert_eq!(
            waits,
            vec![
                Duration::from_secs(120),
                Duration::from_secs(240),
                Duration::from_secs(480),
                Duration::from_secs(960),
                Duration::from_secs(1920),
                POLL_BACKOFF_CAP,
                POLL_BACKOFF_CAP,
                POLL_BACKOFF_CAP,
            ]
        );
    }

    #[tokio::test]
    async fn retry_after_is_honoured_as_a_minimum_wait() {
        // AC: Retry-After is honoured as a MINIMUM for the account's back-off window. When
        // it exceeds the exponential it wins; when it is smaller, the larger exponential
        // governs but the window is never below Retry-After.
        // Larger than the 120 s first-cycle exponential → Retry-After (600 s) wins.
        let (_d1, mut bigger) = rate_limit_daemon(
            FakeRosterPoller::new().rate_limited("u-A", Some(Duration::from_secs(600))),
        )
        .await;
        assert_eq!(tick_backoff_secs(&bigger.tick().await), Some(600));

        // Smaller than the exponential → the 120 s exponential governs (and is ≥ 10 s).
        let (_d2, mut smaller) = rate_limit_daemon(
            FakeRosterPoller::new().rate_limited("u-A", Some(Duration::from_secs(10))),
        )
        .await;
        assert_eq!(tick_backoff_secs(&smaller.tick().await), Some(120));
    }

    #[tokio::test]
    async fn a_large_retry_after_is_clamped_to_the_cap() {
        // AC (issue #294): a server `Retry-After` is honoured as a MINIMUM but clamped to a
        // sane MAXIMUM — POLL_BACKOFF_CAP — so a pathological value cannot dark the account
        // past the exponential ceiling. Supersedes the pre-#294 behaviour (a larger
        // `Retry-After` overrode the cap unboundedly), whose premise this reverses.
        //
        // A full day of `Retry-After` clamps down to the 1 h cap.
        let one_day = Duration::from_secs(86_400);
        assert!(one_day > POLL_BACKOFF_CAP);
        let (_d1, mut pathological) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", Some(one_day))).await;
        assert_eq!(
            tick_backoff_secs(&pathological.tick().await),
            Some(POLL_BACKOFF_CAP.as_secs()),
        );

        // Just below the cap is still honoured in full — the clamp bounds only the excess,
        // it does not swallow a legitimate long-but-sane `Retry-After`.
        let just_under = POLL_BACKOFF_CAP - Duration::from_secs(1);
        let (_d2, mut sane) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", Some(just_under))).await;
        assert_eq!(
            tick_backoff_secs(&sane.tick().await),
            Some(just_under.as_secs()),
        );
    }

    #[tokio::test]
    async fn the_tick_line_labels_the_back_off_source() {
        // AC (issue #295): the tick line distinguishes a SERVER-ADVISED wait from the
        // daemon's SELF-CAPPED exponential. `retry_after_secs` carries the RAW server
        // `Retry-After` (pre-cap) when the response supplied one, and is ABSENT otherwise —
        // so a `backoff_secs` an operator previously could not place now has a source. The
        // sibling `backoff_secs` (the effective wait) is unchanged; this pins the new label.

        // (1) Self-capped: no server `Retry-After` → the 120 s wait is the exponential, and
        // the source label is absent (the unambiguous "our own back-off" case).
        let (_d1, mut self_capped) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        let tick = self_capped.tick().await;
        assert_eq!(tick_backoff_secs(&tick), Some(120));
        assert_eq!(
            tick_retry_after_secs(&tick),
            None,
            "no server Retry-After ⇒ self-capped, so no source label",
        );

        // (2) Server-advised, dominant: a `Retry-After` (900 s) ABOVE the 120 s exponential
        // drives the wait, and the raw server value is surfaced beside it.
        let (_d2, mut dominant) = rate_limit_daemon(
            FakeRosterPoller::new().rate_limited("u-A", Some(Duration::from_secs(900))),
        )
        .await;
        let tick = dominant.tick().await;
        assert_eq!(tick_backoff_secs(&tick), Some(900));
        assert_eq!(tick_retry_after_secs(&tick), Some(900));

        // (3) Server-advised, dominated: a `Retry-After` (10 s) BELOW the 120 s exponential
        // does NOT drive the wait, yet the label still surfaces the raw server value — so the
        // operator sees the server WAS in the loop but our exponential (120 s) governed.
        let (_d3, mut dominated) = rate_limit_daemon(
            FakeRosterPoller::new().rate_limited("u-A", Some(Duration::from_secs(10))),
        )
        .await;
        let tick = dominated.tick().await;
        assert_eq!(tick_backoff_secs(&tick), Some(120));
        assert_eq!(tick_retry_after_secs(&tick), Some(10));

        // (4) Server-advised, capped: the flagship #295 case. A pathological `Retry-After`
        // (a full day) the #294 cap clamps to 1 h is now VISIBLE as the raw pre-cap label —
        // the exact `backoff_secs=3600` ambiguity (server-advised vs self-capped) resolved.
        let (_d4, mut capped) = rate_limit_daemon(
            FakeRosterPoller::new().rate_limited("u-A", Some(Duration::from_secs(86_400))),
        )
        .await;
        let tick = capped.tick().await;
        assert_eq!(tick_backoff_secs(&tick), Some(POLL_BACKOFF_CAP.as_secs()));
        assert_eq!(tick_retry_after_secs(&tick), Some(86_400));
    }

    #[tokio::test]
    async fn a_clean_cycle_resets_the_rate_limit_back_off() {
        // Once the account polls clean again the back-off clears (no more skipping, streak
        // reset), so a LATER 429 restarts at 2× — not where the prior episode left off.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(120));
        daemon.clock.advance(Duration::from_secs(120));
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(240));

        // A clean poll clears the back-off and resets the streak. Advance past the window
        // so the account is actually re-polled (not skipped) this tick.
        daemon.clock.advance(Duration::from_secs(240));
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let clean = daemon.tick().await;
        assert!(
            tick_polled(&clean),
            "the window elapsed → the account is re-polled"
        );
        assert_eq!(tick_backoff_secs(&clean), None);

        // A later 429 restarts the climb at the base multiplier, not at 480 (the cleared
        // window means the account is polled straightaway, no advance needed).
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(120));
    }

    #[tokio::test]
    async fn only_throttling_outcomes_trigger_the_back_off() {
        // Back-off is scoped to 429 (rate-limit) and 5xx / network (transient). A 403
        // (scope) and a 401 (unauthorized) authenticate-or-reject the token but are not
        // throttling — neither arms a back-off window; a transient does.
        let (_d1, mut scope) =
            rate_limit_daemon(FakeRosterPoller::new().scope_missing("u-A")).await;
        assert_eq!(tick_backoff_secs(&scope.tick().await), None);

        let (_d2, mut unauth) =
            rate_limit_daemon(FakeRosterPoller::new().unauthorized("u-A")).await;
        assert_eq!(tick_backoff_secs(&unauth.tick().await), None);

        let (_d3, mut transient) = rate_limit_daemon(FakeRosterPoller::new().failing("u-A")).await;
        assert_eq!(tick_backoff_secs(&transient.tick().await), Some(120));
    }

    #[tokio::test]
    async fn a_non_throttling_outcome_clears_an_armed_back_off_window() {
        // AC (issue #293): only 429 / transient SUSTAIN the per-account back-off — a
        // non-throttling outcome (here a 403, like a 401 or a clean poll) CLEARS an
        // already-armed window and resets the streak, so the account is re-polled
        // straightaway and a later 429 restarts the climb at the base, not where the
        // throttle episode left off.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        // Arm A's window (streak 1 → 120 s).
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(120));

        // Past the window, a 403 poll clears the streak + window (no back-off imposed).
        daemon.clock.advance(Duration::from_secs(120));
        daemon.poller = FakeRosterPoller::new().scope_missing("u-A");
        let cleared = daemon.tick().await;
        assert!(
            tick_polled(&cleared),
            "the window elapsed → A is re-polled, not skipped"
        );
        assert_eq!(tick_backoff_secs(&cleared), None);

        // A later 429 restarts at the base 120 s (not 240 s), proving the streak was reset.
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(120));
    }

    // --- durable per-account 429 / back-off / velocity observability (issue #399) ---

    #[test]
    fn usage_velocity_computes_signed_rounded_percent_deltas() {
        // The pure quantization (issue #399): `to_pct(next) - to_pct(prev)`, so a velocity agrees
        // with the percents `status` / the swap line show, and a difference of two `0..=100`
        // percents is a signed value in `-100..=100`.
        let r = |session: f64, weekly: f64| Usage {
            session,
            weekly,
            weekly_resets_at: None,
            session_resets_at: None,
        };
        // Climbing: both dimensions POSITIVE.
        assert_eq!(usage_velocity(&r(0.10, 0.20), &r(0.17, 0.22)), (7, 2));
        // A window reset dropped the reading: NEGATIVE session delta.
        assert_eq!(usage_velocity(&r(0.95, 0.40), &r(0.03, 0.40)), (-92, 0));
        // Flat: zero in both dimensions (the no-op the emitter stays silent on).
        assert_eq!(usage_velocity(&r(0.50, 0.50), &r(0.50, 0.50)), (0, 0));
    }

    #[tokio::test]
    async fn a_rate_limit_emits_a_durable_backoff_event() {
        // AC (issue #399): the previously stderr-only 429 back-off is now DURABLE. A 429 poll emits
        // a `usage_backoff` event carrying the account UUID (not a label), class=rate_limited, the
        // streak, and the armed window — so a back-off episode is diagnosable from the event log
        // alone. No server `Retry-After` was scripted, so `retry_after_secs` is `None`.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        let first = daemon.tick().await;
        assert!(
            first.events.contains(&Event::UsageBackoff {
                account: "u-A".to_owned(),
                class: BackoffClass::RateLimited,
                consecutive: 1,
                retry_after_secs: None,
                backoff_secs: 120,
            }),
            "the 429 must emit a durable usage_backoff ENTER: {:?}",
            first.events,
        );
    }

    #[tokio::test]
    async fn the_durable_backoff_event_carries_the_raw_retry_after() {
        // The durable event carries the RAW server `Retry-After` (pre-cap #294/#295), so the
        // flagship pathological case stays diagnosable from the log alone: a full day of
        // `Retry-After` clamped to the 1 h window renders `backoff_secs=3600 retry_after_secs=86400`.
        let (_dir, mut daemon) = rate_limit_daemon(
            FakeRosterPoller::new().rate_limited("u-A", Some(Duration::from_secs(86_400))),
        )
        .await;
        let first = daemon.tick().await;
        assert!(first.events.contains(&Event::UsageBackoff {
            account: "u-A".to_owned(),
            class: BackoffClass::RateLimited,
            consecutive: 1,
            retry_after_secs: Some(86_400),
            backoff_secs: POLL_BACKOFF_CAP.as_secs(),
        }));
    }

    #[tokio::test]
    async fn a_transient_emits_a_transient_class_backoff_event() {
        // A `5xx` / network transient arms the same back-off, but the durable event records
        // class=transient — so `grep class=rate_limited` counts genuine 429s (the #399 "429 count"),
        // not every back-off.
        let (_dir, mut daemon) = rate_limit_daemon(FakeRosterPoller::new().failing("u-A")).await;
        let first = daemon.tick().await;
        assert!(first.events.contains(&Event::UsageBackoff {
            account: "u-A".to_owned(),
            class: BackoffClass::Transient,
            consecutive: 1,
            retry_after_secs: None,
            backoff_secs: 120,
        }));
    }

    #[tokio::test]
    async fn consecutive_rate_limits_widen_the_durable_backoff_window() {
        // Each throttled re-poll emits a fresh durable ENTER with the climbing streak and the
        // widened window (120 → 240 → 480). This WIDENING is the residual-late-swap signal (#363's
        // ~1674 s active-account gap) that a single first-throttle line would hide — the reason the
        // event is emitted on EVERY throttle, not just the first.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        let mut seen = Vec::new();
        for _ in 0..3 {
            let outcome = daemon.tick().await;
            for e in &outcome.events {
                if let Event::UsageBackoff {
                    consecutive,
                    backoff_secs,
                    ..
                } = e
                {
                    seen.push((*consecutive, *backoff_secs));
                }
            }
            // Step past the just-armed window so the next tick re-polls (not skips) A.
            if let Some(secs) = tick_backoff_secs(&outcome) {
                daemon.clock.advance(Duration::from_secs(secs));
            }
        }
        assert_eq!(seen, vec![(1, 120), (2, 240), (3, 480)]);
    }

    #[tokio::test]
    async fn a_recovering_poll_emits_a_durable_backoff_cleared_event() {
        // AC (issue #399): the window's EXIT is durable too — a non-throttling poll that CLEARS an
        // armed window emits `usage_backoff_cleared`, bracketing the episode's span. A 403 here
        // (like a 401 or a clean read) is the non-throttling clear.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        daemon.tick().await; // arm the window
        daemon.clock.advance(Duration::from_secs(120));
        daemon.poller = FakeRosterPoller::new().scope_missing("u-A");
        let cleared = daemon.tick().await;
        assert!(
            cleared.events.contains(&Event::UsageBackoffCleared {
                account: "u-A".to_owned(),
            }),
            "clearing an armed window must emit a durable EXIT: {:?}",
            cleared.events,
        );
    }

    #[tokio::test]
    async fn a_clean_poll_without_a_prior_backoff_emits_no_cleared_event() {
        // The EXIT is a TRUE edge: a clean poll with no armed window emits NO `usage_backoff_cleared`
        // (mirroring `usage_rollup`'s no-op silence), so the always-on event log is not spammed with
        // a per-clean-poll line.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.10, 0.20)).await;
        let clean = daemon.tick().await;
        assert!(
            !clean
                .events
                .iter()
                .any(|e| matches!(e, Event::UsageBackoffCleared { .. })),
            "a clean poll with no prior back-off must be silent: {:?}",
            clean.events,
        );
    }

    #[tokio::test]
    async fn usage_velocity_is_emitted_on_the_second_reading() {
        // AC (issue #399): velocity is queryable from the durable log. The FIRST reading has no
        // prior to diff (silent); the SECOND emits a `usage_velocity` with the signed percent delta
        // — here session climbed 10% → 17% (=+7) while weekly held at 20% (=0).
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.10, 0.20)).await;
        let first = daemon.tick().await;
        assert!(
            !first
                .events
                .iter()
                .any(|e| matches!(e, Event::UsageVelocity { .. })),
            "the first reading has no prior to diff: {:?}",
            first.events,
        );
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.17, 0.20);
        let second = daemon.tick().await;
        assert!(
            second.events.contains(&Event::UsageVelocity {
                account: "u-A".to_owned(),
                session_delta_pct: 7,
                weekly_delta_pct: 0,
            }),
            "the second reading must emit its velocity: {:?}",
            second.events,
        );
    }

    #[tokio::test]
    async fn usage_velocity_is_silent_when_the_reading_is_flat() {
        // A flat reading (no measurable change) emits no `usage_velocity` — the no-op silence that
        // keeps an idle account quiet on the always-on event log (mirroring `usage_rollup`).
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.10, 0.20)).await;
        daemon.tick().await; // seed the prior reading
        let flat = daemon.tick().await; // an identical reading → zero delta
        assert!(
            !flat
                .events
                .iter()
                .any(|e| matches!(e, Event::UsageVelocity { .. })),
            "a flat reading must emit no velocity: {:?}",
            flat.events,
        );
    }

    #[tokio::test]
    async fn usage_velocity_is_negative_when_a_window_resets() {
        // A window reset drops the reading, so the delta is NEGATIVE (session 90% → 5% = -85) — the
        // durable log distinguishes a reset from a climb by sign, the signal #368 consumes.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.90, 0.30)).await;
        daemon.tick().await; // seed a prior reading at 90% session
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.05, 0.30);
        let reset = daemon.tick().await;
        assert!(reset.events.contains(&Event::UsageVelocity {
            account: "u-A".to_owned(),
            session_delta_pct: -85,
            weekly_delta_pct: 0,
        }));
    }

    #[tokio::test]
    async fn usage_velocity_does_not_span_a_throttle_gap() {
        // A velocity always spans two CONSECUTIVE real readings. A throttle clears the prior reading
        // (its `Err` result sets `last_readings` to `None`), so the recovering clean poll has
        // nothing to diff — no spurious velocity across an unknown-duration gap.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.90, 0.30)).await;
        daemon.tick().await; // seed a prior reading at 90% session

        // A 429 wipes the prior reading and arms a window.
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        daemon.tick().await;
        daemon.clock.advance(Duration::from_secs(120));

        // The recovering clean poll has no prior to diff → no velocity.
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.05, 0.31);
        let post_gap = daemon.tick().await;
        assert!(
            !post_gap
                .events
                .iter()
                .any(|e| matches!(e, Event::UsageVelocity { .. })),
            "a velocity must not span a throttle gap: {:?}",
            post_gap.events,
        );
    }

    #[tokio::test]
    async fn startup_delay_is_bounded_and_deterministic_under_a_seed() {
        // The start-up de-burst draws a uniform [0, STARTUP_DELAY_CAP) wait,
        // deterministic under a seeded RNG (no wall clock) so repeated restarts of the
        // same config decorrelate their first poll.
        let cap = Duration::from_secs_f64(STARTUP_DELAY_CAP);
        let (_d1, daemon) = rate_limit_daemon(FakeRosterPoller::new()).await;
        let mut a_daemon = daemon.with_seed(2024);
        let a: Vec<Duration> = (0..64).map(|_| a_daemon.startup_delay()).collect();

        let (_d2, daemon) = rate_limit_daemon(FakeRosterPoller::new()).await;
        let mut b_daemon = daemon.with_seed(2024);
        let b: Vec<Duration> = (0..64).map(|_| b_daemon.startup_delay()).collect();

        assert_eq!(a, b, "same seed must replay the same start-up delays");
        assert!(
            a.iter().all(|d| *d < cap),
            "every start-up delay must be < the cap"
        );
        assert!(
            a.iter().any(|&d| d != a[0]),
            "the jitter must actually spread the delay"
        );
    }

    /// A two-account daemon (`work` active + `spare`), both tokens stashed and the
    /// canonical holding `work`'s — for the per-account back-off tests (issue #293), where
    /// a `429` on the NON-active `spare` must back off ONLY `spare` and leave the active
    /// `work` polling on its normal cadence.
    async fn two_account_rate_limit_daemon(
        poller: FakeRosterPoller,
    ) -> (tempfile::TempDir, FakeDaemon) {
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let daemon = Daemon::new(
            vec![account("u-A", "work"), account("u-B", "spare")],
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        (dir, daemon)
    }

    #[tokio::test]
    async fn a_non_active_rate_limit_backs_off_only_that_account() {
        // AC (issue #293, replacing the former endpoint-global test): a `429` on a
        // NON-active account backs off ONLY that account — the active account (and every
        // other) keeps polling on its normal cadence, and the loop never globally waits.
        // `spare` is throttled; `work` (active) polls clean under its trigger and holds.
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .rate_limited("u-B", None),
        )
        .await;

        // Tick 1 polls the ACTIVE A (clean) — no back-off, loop keeps cadence.
        let first = daemon.tick().await;
        assert_eq!(first.next_wait, None);
        assert_eq!(tick_backoff_secs(&first), None);

        // Tick 2 polls the NON-active B (round-robin), which is throttled. The loop does
        // NOT globally back off (`next_wait` stays `None` — the core AC); B's back-off is
        // scoped to B (a 120 s window surfaced on the decision diagnostic). Under the former
        // endpoint-global rule this `next_wait` would have been `Some(120 s)`.
        let second = daemon.tick().await;
        assert_eq!(second.next_wait, None);
        assert_eq!(tick_backoff_secs(&second), Some(120));

        // Tick 3 wraps back to the ACTIVE A and re-polls it on the normal cadence — the
        // non-active throttle never silenced the active account.
        let third = daemon.tick().await;
        assert!(
            third.diagnostics.contains(&Diagnostic::Poll {
                account: "work".to_owned(),
                outcome: PollClass::Live,
            }),
            "tick 3 must re-poll the active account: {:?}",
            third.diagnostics
        );

        // Tick 4 comes back around to B, still inside its 120 s window → SKIPPED (frozen
        // clock), so B is not re-polled while it backs off.
        assert!(
            !tick_polled(&daemon.tick().await),
            "the throttled non-active account is skipped while backing off"
        );

        // Advancing past B's window lets B be re-polled on its next turn: tick 5 → A,
        // tick 6 → B (window elapsed → re-polled).
        daemon.clock.advance(Duration::from_secs(120));
        let _fifth = daemon.tick().await; // A
        assert!(
            tick_polled(&daemon.tick().await),
            "B is re-polled once its back-off window elapses"
        );
    }

    #[tokio::test]
    async fn a_throttled_non_active_accounts_retry_after_governs_its_own_back_off() {
        // The staggered loop (issue #80) polls ONE account per tick. A throttled NON-active
        // account's `Retry-After` now governs ITS OWN back-off window (issue #293), not the
        // whole loop: the active A keeps its cadence and the loop never globally waits. Here
        // A polls clean and B carries a `Retry-After` of 300 s on its round-robin tick;
        // 300 s > the 120 s first-cycle exponential, so B's hint governs B's own window.
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .rate_limited("u-B", Some(Duration::from_secs(300))),
        )
        .await;
        // Tick 1: active A, clean — no back-off, loop keeps cadence.
        let first = daemon.tick().await;
        assert_eq!(first.next_wait, None);
        assert_eq!(tick_backoff_secs(&first), None);
        // Tick 2: non-active B, throttled with Retry-After 300 → B's OWN window is 300 s,
        // and the loop still does not globally wait.
        let second = daemon.tick().await;
        assert_eq!(second.next_wait, None);
        assert_eq!(tick_backoff_secs(&second), Some(300));
    }

    // --- #80 staggered poll scheduling, #366 active interleave -------------
    //
    // The cycle no longer bursts every account in one tick. Each tick polls ONE
    // account from a staggered schedule that interleaves the active account before
    // each peer (`[active, p1, active, p2, …]`, #366) — so the active (its swap-away
    // trigger is the most time-sensitive) is re-observed every ~2 ticks, while the
    // enabled non-quarantined peers are each polled once — carrying the rest at their
    // last-known reading. The swap-away decision HOLDS until a warm-up cycle has polled
    // everyone once, and the per-poll wait is the full interval spread across the
    // rotation SIZE N (NOT the schedule length — #366 rate-neutrality). Every seam is
    // hermetic — no real clock or network (AC #4).

    /// A three-account daemon (`work` active, `spare`, `backup`) with the canonical
    /// holding `work`'s token — the fixture for the scheduling tests below. The
    /// caller supplies the poller so each test scripts its own per-account readings.
    async fn three_account_daemon(poller: FakeRosterPoller) -> FakeDaemon {
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "backup"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        // Keep the temp `~/.claude.json` alive for the daemon's lifetime by leaking
        // the guard — these are short-lived unit-test daemons (as `lifecycle_daemon`).
        std::mem::forget(dir);
        let tun = tunables(95, 80, 0);
        Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
    }

    #[tokio::test]
    async fn each_tick_polls_one_account_and_carries_the_rest_at_their_last_reading() {
        // AC #1/#2: no N-request burst — each tick polls exactly ONE account, and the
        // decision set accumulates the others as last-known readings rather than
        // re-polling them. Distinct per-account session values make the polled slot
        // identifiable: exactly one new reading lands per tick, in the interleaved order
        // (active `work`, `spare`, active `work` AGAIN, `backup` — issue #366), so the
        // full peer set is carried only once the schedule's last peer is reached.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.11, 0.10)
                .ok("u-B", 0.22, 0.10)
                .ok("u-C", 0.33, 0.10),
        )
        .await;
        let sessions = |d: &FakeDaemon| -> Vec<Option<f64>> {
            d.state
                .last_readings
                .iter()
                .map(|u| u.as_ref().map(|r| r.session))
                .collect()
        };

        daemon.tick().await;
        assert_eq!(
            sessions(&daemon),
            vec![Some(0.11), None, None],
            "tick 1 polls only the active work; spare/backup are still unread"
        );
        daemon.tick().await;
        assert_eq!(
            sessions(&daemon),
            vec![Some(0.11), Some(0.22), None],
            "tick 2 adds spare, carrying work's earlier reading"
        );
        daemon.tick().await;
        assert_eq!(
            sessions(&daemon),
            vec![Some(0.11), Some(0.22), None],
            "tick 3 RE-polls the interleaved active work (#366); backup is still unread"
        );
        daemon.tick().await;
        assert_eq!(
            sessions(&daemon),
            vec![Some(0.11), Some(0.22), Some(0.33)],
            "tick 4 adds backup — the schedule's last peer — so every account is now carried"
        );
    }

    #[tokio::test]
    async fn the_poll_schedule_interleaves_the_active_before_each_peer_and_wraps() {
        // AC #1/#2 (issue #366): the schedule interleaves the active account before each
        // enabled non-quarantined peer — `[active, p1, active, p2, …]` — so the active is
        // re-observed every ~2 ticks; the cursor advances one entry per tick and wraps at
        // the (now longer) cycle boundary.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;

        // With `spare` (index 1) active, the active leads and is re-inserted before the
        // second peer: [active, peer0, active, peer2].
        assert_eq!(daemon.build_poll_schedule(Some(1)), vec![1, 0, 1, 2]);

        // Driving the cursor a full cycle (4 entries) plus one yields the wrap to the lead.
        let polled: Vec<usize> = (0..5)
            .map(|_| daemon.next_poll_index(Some(1)).unwrap())
            .collect();
        assert_eq!(
            polled,
            vec![1, 0, 1, 2, 1],
            "active interleaved before each peer, then wrap to the lead"
        );
    }

    #[tokio::test]
    async fn the_poll_schedule_interleaves_before_each_peer_and_handles_degenerate_rosters() {
        // #366 branch coverage: the active is interleaved before EVERY enabled
        // non-quarantined peer, an excluded peer gets NO active insertion, and the two
        // degenerate shapes (no active / an active with no peers) still yield a valid
        // schedule rather than an empty one.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;

        // Active 0 with two enabled peers (1, 2): the active is re-inserted before EACH.
        assert_eq!(daemon.build_poll_schedule(Some(0)), vec![0, 1, 0, 2]);

        // No active: just the peers in roster order, nothing to interleave.
        assert_eq!(daemon.build_poll_schedule(None), vec![0, 1, 2]);

        // Quarantine peer 2: it drops out entirely — the active is NOT interleaved before
        // an excluded peer (so no `2`, and no extra leading `0` for it).
        daemon.state.health[2].quarantined = true;
        assert_eq!(daemon.build_poll_schedule(Some(0)), vec![0, 1]);

        // Quarantine the last remaining peer too: an active with NO peers still polls
        // itself (its swap-away trigger / dead-active re-probe), never an empty schedule.
        daemon.state.health[1].quarantined = true;
        assert_eq!(daemon.build_poll_schedule(Some(0)), vec![0]);
    }

    #[tokio::test]
    async fn the_sub_interval_spreads_a_cycle_across_the_rotation() {
        // AC #1: the per-poll wait is the full interval (60 s here, fixed) divided by
        // the rotation size, so a 3-account cycle spaces its polls 20 s apart and a
        // full sweep still spans ~one interval — instead of three back-to-back polls.
        let mut three = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        assert_eq!(three.next_subinterval(), Duration::from_secs(20));

        // A single-account roster has nothing to stagger: it waits the WHOLE interval,
        // so the cadence is unchanged from before the split (divisor clamped to ≥ 1).
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let mut solo: FakeDaemon = Daemon::new(
            vec![account("u-A", "work")],
            FakeRosterPoller::new().ok("u-A", 0.10, 0.10),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        assert_eq!(solo.next_subinterval(), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn the_warm_up_cycle_holds_the_swap_until_every_account_is_polled_once() {
        // AC #2: until the staggered loop has polled every account once the carried
        // readings are partial — swapping then could pick a suboptimal target or
        // declare a spurious all-exhausted. So the swap-away decision HOLDS through the
        // warm-up cycle and fires only on the full last-known set. Active `work` is over
        // its trigger from tick 1, yet the swap waits until every account has been polled
        // once — which the #366 active-interleave (`[work, spare, work, backup]`) reaches
        // at the FOURTH tick (tick 3 re-observes the active before the last peer).
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.97, 0.10) // active, over the 95 % session trigger
                .ok("u-B", 0.10, 0.10) // the viable target
                .ok("u-C", 0.10, 0.10),
        )
        .await;

        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::Held, "tick 1: still warming up");
        assert!(!daemon.state.warmed_up);

        let second = daemon.tick().await;
        assert_eq!(second.action, TickAction::Held, "tick 2: still warming up");
        assert!(!daemon.state.warmed_up);

        let third = daemon.tick().await;
        assert_eq!(
            third.action,
            TickAction::Held,
            "tick 3: re-observes the interleaved active — backup still unpolled, still warming up"
        );
        assert!(!daemon.state.warmed_up);

        let fourth = daemon.tick().await;
        assert_eq!(
            fourth.action,
            TickAction::Swapped { from: 0, to: 1 },
            "tick 4: warm-up complete → the swap fires on the full last-known set"
        );
        assert!(daemon.state.warmed_up);
    }

    #[tokio::test]
    async fn the_interleave_re_observes_the_active_in_band_and_swaps_before_the_ceiling() {
        // #367 regression lock (umbrella #363) — the reaction-latency PAYOFF of the #366
        // interleave, pinning the #365 `late=` marker. The active account's session usage
        // CLIMBS while it is active: it reads ~0.93 now, and one full round-robin sweep
        // later it would already be at the 1.00 ceiling. Pre-#366 the active was
        // re-observed only ONCE per full sweep (`[active, p1, p2, …]`), so the daemon's
        // next look already read 1.00 and it swapped LATE (session_pct=100, `late=true`).
        // The #366 interleave (`[active, p1, active, p2, …]`) re-observes the active every
        // ~2 ticks, so it catches an IN-BAND reading (∈ [95 %, 100 %)) and swaps BEFORE the
        // ceiling (session_pct < 100, `late=false`).
        //
        // The test drives the REAL staggered loop, so `build_poll_schedule` (#366) is what
        // decides WHEN the climbing value is re-observed: reverting #366 restores the
        // sweep-latency schedule → the active is next seen at 1.00 → the swap lands at
        // session_pct=100 and this test FAILS. Hermetic: fake clock + fake poller via
        // `FakeDaemon`, no real clock/network, no new test infrastructure (AC).
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;

        // The active account's TRUE session usage as a function of wall-time (ticks
        // elapsed); the peers stay idle at 0.10. Re-scripting the injected poller each tick
        // models "the world moved on between polls" — and it is the SCHEDULE, not the test,
        // that decides which of these climbing values the daemon actually observes:
        // interleaved (`[work, spare, work, backup]`) re-looks at `work` at tick 2 (0.97,
        // in-band); the pre-#366 (`[work, spare, backup]`) would only re-look at tick 3
        // (1.00, at the ceiling).
        let active_session = |tick: usize| -> f64 {
            match tick {
                0 | 1 => 0.93, // first observation: below the 95 % trigger → hold
                2 => 0.97,     // in-band on the interleaved re-observation ∈ [95 %, 100 %)
                _ => 1.00,     // one full sweep later it has hit the ceiling
            }
        };

        // Drive the staggered loop until the first swap fires — bounded, so a
        // never-swapping regression fails loudly here instead of hanging the test.
        let mut swap = None;
        for tick in 0..8 {
            // Re-script only the active's climbing reading each tick; peers stay idle. This
            // reuses `FakeRosterPoller` verbatim (no new infrastructure) — the field write
            // mirrors the existing `daemon.state.…` in-module test idiom.
            daemon.poller = FakeRosterPoller::new()
                .ok("u-A", active_session(tick), 0.10)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10);
            let outcome = daemon.tick().await;
            if matches!(outcome.action, TickAction::Swapped { .. }) {
                swap = Some(outcome);
                break;
            }
        }
        let outcome = swap.expect("the climbing active must trigger a swap-away within the sweep");

        // The swap fired OFF the active (`work`, 0) onto the soonest-reset viable peer
        // (`spare`, 1, by the earliest-index tie-break).
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });

        let swap_event = outcome
            .events
            .iter()
            .find(|e| matches!(e, Event::Swap { .. }))
            .expect("a swap surfaces a structured Event::Swap (#9)");
        let Event::Swap { session_pct, .. } = swap_event else {
            unreachable!("filtered to Event::Swap above")
        };
        // AC: the interleave re-observed the active WITHIN the sweep, so the swap was taken
        // on an in-band reading — at/above the 95 % trigger yet strictly below the 100 %
        // ceiling. On the pre-#366 sweep-latency schedule this reading would be 100.
        assert!(
            *session_pct >= 95 && *session_pct < 100,
            "expected an in-band swap ∈ [95, 100); got session_pct={session_pct}",
        );

        // Companion AC (#365 marker): an in-band swap is NOT late — `Event::to_log_line`
        // appends `late=true` only at/above the ceiling (session_pct >= 100), so the
        // rendered line must carry no `late` marker.
        let line = swap_event.to_log_line(std::time::SystemTime::UNIX_EPOCH);
        assert!(
            !line.contains("late"),
            "an in-band swap must not be marked late; got `{line}`",
        );
    }

    #[tokio::test]
    async fn a_reauth_rewrites_the_canonical_and_the_daemon_restashes_the_account() {
        // #13 core: tick 1 primes the watch on A's token. The operator then re-auths
        // A via `claude /login`, rewriting the canonical to a FRESH token (display
        // stays A — same account, refreshed credential). Tick 2 detects the
        // out-of-band change and re-stashes A with the new token, so A's stash tracks
        // the live credential; tick 3 sees no further change and does not re-fire.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A stays below the trigger throughout: the point is the re-stash, not a swap.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        // Tick 1 primes the watch on the current canonical — no re-stash.
        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::Held);
        assert!(
            !first
                .events
                .iter()
                .any(|e| matches!(e, Event::ReStash { .. })),
            "the first observation primes, it does not re-stash"
        );
        assert_eq!(daemon.state.active, Some(0));

        // The operator re-auths A: `claude /login` rewrites the canonical token.
        daemon
            .store
            .write(&cred(b"A-reauthed-token"))
            .await
            .unwrap();

        // Tick 2 detects the change and re-stashes A with the new token.
        let second = daemon.tick().await;
        assert_eq!(
            second.events,
            vec![Event::ReStash {
                account: "work".to_owned(),
            }]
        );
        let a = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(a.credential.expose(), b"A-reauthed-token");
        // The identity half is preserved, and A is still the resolved active account.
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
        assert_eq!(daemon.state.active, Some(0));

        // Tick 3: no further change → the committed baseline means no repeat re-stash.
        let third = daemon.tick().await;
        assert!(
            !third
                .events
                .iter()
                .any(|e| matches!(e, Event::ReStash { .. })),
            "a committed change must not re-fire"
        );
    }

    #[tokio::test]
    async fn a_reauth_to_a_different_account_restashes_it_and_reresolves_active() {
        // #13: the operator `claude /login`s into account B while A was active, so
        // the canonical becomes B's fresh token AND the display switches to B. The
        // daemon re-stashes B with the new token (resolved via the display, since no
        // stash matches the fresh token yet) and re-resolves the active account to B.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-old-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        // Tick 1 primes on A and resolves A as active.
        daemon.tick().await;
        assert_eq!(daemon.state.active, Some(0));

        // `/login` to B: the canonical becomes B's fresh token and the display
        // switches to B (what Claude Code writes to `~/.claude.json`).
        daemon.store.write(&cred(b"B-reauthed")).await.unwrap();
        crate::claude_state::write_oauth_account(&json, &oauth("u-B")).unwrap();

        // Tick 2 detects the change, re-stashes B (resolved via the display), and
        // re-resolves the active account to B.
        let second = daemon.tick().await;
        assert_eq!(
            second.events,
            vec![Event::ReStash {
                account: "spare".to_owned(),
            }]
        );
        let b = daemon.stash.read("Sessiometer/u-B").await.unwrap();
        assert_eq!(b.credential.expose(), b"B-reauthed");
        assert_eq!(b.oauth_account.account_uuid(), "u-B");
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn a_login_to_an_uncaptured_account_is_surfaced_not_onboarded() {
        // Issue #140 scope decision: the operator `claude /login`s into a BRAND-NEW account not
        // in the roster (the canonical becomes its fresh token AND the display switches to its
        // uuid). The daemon detects the out-of-band change but resolves it to NO roster account,
        // so it SURFACES an `Uncaptured Login` (with the displayed uuid) prompting `sessiometer
        // login` — it does NOT auto-onboard it. Edge-triggered: a later tick with the same
        // canonical does not re-surface it.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        // Tick 1 primes on A.
        daemon.tick().await;

        // `/login` to an un-captured account u-Z: canonical becomes Z's token and the display
        // switches to u-Z (what Claude Code writes to `~/.claude.json`).
        daemon.store.write(&cred(b"Z-token")).await.unwrap();
        crate::claude_state::write_oauth_account(&json, &oauth("u-Z")).unwrap();

        // Tick 2 detects the change, cannot map it to the roster, and surfaces it (uuid carried).
        let second = daemon.tick().await;
        assert_eq!(
            second.events,
            vec![Event::UncapturedLogin {
                account_uuid: Some("u-Z".to_owned()),
            }],
            "an un-captured login is surfaced with its displayed uuid, never onboarded"
        );
        // NOT onboarded: the roster is still just A, and no stash was created for u-Z.
        assert_eq!(
            daemon.roster.len(),
            1,
            "the roster is unchanged (no auto-onboard)"
        );
        assert!(
            daemon.stash.read("Sessiometer/u-Z").await.is_err(),
            "no stash is created for the un-captured account"
        );
        // #208: the stale cached active is dropped when the canonical resolves to no
        // roster account, so `status` shows no false `*` on the now-inactive account.
        // Before the fix the stale `Some(0)` (work) survived this un-captured login.
        assert_eq!(
            daemon.state.active, None,
            "the stale active is dropped on an un-captured login → status shows no `*` (#208)"
        );

        // Tick 3: the same canonical is now the committed baseline → no repeat surfacing.
        let third = daemon.tick().await;
        assert!(
            !third
                .events
                .iter()
                .any(|e| matches!(e, Event::UncapturedLogin { .. })),
            "a committed un-captured login must not re-surface every cycle"
        );
    }

    #[tokio::test]
    async fn tick_reports_no_viable_target_when_every_other_account_is_over_the_floor() {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A over trigger; B's session (0.85) is above the floor (0.80) → not viable.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.50)
            .ok("u-B", 0.85, 0.10);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(outcome.action, TickAction::NoViableTarget);
        // Floor-driven exhaustion: B is weekly-viable but over the target-max-usage, so
        // the block is session-wide (#398) — cause=session, naming B ("spare", the
        // account relief comes from at its session reset). The poller reports no
        // session reset, so `resets_at` is omitted (the soonest-reset path is covered
        // by the all-weekly-exhausted test below).
        assert_eq!(
            outcome.events,
            vec![Event::AllExhausted {
                hold: "spare".to_owned(),
                cause: SwapReason::Session,
                resets_at: None,
            }],
        );
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn tick_holds_on_soonest_reset_when_all_accounts_are_weekly_exhausted() {
        // #11 acceptance: every account is weekly-exhausted, so there is no viable
        // swap target. The daemon must HOLD on the least-bad account — the one
        // whose weekly window resets soonest — emit exactly ONE signal, and perform
        // ZERO swaps no matter how many ticks run.
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // All three weekly-exhausted (weekly 0.99 ≥ weekly_trigger 0.98). B resets
        // soonest, so it is the least-bad hold target even though A is active.
        const A_RESET: i64 = 1_782_777_600; // 2026-06-30T00:00:00Z
        const B_RESET: i64 = 1_782_496_800; // 2026-06-26T18:00:00Z (soonest)
        const C_RESET: i64 = 1_782_864_000; // 2026-07-01T00:00:00Z
        let poller = FakeRosterPoller::new()
            .ok_resets("u-A", 0.50, 0.99, A_RESET)
            .ok_resets("u-B", 0.50, 0.99, B_RESET)
            .ok_resets("u-C", 0.50, 0.99, C_RESET);
        // Floor inert (== trigger via tunables_floor_off); weekly_trigger 98, so the
        // swap-away fires on the weekly dimension and every target is excluded.
        let tun = tunables_floor_off(95, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        // First full cycle warms up the carried readings; its last tick detects
        // all-exhausted, holds on B (soonest reset), and emits once (issue #80).
        let first = warmed_tick(&mut daemon).await;
        assert_eq!(first.action, TickAction::NoViableTarget);
        assert_eq!(
            first.events,
            vec![Event::AllExhausted {
                hold: "spare".to_owned(),
                cause: SwapReason::Weekly,
                resets_at: Some(B_RESET),
            }],
        );

        // Two more ticks in the same episode: still no viable target, but the
        // signal is edge-triggered, so NOTHING further is emitted.
        for _ in 0..2 {
            let again = daemon.tick().await;
            assert_eq!(again.action, TickAction::NoViableTarget);
            assert!(
                again.events.is_empty(),
                "all_exhausted must be edge-triggered, got {:?}",
                again.events
            );
        }

        // ZERO swaps across the whole episode: canonical still A, active unchanged.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(daemon.state.active, Some(0));
    }

    #[tokio::test]
    async fn leaving_the_all_exhausted_state_clears_the_edge_guard() {
        // #11 edge re-fire: once the daemon leaves the all-exhausted state the
        // guard clears, so a later re-entry signals afresh. Here a Hold (active
        // below both triggers) is the non-exhausted cycle that resets it.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables_floor_off(95, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // Pretend a prior all-exhausted episode already signaled.
        daemon.state.signaled_all_exhausted = true;

        let outcome = daemon.tick().await;
        assert_eq!(outcome.action, TickAction::Held);
        assert!(
            !daemon.state.signaled_all_exhausted,
            "leaving the all-exhausted state must clear the edge guard",
        );
        // And the full diagnostic vec (#77), in emission order: ONE per-poll line (the
        // staggered loop polls a single account — the active `work` — this tick), then
        // — exactly ONCE — the `AllExhaustedCleared` LEAVE marker (the symmetric
        // partner of the edge-triggered ENTER), then the per-tick Hold decision. The
        // marker is computed from the guard BEFORE the reset above, so a genuine leave
        // is told apart from a never-entered hold (a stale "all exhausted" reading vs a
        // current one — the #77 motivation). Asserting the whole vec — not just the
        // marker count — pins the operator-visible ORDER against an accidental reorder.
        assert_eq!(
            outcome.diagnostics,
            vec![
                Diagnostic::Poll {
                    account: "work".to_owned(),
                    outcome: PollClass::Live,
                },
                Diagnostic::AllExhaustedCleared,
                Diagnostic::Tick {
                    decision: DecisionClass::Hold,
                    backoff_secs: None,
                    retry_after_secs: None,
                },
            ],
        );
    }

    #[test]
    fn diag_poll_class_separates_rate_limited_from_transient() {
        // The DIAGNOSTIC taxonomy (#77) splits a `429` (rate-limited) out as its own
        // class — the signal an operator debugging a throttling storm needs — whereas
        // the dead-credential `classify_poll` folds it into the generic transient.
        assert_eq!(
            diag_poll_class(&Err(Error::UsageUnauthorized)),
            PollClass::Unauthorized
        );
        assert_eq!(
            diag_poll_class(&Err(Error::UsageScopeMissing)),
            PollClass::Scope
        );
        assert_eq!(
            diag_poll_class(&Err(Error::UsageRateLimited {
                status: 429,
                retry_after: None,
            })),
            PollClass::RateLimited
        );
        assert_eq!(
            diag_poll_class(&Err(Error::UsageTransient {
                status: 503,
                retry_after: None,
            })),
            PollClass::Transient
        );
        assert_eq!(
            diag_poll_class(&Err(Error::UsageTokenUnreadable)),
            PollClass::Transient
        );
        // Contrast on the SAME 429: the health axis folds it into `Transient`.
        assert_eq!(
            classify_poll(&Err(Error::UsageRateLimited {
                status: 429,
                retry_after: None,
            })),
            PollOutcome::Transient
        );
    }

    #[test]
    fn decision_class_maps_every_tick_action() {
        // 1:1 and total over the variants (#77); swap participants are dropped — the
        // decision line is a pure label.
        assert_eq!(TickAction::Held.decision_class(), DecisionClass::Hold);
        assert_eq!(
            TickAction::Swapped { from: 0, to: 1 }.decision_class(),
            DecisionClass::Swap
        );
        assert_eq!(
            TickAction::EmergencySwapped { from: 0, to: 1 }.decision_class(),
            DecisionClass::EmergencySwap
        );
        assert_eq!(
            TickAction::ActiveDeadNoTarget.decision_class(),
            DecisionClass::ActiveDeadNoTarget
        );
        assert_eq!(
            TickAction::NoViableTarget.decision_class(),
            DecisionClass::AllExhausted
        );
        assert_eq!(
            TickAction::SkippedActiveUnknown.decision_class(),
            DecisionClass::SkipActiveUnknown
        );
        assert_eq!(
            TickAction::SkippedActiveUnavailable.decision_class(),
            DecisionClass::SkipActiveUnavailable
        );
        assert_eq!(
            TickAction::SkippedCooldown.decision_class(),
            DecisionClass::SkipCooldown
        );
        assert_eq!(
            TickAction::SwapFailed.decision_class(),
            DecisionClass::SwapFailed
        );
        assert_eq!(
            TickAction::KeychainLocked.decision_class(),
            DecisionClass::KeychainLocked
        );
    }

    #[tokio::test]
    async fn an_over_trigger_active_within_the_cooldown_is_skipped() {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.05, 0.05);
        let tun = tunables(95, 80, 100); // cooldown 100s

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );
        // Simulate a swap that just happened: active A, last swap at "now".
        daemon.state.active = Some(0);
        daemon.state.last_swap = Some(LastSwap {
            at: daemon.clock.now(),
        });
        daemon.clock.advance(Duration::from_secs(10)); // still within the 100s cooldown

        // Warm up the carried readings first (issue #80); the warmed tick then takes
        // the real decision — the cooldown skip — within the window.
        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(outcome.action, TickAction::SkippedCooldown);
        // No swap despite A being over the trigger and B wide open.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn an_over_trigger_active_past_the_cooldown_swaps() {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.05, 0.05);
        let tun = tunables(95, 80, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );
        daemon.state.active = Some(0);
        daemon.state.last_swap = Some(LastSwap {
            at: daemon.clock.now(),
        });
        daemon.clock.advance(Duration::from_secs(150)); // past the 100s cooldown

        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
    }

    #[tokio::test]
    async fn a_re_swap_within_the_floor_is_refused_even_with_wide_cooldown_jitter() {
        // Issue #272 (AC #1 + #3): the cooldown floor is NON-BYPASSABLE by jitter. With
        // the cooldown base pinned AT the floor and a spread an order of magnitude
        // wider, every per-cycle draw still clamps to >= the floor, so a re-swap inside
        // the floor window is refused — jitter cannot open a sub-floor gap to flap
        // through. (Complements `an_over_trigger_active_within_the_cooldown_is_skipped`,
        // which pins the fixed-cooldown case.)
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.05, 0.05);
        // Cooldown pinned at the floor, jitter spread 20x wider than it.
        let mut tun = tunables(95, 80, crate::config::COOLDOWN_SECS_FLOOR);
        tun.cooldown_strategy = Strategy {
            base: COOLDOWN_SECS_LO,
            jitter: Jitter::Uniform {
                spread: COOLDOWN_SECS_LO * 20.0,
            },
        };

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        )
        .with_seed(0x0272_5EED);
        daemon.state.active = Some(0);
        daemon.state.last_swap = Some(LastSwap {
            at: daemon.clock.now(),
        });
        // Just inside the floor window: any clamped draw is >= the floor, so this must
        // skip regardless of what the wide jitter drew this cycle.
        daemon
            .clock
            .advance(Duration::from_secs(crate::config::COOLDOWN_SECS_FLOOR - 1));

        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(outcome.action, TickAction::SkippedCooldown);
        // Canonical still A: no swap slipped through the jittered cooldown.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn two_session_saturated_accounts_hold_the_gate_prevents_oscillation() {
        // Session-gate acceptance (the fr <-> pelykh.com fix): with the floor OFF (the
        // default) and TWO accounts both over the 95 session trigger, NEITHER is a
        // viable target for the other — the always-on session gate excludes a session-
        // saturated destination (it would re-trip swap::decide's session dimension). So
        // the daemon HOLDS on every tick: no swap, and therefore no A→B→A. The gate
        // PREVENTS the oscillation at its source; it is not merely paced by the cooldown
        // (the superseded #10 "cooldown alone bounds oscillation" behavior this test
        // used to assert).
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Both hover high (over the 95 trigger), low weekly — the setup that WOULD
        // ping-pong under the old no-session-gate rule.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.95, 0.20)
            .ok("u-B", 0.96, 0.20);
        // Floor OFF (the #10 default); cooldown 100 s, trigger 95, no jitter.
        let tun = tunables_floor_off(95, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );

        // A is active and over the trigger, but B is session-saturated too, so B is
        // NOT a viable target — the daemon holds instead of swapping A→B.
        assert_eq!(
            warmed_tick(&mut daemon).await.action,
            TickAction::NoViableTarget
        );

        // It keeps holding across the whole window AND well past the 100 s cooldown:
        // no swap ever fires, so there is nothing to ping-pong. Were the cooldown the
        // only guard (the old behavior), a swap would have fired on the first tick and
        // again past +100 s; the gate makes both impossible.
        for step in 1..=8u64 {
            daemon.clock.advance(Duration::from_secs(20)); // +20s .. +160s (past cooldown)
            assert_eq!(
                daemon.tick().await.action,
                TickAction::NoViableTarget,
                "two session-saturated accounts must HOLD (no swap) at tick {step}"
            );
        }
    }

    // --- manual-hold: adopt a manual `use` swap (issue #64) ----------------

    #[tokio::test]
    async fn adopt_manual_swap_arms_the_cooldown_so_the_next_poll_holds() {
        // Issue #64 manual-hold: after a manual `use` swap to B (canonical now B's
        // token), the daemon adopts the notification — which ARMS the post-swap
        // cooldown and re-resolves active — so its very next poll HOLDS on B rather
        // than immediately reverting it, EVEN THOUGH B sits over its swap-away trigger.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        // The manual swap already rewrote the canonical to B's token.
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
        // B (the manual target) is OVER its session trigger — absent the cooldown the
        // daemon would swap straight back to the wide-open A.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.05, 0.05)
            .ok("u-B", 0.97, 0.40);
        let tun = tunables(95, 80, 100); // cooldown 100s

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );
        // The daemon has not yet noticed the out-of-band manual swap: no in-memory
        // last_swap, so without the notification its next poll would revert B.
        assert!(daemon.state.last_swap.is_none());

        daemon.adopt_manual_swap().await;

        // Adoption armed the cooldown (last_swap at "now") and re-resolved active to B.
        assert_eq!(daemon.state.active, Some(1));
        let armed = daemon.state.last_swap.as_ref().expect("cooldown armed");
        assert_eq!(armed.at, daemon.clock.now());

        daemon.clock.advance(Duration::from_secs(10)); // within the 100s cooldown
        let outcome = warmed_tick(&mut daemon).await;

        // The daemon HOLDS on the operator's choice — no immediate revert.
        assert_eq!(outcome.action, TickAction::SkippedCooldown);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn without_the_manual_hold_the_daemon_reverts_an_over_trigger_target() {
        // The contrast that makes the manual-hold load-bearing: the SAME fixture, but
        // the daemon is NOT notified (no adopt). It resolves active to B, finds B over
        // the trigger with NO cooldown armed, and immediately reverts B→A — exactly
        // the revert the #64 notification exists to prevent.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.05, 0.05)
            .ok("u-B", 0.97, 0.40);
        let tun = tunables(95, 80, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );

        let outcome = warmed_tick(&mut daemon).await;

        // Without the cooldown armed, the daemon reverts the (unannounced) manual swap.
        assert_eq!(outcome.action, TickAction::Swapped { from: 1, to: 0 });
    }

    #[tokio::test]
    async fn adopt_manual_swap_re_resolves_active_from_the_canonical_not_the_message() {
        // The #64 message carries no target; the daemon re-resolves active from the
        // AUTHORITATIVE canonical item. Here the cached active is STALE (A) while the
        // canonical already holds B's token — adoption corrects it to B, so an
        // out-of-order or contentless message cannot corrupt the daemon's state.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
        let tun = tunables(95, 80, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );
        // A STALE cached active pointing at A, though the canonical is already B.
        daemon.state.active = Some(0);

        daemon.adopt_manual_swap().await;

        // Re-resolved from the canonical (B's token), not left at the stale A.
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn a_daemon_with_the_swap_lock_wired_still_swaps_normally() {
        // Wiring smoke test (#64): a daemon configured with the single-writer lock
        // acquires + releases it around its own swap, so an UNcontended swap proceeds
        // exactly as before. (The lock's mutual-exclusion property is proven in
        // `swap.rs`; here we only confirm `with_swap_lock` does not deadlock the path.)
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.05, 0.05);
        let tun = tunables(95, 80, 100);
        let lock_dir = tempfile::tempdir().unwrap();

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        )
        .with_swap_lock(lock_dir.path().join("swap.lock"));

        let outcome = warmed_tick(&mut daemon).await;

        // The swap landed normally, the lock acquired and released around it: A→B.
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    // --- timing jitter strategies (issue #38) ------------------------------

    /// A minimal daemon over empty seams — enough to exercise the pure
    /// `next_poll_interval` draw without any roster/poll/keychain setup.
    fn poll_daemon(tun: &Tunables, seed: u64) -> FakeDaemon {
        Daemon::new(
            vec![],
            FakeRosterPoller::new(),
            FakeCredentialStore::empty(),
            FakeAccountStash::empty(),
            FakeClock::frozen(),
            PathBuf::from("/nonexistent/.claude.json"),
            tun,
        )
        .with_seed(seed)
    }

    #[test]
    fn next_poll_interval_is_deterministic_and_stays_in_range() {
        // AC: each cycle draws a jittered poll interval within the valid range,
        // deterministic under an injected seed.
        let mut tun = tunables(95, 80, 0);
        tun.poll_strategy = Strategy {
            base: 300.0,
            jitter: Jitter::Normal { stddev: 80.0 },
        };
        let mut a = poll_daemon(&tun, 2024);
        let mut b = poll_daemon(&tun, 2024);
        let seq_a: Vec<f64> = (0..256)
            .map(|_| a.next_poll_interval().as_secs_f64())
            .collect();
        let seq_b: Vec<f64> = (0..256)
            .map(|_| b.next_poll_interval().as_secs_f64())
            .collect();
        assert_eq!(
            seq_a, seq_b,
            "same seed must replay the same poll intervals"
        );
        for s in &seq_a {
            assert!(
                (POLL_SECS_LO..=POLL_SECS_HI).contains(s),
                "poll interval {s}s out of 5..=3600"
            );
        }
        // The normal jitter actually moves the interval off the 300 s base.
        assert!(seq_a.iter().any(|&s| (s - 300.0).abs() > 1.0));
    }

    #[test]
    fn a_fixed_poll_strategy_draws_the_base_verbatim() {
        // The no-jitter path is unchanged behavior: every draw is the base.
        let tun = tunables(95, 80, 0); // poll_strategy = fixed(60.0)
        let mut daemon = poll_daemon(&tun, 1);
        for _ in 0..8 {
            assert_eq!(daemon.next_poll_interval(), Duration::from_secs(60));
        }
    }

    #[tokio::test]
    async fn a_jittered_trigger_is_deterministic_and_varies_the_swap_decision() {
        // Active A sits at a fixed 60% session; a wide uniform trigger jitter
        // spans the whole 50..=99 range, so some cycles draw a trigger ≤ 60
        // (→ swap) and others > 60 (→ hold). Deterministic per seed, but VARYING
        // across seeds — proof the trigger is drawn anew each cycle.
        async fn action_for(seed: u64) -> TickAction {
            let roster = vec![account("u-A", "work"), account("u-B", "spare")];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
            ])
            .await;
            let (_dir, json) = claude_json("u-A");
            let poller = FakeRosterPoller::new()
                .ok("u-A", 0.60, 0.10)
                .ok("u-B", 0.05, 0.05);
            let mut tun = tunables(95, 80, 0);
            tun.trigger_strategy = Strategy {
                base: 95.0,
                jitter: Jitter::Uniform { spread: 100.0 },
            };
            let mut daemon: FakeDaemon = Daemon::new(
                roster,
                poller,
                store,
                stash,
                FakeClock::frozen(),
                json,
                &tun,
            )
            .with_seed(seed);
            warmed_tick(&mut daemon).await.action
        }
        // Determinism: the same seed replays the same decision.
        assert_eq!(action_for(11).await, action_for(11).await);
        // Variation: across seeds the jittered trigger yields BOTH outcomes at
        // the same fixed 60% usage.
        let mut holds = 0;
        let mut swaps = 0;
        for seed in 0..48 {
            match action_for(seed).await {
                TickAction::Held => holds += 1,
                TickAction::Swapped { from: 0, to: 1 } => swaps += 1,
                other => panic!("unexpected action under seed {seed}: {other:?}"),
            }
        }
        assert!(
            holds > 0 && swaps > 0,
            "jittered trigger should produce both holds ({holds}) and swaps ({swaps})"
        );
    }

    #[tokio::test]
    async fn a_jittered_weekly_trigger_is_deterministic_and_varies_the_swap_decision() {
        // The WEEKLY-axis mirror of the jittered-trigger test (issue #41): session
        // is held LOW (never trips its trigger), weekly sits at a fixed 60%, and a
        // wide uniform weekly-trigger jitter spans the whole 50..=99 range — so
        // some cycles draw a weekly trigger ≤ 60 (→ swap on the weekly dimension)
        // and others > 60 (→ hold). Deterministic per seed, varying across seeds:
        // proof the weekly trigger is drawn anew each cycle from its own strategy.
        async fn action_for(seed: u64) -> TickAction {
            let roster = vec![account("u-A", "work"), account("u-B", "spare")];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
            ])
            .await;
            let (_dir, json) = claude_json("u-A");
            // Session fixed low (never trips the 95 session trigger); weekly fixed
            // at 60%, the axis the jittered weekly trigger straddles.
            let poller = FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.60)
                .ok("u-B", 0.05, 0.05);
            let mut tun = tunables(95, 80, 0);
            tun.weekly_trigger_strategy = Strategy {
                base: 95.0,
                jitter: Jitter::Uniform { spread: 100.0 },
            };
            let mut daemon: FakeDaemon = Daemon::new(
                roster,
                poller,
                store,
                stash,
                FakeClock::frozen(),
                json,
                &tun,
            )
            .with_seed(seed);
            warmed_tick(&mut daemon).await.action
        }
        // Determinism: the same seed replays the same decision.
        assert_eq!(action_for(11).await, action_for(11).await);
        // Variation: across seeds the jittered weekly trigger yields BOTH outcomes
        // at the same fixed 60% weekly usage.
        let mut holds = 0;
        let mut swaps = 0;
        for seed in 0..48 {
            match action_for(seed).await {
                TickAction::Held => holds += 1,
                TickAction::Swapped { from: 0, to: 1 } => swaps += 1,
                other => panic!("unexpected action under seed {seed}: {other:?}"),
            }
        }
        assert!(
            holds > 0 && swaps > 0,
            "jittered weekly trigger should produce both holds ({holds}) and swaps ({swaps})"
        );
    }

    #[tokio::test]
    async fn a_jittered_cooldown_is_deterministic_and_varies_the_skip() {
        // Active A is over the (fixed) trigger with a swap 100 s ago; a wide
        // uniform cooldown jitter around 100 s makes some cycles draw a cooldown
        // below the 100 s elapsed (→ swap) and others above it (→ skip).
        // Deterministic per seed, varying across seeds.
        async fn action_for(seed: u64) -> TickAction {
            let roster = vec![account("u-A", "work"), account("u-B", "spare")];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
            ])
            .await;
            let (_dir, json) = claude_json("u-A");
            let poller = FakeRosterPoller::new()
                .ok("u-A", 0.97, 0.40)
                .ok("u-B", 0.05, 0.05);
            let mut tun = tunables(95, 80, 100);
            tun.cooldown_strategy = Strategy {
                base: 100.0,
                jitter: Jitter::Uniform { spread: 200.0 },
            };
            let mut daemon: FakeDaemon = Daemon::new(
                roster,
                poller,
                store,
                stash,
                FakeClock::new(Duration::ZERO),
                json,
                &tun,
            )
            .with_seed(seed);
            daemon.state.active = Some(0);
            daemon.state.last_swap = Some(LastSwap {
                at: daemon.clock.now(),
            });
            daemon.clock.advance(Duration::from_secs(100));
            warmed_tick(&mut daemon).await.action
        }
        assert_eq!(action_for(5).await, action_for(5).await);
        let mut skipped = 0;
        let mut swapped = 0;
        for seed in 0..48 {
            match action_for(seed).await {
                TickAction::SkippedCooldown => skipped += 1,
                TickAction::Swapped { from: 0, to: 1 } => swapped += 1,
                other => panic!("unexpected action under seed {seed}: {other:?}"),
            }
        }
        assert!(
            skipped > 0 && swapped > 0,
            "jittered cooldown should produce both skips ({skipped}) and swaps ({swapped})"
        );
    }

    // --- reconcile-on-start ------------------------------------------------

    #[tokio::test]
    async fn reconcile_co_writes_the_matched_account_when_the_display_is_stale() {
        // Post-swap crash: canonical holds B's token, but the display still shows
        // A (the co-write never landed). Reconcile heals the display to B.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A"); // stale display
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
    }

    #[tokio::test]
    async fn reconcile_leaves_the_display_untouched_when_no_stash_matches() {
        // Normal restart: the active account's token has drifted (refreshed in
        // place), matching no stash. The display is already correct → untouched.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-drifted-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-old-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    #[tokio::test]
    async fn reconcile_is_a_noop_when_the_display_already_matches() {
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    // --- status snapshot + control protocol --------------------------------

    #[test]
    fn status_response_carries_handles_and_percentages_and_never_a_secret() {
        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            generated_at: 0,
            refresh_enabled: false,
            accounts: vec![
                AccountReading {
                    label: "work".to_owned(),
                    active: true,
                    enabled: true,
                    quarantined: false,
                    recovering: false,
                    weekly_exhausted: false,
                    usage: Some(Usage {
                        session: 0.97,
                        weekly: 0.40,
                        weekly_resets_at: None,
                        session_resets_at: None,
                    }),
                    ..Default::default()
                },
                AccountReading {
                    label: "spare".to_owned(),
                    active: false,
                    enabled: true,
                    quarantined: false,
                    recovering: false,
                    weekly_exhausted: false,
                    usage: None,
                    ..Default::default()
                },
            ],
            // A viable candidate rides the wire as a label + the daemon's #393 selection reason (#88).
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::OnlyCandidate),
            }),
        };
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(json.contains("\"label\":\"work\""));
        assert!(json.contains("\"active\":true"));
        // Issue #36: the rotation flag is carried so `status` can mark a parked account.
        assert!(json.contains("\"enabled\":true"));
        assert!(json.contains("\"session_pct\":97"));
        assert!(json.contains("\"weekly_pct\":40"));
        // The unavailable account reports null, not a fabricated 0.
        assert!(json.contains("\"session_pct\":null"));
        // The next-swap candidate is projected as a label (#88); `last_swap` is gone.
        assert!(json.contains("\"next_swap\":"));
        assert!(json.contains("\"state\":\"target\""));
        assert!(json.contains("\"to\":\"spare\""));
        assert!(!json.contains("last_swap"));
        // Issue #15: the projection sources only labels + percentages, so neither an
        // email nor a token can ever reach the wire — the new candidate included.
        assert!(!json.contains('@'));
        assert!(!json.to_lowercase().contains("token"));
    }

    // --- credential-health rollup (issue #119) -----------------------------

    #[test]
    fn credential_health_rolls_up_the_states_by_severity() {
        use RefreshEventOutcome::{Error, NoChange, Refreshed};
        const NOW: i64 = 1_782_777_600;

        // Healthy — not quarantined, no refresh failure, token not yet expired; the refresh
        // telemetry + a future expiry are the positive-liveness signal (no reading needed).
        assert_eq!(
            credential_health(false, Some(Refreshed), 0, Some(NOW + 60), false, NOW),
            CredentialHealth::Healthy
        );
        // A fresh successful usage reading is ALSO a positive-liveness signal (#137): even
        // with `[refresh]` off (no telemetry, no clock), a live-API poll ⇒ Healthy. The
        // no-reading counterpart is `Unknown`, covered in the sibling test.
        assert_eq!(
            credential_health(false, None, 0, None, true, NOW),
            CredentialHealth::Healthy
        );

        // Stale — the access token has expired (`<= now`) but the refresh net is still
        // alive: a transient window the next refresh recovers. The boundary is inclusive.
        assert_eq!(
            credential_health(false, Some(NoChange), 0, Some(NOW), false, NOW),
            CredentialHealth::Stale
        );
        assert_eq!(
            credential_health(false, Some(Refreshed), 0, Some(NOW - 1), false, NOW),
            CredentialHealth::Stale
        );

        // AtRisk — the refresh safety-net is failing (a streak of errors), even while the
        // access token itself has not yet expired.
        assert_eq!(
            credential_health(false, Some(Error), 1, Some(NOW + 60), false, NOW),
            CredentialHealth::AtRisk
        );

        // Degraded — a bare quarantine (the #42 access-token 401-streak) is NON-TERMINAL
        // (issue #427): the refresh token is unproven, so `poke` / a restart revive it. It is
        // 🟠, NOT the terminal 🔴 `Dead` — the honesty fix that stops the false "claude /login".
        assert_eq!(
            credential_health(true, None, 0, None, false, NOW),
            CredentialHealth::Degraded
        );
        // Dead — reserved for PROVEN refresh-token death: a sweep-refresh actually returned
        // `Dead` (#261). Surfaced as 🔴 rather than hidden — this is a DISPLAY rollup, it never
        // quarantines. This is the ONLY 🔴 case (issue #427).
        assert_eq!(
            credential_health(
                false,
                Some(RefreshEventOutcome::Dead),
                0,
                Some(NOW + 60),
                false,
                NOW
            ),
            CredentialHealth::Dead
        );
        // Proven death WINS over a co-occurring quarantine: a quarantined account whose refresh
        // ALSO returned `Dead` is genuinely dead (needs re-login), so it reads 🔴 `Dead`, not
        // 🟠 `Degraded` — `Dead` is checked before `quarantined` (issue #427).
        assert_eq!(
            credential_health(true, Some(RefreshEventOutcome::Dead), 0, None, false, NOW),
            CredentialHealth::Dead
        );

        // Severity ladder (Dead > Degraded > AtRisk > Stale > Healthy): a quarantined account
        // whose token is ALSO expired and whose refresh is ALSO merely FAILING (an `Error`
        // streak, not a proven `Dead`) reads 🟠 `Degraded` — the quarantine outranks the
        // at-risk streak, and without a proven refresh death it is NOT terminal (issue #427).
        // An at-risk account whose token is ALSO expired reads AtRisk, not Stale. A fresh
        // reading NEVER masks a negative signal — even `has_fresh_reading = true` holds here.
        assert_eq!(
            credential_health(true, Some(Error), 3, Some(NOW - 10), true, NOW),
            CredentialHealth::Degraded
        );
        assert_eq!(
            credential_health(false, Some(Error), 2, Some(NOW - 10), true, NOW),
            CredentialHealth::AtRisk
        );
    }

    #[test]
    fn credential_health_reports_unknown_without_a_positive_liveness_signal() {
        use RefreshEventOutcome::NoChange;
        const NOW: i64 = 1_782_777_600;

        // #137: absence of a NEGATIVE signal is not health. A non-active account never
        // successfully polled, `[refresh]` off (no telemetry, no refresh-sourced expiry, no
        // fresh reading) ⇒ Unknown — NOT a false 🟢 that would jump straight to 🔴 the moment
        // the 401-streak quarantines it. This is the exact case that fell through to Healthy
        // before the fix.
        assert_eq!(
            credential_health(false, None, 0, None, false, NOW),
            CredentialHealth::Unknown
        );

        // Any ONE positive-liveness signal lifts it to Healthy:
        //  (a) a fresh successful usage reading (the strongest proof — a live-API poll),
        assert_eq!(
            credential_health(false, None, 0, None, true, NOW),
            CredentialHealth::Healthy
        );
        //  (b) refresh telemetry (the refresh path observed the account alive),
        assert_eq!(
            credential_health(false, Some(NoChange), 0, None, false, NOW),
            CredentialHealth::Healthy
        );
        //  (c) a FUTURE refresh-sourced expiry (the refresh engine read a valid token).
        assert_eq!(
            credential_health(false, None, 0, Some(NOW + 60), false, NOW),
            CredentialHealth::Healthy
        );

        // AC: a LAPSED refresh-sourced expiry (no telemetry, no reading) is a KNOWN stale
        // window the refresh net recovers — Stale wins over the no-evidence check, never
        // Unknown and never a false Healthy.
        assert_eq!(
            credential_health(false, None, 0, Some(NOW - 1), false, NOW),
            CredentialHealth::Stale
        );

        // A negative signal always overrides missing evidence — a bare quarantine ⇒ Degraded
        // (issue #427: NON-TERMINAL, needs a refresh not a re-login), never Unknown, even with
        // no other input.
        assert_eq!(
            credential_health(true, None, 0, None, false, NOW),
            CredentialHealth::Degraded
        );
    }

    #[test]
    fn credential_health_reserves_dead_for_proven_refresh_death_not_a_bare_quarantine() {
        // Issue #427 regression: locks the honesty trajectory 🟢 → 🟠 degraded → 🔴-only-on-proof
        // so a parked account that merely 401-streaked into quarantine can never again render the
        // terminal 🔴 / "claude /login" while its refresh token is still good.
        const NOW: i64 = 1_782_777_600;

        // Healthy — a positive-liveness signal (a fresh reading), refresh path untouched.
        assert_eq!(
            credential_health(false, None, 0, None, true, NOW),
            CredentialHealth::Healthy
        );

        // Degraded (NOT Dead) — the access token 401-streaked into quarantine, but no refresh has
        // returned `Dead`, so the refresh token is unproven and `poke` / a restart revive it. This
        // is the exact false-🔴 the issue fixes: a bare quarantine is 🟠 needs-refresh, never
        // 🔴 needs-re-login — regardless of whether the refresh net is merely failing (`Error`),
        // idle (`None`), or last succeeded (`NoChange`), and regardless of a stale/fresh clock.
        for refresh in [
            None,
            Some(RefreshEventOutcome::Error),
            Some(RefreshEventOutcome::NoChange),
            Some(RefreshEventOutcome::Refreshed),
        ] {
            assert_eq!(
                credential_health(true, refresh, 0, None, false, NOW),
                CredentialHealth::Degraded,
                "a bare quarantine (refresh={refresh:?}) is degraded, never dead"
            );
        }

        // Dead — ONLY once a sweep-refresh actually returns `Dead` (#261 / `CredentialUnrecoverable`):
        // the refresh token itself was rejected, so a re-login is genuinely required. Holds whether
        // or not the account is also quarantined — proven death is checked first and wins.
        assert_eq!(
            credential_health(false, Some(RefreshEventOutcome::Dead), 0, None, false, NOW),
            CredentialHealth::Dead
        );
        assert_eq!(
            credential_health(true, Some(RefreshEventOutcome::Dead), 0, None, false, NOW),
            CredentialHealth::Dead
        );
    }

    #[test]
    fn millis_to_secs_folds_a_known_expiry_at_the_ms_boundary() {
        // The blob's `expiresAt` is epoch MILLISECONDS; the wire and rollup are epoch SECONDS
        // (issue #141 must-carry — a missed fold misfires the operator clock by 1000×). A
        // known instant folds exactly; a sub-second remainder truncates (immaterial for a
        // token-lifetime clock) and matches the refresh fold's `ms / 1000`.
        assert_eq!(millis_to_secs(1_782_777_600_000), 1_782_777_600);
        assert_eq!(millis_to_secs(1_782_777_600_999), 1_782_777_600);
        assert_eq!(millis_to_secs(0), 0);
    }

    #[test]
    fn refresh_health_view_is_none_until_observed_then_reduces_the_outcome() {
        // No refresh observed yet (`[refresh]` off, or not yet swept) → None, so the wire
        // omits the field rather than fabricating a verdict.
        assert_eq!(refresh_health_view(&AccountHealth::default()), None);

        // An alive outcome reduces to `last_ok: true`, carrying the rotation flag (the AC-3
        // durability signal) and the failure streak.
        let alive = AccountHealth {
            last_refresh_outcome: Some(RefreshEventOutcome::NoChange),
            refresh_token_rotated: Some(true),
            consecutive_refresh_failures: 0,
            ..Default::default()
        };
        assert_eq!(
            refresh_health_view(&alive),
            Some(RefreshHealth {
                last_ok: true,
                rotated: true,
                consecutive_failures: 0,
            })
        );

        // A dead/error outcome reduces to `last_ok: false`, surfacing the failure streak the
        // rollup's at-risk input keys off.
        let failing = AccountHealth {
            last_refresh_outcome: Some(RefreshEventOutcome::Error),
            refresh_token_rotated: Some(false),
            consecutive_refresh_failures: 3,
            ..Default::default()
        };
        assert_eq!(
            refresh_health_view(&failing),
            Some(RefreshHealth {
                last_ok: false,
                rotated: false,
                consecutive_failures: 3,
            })
        );
    }

    #[tokio::test]
    async fn apply_refresh_observation_folds_ms_expiry_and_tracks_consecutive_failures() {
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        // `RefreshDelta` lives in `contract` and is not imported at module scope (only
        // the daemon's fold consumes it); name it in full here.
        use crate::contract::RefreshDelta;
        let observe = |outcome, rotated, ms| RefreshObservation {
            account_uuid: "u-A".to_owned(),
            expires_at_ms: Some(ms),
            refresh: Some(RefreshDelta {
                outcome,
                token_rotated: rotated,
            }),
        };

        // A read-only observation (`refresh: None`) updates ONLY the expiry — folded from
        // the engine's milliseconds to the rollup's epoch seconds at this boundary.
        daemon.apply_refresh_observation(&RefreshObservation {
            account_uuid: "u-A".to_owned(),
            expires_at_ms: Some(1_782_777_600_000),
            refresh: None,
        });
        assert_eq!(
            daemon.state.health[0].access_expires_at,
            Some(1_782_777_600)
        );
        assert_eq!(daemon.state.health[0].last_refresh_outcome, None);
        assert_eq!(daemon.state.health[0].consecutive_refresh_failures, 0);

        // Failing refreshes advance the consecutive-failure streak and record the outcome.
        daemon.apply_refresh_observation(&observe(
            RefreshEventOutcome::Error,
            false,
            1_782_777_600_000,
        ));
        assert_eq!(daemon.state.health[0].consecutive_refresh_failures, 1);
        daemon.apply_refresh_observation(&observe(
            RefreshEventOutcome::Dead,
            false,
            1_782_777_600_000,
        ));
        assert_eq!(daemon.state.health[0].consecutive_refresh_failures, 2);
        assert_eq!(
            daemon.state.health[0].last_refresh_outcome,
            Some(RefreshEventOutcome::Dead)
        );

        // Any alive refresh resets the streak to zero — so the at-risk input counts only
        // CONSECUTIVE failures — and slides the expiry forward.
        daemon.apply_refresh_observation(&observe(
            RefreshEventOutcome::Refreshed,
            true,
            1_782_784_800_000,
        ));
        assert_eq!(daemon.state.health[0].consecutive_refresh_failures, 0);
        assert_eq!(daemon.state.health[0].refresh_token_rotated, Some(true));
        assert_eq!(
            daemon.state.health[0].access_expires_at,
            Some(1_782_784_800)
        );

        // An observation for a uuid the daemon no longer holds is ignored (no panic, no
        // spurious mutation) — mirroring `apply_refresh_restore`; the siblings stay pristine.
        daemon.apply_refresh_observation(&RefreshObservation {
            account_uuid: "u-GONE".to_owned(),
            expires_at_ms: Some(0),
            refresh: None,
        });
        assert_eq!(daemon.state.health[1].access_expires_at, None);
    }

    #[tokio::test]
    async fn apply_refresh_restore_un_quarantines_once_and_is_a_noop_otherwise() {
        // Issue #275 (AC-1 + AC-3): the primitive the new `restored` control command drives.
        // A quarantined account is un-quarantined EXACTLY once — the first call flips
        // `quarantined` off, resets `recovery_successes`, and returns its edge-triggered
        // `CredentialRestored`; a second call is a silent `None` (already restored). An unknown
        // uuid and an already-non-quarantined account are both idempotent `None` no-ops. Throughout,
        // the ACTIVE account is untouched — the restore never re-points canonical or swaps active,
        // the guarantee that lets `login <B>` clear B's quarantine WITHOUT activating B.
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let active_before = daemon.state.active;

        // An already-non-quarantined account: no-op, no event.
        assert_eq!(daemon.apply_refresh_restore("u-B"), None);
        // An unknown uuid: no-op, no event (the daemon no longer holds it).
        assert_eq!(daemon.apply_refresh_restore("u-NOPE"), None);

        // Quarantine the PARKED, non-active `spare` (index 1) — the #106 parked-and-stuck case —
        // and seed a recovery streak the restore must clear.
        daemon.state.health[1].quarantined = true;
        daemon.state.health[1].recovery_successes = 2;

        // First restore: un-quarantines and emits exactly the edge event, named by handle only.
        assert_eq!(
            daemon.apply_refresh_restore("u-B"),
            Some(Event::CredentialRestored {
                account: "spare".to_owned(),
            })
        );
        assert!(!daemon.state.health[1].quarantined, "spare un-quarantined");
        assert_eq!(
            daemon.state.health[1].recovery_successes, 0,
            "the recovery streak is reset on restore"
        );

        // Second restore of the now-eligible account: idempotent silent no-op.
        assert_eq!(daemon.apply_refresh_restore("u-B"), None);

        // The active account was never touched by any of the above (AC-1: active unchanged).
        assert_eq!(
            daemon.state.active, active_before,
            "an on-demand restore never changes the active account"
        );
    }

    #[tokio::test]
    async fn unrecoverable_signal_fires_once_and_only_when_quarantined() {
        // Issue #261: a QUARANTINED account whose isolated sweep-refresh returns `Dead` is
        // confirmed unrecoverable — `apply_refresh_observation` yields `credential_unrecoverable`
        // ONCE per quarantine episode, never per re-probe (AC2), and never for a non-quarantined
        // account (the AC's scope gate). The handle is the operator label only (AC3/#15).
        use crate::contract::RefreshDelta;
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let obs = |uuid: &str, outcome| RefreshObservation {
            account_uuid: uuid.to_owned(),
            expires_at_ms: Some(1_782_777_600_000),
            refresh: Some(RefreshDelta {
                outcome,
                token_rotated: false,
            }),
        };

        // A non-quarantined account's dead sweep-refresh does NOT notify — that is the #119
        // refresh-detected death the rollup surfaces, deliberately outside #261's console/macOS
        // operator-signal scope.
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Dead)),
            None
        );
        assert!(!daemon.state.health[0].unrecoverable_signaled);

        // Quarantine account 0 (the #42 verdict); the next dead sweep-refresh CONFIRMS it
        // unrecoverable → exactly one event, named by handle only.
        daemon.state.health[0].quarantined = true;
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Dead)),
            Some(Event::CredentialUnrecoverable {
                account: "work".to_owned(),
            })
        );
        assert!(daemon.state.health[0].unrecoverable_signaled);

        // Every subsequent re-probe of the still-dead token is SILENT — INCLUDING a
        // `Dead`→`Error`→`Dead` flap, which a naive last-outcome guard would double-fire on (a
        // transient sweep `Error` between dead probes must not re-arm the signal).
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Dead)),
            None
        );
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Error)),
            None
        );
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Dead)),
            None
        );
    }

    #[tokio::test]
    async fn unrecoverable_latch_resets_on_requarantine_so_the_signal_can_refire() {
        // Issue #261: the latch is reset at the single quarantine-SET site, so each NEW quarantine
        // episode re-arms the signal. This covers two regressions a `last_refresh_outcome`-based
        // guard fails: (b) a sweep that saw `Dead` BEFORE the account quarantined must STILL fire
        // once it does, and (a) a recover→re-die must re-fire.
        use crate::contract::RefreshDelta;
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let dead = |uuid: &str| RefreshObservation {
            account_uuid: uuid.to_owned(),
            expires_at_ms: Some(1_782_777_600_000),
            refresh: Some(RefreshDelta {
                outcome: RefreshEventOutcome::Dead,
                token_rotated: false,
            }),
        };
        let mut events = Vec::new();
        // Drive account `i` into quarantine through the real poll path (monitor_401_n = 3): the
        // Nth consecutive 401 sets `quarantined` AND resets the #261 latch.
        let quarantine = |d: &mut FakeDaemon, i: usize, sink: &mut Vec<Event>| {
            for _ in 0..3 {
                d.note_poll_outcome(i, &Err(Error::UsageUnauthorized), sink);
            }
        };

        // (b) The sweep sees `Dead` while account 0 is NOT yet quarantined: no signal, but
        // `last_refresh_outcome` is now `Some(Dead)` — the state that would poison a naive guard.
        assert_eq!(daemon.apply_refresh_observation(&dead("u-A")), None);
        assert_eq!(
            daemon.state.health[0].last_refresh_outcome,
            Some(RefreshEventOutcome::Dead)
        );

        // The access token then 401-streaks account 0 into quarantine; the SET clears the latch.
        quarantine(&mut daemon, 0, &mut events);
        assert!(daemon.state.health[0].quarantined);
        assert!(!daemon.state.health[0].unrecoverable_signaled);
        // Despite `last_refresh_outcome` ALREADY being `Dead`, the next dead sweep FIRES — the
        // latch, not the outcome history, gates the edge.
        assert_eq!(
            daemon.apply_refresh_observation(&dead("u-A")),
            Some(Event::CredentialUnrecoverable {
                account: "work".to_owned(),
            })
        );

        // (a) Recover (an operator re-login un-quarantines) then re-die: the fresh episode re-fires.
        daemon.state.health[0].quarantined = false;
        daemon.state.health[0].consec_401 = 0;
        quarantine(&mut daemon, 0, &mut events);
        assert!(!daemon.state.health[0].unrecoverable_signaled);
        assert_eq!(
            daemon.apply_refresh_observation(&dead("u-A")),
            Some(Event::CredentialUnrecoverable {
                account: "work".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn poll_populates_the_display_expiry_clock_without_the_refresh_tick() {
        // Issue #141: with `[refresh]` OFF (no `RefreshObservation` ever folded — the refresh
        // engine, the field's only OTHER writer, is off by default), the poll path alone must
        // surface each polled account's access-token expiry on `status --json`, WITHOUT feeding
        // the naive `access_expires_at <= now → Stale` rollup branch — that would false-🟠 every
        // idle account whose stashed token has lapsed (the rollup's positive-liveness
        // consumption of the poll clock lands under #137).

        // A realistic CC credential: the SECRET token beside the non-secret `expiresAt` (ms).
        // The active account's CANONICAL item and the per-account STASH carry DIFFERENT
        // expiries, so the assertions prove the clock is sourced from the SAME credential the
        // poll used — canonical for the active account, the stash for any other.
        const TOKEN: &str = "sk-ant-oat-SECRET-must-not-leak";
        const CANON_MS: i64 = 1_782_777_600_000;
        const CANON_S: i64 = 1_782_777_600;
        const STASH_MS: i64 = 1_782_784_800_000;
        const STASH_S: i64 = 1_782_784_800;
        let blob = |expires_at_ms: i64| -> Vec<u8> {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{TOKEN}","expiresAt":{expires_at_ms}}}}}"#
            )
            .into_bytes()
        };

        let canon_blob = blob(CANON_MS);
        let stash_blob = blob(STASH_MS);
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "backup"),
        ];
        let store = store_holding(&canon_blob).await; // canonical = the active account's bearer
        let stash = stash_with(&[
            ("Sessiometer/u-A", &stash_blob, "u-A"),
            ("Sessiometer/u-B", &stash_blob, "u-B"),
            ("Sessiometer/u-C", &stash_blob, "u-C"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        std::mem::forget(dir);
        let tun = tunables(95, 80, 0);
        let mut daemon = Daemon::new(
            roster,
            FakeRosterPoller::new()
                .ok("u-A", 0.11, 0.10)
                .ok("u-B", 0.22, 0.10)
                .ok("u-C", 0.33, 0.10),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        // Tick 1 polls the ACTIVE account (u-A) → its expiry is read from the CANONICAL item…
        daemon.tick().await;
        assert_eq!(daemon.state.health[0].poll_expires_at, Some(CANON_S));
        // …while the refresh-sourced field the rollup actually reads stays untouched: with
        // `[refresh]` off it is still `None`, so no lapsed poll clock can reach the Stale branch.
        assert_eq!(daemon.state.health[0].access_expires_at, None);

        // Tick 2 polls a NON-active account (u-B) → its expiry is read from that account's STASH.
        daemon.tick().await;
        assert_eq!(daemon.state.health[1].poll_expires_at, Some(STASH_S));
        assert_eq!(daemon.state.health[1].access_expires_at, None);

        // Project the wire the control socket returns, with `now` set a day AFTER the polled
        // expiry — the exact lapsed-idle case. The clock IS populated (AC: non-null with
        // `[refresh]` off) yet the ACTIVE account stays Healthy, NOT a false-🟠 Stale: the
        // poll clock never reaches the Stale branch, and its own successful poll is the
        // positive-liveness signal keeping it Healthy rather than Unknown (#137).
        let readings = daemon.state.last_readings.clone();
        let snapshot = daemon.snapshot(daemon.state.active, &readings, CANON_S + 86_400);
        assert_eq!(snapshot.accounts[0].access_expires_at, Some(CANON_S));
        assert_eq!(snapshot.accounts[0].health, CredentialHealth::Healthy);
        // The third account (u-C) was never polled this run — no reading, no telemetry, no
        // refresh clock — so #137 reports it ⚪ Unknown, NOT a false 🟢, even as #141's
        // display clock keeps working for the accounts that were polled.
        assert_eq!(snapshot.accounts[2].health, CredentialHealth::Unknown);

        // The clock reached the wire (non-vacuous), and the surrounding token never rode
        // alongside it into any output channel (issue #15 / #141 secret-handling).
        let corpus = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(corpus.contains(r#""access_expires_at":1782777600"#));
        assert!(!corpus.contains(TOKEN));
        // #137 AC: the raw Unknown state rides the `--json` wire as a scriptable token,
        // so a consumer can tell "unverified" apart from a genuine "healthy". The wire key
        // is `auth` (issue #143 renamed the field `health` → `auth`).
        assert!(corpus.contains(r#""auth":"unknown""#));
    }

    #[tokio::test]
    async fn note_health_transitions_seeds_silently_then_emits_one_event_per_change() {
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        const NOW: i64 = 1_782_777_600;

        // The FIRST computation per account SEEDS the baseline WITHOUT emitting — a fresh
        // daemon logs no startup storm. All three start UNKNOWN (#137): not quarantined, no
        // clocks, and no successful poll yet (this daemon was never ticked) — no positive
        // liveness signal, so honestly unverified rather than a false 🟢.
        assert!(daemon.note_health_transitions(NOW).is_empty());
        assert_eq!(
            daemon.state.health[0].last_health,
            Some(CredentialHealth::Unknown)
        );

        // A genuine change emits EXACTLY ONE redacted event (AC-3) — the handle and the new
        // state — and only for the account that changed. A bare quarantine (an access-token
        // 401-streak) transitions to Degraded, NOT Dead (issue #427): the event log carries the
        // honest non-terminal verdict too, so a `grep` never cries a false death.
        daemon.state.health[0].quarantined = true; // → Degraded
        assert_eq!(
            daemon.note_health_transitions(NOW),
            vec![Event::CredentialHealth {
                account: "work".to_owned(),
                state: CredentialHealth::Degraded,
            }]
        );

        // No change ⇒ no event (edge-triggered, not level-triggered).
        assert!(daemon.note_health_transitions(NOW).is_empty());

        // Un-quarantine WITHOUT any new evidence ⇒ back to Unknown, NOT a false Healthy
        // (#137): clearing the quarantine flag does not prove the credential is alive.
        daemon.state.health[0].quarantined = false; // → Unknown (still no liveness signal)
        assert_eq!(
            daemon.note_health_transitions(NOW),
            vec![Event::CredentialHealth {
                account: "work".to_owned(),
                state: CredentialHealth::Unknown,
            }]
        );

        // Evidence ARRIVES — a successful poll for `work` (enabled, non-quarantined, so it
        // surfaces through `decision_readings`) ⇒ Unknown transitions to a real Healthy state.
        daemon.state.last_readings[0] = Some(Usage {
            session: 0.10,
            weekly: 0.10,
            weekly_resets_at: None,
            session_resets_at: None,
        });
        assert_eq!(
            daemon.note_health_transitions(NOW),
            vec![Event::CredentialHealth {
                account: "work".to_owned(),
                state: CredentialHealth::Healthy,
            }]
        );
    }

    /// The #378 daemon-side wiring the pure `SystemicRefreshHealth` unit tests can't reach:
    /// `note_systemic_refresh` folds each sweep through the daemon's OWN configured threshold — here
    /// the default 3, since `three_account_daemon` leaves `systemic_failure_n` at the config default
    /// (the "drives at defaults" integration case the field's doc-comment anticipates) — and an
    /// active episode surfaces in `snapshot`'s `systemic_refresh` projection, the `status`-visible
    /// indicator that shows the mechanism is down without waiting for an account to die.
    #[tokio::test]
    async fn note_systemic_refresh_threads_the_configured_threshold_and_projects_into_snapshot() {
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        const NOW: i64 = 1_782_777_600;
        let no_readings: [Option<Usage>; 3] = [None, None, None];
        let indicator = |d: &FakeDaemon| d.snapshot(None, &no_readings, NOW).systemic_refresh;

        // Below the default threshold the mechanism-down streak climbs but stays silent, and the
        // indicator reads healthy — proving the daemon threads its own `systemic_failure_n`, not a
        // hardcoded N (a broken default or field-plumbing would fire early here).
        assert_eq!(daemon.note_systemic_refresh(SweepHealth::AllError), None);
        assert_eq!(daemon.note_systemic_refresh(SweepHealth::AllError), None);
        assert_eq!(indicator(&daemon), None, "healthy below the threshold");

        // The 3rd consecutive all-error sweep crosses the threshold → exactly one edge-triggered
        // failure carrying the count, and the snapshot now surfaces the mechanism-down count.
        assert_eq!(
            daemon.note_systemic_refresh(SweepHealth::AllError),
            Some(Event::RefreshSystemicFailure { consecutive: 3 })
        );
        assert_eq!(
            indicator(&daemon),
            Some(3),
            "active episode is status-visible"
        );

        // A further all-error sweep keeps climbing but does NOT re-emit (edge-, not level-triggered).
        assert_eq!(daemon.note_systemic_refresh(SweepHealth::AllError), None);
        assert_eq!(indicator(&daemon), Some(4));

        // A single working sweep is the recovery edge: one recovery event, and the indicator clears.
        assert_eq!(
            daemon.note_systemic_refresh(SweepHealth::Working),
            Some(Event::RefreshSystemicRecovered)
        );
        assert_eq!(
            indicator(&daemon),
            None,
            "recovery clears the status indicator"
        );
    }

    #[test]
    fn redaction_meter_covers_the_new_credential_clock_fields() {
        use crate::redaction::meter::{assert_clean, Secrets};
        // The full-loop meter test runs no sweep, so its corpus never carries a populated
        // `refresh_health` / `access_expires_at`. Exercise those new wire fields here with
        // non-default values — the expiry is the SAME instant embedded in the fixture blob's
        // `expiresAt`, so a path that leaked the surrounding token alongside the expiry would
        // surface it — and prove the value-based meter (#15) still reads clean.
        let secrets = Secrets::meter_fixture();
        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            generated_at: 0,
            refresh_enabled: false,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                enabled: true,
                access_expires_at: Some(1_782_777_600),
                refresh_health: Some(RefreshHealth {
                    last_ok: true,
                    rotated: true,
                    consecutive_failures: 0,
                }),
                health: CredentialHealth::Stale,
                ..Default::default()
            }],
            next_swap: None,
        };
        let response = status_response(&snapshot);
        let mut corpus = serde_json::to_string(&response).unwrap();
        corpus.push('\n');
        // The text surface too (it carries the 🟡 glyph for this Stale account).
        corpus.push_str(&crate::cli::render_status(
            &response,
            1_782_700_000,
            None,
            false,
        ));
        // …and the `status --verbose` access-token expiry block (issue #143), a third
        // operator-facing surface that reprojects the same `access_expires_at` clock — so a
        // path leaking the surrounding token alongside the expiry surfaces here too.
        corpus.push_str(&crate::cli::render_access_token_expiry(
            &response,
            1_782_700_000,
        ));

        // Cardinality (#15 non-vacuous gate): the new fields actually reached the scanned
        // corpus before the clean verdict below is trusted.
        assert!(corpus.contains(r#""access_expires_at":1782777600"#));
        assert!(corpus.contains(r#""refresh_health":{"#));
        // The rollup rides the wire under the `auth` key (issue #143 renamed `health` → `auth`).
        assert!(corpus.contains(r#""auth":"stale""#));
        assert_clean(&corpus, &secrets);
    }

    #[test]
    fn status_response_carries_the_refresh_enabled_flag_onto_the_wire() {
        // Issue #138: the daemon's live `[refresh].enabled` is wrapped `Some(..)` on the wire
        // (mirroring `health`) so the thin `status` client can gate its advisory off the daemon's
        // ACTUAL refresh state. A current daemon always sends a definite `Some(true/false)`; only
        // a pre-#138 daemon omits the field (→ the client decodes `None` and suppresses).
        for enabled in [true, false] {
            let snapshot = StatusSnapshot {
                refresh_enabled: enabled,
                ..Default::default()
            };
            assert_eq!(status_response(&snapshot).refresh_enabled, Some(enabled));
        }
    }

    #[tokio::test]
    async fn daemon_snapshot_reflects_with_refresh_enabled() {
        // Issue #138 daemon plumbing: `with_refresh_enabled` (fed `config.refresh.enabled` in the
        // run path) flows onto the display snapshot, so the client's advisory gate sees the
        // daemon's LIVE refresh state. Default (no builder) is the opt-in `false`; the builder
        // flips it. `snapshot` reads only the flag here, so all-`None` readings keep it minimal.
        let default_daemon = lifecycle_daemon().await;
        let readings = vec![None; default_daemon.roster.len()];
        let off = default_daemon.snapshot(Some(0), &readings, 0);
        assert!(!off.refresh_enabled, "the opt-in default carries tick-off");

        let on = lifecycle_daemon()
            .await
            .with_refresh_enabled(true)
            .snapshot(Some(0), &readings, 0);
        assert!(
            on.refresh_enabled,
            "with_refresh_enabled(true) flows to the display snapshot"
        );
    }

    #[tokio::test]
    async fn serve_control_answers_status_with_exactly_one_line() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            generated_at: 0,
            refresh_enabled: false,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                enabled: true,
                quarantined: false,
                recovering: false,
                weekly_exhausted: false,
                usage: Some(Usage {
                    session: 0.50,
                    weekly: 0.25,
                    weekly_resets_at: None,
                    session_resets_at: None,
                }),
                ..Default::default()
            }],
            next_swap: None,
        };
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"status\"}\n").await.unwrap();
        // `status` is a non-secret read — answered for any peer, and producing no
        // control signal (it never mutates daemon state).
        let signal = serve_control(server, &snapshot, false)
            .await
            .unwrap()
            .one_shot();
        assert!(signal.is_none(), "status must not produce a control signal");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert_eq!(
            reply.lines().count(),
            1,
            "exactly one status line: {reply:?}"
        );
        assert!(reply.contains("\"label\":\"work\""));
        assert!(reply.contains("\"session_pct\":50"));
        assert!(!reply.contains('@'));
    }

    #[tokio::test]
    async fn serve_control_rejects_an_unknown_command() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"nope\"}\n").await.unwrap();
        let signal = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .one_shot();
        assert!(signal.is_none(), "an unknown command produces no signal");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("unknown command"), "got {reply:?}");
    }

    #[tokio::test]
    async fn serve_control_writes_the_ok_ack_and_yields_the_shutdown_signal() {
        // Issue #397: an authenticated same-user `shutdown` — the `daemon stop` control path for an
        // unmanaged daemon — is answered with `{"ok":true}` over the stream AND yields the
        // `ShutdownRequested` signal the run loop turns into a graceful `Idle::Shutdown`. The ack is
        // flushed HERE, before the signal ever reaches the run loop, so the client learns the stop
        // was accepted before the daemon goes away.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"shutdown\"}\n").await.unwrap();
        let signal = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .one_shot();
        assert_eq!(
            signal,
            Some(ControlSignal::ShutdownRequested),
            "an authenticated shutdown yields the graceful-stop signal",
        );

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert_eq!(
            reply.trim_end(),
            r#"{"ok":true}"#,
            "the ok ack is written before the daemon exits: {reply:?}",
        );
    }

    #[tokio::test]
    async fn serve_control_honours_a_shutdown_whose_peer_hung_up_before_the_ack() {
        // Issue #397: the daemon accepts control connections only BETWEEN ticks, so a `daemon stop`
        // against a busy daemon can time out and close before the daemon ever reads the request.
        // When the daemon then answers, the ack write fails with `EPIPE`. Delivering the ack is
        // best-effort — the request was already read and authenticated, so the shutdown MUST still
        // take effect. Propagating the write error instead would discard the signal at
        // `UnixControl::serve`'s `Err(_) => Signal(None)` arm: the operator's `daemon stop` would
        // exit 1 AND the daemon would keep running.
        use tokio::io::AsyncWriteExt;

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"shutdown\"}\n").await.unwrap();
        drop(client); // the client gave up waiting and hung up: the ack write will now fail

        let signal = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .expect("a failed ack must not fail the exchange")
            .one_shot();
        assert_eq!(
            signal,
            Some(ControlSignal::ShutdownRequested),
            "an authenticated shutdown survives an undeliverable ack",
        );
    }

    #[tokio::test]
    async fn serve_control_refuses_a_shutdown_from_an_unauthenticated_peer() {
        // Issue #397: `shutdown` is state-affecting (it ends the process), so an UNauthenticated
        // peer is fail-closed with `{"error":"unauthorized"}` and produces NO signal — a stranger
        // can never stop the daemon (the same same-user gate `manual-swapped` #64 / `roster-reload`
        // #139 / `restored` #275 sit behind). Auth is the ONLY gate on this verb, so this is the
        // whole guard: the socket-layer half here, the real `getpeereid` euid comparison that
        // computes the bool in `serve_control_rejects_a_foreign_uid_peer` / `is_same_user`.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"shutdown\"}\n").await.unwrap();
        let signal = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap()
            .one_shot();
        assert!(
            signal.is_none(),
            "an unauthorized shutdown produces no signal — the daemon keeps running",
        );

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("unauthorized"), "fail-closed: {reply:?}");
    }

    #[tokio::test]
    async fn serve_control_bounds_an_oversized_request_line() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Issue #64: the receive path must be BOUNDED. A peer that streams a line
        // longer than the cap — with NO newline and the connection held OPEN — would
        // hang an unbounded `read_line`; only the byte cap can end this read (EOF at
        // the limit), after which the over-long request is rejected as malformed.
        // The client never closes, so it is the cap (not an EOF) that ends the read;
        // a regressed cap is caught by the exchange timeout firing with no reply.
        let oversized = vec![b'{'; MAX_CONTROL_LINE_BYTES as usize + 1];
        let (mut client, server) = tokio::io::duplex(oversized.len() + 64);
        client.write_all(&oversized).await.unwrap();
        let signal = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .one_shot();
        assert!(signal.is_none(), "an oversized request produces no signal");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(
            reply.contains("malformed"),
            "an over-long request is bounded and rejected: {reply:?}"
        );
    }

    // ---- `watch` subscription (issue #165) ------------------------------------------------

    /// A one-account status snapshot for the `watch` tests, stamped with a chosen `generated_at`
    /// and session fraction so a test can tell one pushed snapshot from the next.
    fn watch_snapshot(label: &str, generated_at: i64, session: f64) -> StatusSnapshot {
        StatusSnapshot {
            systemic_refresh: None,
            generated_at,
            refresh_enabled: false,
            next_swap: None,
            accounts: vec![AccountReading {
                label: label.to_owned(),
                active: true,
                enabled: true,
                usage: Some(Usage {
                    session,
                    weekly: 0.10,
                    weekly_resets_at: None,
                    session_resets_at: None,
                }),
                ..Default::default()
            }],
        }
    }

    /// Read exactly one newline-delimited frame line from a `watch` stream, asserting the framing.
    async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> String {
        use tokio::io::AsyncBufReadExt;
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read a frame line");
        assert!(
            line.ends_with('\n'),
            "watch frames are newline-delimited: {line:?}"
        );
        line.trim_end().to_owned()
    }

    #[tokio::test]
    async fn serve_watch_streams_the_initial_snapshot_then_one_per_update() {
        // The daemon side of the latest-snapshot channel, seeded with the first snapshot.
        let (tx, rx) = tokio::sync::watch::channel(versioned_status_response(&watch_snapshot(
            "work", 100, 0.20,
        )));
        let (client, server) = tokio::io::duplex(4096);
        // A far-off heartbeat keeps liveness beats out of this update-focused test.
        let watcher = tokio::spawn(serve_watch(server, rx, Duration::from_secs(3600)));

        let mut reader = tokio::io::BufReader::new(client);
        // 1) The initial full snapshot arrives immediately on connect.
        let initial = read_frame(&mut reader).await;
        match parse_watch_frame(&initial).unwrap() {
            WatchFrame::Snapshot(v) => {
                assert_eq!(v.generated_at, 100);
                assert_eq!(v.status.accounts[0].label, "work");
                assert_eq!(v.status.accounts[0].session_pct, Some(20));
                assert!(!initial.contains('@'), "no email can travel (issue #15)");
            }
            other => panic!("expected an initial snapshot, got {other:?}"),
        }
        // 2) A published state change streams the WHOLE new snapshot (never a delta).
        tx.send_replace(versioned_status_response(&watch_snapshot(
            "work", 200, 0.55,
        )));
        let update = read_frame(&mut reader).await;
        match parse_watch_frame(&update).unwrap() {
            WatchFrame::Snapshot(v) => {
                assert_eq!(v.generated_at, 200);
                assert_eq!(v.status.accounts[0].session_pct, Some(55));
            }
            other => panic!("expected an update snapshot, got {other:?}"),
        }
        drop(reader); // the client goes away → the stream ends cleanly
        watcher.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn serve_watch_beats_during_silence() {
        // No state change ever occurs, so the ONLY frames after the initial snapshot are beats.
        let (_tx, rx) = tokio::sync::watch::channel(versioned_status_response(&watch_snapshot(
            "work", 500, 0.10,
        )));
        let (client, server) = tokio::io::duplex(4096);
        let heartbeat = Duration::from_secs(15);
        let watcher = tokio::spawn(serve_watch(server, rx, heartbeat));

        let mut reader = tokio::io::BufReader::new(client);
        let initial = read_frame(&mut reader).await;
        assert!(matches!(
            parse_watch_frame(&initial).unwrap(),
            WatchFrame::Snapshot(_)
        ));
        // After one interval of SILENCE, a heartbeat fires (bounding a client's stale detection).
        tokio::time::advance(heartbeat + Duration::from_millis(1)).await;
        let beat = read_frame(&mut reader).await;
        match parse_watch_frame(&beat).unwrap() {
            WatchFrame::Heartbeat {
                generated_at,
                schema_version,
            } => {
                assert_eq!(
                    generated_at, 500,
                    "the beat carries the last-known freshness"
                );
                assert_eq!(schema_version, STATUS_SCHEMA_VERSION);
            }
            other => panic!("expected a heartbeat during silence, got {other:?}"),
        }
        drop(reader);
        watcher.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn serve_watch_ends_when_the_client_disconnects() {
        // Issue #165 AC-2 (server side): a dropped subscriber must not leak the streaming task.
        let (_tx, rx) =
            tokio::sync::watch::channel(versioned_status_response(&watch_snapshot("work", 1, 0.0)));
        let (client, server) = tokio::io::duplex(4096);
        let watcher = tokio::spawn(serve_watch(server, rx, Duration::from_secs(3600)));

        let mut reader = tokio::io::BufReader::new(client);
        // The stream is live: the initial snapshot arrived.
        let initial = read_frame(&mut reader).await;
        assert!(matches!(
            parse_watch_frame(&initial).unwrap(),
            WatchFrame::Snapshot(_)
        ));
        // The subscriber goes away → the daemon detects it (read EOF) and ends the stream. Ending
        // via EOF returns `Ok`; a race that ends via a broken write returns `Err` — both are a
        // clean end (the property under test), never a hang, so the timeout is the real assertion.
        drop(reader);
        let ended = tokio::time::timeout(Duration::from_secs(5), watcher).await;
        let joined = ended.expect("serve_watch must end promptly when the client disconnects");
        joined.expect("the watch task must not panic").ok();
    }

    #[tokio::test]
    async fn a_watch_client_detects_a_dropped_daemon_via_socket_close() {
        use tokio::io::AsyncReadExt;
        // Issue #165 AC-2 (client side): a client can tell "disconnected" from a frozen view.
        let (tx, rx) = tokio::sync::watch::channel(versioned_status_response(&watch_snapshot(
            "work", 7, 0.30,
        )));
        let (client, server) = tokio::io::duplex(4096);
        let watcher = tokio::spawn(serve_watch(server, rx, Duration::from_secs(3600)));

        let mut reader = tokio::io::BufReader::new(client);
        // The client reads its initial snapshot — the stream is live.
        let initial = read_frame(&mut reader).await;
        assert!(matches!(
            parse_watch_frame(&initial).unwrap(),
            WatchFrame::Snapshot(_)
        ));
        // The daemon goes away: dropping the publisher ends `serve_watch`, which closes its end of
        // the socket when the task finishes.
        drop(tx);
        let _ = watcher.await.unwrap();
        // Client-side: the next read returns EOF (0 bytes) — a detectable "disconnected / stale"
        // signal rather than a frozen view.
        let mut rest = Vec::new();
        let n = reader.read_to_end(&mut rest).await.unwrap();
        assert_eq!(
            n, 0,
            "a dropped daemon is detectable client-side as socket EOF"
        );
    }

    #[tokio::test]
    async fn serve_control_routes_a_watch_command_to_a_stream() {
        use tokio::io::AsyncWriteExt;
        // A `watch` command is NOT answered with a one-shot reply — it hands the connection back
        // for the caller to stream on, keeping the long-lived stream off the idle select.
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"watch\"}\n").await.unwrap();
        match serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap()
        {
            ServeOutcome::Watch(_stream) => {}
            ServeOutcome::OneShot(_) => {
                panic!("a watch command must route to a stream, not a one-shot reply")
            }
            ServeOutcome::Swap(..) => {
                panic!("a watch command must route to a watch stream, not a swap handoff")
            }
            ServeOutcome::Capture(..) => {
                panic!("a watch command must route to a watch stream, not a capture handoff")
            }
        }
    }

    #[tokio::test]
    async fn unix_control_streams_a_watch_subscription_over_a_real_socket() {
        use tokio::io::AsyncWriteExt;
        // The production path end-to-end: a real `0600` socket, `UnixControl::serve` accepting a
        // `watch` request and SPAWNING the streaming task, and `publish` fanning a state change to
        // it — the wiring the duplex-level `serve_watch` tests above cannot reach.
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&sock).expect("bind");
        let control = UnixControl::new(listener);

        // A client opens the dedicated read-only connection and subscribes.
        let mut client = tokio::net::UnixStream::connect(&sock)
            .await
            .expect("connect");
        client.write_all(b"{\"cmd\":\"watch\"}\n").await.unwrap();
        client.flush().await.unwrap();

        // Publish the current snapshot, THEN accept: the spawned task subscribes at this value and
        // sends it as the initial frame — the same publish-then-serve order the run loop uses.
        control.publish(&watch_snapshot("work", 100, 0.20));
        let yielded = control.serve(&watch_snapshot("work", 100, 0.20)).await;
        assert!(
            matches!(yielded, ControlYield::Signal(None)),
            "a watch subscription produces no control signal"
        );

        let mut reader = tokio::io::BufReader::new(client);
        let initial = read_frame(&mut reader).await;
        match parse_watch_frame(&initial).unwrap() {
            WatchFrame::Snapshot(v) => assert_eq!(v.generated_at, 100),
            other => panic!("expected an initial snapshot, got {other:?}"),
        }
        // A subsequent state change is pushed to the live subscription.
        control.publish(&watch_snapshot("work", 200, 0.55));
        let update = read_frame(&mut reader).await;
        match parse_watch_frame(&update).unwrap() {
            WatchFrame::Snapshot(v) => assert_eq!(v.generated_at, 200),
            other => panic!("expected an update snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_watch_frame_classifies_each_frame_kind() {
        // A snapshot line round-trips to the frozen #164 envelope (the `type` tag is ignored by the
        // payload decode, so a snapshot frame carries the full contract a client already knows).
        let snap = encode_snapshot_frame(&versioned_status_response(&watch_snapshot(
            "work", 42, 0.60,
        )));
        match parse_watch_frame(&snap).unwrap() {
            WatchFrame::Snapshot(v) => {
                assert_eq!(v.generated_at, 42);
                assert_eq!(v.schema_version, STATUS_SCHEMA_VERSION);
                assert_eq!(v.status.accounts[0].session_pct, Some(60));
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
        // A heartbeat line round-trips to its freshness envelope.
        match parse_watch_frame(&encode_heartbeat_frame(42)).unwrap() {
            WatchFrame::Heartbeat {
                generated_at,
                schema_version,
            } => {
                assert_eq!(generated_at, 42);
                assert_eq!(schema_version, STATUS_SCHEMA_VERSION);
            }
            other => panic!("expected Heartbeat, got {other:?}"),
        }
        // An unknown (future) frame kind — or a line with no `type` tag — is IGNORED, not an error:
        // a forward-compatible client skips what it does not understand (the #164 additive ethos).
        assert!(matches!(
            parse_watch_frame(r#"{"type":"future","x":1}"#).unwrap(),
            WatchFrame::Unknown
        ));
        assert!(matches!(
            parse_watch_frame(r#"{"nope":1}"#).unwrap(),
            WatchFrame::Unknown
        ));
        // A malformed line is a hard error.
        assert!(parse_watch_frame("not json").is_err());
    }

    // --- Cross-language wire golden fixtures (issue #340) ----------------------------------
    //
    // The byte-frozen goldens the Swift menubar wire fixtures (`apps/menubar/Tests/Fixtures.swift`)
    // are pinned against. #322 hand-mirrored the daemon's frozen #164 wire contract into Swift
    // `Codable` types + byte-exact fixtures, but nothing caught a FUTURE daemon wire change silently
    // diverging from that hand-written mirror — ADR-0010 keeps Rust out of the Swift build, so the
    // Swift-only suite validates against its OWN now-stale fixtures and stays green. These goldens
    // close that gap: the daemon serializes its own wire encoders here (the single source of truth),
    // the byte-equality pin test below asserts the committed bytes still match (so a wire change
    // can't land without regenerating them), and CI asserts the Swift fixtures are byte-identical to
    // the same bytes (`apps/menubar/Tests/WireGoldenTests.swift`) — forcing the Swift mirror to move
    // in lockstep with any daemon wire change.
    //
    // Unlike the `src/migration.rs` golden (non-deterministic AEAD salt/nonce → a one-time
    // `#[ignore]` emitter, read-only thereafter), wire serialization is DETERMINISTIC, so the pin
    // test re-emits in-process and asserts byte-equality directly — a stronger gate than a frozen
    // read-only capture.

    /// The canonical snapshot frame the golden freezes: `encode_snapshot_frame` for
    /// `watch_snapshot("work", 42, 0.60)` — the SAME input
    /// [`parse_watch_frame_classifies_each_frame_kind`] decodes, so the golden and that test can
    /// never disagree on the representative healthy frame. Mirrored by Swift `Fixtures.snapshotBasic`.
    fn wire_golden_snapshot_frame() -> String {
        encode_snapshot_frame(&versioned_status_response(&watch_snapshot(
            "work", 42, 0.60,
        )))
    }

    /// The canonical heartbeat frame the golden freezes: `encode_heartbeat_frame(42)` — mirrored by
    /// Swift `Fixtures.heartbeatBasic`.
    fn wire_golden_heartbeat_frame() -> String {
        encode_heartbeat_frame(42)
    }

    /// A snapshot frame whose `next_swap` carries the #393 structured reason — the basic golden's
    /// `next_swap` is `null`, so the [`NextSwap::Target`] `reason` field (the whole point of #393)
    /// had NO byte-drift coverage. This freezes the `{"state":"target","to":…,"reason":{"kind":
    /// "soonest_reset","resets_at":…}}` shape, so the cross-language guard now fails if the Rust
    /// reason encoder and the Swift mirror ever diverge. Built as the basic frame with an overridden
    /// `next_swap`, so it differs from `wire_golden_snapshot_frame` in exactly that one field.
    /// Mirrored by Swift `Fixtures.snapshotNextSwap`.
    fn wire_golden_snapshot_next_swap_frame() -> String {
        let mut snapshot = watch_snapshot("work", 42, 0.60);
        snapshot.next_swap = Some(NextSwap::Target {
            to: "spare".to_owned(),
            reason: Some(NextSwapReason::SoonestReset {
                resets_at: 1_893_800_000,
            }),
        });
        encode_snapshot_frame(&versioned_status_response(&snapshot))
    }

    /// One-time emitter for the committed wire goldens. `#[ignore]` — NOT part of the suite; it
    /// WRITES the bytes the pin test and the Swift fixtures consume. Run it ONLY alongside a
    /// deliberate wire-contract change:
    ///   `cargo test -- --ignored emit_wire_golden_fixtures`
    /// then update the Swift mirror (`apps/menubar/Sources/WireModel.swift`) and fixtures
    /// (`apps/menubar/Tests/Fixtures.swift`) so the cross-language byte-equality holds again.
    #[test]
    #[ignore = "one-time wire-golden emitter — run ONLY alongside a deliberate wire-contract change"]
    fn emit_wire_golden_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build/fixtures");
        std::fs::create_dir_all(&dir).expect("create build/fixtures");
        std::fs::write(
            dir.join("wire-snapshot-basic.json"),
            wire_golden_snapshot_frame(),
        )
        .expect("write wire-snapshot golden");
        std::fs::write(
            dir.join("wire-heartbeat-basic.json"),
            wire_golden_heartbeat_frame(),
        )
        .expect("write wire-heartbeat golden");
        std::fs::write(
            dir.join("wire-snapshot-next-swap.json"),
            wire_golden_snapshot_next_swap_frame(),
        )
        .expect("write wire-snapshot-next-swap golden");
    }

    /// The committed snapshot-frame golden — the exact bytes Swift `Fixtures.snapshotBasic` is
    /// pinned to. `include_str!` makes the file a compile-time input, so it must exist before this
    /// module compiles (emit once via [`emit_wire_golden_fixtures`]).
    const WIRE_SNAPSHOT_GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/wire-snapshot-basic.json"
    ));

    /// The committed heartbeat-frame golden — the exact bytes Swift `Fixtures.heartbeatBasic` is
    /// pinned to.
    const WIRE_HEARTBEAT_GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/wire-heartbeat-basic.json"
    ));

    /// The committed next-swap-reason snapshot golden (issue #393) — the exact bytes Swift
    /// `Fixtures.snapshotNextSwap` is pinned to.
    const WIRE_SNAPSHOT_NEXT_SWAP_GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/wire-snapshot-next-swap.json"
    ));

    #[test]
    fn the_committed_wire_goldens_still_match_the_daemon_encoders() {
        // The load-bearing gate. Wire serialization is deterministic, so re-emitting in-process and
        // comparing to the COMMITTED bytes catches any daemon wire-type change — a renamed / added /
        // reordered / re-typed field, a changed enum tag, a `STATUS_SCHEMA_VERSION` bump — that
        // shifts the bytes: the committed golden goes stale and this fails, forcing a regenerate
        // (`emit_wire_golden_fixtures`) that in turn breaks the Swift byte-equality check until the
        // hand-written Swift mirror is updated too. That is the cross-language lockstep #340 exists
        // to enforce — no same-language test can witness a divergence between the Rust wire types
        // and the independently-maintained Swift mirror.
        assert_eq!(
            wire_golden_snapshot_frame(),
            WIRE_SNAPSHOT_GOLDEN,
            "the committed wire-snapshot golden drifted from encode_snapshot_frame — re-run \
             `cargo test -- --ignored emit_wire_golden_fixtures`, then update the Swift mirror \
             (apps/menubar) so its fixtures stay byte-identical"
        );
        assert_eq!(
            wire_golden_heartbeat_frame(),
            WIRE_HEARTBEAT_GOLDEN,
            "the committed wire-heartbeat golden drifted from encode_heartbeat_frame — re-run \
             `cargo test -- --ignored emit_wire_golden_fixtures`, then update the Swift mirror \
             (apps/menubar) so its fixtures stay byte-identical"
        );
        assert_eq!(
            wire_golden_snapshot_next_swap_frame(),
            WIRE_SNAPSHOT_NEXT_SWAP_GOLDEN,
            "the committed wire-snapshot-next-swap golden drifted from the next_swap reason encoder \
             (issue #393) — re-run `cargo test -- --ignored emit_wire_golden_fixtures`, then update \
             the Swift mirror (apps/menubar) so its fixtures stay byte-identical"
        );
    }

    #[test]
    fn control_reply_rejects_malformed_json() {
        let (reply, signal) = control_reply("not json", &StatusSnapshot::default(), true);
        assert!(reply.contains("malformed"));
        assert!(signal.is_none());
    }

    #[test]
    fn manual_swapped_is_honored_only_for_an_authenticated_peer() {
        // Issue #64: `manual-swapped` is state-affecting, so an UNauthenticated peer
        // gets an error and produces NO signal — a stranger can never arm the
        // daemon's cooldown. The same-user peer gets an ack and the adopt signal.
        let snap = StatusSnapshot::default();
        let (denied, no_signal) = control_reply(r#"{"cmd":"manual-swapped"}"#, &snap, false);
        assert!(denied.contains("unauthorized"), "got {denied:?}");
        assert!(
            no_signal.is_none(),
            "an unauthenticated peer must not arm cooldown"
        );

        let (ack, signal) = control_reply(r#"{"cmd":"manual-swapped"}"#, &snap, true);
        assert!(ack.contains("\"ok\":true"), "got {ack:?}");
        assert_eq!(signal, Some(ControlSignal::ManualSwapped));
    }

    #[tokio::test]
    async fn peer_is_same_user_authenticates_a_same_process_peer() {
        // Issue #64: the manual-hold receive path authenticates the peer's uid via
        // `getpeereid(2)` before honoring a state-affecting command. A socket pair
        // made in THIS process has its peer on our own uid, so the real (unsafe) FFI
        // path must report it authenticated — exercising the `getpeereid`/`getuid`
        // computation that the boolean-gated `control_reply` tests take as a given.
        let (ours, _peer) = tokio::net::UnixStream::pair().expect("socketpair");
        assert!(
            peer_is_same_user(&ours),
            "a same-process socket peer is the same local user"
        );
    }

    #[test]
    fn is_same_user_denies_foreign_and_unreadable_credentials() {
        // Issue #196: the pure peer-auth decision, exercised on all three branches so a
        // silent auth-inverting refactor cannot ship green. Fixed uids (no syscall) —
        // the real `getpeereid` path is covered by the socket tests around this one.
        let owner: libc::uid_t = 1_000;
        // Same user → authenticated.
        assert!(
            is_same_user(Some(owner), owner),
            "the socket owner is the same local user"
        );
        // A foreign (non-owner) uid → rejected. Inverting `==`→`!=` would ALLOW this.
        assert!(
            !is_same_user(Some(owner + 1), owner),
            "a foreign uid is not the same local user"
        );
        // Unreadable credential (a `getpeereid` error) → fail closed. Both a fail-OPEN
        // regression (treating `None` as allow) and inverting the comparison ALLOW this.
        assert!(
            !is_same_user(None, owner),
            "an unreadable peer credential must fail closed"
        );
    }

    #[test]
    fn peer_euid_fails_closed_when_getpeereid_errors() {
        use std::os::unix::io::AsRawFd;
        // Issue #196: the fail-closed ERROR branch, driven for real. `getpeereid` on a
        // non-socket fd returns `ENOTSOCK` — the syscall itself errors — so `peer_euid`
        // must yield `None` and the decision must then DENY. A fail-open regression that
        // surfaced a default uid on error (e.g. the pre-`Option` `euid` left at 0) would
        // return `Some(_)` here. A regular file's fd is a real, valid fd that is simply
        // not a socket, so this exercises the `rc != 0` branch portably.
        let file = tempfile::tempfile().expect("tempfile");
        let euid = peer_euid(file.as_raw_fd());
        assert_eq!(
            euid, None,
            "getpeereid on a non-socket fd must fail (no credential)"
        );
        // SAFETY: `getuid` cannot fail and has no preconditions.
        assert!(
            !is_same_user(euid, unsafe { libc::getuid() }),
            "a getpeereid error must deny — fail closed"
        );
    }

    #[tokio::test]
    async fn serve_control_rejects_a_foreign_uid_peer() {
        use std::os::unix::io::AsRawFd;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Issue #196: drive a REAL socket peer through the serve path and assert the
        // state-affecting `manual-swapped` is REJECTED when the peer is not the socket
        // owner. A genuinely foreign-uid peer cannot be spawned without root, so "foreign"
        // is realized faithfully: the peer's uid is read for real via `getpeereid`, then
        // compared against an owner uid deliberately NOT it. The real credential read and
        // the real serve exchange are exercised; only the owner identity is synthesized.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");

        // Real getpeereid read of the connected peer — its true euid (our own).
        let peer_uid =
            peer_euid(server.as_raw_fd()).expect("a connected peer has a readable credential");
        // A socket owner that is NOT the peer → the peer is foreign to this socket.
        let authenticated = is_same_user(Some(peer_uid), peer_uid.wrapping_add(1));
        assert!(
            !authenticated,
            "a foreign uid must not authenticate (guards against an inverted decision)"
        );

        client
            .write_all(b"{\"cmd\":\"manual-swapped\"}\n")
            .await
            .expect("write request");
        let signal = serve_control(server, &StatusSnapshot::default(), authenticated)
            .await
            .expect("serve")
            .one_shot();
        assert!(
            signal.is_none(),
            "a foreign-uid peer must NOT arm the daemon (no control signal)"
        );

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.expect("read reply");
        assert!(
            reply.contains("unauthorized"),
            "a foreign-uid manual-swapped must be rejected: {reply:?}"
        );
    }

    // --- socket `swap` command (issue #167) --------------------------------

    #[tokio::test]
    async fn serve_control_hands_back_an_authenticated_swap_command() {
        use tokio::io::AsyncWriteExt;
        // An AUTHENTICATED, well-formed `swap` is NOT answered inline: like `watch`, it hands the
        // OPEN connection back (with the parsed target + force) so the run loop performs the swap
        // against `&mut Daemon` and writes the redacted ack from the REAL outcome — an outcome this
        // pure serve cannot know. No reply is written here.
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"cmd\":\"swap\",\"target\":\"spare\",\"force\":true}\n")
            .await
            .unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .swap();
        assert_eq!(
            command,
            SwapCommand {
                target: "spare".to_owned(),
                force: true,
            }
        );
    }

    #[tokio::test]
    async fn serve_control_defaults_an_omitted_swap_force_flag_to_false() {
        use tokio::io::AsyncWriteExt;
        // `force` is `#[serde(default)]`: a `swap` that OMITS it is a NON-force request (the common
        // `use <target>` case), never a parse error — so a plain policy-gated swap routes cleanly.
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"cmd\":\"swap\",\"target\":\"spare\"}\n")
            .await
            .unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .swap();
        assert_eq!(command.target, "spare");
        assert!(!command.force, "an omitted force flag defaults to false");
    }

    #[tokio::test]
    async fn serve_control_rejects_an_unauthenticated_swap_command() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // AC (peer-credential authN): a `swap` is STATE-AFFECTING, so a non-owner peer is rejected
        // BEFORE any handoff — the swap never reaches the run loop (`one_shot()` proves there is NO
        // `Swap` handoff), and the peer gets `unauthorized` and learns nothing past the rejection.
        // This is the socket-layer half of the guard; the real `getpeereid` euid comparison that
        // computes the bool is proven by `serve_control_rejects_a_foreign_uid_peer` / `is_same_user`.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"swap\",\"target\":\"spare\",\"force\":true}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "an unauthenticated swap must not hand off to the run loop",
        );
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(
            reply.contains("unauthorized"),
            "an unauthenticated swap is refused: {reply:?}",
        );
    }

    #[tokio::test]
    async fn serve_control_rejects_an_authenticated_swap_with_no_target() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Authenticated but malformed: a `swap` carrying no `target` has nothing to resolve, so it is
        // refused as `malformed request` (bounded / malformed-safe like an unparseable line) with NO
        // handoff. Checked only AFTER auth — the authenticated-but-malformed branch.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"swap\",\"force\":true}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "a targetless swap must not hand off to the run loop",
        );
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("malformed"), "got {reply:?}");
    }

    // --- socket `capture` command (issue #359) ------------------------------

    #[tokio::test]
    async fn serve_control_hands_back_an_authenticated_capture_command() {
        use tokio::io::AsyncWriteExt;
        // An AUTHENTICATED `capture` is NOT answered inline: like `swap`, it hands the OPEN
        // connection back (with the parsed label) so the run loop performs the capture against
        // `&mut Daemon` and writes the redacted ack from the REAL outcome. No reply is written here.
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"cmd\":\"capture\",\"label\":\"work\"}\n")
            .await
            .unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .capture();
        assert_eq!(
            command,
            CaptureCommand {
                label: Some("work".to_owned()),
            }
        );
    }

    #[tokio::test]
    async fn serve_control_defaults_an_omitted_capture_label_to_none() {
        use tokio::io::AsyncWriteExt;
        // `label` is `#[serde(default)]` and OPTIONAL: a `capture` that omits it is well-formed (the
        // daemon auto-derives the label from the account uuid, never the email — #15/#134), never a
        // parse error and never a `malformed request` — so, unlike a targetless `swap`, it hands off.
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"capture\"}\n").await.unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .capture();
        assert_eq!(command, CaptureCommand { label: None });
    }

    #[tokio::test]
    async fn serve_control_rejects_an_unauthenticated_capture_command() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // AC (peer-credential authN): a `capture` is STATE-AFFECTING, so a non-owner peer is rejected
        // BEFORE any handoff — the capture never reaches the run loop (`one_shot()` proves there is NO
        // `Capture` handoff, so ZERO credential work happens), and the peer gets `unauthorized` and
        // learns nothing past the rejection. The socket-layer half of the guard, exactly like `swap`.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"capture\",\"label\":\"work\"}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "an unauthenticated capture must not hand off to the run loop",
        );
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(
            reply.contains("unauthorized"),
            "an unauthenticated capture is refused: {reply:?}",
        );
    }

    // --- swap_command_verdict (pure re-validation, issue #167) --------------

    #[test]
    fn swap_command_verdict_accepts_a_viable_target_without_force() {
        // The happy path: an active account exists and the target is viable (not quarantined, weekly
        // headroom, no cooldown) → proceed with the swap, no force needed.
        assert!(matches!(
            swap_command_verdict(1, Some(0), false, false, false, false),
            SwapVerdict::Swap
        ));
    }

    #[test]
    fn swap_command_verdict_treats_a_non_force_already_active_target_as_a_noop() {
        // Non-force + target already active → a no-op success (nothing to write), mirroring the
        // standalone `use` no-op (the caller fills the `to` label).
        assert!(matches!(
            swap_command_verdict(0, Some(0), false, false, false, false),
            SwapVerdict::AlreadyActive
        ));
    }

    #[test]
    fn swap_command_verdict_force_onto_the_active_account_proceeds_as_a_self_swap() {
        // A `force` request onto the ALREADY-active account is NOT the no-op — it proceeds as a
        // self-swap (the `use --force <active>` display-repair path), so force is honored end to end.
        assert!(matches!(
            swap_command_verdict(0, Some(0), false, false, false, true),
            SwapVerdict::Swap
        ));
    }

    #[test]
    fn swap_command_verdict_rejects_with_no_active_account_even_under_force() {
        // A normal re-stash swap needs an OUTGOING (active) account. With none, the daemon cannot run
        // it, and `force` cannot MANUFACTURE one (adopt-target #212 is the decoupled standalone path).
        // BOTH the non-force and the force request reject — force is policy-only, never a
        // precondition bypass.
        assert!(matches!(
            swap_command_verdict(1, None, false, false, false, false),
            SwapVerdict::Reject(SwapRejection::NoActiveAccount)
        ));
        assert!(matches!(
            swap_command_verdict(1, None, false, false, false, true),
            SwapVerdict::Reject(SwapRejection::NoActiveAccount)
        ));
    }

    #[test]
    fn swap_command_verdict_rejects_each_non_viable_target_without_force() {
        // AC (daemon re-validates the target itself): a quarantined target, a weekly-exhausted
        // target, and an in-cooldown swap each reject WITHOUT force, with the matching redacted
        // reason. These facts are computed by the caller from the daemon's OWN state — never a
        // client "greyed out" hint.
        assert!(matches!(
            swap_command_verdict(1, Some(0), true, false, false, false),
            SwapVerdict::Reject(SwapRejection::Quarantined)
        ));
        assert!(matches!(
            swap_command_verdict(1, Some(0), false, true, false, false),
            SwapVerdict::Reject(SwapRejection::WeeklyExhausted)
        ));
        assert!(matches!(
            swap_command_verdict(1, Some(0), false, false, true, false),
            SwapVerdict::Reject(SwapRejection::Cooldown)
        ));
    }

    #[test]
    fn swap_command_verdict_force_bypasses_every_policy_gate_at_once() {
        // `force` is POLICY-only: it bypasses ALL THREE viability/cooldown gates together
        // (quarantined AND weekly-exhausted AND in-cooldown) → proceed. It never reaches the SAFETY
        // aborts (the locked keychain / swap lock), which live in the engine BELOW this verdict — so
        // this proves force relaxes POLICY, not that it can bypass a safety abort (that is
        // `classify_swap_failure` + the locked-keychain integration test).
        assert!(matches!(
            swap_command_verdict(1, Some(0), true, true, true, true),
            SwapVerdict::Swap
        ));
    }

    // --- classify_swap_failure (engine error → redacted reason, issue #167) ---

    #[test]
    fn classify_swap_failure_maps_the_two_force_proof_safety_aborts_to_their_own_codes() {
        // AC (force cannot bypass a SAFETY abort): both surface as their OWN redacted reason (not the
        // opaque `Failed`), making "force cannot bypass the locked-keychain abort / the swap lock"
        // observable in the ack. A locked keychain and a fail-closed single-writer lock each map
        // through distinctly.
        assert_eq!(
            classify_swap_failure(&Error::KeychainLocked { op: "read" }),
            SwapRejection::KeychainLocked
        );
        assert_eq!(
            classify_swap_failure(&Error::SwapLockBusy),
            SwapRejection::SwapLockBusy
        );
    }

    #[test]
    fn classify_swap_failure_routes_a_vanished_canonical_to_no_active_and_else_to_failed() {
        // A canonical scrubbed since the daemon last resolved active → the recovery signal (adopt-
        // target is the standalone path); every other engine error is the opaque `Failed` (#15: no
        // internal detail on the wire). The #211 wrong-identity re-stash guard is one such `Failed`.
        assert_eq!(
            classify_swap_failure(&Error::CredentialNotFound),
            SwapRejection::NoActiveAccount
        );
        assert_eq!(
            classify_swap_failure(&Error::SwapWrongIdentityRestash),
            SwapRejection::Failed
        );
    }

    // --- perform_socket_swap (daemon swap-apply, issue #167) ----------------

    #[tokio::test]
    async fn perform_socket_swap_reroutes_the_canonical_and_arms_the_cooldown() {
        // AC (unify `use` onto the daemon; no torn write): a well-formed `swap` runs the daemon's OWN
        // single-writer swap — the SAME engine the auto-swaps use — rerouting the canonical OFF the
        // active account ONTO the target, advancing in-memory active, arming the post-swap cooldown,
        // and emitting the durable `Event::Swap` (reason Manual — operator-driven, not
        // session-triggered). The ack carries the two non-secret labels and NOTHING else.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Both under the session trigger, so warm-up HOLDS (no auto-swap) and simply resolves active
        // = work(0) with viable last-known readings — the realistic pre-swap state.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 100);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        warmed_tick(&mut daemon).await;
        assert_eq!(
            daemon.state.active,
            Some(0),
            "warm-up resolved active = work"
        );

        let (ack, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: false,
            })
            .await;

        // The ack names the two non-secret labels…
        assert_eq!(
            ack,
            SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }
        );
        // …and its SERIALIZED bytes leak neither the credential (named `*-token`) nor an email (#15).
        let wire = serde_json::to_string(&ack).unwrap();
        assert!(!wire.contains('@'), "the ack leaks no email: {wire}");
        assert!(
            !wire.to_lowercase().contains("token"),
            "the ack leaks no credential: {wire}",
        );
        // The canonical now holds B's token and the display shows B — a REAL, complete write.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
        // In-memory active advanced and the cooldown is armed, so the next auto-tick holds.
        assert_eq!(daemon.state.active, Some(1));
        assert!(
            daemon.state.last_swap.is_some(),
            "a completed swap arms the post-swap cooldown",
        );
        // The durable event is the MANUAL (operator-driven) swap, session_pct 0 (not session-driven).
        assert_eq!(
            event,
            Some(Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason: SwapReason::Manual,
                session_pct: 0,
            })
        );
    }

    #[tokio::test]
    async fn perform_socket_swap_revalidates_a_weekly_exhausted_target_then_force_overrides_it() {
        // AC (daemon's own re-validation + force is policy-only): the daemon computes weekly
        // exhaustion from its OWN last-known reading (never a client hint). WITHOUT force the target
        // is refused with ZERO writes; WITH force the SAME target swaps — the operator's explicit
        // policy override, honored end to end (a REAL write lands, reason Forced).
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // spare's weekly (0.99) is at/above the 0.98 base (`tunables` WEEKLY_TRIGGER=98) → exhausted;
        // work stays active and viable so warm-up holds.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.99);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        warmed_tick(&mut daemon).await;
        assert_eq!(daemon.state.active, Some(0));

        // WITHOUT force: the daemon re-validates and refuses — ZERO writes, no event.
        let (rejected, no_event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: false,
            })
            .await;
        assert_eq!(
            rejected,
            SwapAck::Rejected {
                reason: SwapRejection::WeeklyExhausted,
            }
        );
        assert!(no_event.is_none(), "a refused swap emits no event");
        assert!(
            daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token")),
            "the refused swap wrote nothing",
        );
        assert_eq!(daemon.state.active, Some(0));

        // WITH force: the SAME non-viable target now swaps (policy override).
        let (accepted, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: true,
            })
            .await;
        assert_eq!(
            accepted,
            SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }
        );
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(daemon.state.active, Some(1));
        // A forced swap records the FORCED reason (distinct from Manual), still session_pct 0.
        assert_eq!(
            event,
            Some(Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason: SwapReason::Forced,
                session_pct: 0,
            })
        );
    }

    #[tokio::test]
    async fn perform_socket_swap_force_cannot_bypass_a_locked_keychain() {
        // AC (force cannot bypass the locked-keychain abort): `force` is POLICY-only. A forced swap
        // onto a VIABLE target is still REFUSED when the keychain is locked (locked ≠ gone — retry
        // when unlocked), with ZERO writes: canonical untouched, active unchanged, no event, no
        // cooldown. The abort lives in the swap ENGINE (its read-everything-before-mutating step-1
        // read), below the force-bypassable policy verdict, so no verdict can reach past it.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        warmed_tick(&mut daemon).await;
        assert_eq!(daemon.state.active, Some(0));

        // Lock the keychain, THEN force-swap: the engine's step-1 read aborts before any mutation.
        daemon.store.set_locked(true);
        let (ack, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: true,
            })
            .await;

        assert_eq!(
            ack,
            SwapAck::Rejected {
                reason: SwapRejection::KeychainLocked,
            }
        );
        assert!(event.is_none(), "a refused swap emits no event");
        // ZERO writes: once unlocked the canonical still holds A's token, the display still shows A,
        // in-memory active never advanced, and no cooldown was armed. `force` forged no torn write.
        daemon.store.set_locked(false);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
        assert_eq!(daemon.state.active, Some(0));
        assert!(
            daemon.state.last_swap.is_none(),
            "a refused swap arms no cooldown",
        );
    }

    #[tokio::test]
    async fn perform_socket_swap_rejects_an_unknown_target_and_writes_nothing() {
        // The daemon resolves the target against its OWN roster and NEVER guesses (#17): a handle
        // matching no account is `UnknownTarget` with ZERO writes and no event — even under force
        // (there is nothing to resolve, so resolution failure is not force-bypassable).
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        warmed_tick(&mut daemon).await;

        let (ack, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "ghost".to_owned(),
                force: true,
            })
            .await;
        assert_eq!(
            ack,
            SwapAck::Rejected {
                reason: SwapRejection::UnknownTarget,
            }
        );
        assert!(event.is_none());
        assert!(
            daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token")),
            "an unknown target wrote nothing",
        );
        assert_eq!(daemon.state.active, Some(0));
    }

    #[tokio::test]
    async fn perform_socket_swap_reports_an_already_active_target_as_a_noop() {
        // Non-force swap onto the ALREADY-active account: a no-op SUCCESS (nothing written), the
        // `AlreadyActive` ack — the daemon-routed mirror of the standalone `use` no-op.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        warmed_tick(&mut daemon).await;
        assert_eq!(daemon.state.active, Some(0));

        let (ack, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "work".to_owned(),
                force: false,
            })
            .await;
        assert_eq!(
            ack,
            SwapAck::AlreadyActive {
                to: "work".to_owned(),
            }
        );
        assert!(event.is_none());
        assert!(
            daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token")),
            "an already-active no-op writes nothing",
        );
        assert!(daemon.state.last_swap.is_none(), "a no-op arms no cooldown");
    }

    // --- classify_capture_failure (engine error → redacted reason, issue #359) ---

    #[test]
    fn classify_capture_failure_maps_each_engine_error_to_its_redacted_reason() {
        // The two SAFETY aborts surface as their OWN redacted codes (not the opaque `Failed`): a
        // LOCKED keychain (the token read aborts mid-capture; locked ≠ gone) and a contended
        // single-writer swap lock (fail-closed BEFORE any read). A missing active account — an
        // absent / no-`oauthAccount` `~/.claude.json`, or a vanished canonical credential — routes
        // to `NoActiveAccount`; every other engine error is the opaque `Failed` (#15: no internal
        // detail on the wire). The capture mirror of `classify_swap_failure`.
        assert_eq!(
            classify_capture_failure(&Error::KeychainLocked { op: "read" }),
            CaptureRejection::KeychainLocked
        );
        assert_eq!(
            classify_capture_failure(&Error::SwapLockBusy),
            CaptureRejection::SwapLockBusy
        );
        assert_eq!(
            classify_capture_failure(&Error::ClaudeStateNotFound {
                path: PathBuf::from("/nope/.claude.json"),
            }),
            CaptureRejection::NoActiveAccount
        );
        assert_eq!(
            classify_capture_failure(&Error::OauthAccountMissing),
            CaptureRejection::NoActiveAccount
        );
        assert_eq!(
            classify_capture_failure(&Error::CredentialNotFound),
            CaptureRejection::NoActiveAccount
        );
        assert_eq!(
            classify_capture_failure(&Error::SwapWrongIdentityRestash),
            CaptureRejection::Failed
        );
    }

    // --- perform_socket_capture (daemon capture-apply, issue #359) -----------

    #[tokio::test]
    async fn perform_socket_capture_captures_the_active_account_and_reconciles_the_roster() {
        // AC (authenticated peer → redacted success ack; capture is the daemon's OWN work): a
        // well-formed `capture` reads the active identity + token through the #357 `capture_locked`
        // primitive, stashes BOTH halves, appends the new roster row, persists `config.toml`, and
        // reconciles the in-memory rotation to it — emitting one redacted `Event::Capture`. The ack
        // carries the operator LABEL + running count and NOTHING secret. Canonical-READ-ONLY: the
        // keychain token and `~/.claude.json` are never rewritten (#359 — capture only writes a
        // per-account stash + a roster row).
        let roster = vec![account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-B", b"B-token", "u-B")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-B", "spare")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        )
        .with_config_path(config_path.clone());

        // Label GIVEN → names the new account; the account is not yet rostered, so this captures.
        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand {
                label: Some("work".to_owned()),
            })
            .await;

        // The ack names the operator LABEL + the running count (2 in rotation now)…
        assert_eq!(
            ack,
            CaptureAck::Captured {
                label: "work".to_owned(),
                count: 2,
            }
        );
        // …and its SERIALIZED bytes leak neither the credential (named `*-token`) nor an email (#15).
        let wire = serde_json::to_string(&ack).unwrap();
        assert!(!wire.contains('@'), "the ack leaks no email: {wire}");
        assert!(
            !wire.to_lowercase().contains("token"),
            "the ack leaks no credential: {wire}",
        );
        // The durable audit line: the resolved roster LABEL handle + the `captured` outcome token.
        assert_eq!(
            event,
            Some(Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::Captured,
            })
        );
        // The in-memory rotation reconciled to the freshly-written roster: u-A joined u-B.
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-A"]);
        // The on-disk `config.toml` grew the SAME row — persisted, so a restart keeps it.
        let on_disk = Config::load_path(&config_path).unwrap();
        assert_eq!(on_disk.roster.len(), 2);
        assert_eq!(on_disk.roster[1].account_uuid, "u-A");
        assert_eq!(on_disk.roster[1].label, "work");
        // Both credential halves are stashed together under u-A's uuid-derived service.
        let stashed = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"A-token");
        assert_eq!(stashed.oauth_account.account_uuid(), "u-A");
        // Canonical-READ-ONLY: the keychain still holds A's token and `~/.claude.json` still shows
        // u-A — capture rewrote NEITHER (it is not a swap).
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    #[tokio::test]
    async fn perform_socket_capture_refreshes_an_already_rostered_account_without_a_duplicate_row()
    {
        // AC (already-rostered active account → idempotent refresh, NOT a duplicate row): capturing
        // the active account when it is ALREADY rostered re-points its stash to the current token
        // and updates its row IN PLACE — the count is unchanged and no second row appears. An
        // omitted label keeps the operator's existing name (never clobbered by an auto-derived
        // uuid). The ack is `Refreshed`, the event outcome `refreshed`.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-token-v2").await; // the canonical rotated since the last stash
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token-v1", "u-A")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
        .with_config_path(config_path.clone());

        // Label OMITTED → the existing "work" is kept (an auto-derived uuid never clobbers it).
        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand { label: None })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Refreshed {
                label: "work".to_owned(),
                count: 1,
            }
        );
        assert_eq!(
            event,
            Some(Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::Refreshed,
            })
        );
        // NO duplicate row — in-memory AND on-disk stay a single u-A account.
        assert_eq!(roster_uuids(&daemon), vec!["u-A"]);
        assert_eq!(Config::load_path(&config_path).unwrap().roster.len(), 1);
        // The stash re-pointed to the CURRENT token under the SAME single service (no new entry).
        assert_eq!(daemon.stash.len(), 1);
        let stashed = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"A-token-v2");
    }

    #[tokio::test]
    async fn perform_socket_capture_refuses_when_no_active_account_and_writes_nothing() {
        // AC (no active account → redacted refusal, ZERO writes): with no readable `~/.claude.json`
        // identity there is nothing to capture. The identity read fails FIRST (before the token),
        // so the capture is a true no-op: no stash, no roster row, no `config.toml` change, no
        // in-memory reconcile. The ack is the redacted `NoActiveAccount`; the event still names the
        // operator's label HINT (the only handle a pre-stash failure has) + the `no_active_account`
        // outcome.
        let roster = vec![account("u-B", "spare")];
        let store = store_holding(b"A-token").await; // a token exists, but the identity read aborts first
        let stash = stash_with(&[("Sessiometer/u-B", b"B-token", "u-B")]).await;
        let absent_dir = tempfile::tempdir().unwrap();
        let absent_json = absent_dir.path().join("absent.json"); // never created
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-B", "spare")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            absent_json,
            &tun,
        )
        .with_config_path(config_path.clone());

        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand {
                label: Some("work".to_owned()),
            })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Rejected {
                reason: CaptureRejection::NoActiveAccount,
            }
        );
        assert_eq!(
            event,
            Some(Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::NoActiveAccount,
            })
        );
        // ZERO writes: the roster is untouched in memory AND on disk, and nothing new was stashed.
        assert_eq!(roster_uuids(&daemon), vec!["u-B"]);
        assert_eq!(Config::load_path(&config_path).unwrap().roster.len(), 1);
        assert_eq!(daemon.stash.len(), 1);
        assert!(!daemon.stash.contains("Sessiometer/u-A"));
    }

    #[tokio::test]
    async fn perform_socket_capture_refuses_a_locked_keychain_and_writes_nothing() {
        // AC (locked keychain → redacted SAFETY abort, ZERO writes): the identity reads fine but the
        // active-token read hits a LOCKED keychain (locked ≠ gone — retry when unlocked). The
        // capture aborts with the redacted `KeychainLocked` reason and writes NOTHING — no stash, no
        // roster row, no reconcile. An omitted label leaves the event handle `None` (no identity was
        // ever paired to a label).
        let roster = vec![account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-B", b"B-token", "u-B")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-B", "spare")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        )
        .with_config_path(config_path.clone());

        // Lock the keychain, THEN capture: the token read aborts after the (successful) identity read.
        daemon.store.set_locked(true);
        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand { label: None })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Rejected {
                reason: CaptureRejection::KeychainLocked,
            }
        );
        assert_eq!(
            event,
            Some(Event::Capture {
                account: None,
                outcome: CaptureEventOutcome::KeychainLocked,
            })
        );
        // ZERO writes: the roster is untouched in memory AND on disk, and nothing new was stashed.
        assert_eq!(roster_uuids(&daemon), vec!["u-B"]);
        assert_eq!(Config::load_path(&config_path).unwrap().roster.len(), 1);
        assert_eq!(daemon.stash.len(), 1);
        assert!(!daemon.stash.contains("Sessiometer/u-A"));
        // Once unlocked the canonical still holds A's token — the abort forged no write.
        daemon.store.set_locked(false);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    // --- next_swap candidate (issue #88) + swap report ---------------------

    #[test]
    fn status_response_projects_the_next_swap_candidate_and_drops_last_swap() {
        // A viable candidate projects as a label plus the daemon's #393 selection reason (#88),
        // never a token/email (#15).
        let target = StatusSnapshot {
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::SoonestReset {
                    resets_at: 1_782_800_000,
                }),
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&status_response(&target)).unwrap();
        assert!(json.contains("\"next_swap\":"), "got {json}");
        assert!(json.contains("\"state\":\"target\""), "got {json}");
        assert!(json.contains("\"to\":\"spare\""), "got {json}");
        // #393: the structured reason nests under `reason`, internally tagged on `kind`, carrying
        // the `min_by_key` epoch `pick_target` used to formerly compute and discard.
        assert!(
            json.contains("\"reason\":{\"kind\":\"soonest_reset\",\"resets_at\":1782800000}"),
            "got {json}"
        );
        // #15: a label only — never an email or token sigil.
        assert!(!json.contains('@'));
        assert!(!json.to_lowercase().contains("token"));

        // The two no-candidate verdicts project as bare reasons (no label at all), so
        // the client can tell `no viable target` from `awaiting usage data`.
        let no_target = StatusSnapshot {
            next_swap: Some(NextSwap::NoViableTarget),
            ..Default::default()
        };
        assert!(serde_json::to_string(&status_response(&no_target))
            .unwrap()
            .contains("\"next_swap\":{\"state\":\"no_viable_target\"}"));
        let awaiting = StatusSnapshot {
            next_swap: Some(NextSwap::AwaitingData),
            ..Default::default()
        };
        assert!(serde_json::to_string(&status_response(&awaiting))
            .unwrap()
            .contains("\"next_swap\":{\"state\":\"awaiting_data\"}"));

        // No anchor → null candidate; and the dropped `last_swap` field never appears.
        let none = StatusSnapshot {
            next_swap: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&status_response(&none)).unwrap();
        assert!(json.contains("\"next_swap\":null"), "got {json}");
        assert!(!json.contains("last_swap"), "got {json}");
    }

    // --- the frozen versioned wire contract (issue #164) -----------------------

    #[test]
    fn versioned_status_response_stamps_the_current_version_and_generated_at() {
        let snapshot = StatusSnapshot {
            generated_at: 1_782_777_600,
            ..Default::default()
        };
        let versioned = versioned_status_response(&snapshot);
        assert_eq!(versioned.schema_version, STATUS_SCHEMA_VERSION);
        assert_eq!(versioned.generated_at, 1_782_777_600);
    }

    #[test]
    fn the_status_wire_is_flat_and_carries_the_frozen_meta() {
        // AC-1: the snapshot carries `schema_version` + `generated_at`, and the payload stays
        // FLAT at the top level (the settled #137–#143 shape, only prefixed with the two meta
        // fields — so existing internal readers that decode a bare `StatusResponse` still work).
        let snapshot = StatusSnapshot {
            generated_at: 1_782_777_600,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&versioned_status_response(&snapshot)).unwrap();
        assert!(
            json.contains(r#""schema_version":{"major":1,"minor":2}"#),
            "got {json}"
        );
        assert!(json.contains(r#""generated_at":1782777600"#), "got {json}");
        // Flat: the payload's `accounts` sits at the top level, not nested under a wrapper key.
        assert!(json.contains(r#""accounts":[{"#), "got {json}");
    }

    #[test]
    fn the_control_status_reply_is_the_versioned_envelope() {
        // The end-to-end wire: a `status` control request replies with the frozen envelope a
        // read-only client decodes (issue #164) — version + freshness stamp + payload, no signal.
        let snapshot = StatusSnapshot {
            generated_at: 42,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (reply, signal) = control_reply(r#"{"cmd":"status"}"#, &snapshot, false);
        assert!(signal.is_none(), "status is a pure read");
        let parsed: VersionedStatus = serde_json::from_str(reply.trim_end()).unwrap();
        assert_eq!(parsed.schema_version, STATUS_SCHEMA_VERSION);
        assert_eq!(parsed.generated_at, 42);
        assert_eq!(parsed.status.accounts[0].label, "work");
    }

    #[test]
    fn a_bare_status_response_decodes_from_the_versioned_wire() {
        // The flatten envelope keeps the wire FLAT (issue #164), so the internal readers that
        // decode a BARE `StatusResponse` (`poke::daemon_status_best_effort`,
        // `use_account::query_status`) are UNAFFECTED by the two meta fields — serde ignores the
        // extra top-level `schema_version` / `generated_at` keys they do not name. This is the
        // backward-compat guarantee the flatten design rests on.
        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            generated_at: 1_782_777_600,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                ..Default::default()
            }],
            next_swap: Some(NextSwap::NoViableTarget),
            refresh_enabled: false,
        };
        let wire = serde_json::to_string(&versioned_status_response(&snapshot)).unwrap();
        let bare: StatusResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(bare.accounts.len(), 1);
        assert_eq!(bare.accounts[0].label, "work");
        assert!(bare.accounts[0].active);
        assert_eq!(bare.next_swap, Some(NextSwap::NoViableTarget));
    }

    #[test]
    fn a_pre_393_target_without_a_reason_decodes_as_none() {
        // Forward-compat (#393): `reason` is an ADDITIVE field (schema 1.2), so a pre-#393 daemon
        // emits a bare `target` with no `reason` key. `#[serde(default)]` must read that as `None`
        // — never a deserialize error — which is the contract the Swift client's `decodeIfPresent`
        // path mirrors (`WireDecoderTests.testPreReasonTargetDecodesWithNilReason`). An explicit
        // `null` decodes the same way, and a `None` round-trips back out as `"reason":null`
        // (this wire carries no `skip_serializing_if`, per the codebase convention).
        let expected = NextSwap::Target {
            to: "spare".to_owned(),
            reason: None,
        };

        let absent: NextSwap = serde_json::from_str(r#"{"state":"target","to":"spare"}"#).unwrap();
        assert_eq!(absent, expected, "an absent `reason` key decodes as None");

        let explicit_null: NextSwap =
            serde_json::from_str(r#"{"state":"target","to":"spare","reason":null}"#).unwrap();
        assert_eq!(explicit_null, expected, "an explicit null decodes as None");

        assert_eq!(
            serde_json::to_string(&expected).unwrap(),
            r#"{"state":"target","to":"spare","reason":null}"#
        );
    }

    #[test]
    fn every_next_swap_reason_variant_round_trips_its_wire_tag() {
        // The `kind` tags are a CROSS-LANGUAGE contract: `WireModel.swift` matches these exact
        // strings and treats an unknown tag as a hard decode error, so a tag rename here silently
        // breaks the panel. Pin all three shapes (only `soonest_reset` carries a payload).
        for (reason, wire) in [
            (
                NextSwapReason::SoonestReset {
                    resets_at: 1_782_800_000,
                },
                r#"{"kind":"soonest_reset","resets_at":1782800000}"#,
            ),
            (
                NextSwapReason::OnlyCandidate,
                r#"{"kind":"only_candidate"}"#,
            ),
            (NextSwapReason::RosterOrder, r#"{"kind":"roster_order"}"#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), wire);
            assert_eq!(
                serde_json::from_str::<NextSwapReason>(wire).unwrap(),
                reason
            );
        }
    }

    #[test]
    fn the_versioned_status_wire_carries_no_secret() {
        // AC-3 (redaction unchanged): the envelope adds only a version object + a timestamp, so
        // the wire still carries no email / token / fingerprint (issue #15).
        let snapshot = StatusSnapshot {
            generated_at: 1_782_777_600,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&versioned_status_response(&snapshot)).unwrap();
        assert!(!json.contains('@'), "got {json}");
        assert!(!json.to_lowercase().contains("token"), "got {json}");
    }

    #[tokio::test]
    async fn refresh_exclusions_name_the_active_and_imminent_target_not_dead_accounts() {
        // Issues #105 + #106: the periodic refresh tick touches PARKED accounts only, so the
        // daemon hands it the uuids to skip — computed from the authoritative swap state the
        // tick has no view of:
        //   - the ACTIVE account (the live session's credential — never refresh it), and
        //   - the IMMINENT swap target (the same account `next_swap` surfaces; a swap promotes
        //     it by reading its stash WITHOUT rewriting it (#6), so the engine's CAS re-stash
        //     (#102) could not observe the promotion — exclude it ahead of the window).
        // A QUARANTINED (dead, #42) account is NO LONGER excluded (#106 reverses #105): it is
        // a RESTORE candidate, reported separately by `refresh_quarantined`. A HEALTHY parked
        // account that is NOT the imminent target is left out of BOTH sets — it is exactly
        // what the tick exists to keep fresh on the routine near-expiry path.
        let roster = vec![
            account("u-A", "work"),    // active
            account("u-B", "spare"),   // viable, soonest reset -> imminent swap target
            account("u-C", "backup"),  // quarantined (dead) -> restore candidate, NOT excluded
            account("u-D", "reserve"), // healthy parked, later reset -> in neither set
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
            ("Sessiometer/u-D", b"D-token", "u-D"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0); // target-max-usage 0.80
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        // Seed the post-tick state the exclusion set reads from: active = `work`, and carried
        // readings that make `spare` (reset 200) the soonest-reset viable target ahead of
        // `reserve` (reset 500). `backup` is dead — its masked-away reading is irrelevant.
        daemon.state.active = Some(0);
        daemon.state.last_readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100), // soonest overall — but it is the active account
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.50, // below the 0.80 floor -> viable
                weekly: 0.10,
                weekly_resets_at: Some(200), // soonest among the viable -> the target
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: Some(300),
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10, // also viable…
                weekly: 0.10,
                weekly_resets_at: Some(500), // …but a later reset, so never the target
                session_resets_at: None,
            }),
        ];
        daemon.state.health[2].quarantined = true; // `backup` is dead

        let excluded = daemon.refresh_exclusions();
        let quarantined = daemon.refresh_quarantined();

        // Excluded = active (u-A) + the imminent target (u-B) ONLY — NOT the dead `backup`
        // (u-C, now a restore candidate) and NOT the healthy parked `reserve` (u-D).
        assert_eq!(excluded, vec!["u-A".to_owned(), "u-B".to_owned()]);
        assert!(
            !excluded.contains(&"u-C".to_owned()),
            "dead account is no longer excluded"
        );
        assert!(!excluded.contains(&"u-D".to_owned()));
        // The dead account is reported for the RESTORE path instead (#106).
        assert_eq!(quarantined, vec!["u-C".to_owned()]);
    }

    #[test]
    fn swap_report_renders_only_for_a_swap_outcome() {
        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            generated_at: 0,
            refresh_enabled: false,
            accounts: vec![
                AccountReading {
                    label: "work".to_owned(),
                    active: false,
                    enabled: true,
                    quarantined: false,
                    recovering: false,
                    weekly_exhausted: false,
                    usage: None,
                    ..Default::default()
                },
                AccountReading {
                    label: "spare".to_owned(),
                    active: true,
                    enabled: true,
                    quarantined: false,
                    recovering: false,
                    weekly_exhausted: false,
                    usage: None,
                    ..Default::default()
                },
            ],
            next_swap: None,
        };
        let outcome = |action| TickOutcome {
            action,
            events: Vec::new(),
            diagnostics: Vec::new(),
            snapshot: snapshot.clone(),
            next_wait: None,
        };
        assert_eq!(
            swap_report(&outcome(TickAction::Swapped { from: 0, to: 1 })).as_deref(),
            Some("swapped off work onto spare"),
        );
        // #42: an emergency swap echoes too, named distinctly so the operator sees a
        // dead credential forced the rotation.
        assert_eq!(
            swap_report(&outcome(TickAction::EmergencySwapped { from: 0, to: 1 })).as_deref(),
            Some("emergency-swapped off work onto spare (dead credential)"),
        );
        assert_eq!(swap_report(&outcome(TickAction::Held)), None);
        assert_eq!(swap_report(&outcome(TickAction::SkippedCooldown)), None);
        assert_eq!(swap_report(&outcome(TickAction::NoViableTarget)), None);
        // A dead active account with no viable target holds — no console echo.
        assert_eq!(swap_report(&outcome(TickAction::ActiveDeadNoTarget)), None);
    }

    #[test]
    fn unrecoverable_report_names_the_handle_and_the_relogin_action() {
        // Issue #261 AC1/AC3: the operator message names the account HANDLE and the fix
        // (`claude /login`), and is sourced from the LABEL alone — no token or email (#15). Both
        // operator channels (console + macOS) carry this exact string, so testing it covers both.
        let line = unrecoverable_report("work");
        assert!(line.contains("work"), "must name the handle: {line}");
        assert!(
            line.contains("claude /login"),
            "must name the fix action: {line}"
        );
        // The whole message is the handle interpolated into a fixed non-secret template — a label
        // is the ONLY dynamic input, mirroring `Event::CredentialUnrecoverable`'s redaction.
        assert_eq!(
            line,
            "account work needs re-login — its refresh token is dead; run: claude /login"
        );
    }

    #[tokio::test]
    async fn swap_log_lines_name_to_as_the_now_active_account_from_as_swapped_away() {
        // DECIDER (issue #89): the from→to direction on BOTH operator surfaces — the
        // foreground console echo (`swap_report`) AND the durable `event=swap` log line
        // — must match the PHYSICAL outcome of a real swap: `to` is the account the
        // daemon just made active (swapped ONTO), `from` the one it swapped OFF. Drive
        // a genuine swap (`work` active and over the session trigger → the viable target
        // `spare`) and tie both rendered lines back to `state.active`, so a future
        // inversion of either surface — or of the `Event::Swap` / `TickAction` source —
        // fails HERE instead of silently misleading the operator. (#15: labels only.)
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40) // active, over the session trigger
            .ok("u-B", 0.05, 0.05); // the only viable target
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );

        let outcome = warmed_tick(&mut daemon).await;

        // The physical outcome: `work` (index 0) was swapped OFF; `spare` (index 1) is
        // now active. `to` must name the now-active account on every surface.
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        assert_eq!(
            daemon.state.active,
            Some(1),
            "physical outcome: `spare` (index 1) is now the active account"
        );

        // Surface 1 — the foreground console echo: off=<swapped-away>, onto=<now-active>.
        assert_eq!(
            swap_report(&outcome).as_deref(),
            Some("swapped off work onto spare"),
            "console echo must name the swapped-away account, then the now-active one",
        );

        // Surface 2 — the durable event log agrees: from=<swapped-away> to=<now-active>.
        let swap_event = outcome
            .events
            .iter()
            .find(|e| matches!(e, Event::Swap { .. }))
            .expect("a swap surfaces a structured Event::Swap (#9)");
        let log_line = swap_event.to_log_line(std::time::SystemTime::UNIX_EPOCH);
        assert!(
            log_line.contains("event=swap from=work to=spare"),
            "event log must name from=<swapped-away> to=<now-active>; got `{log_line}`",
        );
    }

    #[tokio::test]
    async fn next_swap_classifies_the_candidate_from_the_readings() {
        // The daemon-side candidate (#88) IS `pick_target` mapped to a label, plus the
        // two no-candidate verdicts the wire must distinguish. Reuses the 3-account
        // harness (work=0, spare=1, backup=2; target_max_usage 0.80, weekly_trigger_base
        // 0.98). This pins the projection/classification wrapper — `pick_target`'s own
        // selection logic is covered by its dedicated suite above.
        let daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let usage = |session: f64, weekly: f64| {
            Some(Usage {
                session,
                weekly,
                weekly_resets_at: None,
                session_resets_at: None,
            })
        };

        // Viable target → the choice mapped to a label plus its #393 reason. spare and backup are
        // both under the floor and weekly-viable; with no known reset the tie falls to the earliest
        // roster index (spare). Because TWO targets were viable and no reset-time comparison could
        // discriminate them, the reason is `RosterOrder` — neither a fabricated `SoonestReset` with
        // no epoch to carry, nor `OnlyCandidate`, which would tell the operator "only viable
        // target" while backup was equally viable.
        assert_eq!(
            daemon.next_swap(
                Some(0),
                &[usage(0.97, 0.40), usage(0.10, 0.10), usage(0.20, 0.10)]
            ),
            Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::RosterOrder),
            }),
        );

        // Readings in hand but none viable (both over the 0.80 target-max-usage) → a
        // genuine no-viable-target verdict, NOT awaiting-data.
        assert_eq!(
            daemon.next_swap(
                Some(0),
                &[usage(0.97, 0.40), usage(0.95, 0.10), usage(0.90, 0.10)]
            ),
            Some(NextSwap::NoViableTarget),
        );

        // Every other account weekly-exhausted (>= 0.98 base) → no viable target.
        assert_eq!(
            daemon.next_swap(
                Some(0),
                &[usage(0.97, 0.40), usage(0.10, 0.99), usage(0.10, 0.99)]
            ),
            Some(NextSwap::NoViableTarget),
        );

        // No reading for any other account yet — the post-restart moment #88 exists to
        // surface distinctly.
        assert_eq!(
            daemon.next_swap(Some(0), &[usage(0.97, 0.40), None, None]),
            Some(NextSwap::AwaitingData),
        );

        // MIXED warm-up: one live other already polled-and-disqualified (spare over the
        // 0.80 floor), another still unpolled (backup). This is the ONLY input that
        // separates the `all_unpolled` rule from a naive any-unpolled one — `all_unpolled`
        // is false (spare has a reading), so the verdict is `no viable target`, NOT
        // `awaiting usage data`, even though a live account is still awaiting its first
        // poll. Pins the deliberate all-vs-any choice (an `&=`→`=` mutation flips this).
        assert_eq!(
            daemon.next_swap(Some(0), &[usage(0.97, 0.40), usage(0.95, 0.10), None]),
            Some(NextSwap::NoViableTarget),
        );

        // No active anchor to swap from → no candidate at all (renders a bare `none`).
        assert_eq!(
            daemon.next_swap(None, &[usage(0.97, 0.40), None, None]),
            None
        );
    }

    #[tokio::test]
    async fn next_swap_reads_all_quarantined_others_as_no_viable_target() {
        // A `None` reading for another account has two causes the #88 substates must NOT
        // conflate: a not-yet-polled cold start (genuine `awaiting usage data`) vs a
        // QUARANTINED account (#42) whose reading `decision_readings` masks to `None`.
        // When every OTHER enabled account is quarantined there is no live target, so the
        // footer must say `no viable target` — promising "usage data" that needs a
        // re-login, not a poll, would mislead. Reuses the 3-account harness (work=0
        // active, spare=1, backup=2).
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let usage = |session: f64, weekly: f64| {
            Some(Usage {
                session,
                weekly,
                weekly_resets_at: None,
                session_resets_at: None,
            })
        };

        // Both other accounts dead (their readings masked to `None`, as the snapshot
        // would pass them) → no viable target, NOT awaiting-data.
        daemon.state.health[1].quarantined = true;
        daemon.state.health[2].quarantined = true;
        assert_eq!(
            daemon.next_swap(Some(0), &[usage(0.97, 0.40), None, None]),
            Some(NextSwap::NoViableTarget),
        );

        // Revive one: a live, not-yet-polled other account restores the genuine
        // cold-start `awaiting usage data` verdict (the substate is unchanged for it).
        daemon.state.health[1].quarantined = false;
        assert_eq!(
            daemon.next_swap(Some(0), &[usage(0.97, 0.40), None, None]),
            Some(NextSwap::AwaitingData),
        );
    }

    // --- single-instance lock ----------------------------------------------

    #[test]
    fn instance_lock_blocks_a_second_acquisition_then_frees_on_drop() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");

        let lock = InstanceLock::acquire(&path).expect("first acquisition succeeds");
        // A second acquisition while the first is held is refused — the exit-3
        // signal a second `run` exits on, without disturbing the first.
        assert!(matches!(
            InstanceLock::acquire(&path),
            Err(Error::AlreadyRunning)
        ));
        // The lock file is 0600.
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Dropping the holder releases the lock (kernel-released on close).
        drop(lock);
        let _reacquired =
            InstanceLock::acquire(&path).expect("the lock is free after the first is dropped");
    }

    #[test]
    fn instance_lock_is_held_probe_reports_absent_held_and_freed() {
        // Issue #396: the read-only lock-fallback probe behind `daemon status`. It must never
        // disturb a live holder (non-blocking flock over a separate open), and it distinguishes
        // three states: absent lock file, held-by-a-live-daemon, and present-but-free.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");

        // Absent lock file ⇒ the daemon has never created it ⇒ not held (⇒ "not running").
        assert!(!InstanceLock::is_held(&path).expect("an absent lock probes cleanly as not-held"));

        // A held single-instance lock ⇒ the probe (a SEPARATE open + non-blocking flock, as a
        // `daemon status` in another process would do) sees it held — without disturbing the
        // holder, which is still live below.
        let lock = InstanceLock::acquire(&path).expect("acquire the single-instance lock");
        assert!(InstanceLock::is_held(&path).expect("a held lock probes as held"));
        // The probe did not steal the lock: a second real acquisition is still refused.
        assert!(matches!(
            InstanceLock::acquire(&path),
            Err(Error::AlreadyRunning)
        ));

        // Released (holder dropped) ⇒ present-but-free ⇒ not held. The file now EXISTS
        // (acquire created it), so this is the stale-lock-file path — distinct from the
        // absent path above, and the probe's own acquire+release leaves nothing signalled.
        drop(lock);
        assert!(
            !InstanceLock::is_held(&path).expect("a released lock probes as not-held"),
            "a present-but-unlocked file must read as not-held (stale lock file)",
        );
    }

    // --- run loop ----------------------------------------------------------

    #[tokio::test]
    async fn run_loop_ticks_deterministically_and_stops_on_shutdown() {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // all Hold
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(4): 3 idle shutdown-checks + 1 start-up check (#76 de-burst) the run
        // loop now polls before the first poll.
        let mut shutdown = FakeShutdown::after(4);
        let control = NoControl;

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The fake clock makes the cadence deterministic: exactly 3 ticks ran.
        assert_eq!(daemon.state.ticks, 3);
    }

    #[tokio::test]
    async fn run_loop_performs_an_authenticated_swap_and_writes_a_redacted_ack() {
        use tokio::io::AsyncReadExt;
        // AC (unify `use` + redacted ack), END-TO-END through the run loop — the wiring the pieces
        // can't prove alone: an authenticated `swap` handed back by `Control::serve` (as
        // `ControlYield::Swap`) drives the post-idle glue — `perform_socket_swap` (the daemon's OWN
        // single-writer swap) + the durable `Event::Swap` + `write_swap_ack` back to the client. The
        // loop reroutes the canonical (no torn write) AND the client reads a redacted `accepted` ack
        // (no token / email).
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // both viable + under trigger → the swap is accepted, no revert
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json.clone(),
            &tun,
        );

        // A real socket pair: the loop writes the ack to the SERVER end (handed back by the fake
        // control); the test reads it from the CLIENT end after the loop closes the server end.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        let control = OnceSwap {
            command: SwapCommand {
                target: "spare".to_owned(),
                force: false,
            },
            stream: RefCell::new(Some(server)),
            fired: Cell::new(false),
        };

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let mut shutdown = FakeShutdown::after(4);
        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The loop performed the swap through the daemon's own writer: the canonical now holds B, the
        // display shows B, and in-memory active advanced — a complete, un-torn write.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
        assert_eq!(daemon.state.active, Some(1));

        // The client reads the redacted ack the loop wrote back (server end closed → EOF after it).
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        let ack: SwapAck = serde_json::from_str(reply.trim_end()).expect("a one-line ack");
        assert_eq!(
            ack,
            SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }
        );
        // #15: the wire ack leaks neither the credential (named `*-token`) nor an email.
        assert!(
            !reply.contains('@'),
            "the ack wire leaks no email: {reply:?}"
        );
        assert!(
            !reply.to_lowercase().contains("token"),
            "the ack wire leaks no credential: {reply:?}",
        );
        // The durable manual-swap event landed in the log (the best-effort emit before the ack).
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            logged.contains("event=swap from=work to=spare reason=manual"),
            "the swap event is logged: {logged}",
        );
    }

    #[tokio::test]
    async fn run_loop_performs_an_authenticated_capture_and_writes_a_redacted_ack() {
        use tokio::io::AsyncReadExt;
        // AC (daemon does ALL the credential work + redacted ack), END-TO-END through the run loop —
        // the wiring the pieces can't prove alone: an authenticated `capture` handed back by
        // `Control::serve` (as `ControlYield::Capture`) drives the post-idle glue —
        // `perform_socket_capture` (the daemon's OWN #357-locked capture) + the durable
        // `Event::Capture` + `write_capture_ack` back to the client. The loop stashes the active
        // account and grows the roster (a REAL write), AND the client reads a redacted `captured` ack
        // (no token / email). The active account (u-A) starts OUTSIDE the roster — capture is what
        // adds it.
        let roster = vec![account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-B", b"B-token", "u-B")]).await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-B", "spare")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json.clone(),
            &tun,
        )
        .with_config_path(config_path.clone());

        // A real socket pair: the loop writes the ack to the SERVER end (handed back by the fake
        // control); the test reads it from the CLIENT end after the loop closes the server end.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        let control = OnceCapture {
            command: CaptureCommand {
                label: Some("work".to_owned()),
            },
            stream: RefCell::new(Some(server)),
            fired: Cell::new(false),
        };

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let mut shutdown = FakeShutdown::after(4);
        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The loop captured through the daemon's own #357-locked path: the new account joined the
        // in-memory rotation, the on-disk roster grew, and both credential halves were stashed under
        // u-A — a complete write.
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-A"]);
        assert_eq!(Config::load_path(&config_path).unwrap().roster.len(), 2);
        assert!(daemon.stash.contains("Sessiometer/u-A"));
        // Canonical-READ-ONLY: capture rewrote neither the keychain nor `~/.claude.json`.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));

        // The client reads the redacted ack the loop wrote back (server end closed → EOF after it).
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        let ack: CaptureAck = serde_json::from_str(reply.trim_end()).expect("a one-line ack");
        assert_eq!(
            ack,
            CaptureAck::Captured {
                label: "work".to_owned(),
                count: 2,
            }
        );
        // #15: the wire ack leaks neither the credential (named `*-token`) nor an email.
        assert!(
            !reply.contains('@'),
            "the ack wire leaks no email: {reply:?}"
        );
        assert!(
            !reply.to_lowercase().contains("token"),
            "the ack wire leaks no credential: {reply:?}",
        );
        // The durable capture event landed in the log (the best-effort emit before the ack).
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            logged.contains("event=capture account=work outcome=captured"),
            "the capture event is logged: {logged}",
        );
    }

    #[tokio::test]
    async fn run_loop_publishes_a_snapshot_to_watchers_every_tick() {
        // Issue #165: the run loop must feed the `watch` channel each cycle, so a subscriber gets a
        // whole snapshot on every state change. Every `NoControl`-based run-loop test leaves
        // `publish` undriven; this recording seam counts the calls. Same deterministic harness as
        // `run_loop_ticks_deterministically_and_stops_on_shutdown` — `after(4)` runs exactly 3
        // ticks — so the loop must have published 3 times; a regression that dropped the per-tick
        // `publish` (or gated it behind a subscriber count) would record 0.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // all Hold
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        let mut shutdown = FakeShutdown::after(4);
        let published = Rc::new(Cell::new(0usize));
        let control = RecordingControl {
            published: Rc::clone(&published),
        };

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        assert_eq!(daemon.state.ticks, 3);
        assert_eq!(
            published.get(),
            3,
            "the run loop publishes a snapshot to watchers once per tick"
        );
    }

    #[tokio::test]
    async fn run_loop_honours_shutdown_during_the_startup_delay() {
        // Issue #76: the start-up de-burst delay is shutdown-responsive — a SIGINT /
        // SIGTERM arriving DURING the initial jittered wait exits cleanly, before the
        // first poll, rather than being deferred for up to STARTUP_DELAY_CAP. With
        // `after(1)` the very first `requested()` poll — the start-up check, ahead of
        // the first tick — resolves, so the loop returns having run ZERO ticks. A
        // regression to a bare `clock.tick(startup_delay).await` would run one tick
        // first (the start-up check no longer consumes `after(1)`), failing this.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        let mut shutdown = FakeShutdown::after(1);
        let control = NoControl;

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        assert_eq!(
            daemon.state.ticks, 0,
            "shutdown during the start-up delay must exit before the first tick"
        );
    }

    #[tokio::test]
    async fn run_loop_adopts_a_manual_swapped_signal_through_the_idle_select() {
        // Issue #64: the run loop's idle select must route a `ManualSwapped` control
        // signal into `adopt_manual_swap` — the one seam the `Idle` enum exists for,
        // which every `NoControl`-based run-loop test leaves undriven. In a HOLDS-ONLY
        // world no tick ever arms `last_swap`, so a cooldown armed after the loop can
        // ONLY have come from adoption running — i.e. the signal reached
        // `adopt_manual_swap` through the LIVE select, not as a disconnected unit call.
        // A regression that turned the `Some(ManualSwapped) => break` arm back into a
        // `continue` would leave `last_swap` None and fail this test.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Both accounts sit UNDER their triggers, so every tick is a Hold — no tick
        // can arm `last_swap` on its own.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 100);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        );
        assert!(
            daemon.state.last_swap.is_none(),
            "no cooldown is armed before the loop"
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // Tick 1 → idle delivers `ManualSwapped` (adopt) → tick 2 → shutdown.
        // after(3): 2 idle shutdown-checks + 1 start-up check (#76 de-burst). The
        // start-up check must NOT win (it pends), or the adoption never fires.
        let mut shutdown = FakeShutdown::after(3);
        let control = OnceManualSwap::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The signal reached `adopt_manual_swap` through the idle select: it
        // re-resolved active from the canonical (A) and armed the cooldown — the only
        // way `last_swap` is Some after a holds-only run.
        assert!(
            daemon.state.last_swap.is_some(),
            "the ManualSwapped signal must arm the cooldown via adoption"
        );
        assert_eq!(daemon.state.active, Some(0));
    }

    // --- runtime roster-reload (issue #139) --------------------------------

    /// A minimal daemon for the pure [`reconcile_roster`] tests (#139): reconcile
    /// touches no seam (no poll / store / clock / json read), so inert fixtures and a
    /// throwaway `claude_json` path suffice. State is seeded directly on `daemon.state`.
    fn reconcile_daemon(roster: Vec<Account>) -> FakeDaemon {
        let tun = tunables(95, 80, 100);
        Daemon::new(
            roster,
            FakeRosterPoller::new(),
            FakeCredentialStore::empty(),
            FakeAccountStash::empty(),
            FakeClock::frozen(),
            PathBuf::from("unused-by-reconcile"),
            &tun,
        )
    }

    /// A carried usage reading for seeding `last_readings` in the reconcile tests.
    fn reading(session: f64, weekly: f64) -> Usage {
        Usage {
            session,
            weekly,
            weekly_resets_at: None,
            session_resets_at: None,
        }
    }

    fn roster_uuids(daemon: &FakeDaemon) -> Vec<String> {
        daemon
            .roster
            .iter()
            .map(|a| a.account_uuid.clone())
            .collect()
    }

    #[test]
    fn reconcile_roster_onboards_a_new_account_and_preserves_the_rest() {
        // AC: after an onboard, the daemon's in-memory roster reflects the new account
        // — appended with DEFAULT state — while every persisting account keeps its
        // carried health / reading / warm-up state (a capture of ANOTHER account must
        // not reset a healthy one).
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true; // A carries a distinctive health mark
        daemon.state.last_readings[1] = Some(reading(0.30, 0.40)); // B carries a reading
        daemon.state.polled_once = vec![true, true];

        daemon.reconcile_roster(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ]);

        // The new account is now in the live roster…
        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-B", "u-C"]);
        // …with every parallel state array grown to match (no length skew).
        assert_eq!(daemon.state.health.len(), 3);
        assert_eq!(daemon.state.last_readings.len(), 3);
        assert_eq!(daemon.state.polled_once.len(), 3);
        // Persisting accounts keep their carried state.
        assert!(daemon.state.health[0].quarantined, "A's health preserved");
        assert_eq!(
            daemon.state.last_readings[1],
            Some(reading(0.30, 0.40)),
            "B's reading preserved"
        );
        // The onboarded account joins with DEFAULT state (unpolled, no reading, healthy).
        assert!(!daemon.state.health[2].quarantined);
        assert_eq!(daemon.state.last_readings[2], None);
        assert!(!daemon.state.polled_once[2]);
        // Active (A) is unchanged — an append never shifts existing indices.
        assert_eq!(daemon.state.active, Some(0));
    }

    #[test]
    fn reconcile_roster_preserves_state_on_a_relogin_that_updates_the_label() {
        // A relogin of an EXISTING account (same account_uuid) updates the roster
        // CONTENT (e.g. a renamed label) without duplicating the entry, and preserves
        // the account's carried decision state. (Un-quarantine on relogin is the
        // daemon's separate canonical-change path #107, not reconcile's job.)
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.last_readings[1] = Some(reading(0.11, 0.22));
        daemon.state.health[1].recovery_successes = 2;

        daemon.reconcile_roster(vec![
            account("u-A", "work"),
            account("u-B", "spare-renamed"),
        ]);

        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-B"], "no duplicate");
        assert_eq!(daemon.roster[1].label, "spare-renamed", "label updated");
        assert_eq!(
            daemon.state.last_readings[1],
            Some(reading(0.11, 0.22)),
            "carried reading preserved across the relogin"
        );
        assert_eq!(
            daemon.state.health[1].recovery_successes, 2,
            "health preserved"
        );
    }

    #[test]
    fn reconcile_roster_picks_up_an_enabled_flip() {
        // A `disable` / `enable` (#36) flips an account's `enabled` flag on disk; the
        // reload adopts the new flag (rotation membership) while preserving the
        // account's carried decision state — so the flip takes effect in the live
        // rotation without a restart, not merely at the next daemon start.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.last_readings[1] = Some(reading(0.10, 0.20));

        // `disable spare` on disk → the reloaded roster carries B parked.
        daemon.reconcile_roster(vec![
            account("u-A", "work"),
            disabled_account("u-B", "spare"),
        ]);

        assert!(
            !daemon.roster[1].enabled,
            "B is now parked in the live roster"
        );
        assert_eq!(
            daemon.state.last_readings[1],
            Some(reading(0.10, 0.20)),
            "B's carried reading is preserved across the flip"
        );
    }

    #[test]
    fn reconcile_roster_drops_a_removed_account_and_its_state() {
        // A `remove` on disk drops the account (and its state) from the live rotation;
        // the survivors keep their carried state, re-keyed by uuid across the gap.
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ]);
        daemon.state.active = Some(0);
        daemon.state.last_readings[0] = Some(reading(0.10, 0.10)); // A reading
        daemon.state.health[2].recovery_successes = 3; // C health mark

        daemon.reconcile_roster(vec![account("u-A", "work"), account("u-C", "third")]);

        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-C"], "B dropped");
        assert_eq!(daemon.state.health.len(), 2);
        // A (still index 0) keeps its reading; C (now index 1) keeps its health mark.
        assert_eq!(daemon.state.last_readings[0], Some(reading(0.10, 0.10)));
        assert_eq!(
            daemon.state.health[1].recovery_successes, 3,
            "C re-keyed by uuid"
        );
        assert_eq!(daemon.state.active, Some(0), "active A preserved");
    }

    #[test]
    fn reconcile_roster_remaps_active_across_an_index_shift() {
        // The active account is re-resolved by uuid, not by stale index: removing an
        // EARLIER account shifts the active account's index, and reconcile follows it.
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ]);
        daemon.state.active = Some(2); // C is active at index 2

        daemon.reconcile_roster(vec![account("u-B", "spare"), account("u-C", "third")]); // A removed

        // C is now at index 1 — active tracks the uuid, not the old slot.
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-C"]);
        assert_eq!(daemon.state.active, Some(1));
    }

    #[test]
    fn reconcile_roster_drops_active_to_none_when_the_active_account_is_removed() {
        // Removing the ACTIVE account leaves active `None` — the next tick re-resolves
        // active from the canonical credential (polls-without-swapping meanwhile).
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.active = Some(0); // A active

        daemon.reconcile_roster(vec![account("u-B", "spare")]); // A removed

        assert_eq!(roster_uuids(&daemon), vec!["u-B"]);
        assert_eq!(daemon.state.active, None);
    }

    #[test]
    fn reconcile_roster_to_an_empty_roster_clears_active_and_state() {
        // Reachable edge: removing the LAST account (a `remove` of the final entry)
        // reconciles to an empty roster — every parallel array empties and active drops
        // to `None`. A degenerate-but-valid runtime state (the daemon then polls
        // nothing); it must not panic on the length-zero reshape.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work")]);
        daemon.state.active = Some(0);
        daemon.state.last_readings[0] = Some(reading(0.10, 0.10));

        daemon.reconcile_roster(vec![]);

        assert!(daemon.roster.is_empty());
        assert!(daemon.state.health.is_empty());
        assert!(daemon.state.last_readings.is_empty());
        assert!(daemon.state.polled_once.is_empty());
        assert_eq!(daemon.state.active, None);
    }

    #[test]
    fn reconcile_roster_resets_the_stale_poll_schedule() {
        // The staggered poll schedule holds OLD roster indices; reconcile clears it so
        // `next_poll_index` rebuilds a fresh one (over the new roster) next cycle,
        // rather than indexing the reshaped roster with a stale cursor.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.poll_schedule = vec![0, 1];
        daemon.state.poll_pos = 1;

        daemon.reconcile_roster(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ]);

        assert!(daemon.state.poll_schedule.is_empty(), "schedule reset");
        assert_eq!(daemon.state.poll_pos, 0, "cursor reset");
    }

    #[test]
    fn control_reply_roster_reload_authenticated_signals_a_reload() {
        // Issue #139: an authenticated same-user peer's `roster-reload` acks and yields
        // the `RosterReloadRequested` signal the run loop acts on.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"roster-reload"}"#, &snap, true);
        assert_eq!(reply, r#"{"ok":true}"#);
        assert_eq!(signal, Some(ControlSignal::RosterReloadRequested));
    }

    #[test]
    fn control_reply_roster_reload_unauthenticated_is_refused_with_no_signal() {
        // Issue #139: `roster-reload` is state-affecting, so an UNauthenticated peer is
        // refused and produces NO signal — a stranger can never make the daemon re-read
        // its config (mirrors the `manual-swapped` #64 auth gate).
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"roster-reload"}"#, &snap, false);
        assert_eq!(reply, r#"{"error":"unauthorized"}"#);
        assert_eq!(signal, None);
    }

    #[test]
    fn control_reply_restored_authenticated_signals_a_restore() {
        // Issue #275: an authenticated same-user peer's `restored` acks and yields the
        // `Restored(uuid)` signal the run loop applies via `apply_refresh_restore` — carrying
        // the exact uuid from the request line, un-touched.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"restored","uuid":"u-B"}"#, &snap, true);
        assert_eq!(reply, r#"{"ok":true}"#);
        assert_eq!(signal, Some(ControlSignal::Restored("u-B".to_owned())));
    }

    #[test]
    fn control_reply_restored_unauthenticated_is_refused_with_no_signal() {
        // Issue #275 (AC-2): `restored` is state-affecting, so an UNauthenticated peer is refused
        // and produces NO signal — a stranger can never un-quarantine an account (parity with the
        // `manual-swapped` #64 / `roster-reload` #139 auth gate). Auth is checked FIRST: even a
        // well-formed request carrying a uuid gets `unauthorized`, never leaking well-formedness.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"restored","uuid":"u-B"}"#, &snap, false);
        assert_eq!(reply, r#"{"error":"unauthorized"}"#);
        assert_eq!(signal, None);
    }

    #[test]
    fn control_reply_restored_without_uuid_is_malformed_and_yields_no_signal() {
        // Issue #275: a `restored` that parses but carries no `uuid` has no target to restore, so
        // it is refused as malformed (bounded / malformed-safe like every command) — no signal, no
        // spurious ack. Checked only after auth, so this is the authenticated-but-malformed branch.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"restored"}"#, &snap, true);
        assert_eq!(reply, r#"{"error":"malformed request"}"#);
        assert_eq!(signal, None);
    }

    #[test]
    fn control_reply_shutdown_authenticated_signals_a_graceful_stop() {
        // Issue #397: an authenticated same-user peer's `shutdown` — the `daemon stop` control
        // path for an UNMANAGED daemon — acks `{"ok":true}` and yields the `ShutdownRequested`
        // signal the run loop turns into a graceful `Idle::Shutdown` (so an in-flight swap
        // completes before exit). The pure request→(reply, signal) mapping, mirroring the
        // `roster-reload` #139 gate.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"shutdown"}"#, &snap, true);
        assert_eq!(reply, r#"{"ok":true}"#);
        assert_eq!(signal, Some(ControlSignal::ShutdownRequested));
    }

    #[test]
    fn control_reply_shutdown_unauthenticated_is_refused_with_no_signal() {
        // Issue #397 (AC): `shutdown` is state-affecting — it ends the process — so an
        // UNauthenticated peer is refused with `{"error":"unauthorized"}` and produces NO signal:
        // a stranger can never stop the daemon (parity with the `manual-swapped` #64 /
        // `roster-reload` #139 / `restored` #275 same-user gate). Fail-closed on the auth verdict.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"shutdown"}"#, &snap, false);
        assert_eq!(reply, r#"{"error":"unauthorized"}"#);
        assert_eq!(signal, None);
    }

    #[tokio::test]
    async fn notify_restored_sends_the_uuid_command_and_reads_the_ack() {
        // Issue #276: the client-side `restored` notify writes exactly one newline-delimited
        // `{"cmd":"restored","uuid":"<uuid>"}` request — the uuid embedded and escaped by
        // serde_json (unlike the payload-less `roster-reload`) — and returns Ok once the daemon
        // acks. This is the CLI→daemon wire contract that #275's `control_reply` handler parses
        // back into `Restored("u-B")`, closing the loop `reconcile_login` (#276) drives.
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        // Server: accept one connection, assert the exact request line, ack once.
        let server = async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut buffered = tokio::io::BufReader::new(stream);
            let mut request = String::new();
            buffered.read_line(&mut request).await.unwrap();
            assert_eq!(request.trim_end(), r#"{"cmd":"restored","uuid":"u-B"}"#);
            buffered.write_all(br#"{"ok":true}"#).await.unwrap();
            buffered.write_all(b"\n").await.unwrap();
            buffered.flush().await.unwrap();
        };

        let (_, result) = tokio::join!(server, notify_restored(&socket, "u-B"));
        assert!(
            result.is_ok(),
            "a served restored notify returns Ok: {result:?}"
        );
    }

    #[tokio::test]
    async fn notify_restored_errs_when_no_daemon_is_listening() {
        // Issue #276 (AC-2): with no socket bound, the notify surfaces an Err — which the
        // best-effort `notify_daemon_restored` wrapper logs and swallows, so `login` still
        // succeeds (the on-disk stash/roster write is authoritative). A missing / wedged daemon
        // must never fail the verb — the daemon-down counterpart of the roster-reload best-effort
        // contract (#139) and the `use` manual-hold notify (#64).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock"); // never bound
        assert!(notify_restored(&socket, "u-B").await.is_err());
    }

    #[tokio::test]
    async fn adopt_roster_reload_reads_the_new_roster_from_disk() {
        // AC (end-to-end, no torn read): with a config path wired, the reload re-reads
        // the freshly-written `config.toml` and reconciles the in-memory roster to it —
        // onboarding the new account while preserving a persisting account's state. The
        // on-disk file is written whole, exactly as production's atomic rename leaves it.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        write_roster_config(
            &config_path,
            &[("u-A", "work"), ("u-B", "spare"), ("u-C", "third")],
        );

        let mut daemon: FakeDaemon =
            reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")])
                .with_config_path(config_path);
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true; // A's state must survive the reload

        daemon.adopt_roster_reload().await;

        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-B", "u-C"]);
        assert!(daemon.state.health[0].quarantined, "A's state preserved");
        assert_eq!(daemon.state.active, Some(0));
    }

    #[tokio::test]
    async fn adopt_roster_reload_keeps_the_current_roster_on_a_malformed_config() {
        // Best-effort: a malformed / mid-edit `config.toml` never drops the live
        // rotation — the current in-memory roster is kept and the reload is skipped.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, b"]not valid toml[").unwrap();

        let mut daemon: FakeDaemon =
            reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")])
                .with_config_path(config_path);

        daemon.adopt_roster_reload().await;

        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B"],
            "roster unchanged on a bad read"
        );
    }

    #[tokio::test]
    async fn adopt_roster_reload_is_a_noop_without_a_config_path() {
        // With no config path wired (the hermetic default), a reload signal is a silent
        // no-op — there is nothing to read, and the roster is left as-is.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work")]);

        daemon.adopt_roster_reload().await;

        assert_eq!(roster_uuids(&daemon), vec!["u-A"]);
    }

    #[tokio::test]
    async fn run_loop_adopts_a_roster_reload_signal_through_the_idle_select() {
        // Issue #139: the run loop's idle select must route a `RosterReloadRequested`
        // control signal into `adopt_roster_reload` — proving the whole daemon-side
        // chain (signal → idle break → disk re-read → reconcile) end-to-end, the one
        // wiring `NoControl`-based tests leave undriven. A regression turning the
        // `Some(RosterReloadRequested) => break` arm into a `continue` would leave the
        // in-memory roster at its startup two accounts and fail this test.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        write_roster_config(
            &config_path,
            &[("u-A", "work"), ("u-B", "spare"), ("u-C", "third")],
        );

        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_json_dir, json) = claude_json("u-A");
        // Holds-only readings so no swap perturbs the idle path.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 100);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        )
        .with_config_path(config_path);
        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B"],
            "startup roster is the two captured accounts"
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // Tick 1 → idle delivers `RosterReloadRequested` (reload) → tick 2 → shutdown.
        // after(3): 1 start-up check (pends) + 2 idle shutdown-checks.
        let mut shutdown = FakeShutdown::after(3);
        let control = OnceRosterReload::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The signal reached `adopt_roster_reload` through the idle select: the daemon
        // re-read `config.toml` and the third account is now in the LIVE rotation — no
        // restart.
        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B", "u-C"],
            "the onboarded account joined the live rotation without a restart"
        );
    }

    #[tokio::test]
    async fn run_loop_restored_control_command_un_quarantines_without_activating() {
        // Issue #275: the run loop's idle select must route a `Restored(uuid)` control signal
        // into `apply_refresh_restore` — un-quarantining the named PARKED account and logging its
        // edge-triggered `credential_restored` — WITHOUT a canonical write or an active-account
        // change. This is the on-demand un-quarantine path, decoupled from the #106 sweep (which is
        // starved, #260). The control-driven analog of `run_loop_emits_refresh_events_and_applies_restores`:
        // a regression turning the `Some(Restored) => break` arm into a `continue`, or dropping the
        // post-idle `apply_refresh_restore` call, would leave `spare` quarantined and fail here.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // `spare` (PARKED, non-active) is quarantined ("needs re-login"); `work` is active. The
        // warm-up tick polls only the active `work`, so this flag survives untouched into the idle
        // where the control signal delivers the on-demand restore.
        daemon.state.health[1].quarantined = true;

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // Tick 1 → idle delivers `Restored(u-B)` → tick 2 → shutdown. after(3): 1 start-up check
        // (pends) + 2 idle shutdown-checks — the same cadence as the roster-reload adoption test.
        let mut shutdown = FakeShutdown::after(3);
        let control = OnceRestored::new("u-B");

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The signal reached `apply_refresh_restore` through the idle select: `spare` is
        // un-quarantined in memory and its edge-triggered `credential_restored` rode the event log
        // exactly once — no sweep involved.
        assert!(
            !daemon.state.health[1].quarantined,
            "the restored account is un-quarantined"
        );
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            logged
                .matches("event=credential_restored account=spare")
                .count(),
            1,
            "exactly one credential_restored logged: {logged:?}"
        );
        // The active account is UNCHANGED — `work` (index 0), never the restored `spare` (index 1).
        // The on-demand restore never re-points canonical or swaps active (#275).
        assert_eq!(
            daemon.state.active,
            Some(0),
            "work stays active; restoring the parked spare never activates it"
        );
    }

    #[tokio::test]
    async fn run_loop_shutdown_control_command_exits_the_loop_gracefully() {
        // Issue #397 (AC): the run loop's idle select must route an authenticated `shutdown`
        // control signal — the `daemon stop` path for an UNMANAGED daemon — into the SAME graceful
        // `Idle::Shutdown` exit a SIGINT / SIGTERM drives. `OnceShutdown` fires `ShutdownRequested`
        // on the first idle, and `FakeShutdown::after(100)` guarantees the SIGINT/SIGTERM seam never
        // fires here — so the ONLY thing that can end this loop is the control signal. A regression
        // turning that arm into a `continue` (or dropping it) would spin the idle forever rather
        // than pass.
        //
        // The AC's "an in-flight swap completes before exit" half is a property of the SHARED
        // `Idle::Shutdown` exit, not of the trigger: a swap always runs to completion inside `tick`
        // (shutdown is observed only BETWEEN ticks), as `run_loop_completes_a_swap_before_a_
        // concurrent_shutdown` proves for the signal path. The socket verb funnels into that
        // identical exit, so it inherits the no-half-swap guarantee by construction.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Holds-only readings — no swap perturbs the idle path; the shutdown signal drives the exit.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(100): the SIGINT/SIGTERM seam never resolves within this test, so a passing run
        // proves the CONTROL signal — not a signal — ended the loop.
        let mut shutdown = FakeShutdown::after(100);
        let control = OnceShutdown::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .expect("an authenticated shutdown control command exits the loop cleanly (Ok)");

        // Exactly one warm-up tick ran, then the first idle delivered `ShutdownRequested` and broke
        // to `Idle::Shutdown` — the graceful exit, with the tick already complete.
        assert_eq!(
            daemon.state.ticks, 1,
            "one tick, then the control-driven graceful exit",
        );
    }

    #[tokio::test]
    async fn run_loop_external_login_watch_restashes_off_the_poll_cadence() {
        // Issue #140: the external-login watch's `until_due` resolves inside the idle select (off
        // the usage-poll cadence) and, on reading a canonical that differs from the daemon's
        // last-committed baseline, the run loop breaks the idle to re-tick — so the very next
        // tick's `reconcile_canonical_change` re-stashes the account. The watch and the daemon
        // share ONE canonical store (as in production, one keychain item): the watch simulates a
        // manual `claude /login` by rewriting it mid-idle, and the re-tick it triggers re-stashes
        // A with the fresh token. The run-loop analog of the direct-tick #13 re-stash test —
        // proving the pickup happens WITHOUT waiting a full poll interval.
        let store = Rc::new(FakeCredentialStore::empty());
        store.write(&cred(b"A-token")).await.unwrap();
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
        let tun = tunables(95, 80, 0);
        let mut daemon = Daemon::new(
            roster,
            poller,
            store.clone(),
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(3): #1 start-up check (pends), #2 idle-iter-1 (watch fires → detect → re-tick),
        // #3 idle-iter-2 → shutdown. Exactly ONE watch-driven re-tick, then a clean stop.
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;
        let mut login_watch = OnceExternalLogin::new(store.clone(), b"A-reauthed");

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut login_watch,
        )
        .await
        .unwrap();

        // The watch broke the idle and the re-tick re-stashed A with the fresh token — the
        // out-of-band login was picked up off the poll cadence.
        let a = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-reauthed",
            "the re-tick the watch triggered re-stashed A with the freshly-logged-in token"
        );
        assert_eq!(
            a.oauth_account.account_uuid(),
            "u-A",
            "the identity half is preserved through the re-stash"
        );
    }

    #[tokio::test]
    async fn run_loop_external_login_watch_ignores_an_unchanged_canonical() {
        // Issue #140 (healthy no-change path unchanged): the watch fires and reads a canonical
        // BYTE-IDENTICAL to the daemon's baseline — no out-of-band login — so the run loop does
        // NOT break the idle and no re-stash happens. The probe was reached but correctly did
        // nothing.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let logpath = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&logpath).unwrap();
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;
        // The watch reports the SAME token the daemon primed on — no change.
        let mut login_watch = ScriptedExternalLogin::returning(Some(cred(b"A-token")));

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut login_watch,
        )
        .await
        .unwrap();

        // The probe ran but the unchanged read produced no re-stash — A's stash is untouched and
        // no restash line was logged.
        assert!(
            login_watch.probed.get(),
            "the watch's read_canonical was reached in the idle"
        );
        let a = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "an unchanged canonical triggers no re-stash"
        );
        let logged = std::fs::read_to_string(&logpath).unwrap_or_default();
        assert!(
            !logged.contains("event=restash"),
            "no restash on the no-change path: {logged:?}"
        );
    }

    #[tokio::test]
    async fn run_loop_external_login_watch_tolerates_an_unreadable_probe() {
        // Issue #140 fail-safe (a detection error must not break the loop): the watch fires but
        // its canonical read fails (locked / absent → `None`), so the run loop detects nothing,
        // does NOT break, and idles on normally to a clean shutdown — no crash, no stall, no
        // spurious re-stash. Same fail-open discipline as #156's collector and #162's refresh.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;
        // The probe cannot read the canonical this cycle (a locked / absent item → None).
        let mut login_watch = ScriptedExternalLogin::returning(None);

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut login_watch,
        )
        .await
        .unwrap();

        // The loop exited cleanly, the probe was reached, and the failed read triggered no
        // re-stash — a detection error neither broke nor perturbed the poll/swap loop.
        assert!(
            login_watch.probed.get(),
            "the watch's read_canonical was reached in the idle"
        );
        let a = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "a failed probe triggers no re-stash"
        );
    }

    #[tokio::test]
    async fn external_login_watcher_reads_the_canonical_and_fails_open() {
        // Issue #140: the PRODUCTION watcher's `read_canonical` returns the current canonical
        // over its own store, and FAILS OPEN to `None` on a locked or absent read — so a probe
        // that cannot read simply detects nothing, never an error that could break the loop.
        let readable = FakeCredentialStore::empty();
        readable.write(&cred(b"A-token")).await.unwrap();
        let mut w = ExternalLoginWatcher::new(readable);
        assert_eq!(
            w.read_canonical().await.unwrap().expose(),
            b"A-token",
            "a readable canonical is returned"
        );

        // A LOCKED keychain read is swallowed to `None`, not surfaced as an error.
        let locked = FakeCredentialStore::empty();
        locked.set_locked(true);
        let mut w = ExternalLoginWatcher::new(locked);
        assert!(
            w.read_canonical().await.is_none(),
            "a locked read fails open to None"
        );

        // An ABSENT canonical (no item yet) likewise fails open to `None` — never an error.
        let mut w = ExternalLoginWatcher::new(FakeCredentialStore::empty());
        assert!(
            w.read_canonical().await.is_none(),
            "an absent canonical fails open to None"
        );
    }

    #[tokio::test]
    async fn run_loop_runs_a_refresh_sweep_in_the_idle_path() {
        // Issue #105: an ENABLED ticker's `until_due` resolves inside the idle select, and the
        // run loop then runs its `sweep` — handing it the daemon's live exclusion set (with the
        // active account among the uuids to skip, the "parked only" contract). This is the one
        // run-loop test that drives the live `until_due → sweep` wiring; every other passes the
        // inert `NoopRefreshTicker`, whose `until_due` never resolves.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(4): #1 start-up check, #2 idle-iter-1 outer (refresh wins → sweep), #3 the
        // sweep's NESTED shutdown arm (pends, so the sweep runs), #4 idle-iter-2 outer →
        // shutdown. So the sweep fires once, then the loop stops cleanly.
        let mut shutdown = FakeShutdown::after(4);
        let control = NoControl;
        let mut ticker = OnceRefreshTicker::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut ticker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The sweep ran exactly once, and the daemon handed it the active account (u-A) to
        // skip — the refresh tick reached the idle path with the right exclusions.
        let swept = ticker.swept.borrow();
        assert_eq!(swept.len(), 1, "exactly one sweep ran: {swept:?}");
        assert!(
            swept[0].contains(&"u-A".to_owned()),
            "the active account is excluded from the sweep: {:?}",
            swept[0]
        );
        // No account is quarantined here, so the tick is NEVER handed a recovery prompt — the
        // #280 signal is false whenever there is no restore work (contrast to the quarantined case).
        assert!(
            ticker.due_recovery.borrow().iter().all(|&r| !r),
            "no quarantine means no recovery prompt: {:?}",
            ticker.due_recovery.borrow(),
        );
    }

    #[tokio::test]
    async fn run_loop_lets_shutdown_interrupt_an_in_flight_refresh_sweep() {
        // Issue #105: the refresh arm runs its sweep under a NESTED select whose only other arm
        // is shutdown — so a SIGINT/SIGTERM cuts an in-flight (here deliberately wedged) sweep
        // and the loop returns, rather than deadlocking on a stuck refresh cycle. A control read
        // is NOT in that nested select, so it cannot interrupt a sweep (no token forfeit, no
        // starvation). A regression that awaited `sweep` directly — dropping the nested shutdown
        // arm — would hang here; the `timeout` turns that hang into a clean failure.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(3): #1 start-up check, #2 idle-iter-1 outer (refresh wins → enter nested), #3
        // the sweep's NESTED shutdown arm fires → break. The wedged sweep is cut by shutdown.
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;
        let mut ticker = HangingRefreshTicker::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        // Reaching the assertion at all is the proof; the timeout guards against the regression
        // (a directly-awaited sweep) deadlocking the suite instead of failing cleanly.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_loop(
                &mut daemon,
                &mut log,
                &mut diag,
                &mut shutdown,
                &control,
                &mut ticker,
                &mut NoopExternalLoginWatch,
            ),
        )
        .await;
        assert!(
            result.is_ok(),
            "shutdown must interrupt the wedged sweep, not deadlock"
        );
        result.unwrap().unwrap();
    }

    #[tokio::test]
    async fn run_loop_emits_refresh_events_and_applies_restores() {
        // Issue #106: the run loop drains a sweep's `SweepOutcome` — it EMITS each per-cycle
        // refresh event to the event log, and APPLIES each reported restore (un-quarantining the
        // recovered account + logging its edge-triggered `credential_restored`). A quarantined
        // PARKED account (`spare`) is never re-polled by the swap path (#42 revival can't fire)
        // and not re-logged-in (#107 can't fire) — the exact gap #106 closes: it would stay stuck
        // forever even though its refresh token still works. Here the sweep reports it recovered
        // and the loop flips it back to eligible.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // `spare` is quarantined ("needs re-login", #42) but its refresh token still works — the
        // parked-and-stuck account #106 rescues. The single warm-up tick polls only the active
        // `work`, so this flag survives untouched into the idle sweep.
        daemon.state.health[1].quarantined = true;

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(4): one sweep fires in idle-iter-1, then idle-iter-2 sees shutdown — the same
        // cadence as `run_loop_runs_a_refresh_sweep_in_the_idle_path`.
        let mut shutdown = FakeShutdown::after(4);
        let control = NoControl;
        // The sweep reports `spare` refreshed: one refresh event to log + one restore to apply.
        let mut ticker = OnceRefreshTicker::returning(SweepOutcome {
            events: vec![Event::Refresh {
                account: "spare".to_owned(),
                outcome: RefreshEventOutcome::Refreshed,
                expires_before: Some(1_000_000),
                expires_after: Some(1_003_600),
                refresh_token_rotated: false,
                reason: None,
                backoff_secs: None,
            }],
            restored: vec!["u-B".to_owned()],
            observations: Vec::new(),
        });

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut ticker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The daemon handed the sweep its quarantined set — the RESTORE candidates — so the tick
        // could attempt them even though they sit far from near-expiry.
        let swept_q = ticker.swept_quarantined.borrow();
        assert_eq!(swept_q.len(), 1, "exactly one sweep ran: {swept_q:?}");
        assert!(
            swept_q[0].contains(&"u-B".to_owned()),
            "the quarantined parked account is offered to the sweep: {:?}",
            swept_q[0]
        );

        // The per-cycle refresh event rode the event log, and the reported restore both
        // un-quarantined `spare` in memory AND logged its edge-triggered `credential_restored`.
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            logged.contains("event=refresh account=spare outcome=refreshed"),
            "the refresh event reached the log: {logged:?}"
        );
        assert!(
            logged.contains("event=credential_restored account=spare"),
            "the restore logged its credential_restored: {logged:?}"
        );
        assert!(
            !daemon.state.health[1].quarantined,
            "the restored account is un-quarantined"
        );
    }

    #[tokio::test]
    async fn run_loop_prompts_the_tick_when_a_quarantined_parked_account_is_present() {
        // Issue #280: the run loop threads the "≥1 quarantined-PARKED account" signal into the
        // tick's DUE computation (`until_due`), not only into `sweep`. With `spare` quarantined and
        // parked, the FIRST idle wait is handed `has_recovery_work = true` — so the restore is
        // prompt (the idle floor) instead of deferred a full refresh cadence. After that period's
        // sweep the prompt is DISARMED (`recovery_prompted`), so every later wait this period sees
        // `false` — the coupling that keeps a still-quarantined account off the sub-poll retry
        // storm ADR-0007 rejected.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // `spare` is quarantined AND parked (u-A is active) — the exact "recovery work" the prompt
        // targets. It survives the warm-up tick, which polls only the active `work`.
        daemon.state.health[1].quarantined = true;

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(4): one sweep fires in idle-iter-1, then idle-iter-2 sees shutdown — the same
        // cadence as the sibling refresh run-loop tests.
        let mut shutdown = FakeShutdown::after(4);
        let control = NoControl;
        let mut ticker = OnceRefreshTicker::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut ticker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The tick was asked to become due WITH recovery work on the first wait — the prompt
        // reached `until_due`, not just `sweep`.
        let due_recovery = ticker.due_recovery.borrow();
        assert_eq!(
            due_recovery.first(),
            Some(&true),
            "the first idle wait must see the quarantined-parked recovery prompt: {due_recovery:?}",
        );
        // …and every wait AFTER the period's sweep is disarmed (once per period — no sub-poll storm).
        assert!(
            due_recovery.iter().skip(1).all(|&r| !r),
            "the recovery prompt is disarmed after the sweep: {due_recovery:?}",
        );
    }

    #[tokio::test]
    async fn run_loop_completes_a_swap_before_a_concurrent_shutdown() {
        // The warm-up cycle (issue #80) polls A then B across two staggered ticks;
        // the swap fires on the warm-up-completing second tick. Shutdown is then
        // requested. Because a swap runs to completion inside `tick` (shutdown is only
        // observed between ticks), the post-loop state is coherent — no half-swap.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(3): 1 start-up check (#76 de-burst) + 2 idle shutdown-checks — run
        // both warm-up ticks (poll A, then poll B + swap), then stop.
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The warm-up-completing tick's swap landed fully: canonical = B, display = B,
        // active = B.
        assert_eq!(daemon.state.ticks, 2);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
        assert_eq!(daemon.state.active, Some(1));

        // End-to-end (issue #9): the swap wrote one structured swap line — handles only
        // (work → spare), never a token or email. The session reading (0.97) is at/over the
        // 95 % trigger, so the line is tagged `reason=session` with the outgoing account's
        // `session_pct`. Since #137, `spare` also logs one honest Unknown→healthy transition
        // as the swap makes it active and its first poll verifies it — an expected companion
        // line, not spurious output (and itself #15-clean: a handle + a bare state token).
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            logged.lines().count(),
            2,
            "swap + spare health line: {logged:?}"
        );
        assert!(
            logged.contains("event=swap from=work to=spare reason=session session_pct=97"),
            "got: {logged:?}"
        );
        assert!(
            logged.contains("event=credential_health account=spare state=healthy"),
            "spare verified healthy once polled after the swap (#137): {logged:?}"
        );
        assert!(logged.starts_with("ts="), "stamped: {logged:?}");
        assert!(!logged.contains('@'), "no email: {logged:?}");
    }

    #[tokio::test]
    async fn note_poll_outcome_walks_the_401_streak_and_emits_one_event_per_named_condition() {
        // The daemon-side poll-outcome → event mapping and the per-account 401
        // streak (issue #9) are exercised directly: `note_poll_outcome` turns each
        // poll `Result` into at most one event and maintains the streak. Driving it
        // by hand (rather than through the loop) lets us assert the reset, which a
        // static poller cannot script on a single account across ticks.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let mut events = Vec::new();
        // Issue #42: the per-account 401 streak now lives in `health[i].consec_401`.
        let streak_of = |d: &FakeDaemon| {
            d.state
                .health
                .iter()
                .map(|h| h.consec_401)
                .collect::<Vec<_>>()
        };

        // A 401 on account 0 starts its streak at 1; a second consecutive 401
        // climbs to 2 — one `monitor_401` per occurrence, account 1 untouched.
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(streak_of(&daemon), vec![2, 0]);
        assert_eq!(
            events,
            vec![
                Event::Monitor401 {
                    account: "work".to_owned(),
                    consecutive: 1,
                },
                Event::Monitor401 {
                    account: "work".to_owned(),
                    consecutive: 2,
                },
            ]
        );

        // A success resets account 0's streak and emits nothing.
        events.clear();
        daemon.note_poll_outcome(
            0,
            &Ok(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            &mut events,
        );
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert!(events.is_empty());

        // After the reset the next 401 restarts the streak at 1 (not 3).
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(streak_of(&daemon), vec![1, 0]);
        assert_eq!(
            events,
            vec![Event::Monitor401 {
                account: "work".to_owned(),
                consecutive: 1,
            }]
        );

        // A locked keychain is detected at top-of-tick now, not per-account (issue
        // #13), so this arm emits NOTHING — it only resets the streak, like any
        // other non-401 outcome. Account 0's streak (1) is left untouched.
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::KeychainLocked { op: "read" }), &mut events);
        assert_eq!(streak_of(&daemon), vec![1, 0]);
        assert!(events.is_empty());

        // A 403 (missing usage scope) on account 0 emits `usage_scope_fail` and
        // resets its streak — every non-401 outcome clears the streak.
        events.clear();
        daemon.note_poll_outcome(0, &Err(Error::UsageScopeMissing), &mut events);
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert_eq!(
            events,
            vec![Event::UsageScopeFail {
                account: "work".to_owned(),
            }]
        );

        // A transient error is silent and also resets (no event, streak cleared).
        events.clear();
        daemon.note_poll_outcome(
            0,
            &Err(Error::UsageTransient {
                status: 0,
                retry_after: None,
            }),
            &mut events,
        );
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn run_loop_logs_one_line_per_poll_rejection_each_tick() {
        // Issue #9 acceptance (as amended by #13, #80): each PER-ACCOUNT poll
        // rejection — a 401 and a 403 (missing usage scope) — emits EXACTLY one
        // structured line per occurrence. A per-account keychain-lock is now SILENT
        // here: the lock is process-global and signaled once at top-of-tick (#13),
        // not per poll. The staggered loop (#80) polls ONE account per tick, the active
        // interleaved before each peer (#366 → A, B, A, C), so a full sweep of the
        // 3-account roster takes four ticks; those four ticks poll A twice (ticks 1 and
        // 3) — proving the per-account 401 streak climbs 1 → 2 across its own re-polls —
        // with B's (silent) lock on tick 2 and C's 403 on tick 4, demonstrating
        // `note_poll_outcome` is wired into the loop and serialized.
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "backup"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .unauthorized("u-A")
            .keychain_locked("u-B")
            .scope_missing("u-C");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(5): 4 idle shutdown-checks + 1 start-up check (#76 de-burst) — four
        // staggered ticks; the #366 active-interleave makes them (A, B, A, C).
        let mut shutdown = FakeShutdown::after(5);
        let control = NoControl;

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        assert_eq!(daemon.state.ticks, 4);

        // Across the four staggered ticks (#80, interleaved #366 → A, B, A, C), A 401s
        // twice (ticks 1, 3) and C 403s once (tick 4) → three event lines, each stamped,
        // none carrying secret material (handles only — never a token or email). The
        // locked account B contributes nothing per-account (#13).
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(logged.lines().count(), 3, "three lines: {logged:?}");
        assert!(
            logged.lines().all(|l| l.starts_with("ts=")),
            "stamped: {logged:?}"
        );
        assert!(!logged.contains('@'), "no email: {logged:?}");

        // The 401 streak is per-occurrence and climbs across ticks.
        assert!(
            logged.contains("event=monitor_401 account=work consecutive=1"),
            "{logged:?}"
        );
        assert!(
            logged.contains("event=monitor_401 account=work consecutive=2"),
            "{logged:?}"
        );
        // The per-account keychain-lock is silent now (#13): NO lock line appears,
        // even though account `spare`'s poll returned a locked error every tick.
        assert!(
            !logged.contains("event=keychain_locked_wait"),
            "a per-account lock must not emit a line: {logged:?}"
        );
        // The 403 line renders once per poll of C (one poll across the four staggered
        // ticks, #80) and carries `status=403`.
        assert_eq!(
            logged
                .lines()
                .filter(|l| l.contains("event=usage_scope_fail account=backup status=403"))
                .count(),
            1,
            "{logged:?}"
        );
        // The active account was unavailable every tick, so no swap line appears;
        // the streak is pure observability. Final state: account 0 saw two 401s.
        assert!(!logged.contains("event=swap"), "{logged:?}");
        let streak_of = |d: &FakeDaemon| {
            d.state
                .health
                .iter()
                .map(|h| h.consec_401)
                .collect::<Vec<_>>()
        };
        assert_eq!(streak_of(&daemon), vec![2, 0, 0]);
    }

    #[tokio::test]
    async fn run_loop_logs_a_weekly_reason_when_only_the_weekly_dimension_trips() {
        // Issue #9: a swap driven by the WEEKLY dimension (session below its
        // trigger) is logged `reason=weekly`, while `session_pct` still reports the
        // outgoing account's session reading (the schema carries no weekly percent).
        // This guards the reason re-derivation against mislabeling a weekly-only
        // swap as `session`.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Session 0.50 is below the 95 % session trigger; weekly 0.99 is over the
        // fixed 98 % weekly trigger → a weekly-only swap. Target B is under the floor.
        // The swap fires on the warm-up-completing second staggered tick (#80).
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.99)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(3): 2 idle shutdown-checks + 1 start-up check (#76 de-burst) — two
        // warm-up ticks (poll A, then poll B + swap).
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The swap line carries the weekly reason; since #137 `spare` also logs one honest
        // Unknown→healthy transition once the swap makes it active and its poll verifies it.
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            logged.lines().count(),
            2,
            "swap + spare health line: {logged:?}"
        );
        assert!(
            logged.contains("event=swap from=work to=spare reason=weekly session_pct=50"),
            "got: {logged:?}"
        );
        assert!(
            logged.contains("event=credential_health account=spare state=healthy"),
            "spare verified healthy once polled after the swap (#137): {logged:?}"
        );
    }

    // --- #42 dead-credential lifecycle -------------------------------------
    //
    // The persistent-401 lifecycle: detect (N consecutive 401s → DEAD), quarantine
    // (skip the dead account, never halt the rotation), emergency-swap (escape a dead
    // ACTIVE account immediately, bypassing trigger + cooldown), auto-recover (M
    // consecutive live polls un-quarantine a re-logged-in account), and signal (one
    // edge-triggered event per transition + a durable "needs re-login" status). The
    // pure `classify_poll` mapping and the per-account health that carries the streak
    // ACROSS ticks (the issue's CODE PREREQUISITE) are exercised directly.

    /// A two-account daemon (`work` active, `spare` spare) with both tokens stashed
    /// and the canonical holding `work`'s — the common fixture for the lifecycle
    /// tests below. `monitor_401_n` = 3, `monitor_recovery_m` = 2 (the test defaults).
    async fn lifecycle_daemon() -> FakeDaemon {
        lifecycle_daemon_with(FakeRosterPoller::new(), tunables(95, 80, 0)).await
    }

    /// Like [`lifecycle_daemon`] but with a caller-chosen poller + tunables, for the
    /// tick-driven tests that script per-account poll outcomes.
    async fn lifecycle_daemon_with(poller: FakeRosterPoller, tun: Tunables) -> FakeDaemon {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        // Keep the temp `~/.claude.json` alive for the daemon's lifetime by leaking
        // the guard — these are short-lived unit-test daemons.
        std::mem::forget(dir);
        Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
    }

    fn live(session: f64, weekly: f64) -> Result<Usage> {
        Ok(Usage {
            session,
            weekly,
            weekly_resets_at: None,
            session_resets_at: None,
        })
    }

    #[tokio::test]
    async fn classify_poll_maps_each_result_to_its_liveness_class() {
        // The typed poll outcome (issue #42 CODE PREREQUISITE): the HTTP taxonomy is
        // folded onto the liveness/death axis in exactly one place. Success is Live,
        // 401 is Unauthorized (the death signal), 403 is its own ScopeMissing class,
        // and EVERY other failure collapses into the single Transient class.
        assert_eq!(classify_poll(&live(0.5, 0.5)), PollOutcome::Live);
        assert_eq!(
            classify_poll(&Err(Error::UsageUnauthorized)),
            PollOutcome::Unauthorized
        );
        assert_eq!(
            classify_poll(&Err(Error::UsageScopeMissing)),
            PollOutcome::ScopeMissing
        );
        for err in [
            Error::UsageTransient {
                status: 0,
                retry_after: None,
            },
            Error::UsageRateLimited {
                status: 429,
                retry_after: None,
            },
            Error::UsageRejected { status: 400 },
            Error::KeychainLocked { op: "read" },
            Error::UsageTokenUnreadable,
            Error::UsageParse("no dimension".to_owned()),
        ] {
            assert_eq!(
                classify_poll(&Err(err)),
                PollOutcome::Transient,
                "every non-401/403 failure folds into Transient",
            );
        }
    }

    #[tokio::test]
    async fn nth_consecutive_401_quarantines_the_account_and_signals_once() {
        // Detection + edge-trigger + anti-spam, driven directly (a static poller
        // cannot script a streak that crosses the threshold). Driving `spare`
        // (non-active) isolates detection from the emergency-swap path.
        let mut daemon = lifecycle_daemon().await;
        let mut events = Vec::new();

        // Two 401s climb the streak; below the threshold (3) the account stays alive.
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(!daemon.state.health[1].quarantined);
        assert_eq!(daemon.state.health[1].consec_401, 2);

        // The 3rd consecutive 401 declares the credential DEAD: the climbing
        // `monitor_401` AND exactly one `credential_dead`, on the false→true edge.
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(daemon.state.health[1].quarantined);
        assert_eq!(
            events,
            vec![
                Event::Monitor401 {
                    account: "spare".to_owned(),
                    consecutive: 3,
                },
                Event::CredentialDead {
                    account: "spare".to_owned(),
                },
            ]
        );

        // A 4th 401 on the already-dead account is SILENT — the dead state is a
        // durable status, not a repeated log line (no spam).
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(daemon.state.health[1].quarantined);
        assert!(
            events.is_empty(),
            "an already-dead 401 re-emits nothing: {events:?}"
        );
    }

    // --- Issue #162: poll↔refresh seam ------------------------------------

    /// Shared, mutable per-account poll outcome the #162 seam tests drive: [`SeamPoller`]
    /// reads the CURRENT outcome and [`SeamRefresh`] may FLIP it (revive an expired token)
    /// on refresh — modelling the poll↔refresh seam the fix composes. Keyed by
    /// `account_uuid`, mirroring [`FakeRosterPoller`].
    type SeamOutcomes = Rc<RefCell<HashMap<String, Scripted>>>;

    /// A [`RosterPoller`] reading its per-account outcome from a shared [`SeamOutcomes`]
    /// cell, so a refresh that revives a token is observed on the very next poll.
    struct SeamPoller {
        outcomes: SeamOutcomes,
    }

    impl RosterPoller for SeamPoller {
        async fn poll(&self, account: &Account, _active: bool) -> Result<PolledReading> {
            match self.outcomes.borrow().get(&account.account_uuid) {
                Some(Scripted::Ok(usage)) => Ok(PolledReading {
                    usage: *usage,
                    severity: None,
                }),
                Some(Scripted::Unauthorized) => Err(Error::UsageUnauthorized),
                Some(Scripted::ScopeMissing) => Err(Error::UsageScopeMissing),
                // Only `Ok` / `Unauthorized` / `ScopeMissing` are scripted here; anything
                // else (or an unscripted account) is an unavailable transient gap.
                _ => Err(Error::UsageTransient {
                    status: 0,
                    retry_after: None,
                }),
            }
        }
    }

    /// A [`PollRefresh`] fake for the #162 seam tests: it COUNTS refresh calls (the
    /// once-per-episode guard, AC-4), returns a scripted [`RefreshOutcome`], and — when
    /// `revive_to` is set — REVIVES the account by flipping its shared [`SeamOutcomes`]
    /// entry to a live reading (the false-death the fix rescues). `hard_error` makes the
    /// refresh itself fail (the fail-safe path).
    struct SeamRefresh {
        outcomes: SeamOutcomes,
        outcome: RefreshOutcome,
        revive_to: Option<Usage>,
        hard_error: bool,
        calls: Rc<Cell<u32>>,
    }

    impl PollRefresh for SeamRefresh {
        fn refresh<'a>(
            &'a self,
            account: &'a Account,
        ) -> Pin<Box<dyn Future<Output = Result<RefreshReport>> + 'a>> {
            Box::pin(async move {
                self.calls.set(self.calls.get() + 1);
                if self.hard_error {
                    // A refresh that cannot even run (spawn / lock failure) → could-not-revive.
                    return Err(Error::SwapLockBusy);
                }
                if let Some(usage) = self.revive_to {
                    self.outcomes
                        .borrow_mut()
                        .insert(account.account_uuid.clone(), Scripted::Ok(usage));
                }
                Ok(RefreshReport {
                    outcome: self.outcome,
                    expires_at_delta_secs: None,
                    // A rotation only happens when CC actually performed the exchange (a real
                    // `Refreshed`); NoChange / Dead / Error never rotate. Lets the #279
                    // poll-refresh event test observe a `true` threaded from the report.
                    refresh_token_rotated: matches!(self.outcome, RefreshOutcome::Refreshed),
                    re_stashed: matches!(self.outcome, RefreshOutcome::Refreshed),
                })
            })
        }
    }

    /// A two-account seam daemon (issue #162): `work` (`u-A`) polls healthy and stays the
    /// active account; `spare` (`u-B`) is the non-active account under test (isolating the
    /// refresh-retry from the emergency-swap path, exactly as
    /// [`nth_consecutive_401_quarantines_the_account_and_signals_once`] isolates detection).
    /// The round-robin schedule (#80) polls `work` then `spare`, so `spare` is polled on
    /// every SECOND tick. Returns the daemon plus the shared outcome cell (to re-script
    /// mid-run) and the refresh call-counter (to assert no storm).
    async fn seam_daemon(
        spare_outcome: Scripted,
        refresh_outcome: RefreshOutcome,
        revive_to: Option<Usage>,
        hard_error: bool,
        monitor_401_n: u8,
    ) -> (
        Daemon<SeamPoller, FakeCredentialStore, FakeAccountStash, FakeClock>,
        SeamOutcomes,
        Rc<Cell<u32>>,
    ) {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        // Keep the temp `~/.claude.json` alive for the daemon's life (as `three_account_daemon`).
        std::mem::forget(dir);
        let tun = Tunables {
            monitor_401_n,
            ..tunables(95, 80, 0)
        };
        let outcomes: SeamOutcomes = Rc::new(RefCell::new(HashMap::from([
            ("u-A".to_owned(), Scripted::Ok(reading(0.10, 0.10))),
            ("u-B".to_owned(), spare_outcome),
        ])));
        let calls = Rc::new(Cell::new(0u32));
        let daemon = Daemon::new(
            roster,
            SeamPoller {
                outcomes: outcomes.clone(),
            },
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
        .with_refresh_engine(Box::new(SeamRefresh {
            outcomes: outcomes.clone(),
            outcome: refresh_outcome,
            revive_to,
            hard_error,
            calls: calls.clone(),
        }));
        (daemon, outcomes, calls)
    }

    #[tokio::test]
    async fn a_usage_401_that_clears_after_refresh_does_not_quarantine() {
        // AC-1: a parked account whose access token merely EXPIRED (401) but whose refresh
        // token is valid → the daemon refreshes + re-polls, the re-poll CLEARS, and the #42
        // death streak never advances. This is the false death the fix eliminates.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // the refresh REVIVES the spare's token
            false,
            3,
        )
        .await;
        // Drive three spare polls (round-robin idx 1 → ticks 2, 4, 6).
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(
            !daemon.state.health[1].quarantined,
            "a 401 that clears after one refresh must NOT quarantine the account",
        );
        assert_eq!(
            daemon.state.health[1].consec_401, 0,
            "the successful re-poll resets the streak",
        );
        assert_eq!(
            calls.get(),
            1,
            "exactly one refresh — the revive, not a per-poll storm",
        );
    }

    #[tokio::test]
    async fn a_usage_401_that_survives_a_fresh_token_still_quarantines_after_n() {
        // AC-2 (+ AC-4): a 401 that PERSISTS after a fresh token is the genuine dead signal —
        // it still quarantines after `monitor_401_n` such survivals, and the refresh fires at
        // most ONCE per episode, not on every poll.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed, // the refresh "succeeds" but does NOT revive (no flip)
            None,
            false,
            3,
        )
        .await;
        // spare polled on ticks 2, 4, 6 → three surviving 401s → quarantine at N = 3.
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(
            daemon.state.health[1].quarantined,
            "a 401 that survives the fresh token must still quarantine after N",
        );
        assert_eq!(
            calls.get(),
            1,
            "AC-4: at most ONE refresh per streak episode — no per-poll refresh storm",
        );
    }

    #[tokio::test]
    async fn a_refresh_reporting_dead_is_treated_as_a_genuine_death() {
        // AC-3: the refresh clears the refresh token in place (Dead) — a genuine death. The
        // re-poll is skipped, the 401 stands, and the account quarantines through the streak.
        let (mut daemon, _outcomes, calls) =
            seam_daemon(Scripted::Unauthorized, RefreshOutcome::Dead, None, false, 3).await;
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(
            daemon.state.health[1].quarantined,
            "a refresh that reports the token Dead must quarantine the account",
        );
        assert_eq!(
            calls.get(),
            1,
            "one refresh confirmed the death; the rest of the streak advances directly",
        );
    }

    #[tokio::test]
    async fn a_refresh_that_fails_is_fail_safe_and_still_quarantines() {
        // Fail-safe AC: a refresh that itself ERRORS (spawn / lock failure) is handled — it
        // never crashes the poll loop, and "could not revive" lets the 401 stand so a truly
        // dead account still quarantines after N.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,
            // unused: `hard_error` short-circuits before the report; any error sub-reason stands in.
            RefreshOutcome::Error(crate::refresh::RefreshErrorReason::SpawnFailed),
            None,
            true,
            3,
        )
        .await;
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(
            daemon.state.health[1].quarantined,
            "a refresh failure is treated as could-not-revive → the account still quarantines",
        );
        assert_eq!(
            calls.get(),
            1,
            "the failed refresh is still bounded to one attempt per episode",
        );
    }

    #[tokio::test]
    async fn a_new_streak_episode_may_refresh_again_after_a_recovery() {
        // AC-4 boundary: the once-per-episode guard is per-STREAK, not per-lifetime. A 401
        // refreshes (persists → streak = 1); the streak then RESETS on a live poll, closing
        // the episode; a LATER 401 opens a fresh episode allowed one more refresh.
        let (mut daemon, outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed, // succeeds but does not auto-revive
            None,
            false,
            3,
        )
        .await;
        daemon.tick().await; // tick 1: work (healthy)
        daemon.tick().await; // tick 2: spare 401 → refresh (calls = 1), streak = 1
        assert_eq!(calls.get(), 1);
        assert_eq!(daemon.state.health[1].consec_401, 1);
        // Heal the spare: its next poll is Live → the streak resets, closing the episode.
        outcomes
            .borrow_mut()
            .insert("u-B".to_owned(), Scripted::Ok(reading(0.10, 0.10)));
        daemon.tick().await; // tick 3: work
        daemon.tick().await; // tick 4: spare Live → streak resets to 0
        assert_eq!(daemon.state.health[1].consec_401, 0);
        assert_eq!(calls.get(), 1, "a live poll needs no refresh");
        // Break the spare again → the next spare 401 is a NEW episode → one more refresh.
        outcomes
            .borrow_mut()
            .insert("u-B".to_owned(), Scripted::Unauthorized);
        daemon.tick().await; // tick 5: work
        daemon.tick().await; // tick 6: spare 401 (consec 0) → refresh AGAIN (calls = 2)
        assert_eq!(
            calls.get(),
            2,
            "a fresh streak episode is allowed one more refresh",
        );
    }

    #[tokio::test]
    async fn a_healthy_poll_path_never_refreshes_or_quarantines() {
        // The seam is inert on the happy path — a never-401 account triggers no refresh (no
        // `claude -p` spawn) and never quarantines, so the fix costs the common case nothing.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.20, 0.10)), // spare polls healthy from the start
            RefreshOutcome::Refreshed,
            None,
            false,
            3,
        )
        .await;
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(!daemon.state.health[1].quarantined);
        assert_eq!(
            calls.get(),
            0,
            "a healthy poll path never invokes the refresh seam",
        );
    }

    #[tokio::test]
    async fn the_active_account_is_never_isolated_refreshed_on_a_401() {
        // Issue #253: the #162 refresh-then-retry must NEVER isolated-refresh the ACTIVE account.
        // The #102 engine performs a real OAuth exchange that ROTATES the refresh token
        // server-side (`refresh.rs` Caller contract), invalidating the canonical credential every
        // live Claude Code session reads — the exact hazard #105/`refresh_exclusions` and
        // #250/`poke` already guard. The existing seam tests only drive the non-active `spare`;
        // this covers the active account. Two rotation-INDEPENDENT defects must hold
        // deterministically:
        //   1. caller-contract: the active account is never handed to `engine.refresh` at all
        //      (`calls == 0`), and
        //   2. no masking: its surviving 401 ADVANCES the #42 streak (toward operator re-login),
        //      never a stash re-poll that resets the streak and marks it healthy.
        // `revive_to` is set so that WITHOUT the fix the masking is sharp (a refresh would revive
        // `work` and reset its streak); WITH the fix the refresh never fires, so it is inert.
        let (mut daemon, outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)), // spare stays healthy — isolate the active path
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // would (wrongly) revive `work` IF it were refreshed
            false,
            3,
        )
        .await;
        // Re-script the ACTIVE account (`work` / u-A, idx 0) to 401.
        outcomes
            .borrow_mut()
            .insert("u-A".to_owned(), Scripted::Unauthorized);
        // `work` is polled on ticks 1 and 3 (staggered schedule #80) → two surviving 401s.
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert_eq!(
            calls.get(),
            0,
            "#253: the active account must NEVER be isolated-refreshed — its live session reads \
             the canonical credential the #102 refresh would rotate server-side",
        );
        assert_eq!(
            daemon.state.health[0].consec_401, 2,
            "#253: a still-active account's 401 advances the #42 streak toward operator re-login, \
             never a stash-only refresh + re-poll that resets the streak and masks it healthy",
        );
    }

    #[tokio::test]
    async fn a_reactive_refresh_of_a_swap_target_cannot_race_the_promotion() {
        // Issue #426 council falsifier. The #162 reactive engine is now ALWAYS wired (hoisted out
        // of `[refresh].enabled`), so the swap-race must be proven safe: a reactive refresh must
        // never leave an account that is promoted to active THIS TICK holding a torn canonical.
        // The adversarial tick does BOTH at once — reactively refresh a PARKED account on its 401
        // AND promote that same account to active. It is safe by construction:
        //   - the refresh fires while the target is still PARKED (`state.active != Some(i)`,
        //     token-first #207): no live session reads its token, so the isolated engine writing
        //     only its STASH (#253) harms nothing; and
        //   - the swap runs STRICTLY AFTER the refresh in the single-threaded tick (`refresh_retry`
        //     at the poll seam, THEN `decide_action`) and promotes FROM THAT SAME STASH
        //     (`incoming = target.stash()`, read back in `record_swap`) — so the canonical a live
        //     session reads post-swap is exactly the token the refresh left in the stash, never a
        //     torn / stale one. There is no in-tick ordering that promotes the account FIRST and
        //     then reactively refreshes its now-live canonical.
        let (mut daemon, outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,    // spare (u-B): parked, its access token 401s…
            RefreshOutcome::Refreshed, // …the isolated refresh succeeds…
            Some(reading(0.10, 0.20)), // …reviving it to a VIABLE, below-floor swap target.
            false,
            3,
        )
        .await;
        // work (u-A) is the ACTIVE account, carried OVER its session trigger (0.97 > 0.95) — so the
        // very tick that revives the spare also decides to swap AWAY from work, TO the spare.
        outcomes
            .borrow_mut()
            .insert("u-A".to_owned(), Scripted::Ok(reading(0.97, 0.40)));

        // Tick 1 polls the active work (over trigger, but not yet warmed up → HELD). Tick 2 polls
        // the parked spare: its 401 fires the reactive refresh (revive), and the now-warmed
        // decision swaps work → spare in the SAME tick.
        let _ = daemon.tick().await; // work polled → HELD (pre-warm-up)
        let swap_tick = daemon.tick().await; // spare 401 → reactive refresh → swapped-to

        assert_eq!(
            swap_tick.action,
            TickAction::Swapped { from: 0, to: 1 },
            "the tick that reactively refreshes the parked spare also promotes it — the exact \
             swap-race the falsifier stresses",
        );
        assert_eq!(
            calls.get(),
            1,
            "exactly ONE reactive refresh fired — the PARKED spare's; the active work account was \
             never isolated-refreshed (the #253 / token-first #207 exclusion held throughout)",
        );
        assert_eq!(
            daemon.state.active,
            Some(1),
            "the spare was promoted to the active account",
        );
        // The falsifier's core: the promoted canonical is the token the swap read from the spare's
        // STASH — the very stash the reactive refresh owns (and, with the real engine, CAS-wrote the
        // fresh post-rotation token to one step earlier). A torn race would leave the canonical
        // holding work's OLD token (no promotion) or a stale value; it holds the spare's stash.
        assert!(
            daemon.store.read().await.unwrap().matches(&cred(b"B-token")),
            "the canonical a live session reads holds the spare's stash token, promoted coherently \
             AFTER the reactive refresh — never a torn write to a live credential",
        );
        // Cross-tick ordering: now that the spare is ACTIVE, a later 401 on it can NEVER be
        // reactively refreshed (token-first #207 excludes the active account), so the feared
        // swap-then-refresh-the-now-active ordering cannot arise on a subsequent tick either.
        assert!(
            !daemon.should_refresh_retry(1, &Err(Error::UsageUnauthorized)),
            "the newly-promoted active account is excluded from reactive refresh going forward",
        );
    }

    /// Drive the #162 seam to exactly ONE poll-refresh firing (issue #255) and return the
    /// durable events the REFRESHING tick emitted: tick 1 polls `work` (healthy, seam inert),
    /// tick 2 is `spare`'s first 401 → [`refresh_retry`](Daemon::refresh_retry) → the
    /// `Event::PollRefresh` under test. `refresh_outcome` is what the fake engine reports;
    /// `hard_error` makes the refresh itself fail (the fail-safe `Error` path).
    async fn poll_refresh_tick_events(
        refresh_outcome: RefreshOutcome,
        hard_error: bool,
    ) -> Vec<Event> {
        let (mut daemon, _outcomes, _calls) =
            seam_daemon(Scripted::Unauthorized, refresh_outcome, None, hard_error, 3).await;
        daemon.tick().await; // tick 1: work (healthy) — the seam stays inert
        daemon.tick().await.events // tick 2: spare's first 401 → the poll-refresh fires
    }

    #[tokio::test]
    async fn a_poll_refresh_emits_one_durable_event_per_outcome_branch() {
        // AC (issue #255): every #162 poll-refresh firing emits ONE durable `Event::PollRefresh`
        // carrying the target PARKED account (redacted handle) and the classified refresh outcome
        // — the isolated-refresh ACTION the durable log lacked (only the DOWNSTREAM poll outcome
        // was evented, via `note_poll_outcome`). One firing per outcome branch, asserted like the
        // `Monitor401` / `ReStash` event tests. The event also carries the cycle's rotation flag
        // (issue #279): a real `Refreshed` threads `rotated=true` from the report, while an engine
        // that could not run (`hard_error`) forces `false` via the `Err(_) => false` branch.
        let cases = [
            // (fake engine report outcome, hard engine error?, expected evented outcome)
            (
                RefreshOutcome::Refreshed,
                false,
                RefreshEventOutcome::Refreshed,
            ),
            (
                RefreshOutcome::NoChange,
                false,
                RefreshEventOutcome::NoChange,
            ),
            (RefreshOutcome::Dead, false, RefreshEventOutcome::Dead),
            (
                RefreshOutcome::Error(crate::refresh::RefreshErrorReason::SpawnFailed),
                false,
                RefreshEventOutcome::Error,
            ),
            // The engine could not even RUN (spawn / lock failure): the fail-safe `Error`
            // outcome, mirroring `refresh_tick`'s `error_refresh_event`. The report `outcome`
            // is unused on this path, so any value stands in.
            (RefreshOutcome::Refreshed, true, RefreshEventOutcome::Error),
        ];
        for (report_outcome, hard_error, expected) in cases {
            let events = poll_refresh_tick_events(report_outcome, hard_error).await;
            let poll_refreshes = events
                .iter()
                .filter(|e| matches!(e, Event::PollRefresh { .. }))
                .cloned()
                .collect::<Vec<_>>();
            // The rotation flag threads from the cycle's report on the Ok path (a real
            // `Refreshed` rotates in the seam fake), and is forced `false` when the engine
            // could not even run (`hard_error` → the `Err(_) => false` branch of #279).
            let expected_rotated =
                matches!(report_outcome, RefreshOutcome::Refreshed) && !hard_error;
            assert_eq!(
                poll_refreshes,
                vec![Event::PollRefresh {
                    account: "spare".to_owned(),
                    outcome: expected,
                    refresh_token_rotated: expected_rotated,
                }],
                "report {report_outcome:?} (hard_error={hard_error}) must emit exactly one \
                 poll_refresh event with the redacted handle + mapped outcome + rotation flag",
            );
        }
    }

    // --- #282 in-place ACTIVE-account keep-warm (the FOURTH refresh mechanism) ----
    //
    // The active account's canonical token is kept warm IN PLACE: minted via the isolated
    // spawn on a COPY of the canonical blob, then PROMOTED to the canonical item a live
    // session reads (never the STASH the #253-excluded engine writes). Two firing paths —
    // PROACTIVE (before the token nears expiry) and REACTIVE (a backstop on an active 401,
    // reviving before the 401 counts toward the #42 streak) — plus a per-account STAGGER that
    // de-correlates the roster's mints across the shared ~8h TTL. These tests exercise the
    // seam directly (the near-expiry gate + throttle are pure functions of an injected
    // `now_ms` / [`FakeClock`]) and end-to-end through `tick`.

    #[test]
    fn real_keep_warm_engine_resolves_the_binary_per_cycle_not_frozen_at_construction() {
        // Issue #375, the #282 keep-warm engine's half of the fix (sibling to `refresh_tick`'s
        // `RealRefreshEngine` test). `RealKeepWarmEngine` holds the `[refresh].claude_bin` OVERRIDE
        // and resolves the spawn binary PER CYCLE, so a mid-run symlink re-point is picked up on the
        // next keep-warm with no daemon restart. Built ONCE, resolved across a re-point: the
        // frozen-at-startup design this fixes could only ever return its first result.
        let tmp = tempfile::tempdir().unwrap();
        let installed = tmp.path().join("claude-installed");
        std::fs::write(&installed, b"#!/bin/sh\n").unwrap();
        let link = tmp.path().join("claude");
        std::os::unix::fs::symlink(&installed, &link).unwrap();

        let engine = RealKeepWarmEngine::new(Some(link.clone()));

        // Cycle 1: link → installed (exists) → Ok, returning the symlink path UNCANONICALIZED
        // (issue constraint [C1]: a wrapper symlink is spawned as-is, never resolved to its target).
        assert_eq!(engine.resolve_binary().unwrap(), link);

        // The updater removes the pointed-at binary: the SAME engine resolves to a NON-FATAL error
        // on its next cycle (the daemon leaves the canonical item untouched, retried next cycle),
        // never a reuse of a stale frozen path.
        std::fs::remove_file(&installed).unwrap();
        assert!(matches!(
            engine.resolve_binary(),
            Err(crate::error::Error::ClaudeBinaryNotFound)
        ));
    }

    /// A far-future access-token expiry (epoch ms, ~year 2100): well beyond any keep-warm
    /// horizon, so a canonical carrying it never trips the PROACTIVE near-expiry gate — used to
    /// ISOLATE the reactive path in the `tick`-driven tests (only the scripted 401 fires it).
    const FAR_FUTURE_MS: i64 = 4_102_444_800_000;

    /// A realistic active canonical blob: the non-secret `expiresAt` (epoch ms) beside a
    /// `refreshToken` — an EMPTY `refresh_token` is the DEAD signal (`has_live_refresh_token`
    /// returns false), the invariant-4 case the keep-warm must NOT try to revive.
    fn warm_canonical(expires_at_ms: i64, refresh_token: &str) -> Credential {
        cred(
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat-ACTIVE00SECRET","refreshToken":"{refresh_token}","expiresAt":{expires_at_ms}}}}}"#
            )
            .as_bytes(),
        )
    }

    /// A [`KeepWarm`] fake for the #282 seam tests: it COUNTS mint calls (asserting the proactive
    /// throttle and the once-per-episode reactive guard), returns a scripted [`RefreshOutcome`]
    /// with a `fresh` credential to promote (on `Refreshed`), and — when `revive_to` is set —
    /// REVIVES the active account by flipping its shared [`SeamOutcomes`] entry to a live reading
    /// (the false-death the reactive backstop rescues).
    struct SeamKeepWarm {
        outcomes: SeamOutcomes,
        outcome: RefreshOutcome,
        revive_to: Option<Usage>,
        fresh: Option<Credential>,
        calls: Rc<Cell<u32>>,
    }

    impl KeepWarm for SeamKeepWarm {
        fn keep_warm<'a>(
            &'a self,
            account: &'a Account,
            _canonical: &'a Credential,
        ) -> Pin<Box<dyn Future<Output = Result<KeepWarmMint>> + 'a>> {
            Box::pin(async move {
                self.calls.set(self.calls.get() + 1);
                if let Some(usage) = self.revive_to {
                    self.outcomes
                        .borrow_mut()
                        .insert(account.account_uuid.clone(), Scripted::Ok(usage));
                }
                // A fresh credential to promote ONLY on a real refresh; every other outcome
                // hands back `None` and the daemon leaves the canonical item untouched.
                let credential = match self.outcome {
                    RefreshOutcome::Refreshed => self.fresh.clone(),
                    _ => None,
                };
                Ok((
                    RefreshReport {
                        outcome: self.outcome,
                        expires_at_delta_secs: None,
                        // Only a real exchange rotates the RT; NoChange / Dead / Error never do.
                        refresh_token_rotated: matches!(self.outcome, RefreshOutcome::Refreshed),
                        // A keep-warm PROMOTES rather than re-stashes — never `re_stashed`.
                        re_stashed: false,
                    },
                    credential,
                ))
            })
        }
    }

    /// A two-account keep-warm daemon (issue #282): `work` (`u-A`) is the ACTIVE account under
    /// test, holding `canonical`; `spare` (`u-B`) polls healthy so an emergency swap has a
    /// viable target (the invariant-4 escape). The `SeamKeepWarm` engine is wired with a 1-hour
    /// cadence (the near-expiry horizon + proactive throttle). Returns the daemon plus the shared
    /// outcome cell (to script the active poll / observe a revive) and the mint call-counter.
    async fn keep_warm_daemon(
        active_outcome: Scripted,
        keep_warm_outcome: RefreshOutcome,
        revive_to: Option<Usage>,
        fresh: Option<Credential>,
        canonical: Credential,
    ) -> (
        Daemon<SeamPoller, FakeCredentialStore, FakeAccountStash, FakeClock>,
        SeamOutcomes,
        Rc<Cell<u32>>,
    ) {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = FakeCredentialStore::empty();
        store.write(&canonical).await.unwrap();
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        // The display resolves the active account to `u-A` (the canonical blob token-matches no
        // stash, so resolution falls to the display — exactly the active-account signal #207 uses).
        let (dir, json) = claude_json("u-A");
        std::mem::forget(dir);
        let tun = tunables(95, 80, 0);
        let outcomes: SeamOutcomes = Rc::new(RefCell::new(HashMap::from([
            ("u-A".to_owned(), active_outcome),
            ("u-B".to_owned(), Scripted::Ok(reading(0.10, 0.10))),
        ])));
        let calls = Rc::new(Cell::new(0u32));
        let daemon = Daemon::new(
            roster,
            SeamPoller {
                outcomes: outcomes.clone(),
            },
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
        .with_keep_warm_engine(
            Box::new(SeamKeepWarm {
                outcomes: outcomes.clone(),
                outcome: keep_warm_outcome,
                revive_to,
                fresh,
                calls: calls.clone(),
            }),
            Duration::from_secs(3600),
        );
        (daemon, outcomes, calls)
    }

    #[tokio::test]
    async fn should_keep_warm_retry_is_the_active_only_complement_of_the_162_guard() {
        // The reactive backstop fires on exactly the case #253's `should_refresh_retry` EXCLUDES:
        // the ACTIVE account, first 401 of an episode, seam wired, not quarantined. The two guards
        // partition a 401 by active-ness, so a 401 takes exactly one refresh path.
        let (mut daemon, _outcomes, _calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        let unauthorized = Err(Error::UsageUnauthorized);

        // Active (idx 0), first 401, seam wired, not quarantined → the keep-warm fires; and the
        // #162 isolated path does NOT (its active exclusion), so they never both fire.
        daemon.state.active = Some(0);
        assert!(daemon.should_keep_warm_retry(0, &unauthorized));
        assert!(!daemon.should_refresh_retry(0, &unauthorized));

        // NON-active (idx 1) is the #162 path's job, never the keep-warm's.
        assert!(!daemon.should_keep_warm_retry(1, &unauthorized));

        // A non-401 outcome never fires it.
        assert!(!daemon.should_keep_warm_retry(0, &Ok(reading(0.1, 0.1))));

        // Past the first 401 of the episode (`consec_401 > 0`) → suppressed (no mint storm; the
        // rest of the episode advances the streak directly toward the #42 emergency swap).
        daemon.state.health[0].consec_401 = 1;
        assert!(!daemon.should_keep_warm_retry(0, &unauthorized));
        daemon.state.health[0].consec_401 = 0;

        // A quarantined active account is the streak's job — never re-warmed every re-probe poll.
        daemon.state.health[0].quarantined = true;
        assert!(!daemon.should_keep_warm_retry(0, &unauthorized));
    }

    #[tokio::test]
    async fn should_keep_warm_retry_is_inert_without_a_wired_seam() {
        // With no keep-warm engine wired (the default / `[refresh]`-off daemon) an active 401 is
        // NEVER a keep-warm — the active account simply lapses exactly as before the fix.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.active = Some(0);
        assert!(daemon.keep_warm.is_none());
        assert!(!daemon.should_keep_warm_retry(0, &Err(Error::UsageUnauthorized)));
    }

    #[tokio::test]
    async fn a_reactive_backstop_revives_an_active_401_and_promotes_the_canonical() {
        // AC-2 (positive): the active account's first 401, with a LIVE refresh token, mints a
        // fresh token in place, PROMOTES it to the canonical item, re-polls through it, and the
        // re-poll clears — so the #42 streak never advances (the false-death this fixes). The
        // canonical now holds the FRESH token a live session reads.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // the mint REVIVES the active token
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        let mut events = Vec::new();
        let result = daemon
            .keep_warm_retry(
                0,
                Some(&warm_canonical(FAR_FUTURE_MS, "rt-live")),
                &mut events,
            )
            .await;

        assert!(
            result.is_ok(),
            "a revived active 401 re-polls to a live reading"
        );
        assert_eq!(calls.get(), 1, "exactly one mint fired");
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the fresh token was promoted to the canonical item a live session reads",
        );
        assert_eq!(
            events,
            vec![Event::KeepWarm {
                account: "work".to_owned(),
                trigger: KeepWarmTrigger::Reactive,
                // A keep-warm promotes, so a real refresh renders `refreshed_not_restashed`.
                outcome: RefreshEventOutcome::RefreshedNotReStashed,
                refresh_token_rotated: true,
            }],
            "one durable keep_warm event records the reactive mint",
        );
    }

    #[tokio::test]
    async fn a_dead_refresh_token_active_401_advances_the_streak_without_minting() {
        // AC-2 / invariant 4 (empty-RT): a dead (empty) refresh token cannot be revived by any
        // mint, so the keep-warm SHORT-CIRCUITS — no `claude -p` spawn — and the 401 stands, so
        // the streak advances toward the #42 emergency swap. A truly-dead active credential still
        // quarantines; the escape to a live spare is preserved.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // would revive IF the mint ever ran — it must not
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, ""), // EMPTY refresh token → dead
        )
        .await;
        let mut events = Vec::new();
        let result = daemon
            .keep_warm_retry(0, Some(&warm_canonical(FAR_FUTURE_MS, "")), &mut events)
            .await;

        assert!(
            matches!(result, Err(Error::UsageUnauthorized)),
            "a dead-RT active 401 stands so the #42 streak advances (invariant 4)",
        );
        assert_eq!(
            calls.get(),
            0,
            "a dead refresh token skips the doomed mint spawn"
        );
        assert!(
            events.is_empty(),
            "a skipped keep-warm emits no action event"
        );
        assert_ne!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the canonical item is left untouched when there is nothing to revive",
        );
    }

    #[tokio::test]
    async fn a_reactive_mint_reporting_dead_advances_the_streak() {
        // AC-2 / invariant 4 (Dead outcome): the RT was non-empty at mint time (so the mint DOES
        // run), but CC cleared it in place and the cycle reports `Dead` — a genuine death. No
        // promote, the 401 stands, the streak advances. The mint fired exactly once.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Dead,
            Some(reading(0.10, 0.10)), // a Dead outcome hands back no credential regardless
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        let mut events = Vec::new();
        let result = daemon
            .keep_warm_retry(
                0,
                Some(&warm_canonical(FAR_FUTURE_MS, "rt-live")),
                &mut events,
            )
            .await;

        assert!(
            matches!(result, Err(Error::UsageUnauthorized)),
            "a Dead mint lets the 401 stand so the streak advances (invariant 4)",
        );
        assert_eq!(
            calls.get(),
            1,
            "the mint ran once (the RT was non-empty) and reported Dead"
        );
        assert_ne!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "a Dead outcome promotes nothing",
        );
        assert_eq!(
            events,
            vec![Event::KeepWarm {
                account: "work".to_owned(),
                trigger: KeepWarmTrigger::Reactive,
                outcome: RefreshEventOutcome::Dead,
                refresh_token_rotated: false,
            }],
        );
    }

    #[tokio::test]
    async fn a_reactive_backstop_end_to_end_never_quarantines_a_revivable_active_401() {
        // AC-2 end-to-end through `tick`: the active `work` account 401s on its first poll; the
        // reactive keep-warm mints + promotes + re-polls, the re-poll clears, and `work` is never
        // quarantined. The FAR-FUTURE expiry keeps the proactive path inert, isolating the reactive
        // one. The mint fired once (no storm) and the canonical carries the fresh token.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)),
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        // Tick 1 polls the active `work` first (issue #80 stagger) → its first 401 → the reactive
        // backstop revives it in place.
        let events = daemon.tick().await.events;

        assert!(
            !daemon.state.health[0].quarantined,
            "a revivable active 401 is kept warm in place, never quarantined",
        );
        assert_eq!(
            daemon.state.health[0].consec_401, 0,
            "the cleared re-poll resets the streak",
        );
        assert_eq!(calls.get(), 1, "exactly one mint (no storm)");
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the canonical item now holds the fresh token",
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::KeepWarm {
                    trigger: KeepWarmTrigger::Reactive,
                    ..
                }
            )),
            "the tick emitted the reactive keep_warm action: {events:?}",
        );
    }

    #[tokio::test]
    async fn the_proactive_path_mints_within_the_near_expiry_horizon_and_promotes() {
        // AC-1: a token INSIDE its (staggered) near-expiry horizon is minted BEFORE any 401 and
        // the fresh token promoted to the canonical item — so a live session always reads a warm
        // token and the overnight false-death cascade never starts.
        let now_ms = 1_800_000_000_000;
        // 60 s to expiry, well inside the 1-hour+stagger horizon.
        let canonical = warm_canonical(now_ms + 60_000, "rt-live");
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            canonical.clone(),
        )
        .await;
        let mut events = Vec::new();
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;

        assert_eq!(
            calls.get(),
            1,
            "a near-expiry active token is minted proactively"
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the proactive mint promotes the fresh token to the canonical item",
        );
        assert!(
            daemon.state.health[0].last_keep_warm_attempt.is_some(),
            "the attempt is stamped for the proactive throttle",
        );
        assert_eq!(
            events,
            vec![Event::KeepWarm {
                account: "work".to_owned(),
                trigger: KeepWarmTrigger::Proactive,
                outcome: RefreshEventOutcome::RefreshedNotReStashed,
                refresh_token_rotated: true,
            }],
        );
    }

    #[tokio::test]
    async fn the_proactive_path_skips_far_from_expiry_and_when_quarantined() {
        // AC-1 (negative) / no storm: a token far from expiry is NOT minted (nothing to warm yet),
        // and a quarantined active account is never re-warmed (the #42 streak owns it).
        let now_ms = 1_800_000_000_000;
        let canonical = warm_canonical(now_ms + 60_000, "rt-live"); // near expiry…
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            canonical.clone(),
        )
        .await;
        let mut events = Vec::new();

        // Far from expiry (100 days out) → skip.
        let far = warm_canonical(now_ms + 100 * 86_400_000, "rt-live");
        daemon
            .keep_active_warm(Some(0), Some(&far), now_ms, &mut events)
            .await;
        assert_eq!(calls.get(), 0, "a far-from-expiry token is not warmed");

        // …but even a near-expiry token is skipped once the account is quarantined.
        daemon.state.health[0].quarantined = true;
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;
        assert_eq!(
            calls.get(),
            0,
            "a quarantined active account is never re-warmed"
        );
        assert!(events.is_empty(), "no mint → no event");
    }

    #[tokio::test]
    async fn the_proactive_throttle_admits_one_mint_per_cadence() {
        // No storm: while the token sits in the near-expiry window, the proactive path mints at
        // most once per keep-warm cadence — so a persistently no-op mint cannot spawn `claude -p`
        // every tick. The throttle RELEASES once the cadence elapses.
        let now_ms = 1_800_000_000_000;
        let canonical = warm_canonical(now_ms + 60_000, "rt-live");
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            canonical.clone(),
        )
        .await;
        let mut events = Vec::new();

        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;
        // A second attempt at the SAME instant (frozen clock) is throttled.
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;
        assert_eq!(
            calls.get(),
            1,
            "a second mint inside the cadence is throttled"
        );

        // Once the cadence elapses, the next attempt mints again.
        daemon.clock.advance(Duration::from_secs(3601));
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;
        assert_eq!(calls.get(), 2, "the throttle releases after one cadence");
    }

    #[tokio::test]
    async fn a_promote_aborts_when_a_swap_raced_the_mint() {
        // Invariant 2 (no-torn-swap, ADR-0003): the mint runs WITHOUT the swap lock, so a `use` /
        // auto swap can land meanwhile. Under the lock, `promote_canonical` re-reads the canonical
        // and, finding it no longer resolves to the account it minted for, ABORTS with ZERO writes
        // — never clobbering the concurrent swap.
        let (mut daemon, _outcomes, _calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        // Simulate a swap that landed during the mint: the canonical now token-matches `spare`
        // (`u-B` / idx 1), NOT the active `work` (idx 0) the mint targeted.
        daemon.store.write(&cred(b"B-token")).await.unwrap();

        let promoted = daemon
            .promote_canonical(0, &cred(b"FRESH-A"))
            .await
            .unwrap();
        assert!(
            !promoted,
            "a raced swap aborts the promote (Ok(false), a deliberate no-op)"
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"B-token",
            "the concurrent swap's canonical is left intact — zero writes on abort",
        );
    }

    #[tokio::test]
    async fn a_keep_warm_promote_commits_the_baseline_so_the_140_watch_does_not_misfire() {
        // Invariant 3 (#140 external-login watch): the daemon's OWN in-place canonical write must
        // NOT read back as an operator re-login. `promote_canonical` baseline-commits the fresh
        // credential, so the very next `reconcile_canonical_change` classifies it Unchanged and
        // emits nothing (no ReStash / UncapturedLogin).
        let (mut daemon, _outcomes, _calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        daemon.state.active = Some(0);
        // Seed the watch baseline to the current canonical (as top-of-tick does), then promote.
        let seed = daemon.store.read().await.unwrap();
        daemon.state.canonical_watch.commit(&seed);
        let promoted = daemon
            .promote_canonical(0, &cred(b"FRESH-A"))
            .await
            .unwrap();
        assert!(promoted);

        // The next reconcile against the just-promoted canonical sees NO change → no event.
        let mut events = Vec::new();
        let fresh = daemon.store.read().await.unwrap();
        daemon.reconcile_canonical_change(&fresh, &mut events).await;
        assert!(
            events.is_empty(),
            "the daemon's own keep-warm write must not misfire the #140 watch: {events:?}",
        );
    }

    #[test]
    fn the_keep_warm_stagger_is_deterministic_bounded_and_de_correlated() {
        // AC-3: the per-account stagger de-correlates the roster's keep-warm mints across the
        // shared TTL. It is (a) a deterministic pure function of the uuid — STABLE across restarts,
        // (b) bounded to `[0, cadence)` so no account is ever starved past the `cadence` floor, and
        // (c) DISTINCT across accounts (distinct uuids draw distinct offsets), which is the
        // de-correlation.
        let cadence = Duration::from_secs(3600);
        let a = keep_warm_stagger_secs("u-A", cadence);
        let b = keep_warm_stagger_secs("u-B", cadence);
        let c = keep_warm_stagger_secs("u-C", cadence);

        // Deterministic: same uuid → same offset, every call.
        assert_eq!(
            a,
            keep_warm_stagger_secs("u-A", cadence),
            "stagger is stable per uuid"
        );
        // Bounded to the window (never starves an account past the cadence floor).
        for (uuid, offset) in [("u-A", a), ("u-B", b), ("u-C", c)] {
            assert!(
                offset < 3600,
                "{uuid} stagger {offset} escaped [0, cadence)"
            );
        }
        // De-correlated: distinct accounts draw distinct phases (the whole point).
        assert!(
            a != b && b != c && a != c,
            "distinct uuids must de-correlate: {a}, {b}, {c}",
        );
        // A zero cadence degenerates safely to 0 (no window to stagger within).
        assert_eq!(keep_warm_stagger_secs("u-A", Duration::ZERO), 0);
    }

    #[tokio::test]
    async fn a_near_expiry_active_401_mints_once_not_twice_in_a_tick() {
        // AC-1 "no storm" under the real overnight scenario: when the active token is BOTH inside
        // its near-expiry horizon AND returns a 401 in the same tick, the reactive backstop fires
        // and — because it stamps `last_keep_warm_attempt` — the proactive path that runs later the
        // same tick is THROTTLED. Exactly ONE `claude -p` mint, never two. (Every other keep-warm
        // test isolates one path — a healthy poll for proactive, `FAR_FUTURE_MS` for reactive — so
        // this is the only test that intersects them, the crux of the no-double-mint property.)
        let near = wall_clock_now_ms() + 60_000; // 60 s to expiry → inside the horizon
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized, // the active account 401s this tick
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // the reactive mint revives it
            Some(cred(b"FRESH-A")),
            warm_canonical(near, "rt-live"),
        )
        .await;
        // Tick 1 polls the active `work` first (#80 stagger): 401 → reactive mint → revive →
        // re-poll clears; then the proactive pass runs (near-expiry gate TRUE) but is throttled.
        daemon.tick().await;
        assert_eq!(
            calls.get(),
            1,
            "a near-expiry active 401 mints exactly once — the reactive stamp throttles proactive",
        );
        assert_eq!(
            daemon.state.health[0].consec_401, 0,
            "the revive reset the streak"
        );
        assert!(!daemon.state.health[0].quarantined);
    }

    #[tokio::test]
    async fn a_promote_that_survives_a_still_401ing_token_lets_the_401_stand() {
        // A genuine server-side revocation: the mint reports `Refreshed` and the fresh token IS
        // promoted, but the re-poll through the (now-fresh) canonical STILL 401s — the fresh token
        // did not actually fix the problem. The 401 must stand so the streak advances (never a
        // false "revived" that masks a dead credential). `revive_to = None` keeps the active poll
        // 401ing even after the promote.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            None, // the mint promotes a fresh token but does NOT revive the poll outcome
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        let mut events = Vec::new();
        let result = daemon
            .keep_warm_retry(
                0,
                Some(&warm_canonical(FAR_FUTURE_MS, "rt-live")),
                &mut events,
            )
            .await;

        assert!(
            matches!(result, Err(Error::UsageUnauthorized)),
            "a fresh token that still 401s lets the 401 stand so the streak advances",
        );
        assert_eq!(calls.get(), 1, "the mint ran once");
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the fresh token WAS promoted — the problem is server-side, not a mint failure",
        );
    }

    #[tokio::test]
    async fn a_promote_under_a_configured_swap_lock_acquires_and_writes() {
        // Invariant 2 (production path): with a swap lock configured, `promote_canonical` acquires
        // it (uncontended here), re-reads the canonical under it, confirms the account is still
        // active, and writes the fresh token atomically. The daemon tests otherwise run lock-free
        // (`swap_lock_path = None`); this exercises the `Some(path)` branch the daemon uses in
        // production. (The contended `SwapLockBusy` fail-closed is covered in `swap.rs`.)
        let lock_dir = tempfile::tempdir().unwrap();
        let (mut daemon, _outcomes, _calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        daemon = daemon.with_swap_lock(lock_dir.path().join("swap.lock"));
        daemon.state.active = Some(0);

        let promoted = daemon
            .promote_canonical(0, &cred(b"FRESH-A"))
            .await
            .unwrap();
        assert!(
            promoted,
            "an uncontended locked promote acquires and writes"
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the fresh token was written under the swap lock",
        );
    }

    #[tokio::test]
    async fn a_dead_non_active_account_is_skipped_while_the_rotation_continues() {
        // Quarantine-one (never halt): a dead SPARE is skipped in polling — not a
        // wasted curl, not a swap candidate — while the active account still rotates
        // to a healthy target. The daemon never halts the whole rotation on one dead
        // account.
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "backup"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.10) // active, over the session trigger → wants a swap
            .unauthorized("u-B") // scripted to 401 — but it is dead, so never polled
            .ok("u-C", 0.10, 0.10); // the only healthy target
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // `spare` is already dead from a prior episode.
        daemon.state.health[1].quarantined = true;

        // The staggered schedule (#80) is [work, backup] — the quarantined spare is
        // excluded outright — so the warm-up cycle polls only those two; the swap
        // fires on the warm-up-completing tick.
        let outcome = warmed_tick(&mut daemon).await;

        // The rotation continues: the active account swaps to the healthy `backup`,
        // NOT to the dead `spare` (a quarantined account is never a target).
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 2 });
        // `spare` was skipped, not polled: its 401 script never ran, so its streak
        // stayed 0 and it emitted no `monitor_401`.
        assert_eq!(
            daemon.state.health[1].consec_401, 0,
            "the dead spare was not polled"
        );
        assert!(
            !outcome.events.iter().any(|e| matches!(
                e,
                Event::Monitor401 { account, .. } if account == "spare"
            )),
            "a skipped account emits no poll-outcome event: {:?}",
            outcome.events
        );
    }

    #[tokio::test]
    async fn an_emergency_swap_escapes_a_dead_active_account_bypassing_trigger_and_cooldown() {
        // Emergency-swap: a confirmed-dead ACTIVE account is escaped IMMEDIATELY to
        // the soonest-reset viable target, bypassing BOTH the swap-away trigger (the
        // dead account has no reading to be "over") and the cooldown. A long cooldown
        // plus a just-completed swap would make a NORMAL over-trigger swap
        // `SkippedCooldown`; the emergency path overrides it.
        let mut daemon =
            lifecycle_daemon_with(FakeRosterPoller::new(), tunables(95, 80, 9_999)).await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;
        daemon.state.last_swap = Some(LastSwap {
            at, // zero elapsed against a 9_999s cooldown → a normal swap would defer
        });

        // The dead active has no reading (still 401ing); the spare polled live.
        let readings = vec![
            None,
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::EmergencySwapped { from: 0, to: 1 });
        assert_eq!(
            events,
            vec![Event::EmergencySwap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }]
        );
        // The swap took effect: the spare is now active.
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn emergency_swap_escapes_a_dead_active_ignoring_the_floor() {
        // #398 atomicity: the emergency path drops the target-max-usage reserve. A
        // confirmed-dead ACTIVE account must escape to the ONLY live target even when
        // that target sits OVER the default-on floor (0.80) — liveness beats the
        // reserve. Without the floor-drop (emergency passes `None`, not the configured
        // floor), a default-on floor plus an over-floor live spare would strand the
        // daemon on the dead credential (`ActiveDeadNoTarget`) — a self-DoS. This test
        // gates shipping the default-on flip together with the emergency floor-drop.
        let mut daemon = lifecycle_daemon_with(FakeRosterPoller::new(), tunables(95, 80, 0)).await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;

        // Dead active has no reading; the spare polled live but is OVER the floor
        // (0.85 ≥ 0.80) — the PROACTIVE path would exclude it, the emergency path must
        // not. It is weekly-viable (0.10) and below the session trigger (0.85 < 0.95),
        // so ONLY the floor could have blocked it.
        let readings = vec![
            None,
            Some(Usage {
                session: 0.85,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(
            action,
            TickAction::EmergencySwapped { from: 0, to: 1 },
            "the dead active must escape to the over-floor live spare (floor dropped on emergency)"
        );
        assert_eq!(
            events,
            vec![Event::EmergencySwap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }]
        );
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn a_recovering_active_account_is_held_never_swapped_away() {
        // Thrash-safety / protect-recovery: a quarantined ACTIVE account that is
        // polling live again is the operator's re-login recovering it. Hold — never
        // emergency-swap a credential that now works, never swap away mid-recovery.
        let mut daemon = lifecycle_daemon().await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;

        // The active account polled live (recovering); the spare is also available.
        let readings = vec![
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::Held);
        assert!(
            events.is_empty(),
            "a held recovery emits nothing: {events:?}"
        );
        // `decide_action` never recovers — only `note_poll_outcome` does — so the
        // account is still quarantined here.
        assert!(daemon.state.health[0].quarantined);
        assert_eq!(daemon.state.active, Some(0), "no swap away mid-recovery");
    }

    #[tokio::test]
    async fn a_manual_swap_away_mid_recovery_drops_the_phantom_recovery_probe() {
        // Issue #108: `decide_action` HOLDS the daemon's OWN swap away from a recovering
        // active account (`a_recovering_active_account_is_held_never_swapped_away`), but a
        // manual `use` bypasses that hold. Swapping AWAY from an account mid-recovery
        // turns it into a non-active dead spare — never polled (`build_poll_schedule`) —
        // so its recovery probe would FREEZE below M forever, a phantom partial-progress
        // counter that leaves it durably `needs re-login` while LOOKING mid-recovery.
        // Adopting the manual swap drops the probe so the dead-spare state is honest:
        // still quarantined, no in-flight recovery. This is the control-socket door
        // (`adopt_manual_swap`).
        let mut daemon = lifecycle_daemon().await;
        // `work` (active) is mid-recovery: quarantined, but its OWN token started
        // answering again — 1 of `monitor_recovery_m` = 2 live polls accrued.
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;
        daemon.state.health[0].recovery_successes = 1;

        // The operator runs `use spare`: the canonical now holds spare's token and the
        // control socket signals the daemon to adopt the manual choice.
        daemon.store.write(&cred(b"B-token")).await.unwrap();
        daemon.adopt_manual_swap().await;

        assert_eq!(daemon.state.active, Some(1), "the manual choice is adopted");
        assert!(
            daemon.state.health[0].quarantined,
            "still dead — a swap-away never recovers an account"
        );
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the frozen probe is dropped — no phantom partial progress (#108)"
        );
        // The departed account is now indistinguishable from any other dead spare: still
        // quarantined, but with no in-flight recovery on a slot that is never polled.
    }

    #[tokio::test]
    async fn the_reconcile_seam_also_drops_a_mid_recovery_probe_on_a_detected_swap_away() {
        // Issue #108, second door: when the daemon NOTICES the out-of-band canonical
        // change itself (no control-socket signal reached `adopt_manual_swap`) the same
        // reset must fire. `reconcile_canonical_change` re-resolves active to the swap-TO
        // account and drops the departing mid-recovery account's frozen probe, while
        // leaving the swap-TO account untouched.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;
        daemon.state.health[0].recovery_successes = 1;

        let mut events = Vec::new();
        // Prime the watch on `work`'s current canonical (A-token): first observation,
        // no change detected, nothing reset.
        daemon
            .reconcile_canonical_change(&cred(b"A-token"), &mut events)
            .await;
        assert_eq!(
            daemon.state.health[0].recovery_successes, 1,
            "priming the watch changes no health"
        );

        // The canonical now holds spare's token — an out-of-band manual swap the daemon
        // detects on its own. Reconcile re-stashes spare and drops work's frozen probe.
        daemon
            .reconcile_canonical_change(&cred(b"B-token"), &mut events)
            .await;

        assert!(
            daemon.state.health[0].quarantined,
            "work is still dead — a detected swap-away never recovers it"
        );
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the frozen probe is dropped on the reconcile seam too (#108)"
        );
        assert_eq!(
            daemon.state.active, None,
            "active is dropped for re-resolution against the new canonical"
        );
        // The swap-TO account (`spare`) is healthy and untouched by the probe reset.
        assert!(!daemon.state.health[1].quarantined);
        assert_eq!(daemon.state.health[1].recovery_successes, 0);
    }

    #[tokio::test]
    async fn the_reconcile_seam_drops_the_stale_active_on_an_unresolvable_canonical() {
        // Issue #208, the None-branch counterpart to the swap-away test above: a forced
        // logout / `/login` into an UN-CAPTURED account makes the canonical resolve to NO
        // roster account (`Changed → None`). The stale cached `state.active` must be
        // dropped — mirroring the re-stash (`Changed → Some`) branch — so `status` stops
        // showing a false `*` on the now-inactive account and `decide_action` routes to
        // the safe `SkippedActiveUnknown` path instead of acting on a phantom index.
        // Before the fix the None-branch committed the watch baseline WITHOUT resetting
        // `state.active`, so the stale index survived precisely when the operator trusts
        // `status` during an incident.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.active = Some(0);

        let mut events = Vec::new();
        // Prime the watch on `work`'s current canonical (A-token): first observation, no
        // change detected, the resolved active left in place.
        daemon
            .reconcile_canonical_change(&cred(b"A-token"), &mut events)
            .await;
        assert_eq!(
            daemon.state.active,
            Some(0),
            "priming the watch leaves the resolved active untouched"
        );

        // The canonical now holds a token no stash matches AND the display switches to a
        // uuid not in the roster, so it resolves to no roster account (the None-branch).
        crate::claude_state::write_oauth_account(&daemon.claude_json, &oauth("u-Z")).unwrap();
        daemon
            .reconcile_canonical_change(&cred(b"Z-token"), &mut events)
            .await;

        assert_eq!(
            daemon.state.active, None,
            "the stale active is dropped when the canonical resolves to no roster account (#208)"
        );
        // The un-captured login is still surfaced (never onboarded) — the None-branch's
        // existing behavior is preserved alongside the active reset.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::UncapturedLogin { .. })),
            "the un-captured login is still surfaced: {events:?}"
        );

        // Consequence (AC-3): with the active now unknown, `decide_action` takes the safe
        // poll-only path and fires NO emergency swap on a phantom index.
        let at = daemon.clock.now();
        let active = daemon.state.active;
        let readings = vec![None, None];
        let mut decide_events = Vec::new();
        let action = daemon
            .decide_action(at, active, &readings, &mut decide_events)
            .await;
        assert_eq!(action, TickAction::SkippedActiveUnknown);
        assert!(
            decide_events.is_empty(),
            "no swap fires on an unknown active: {decide_events:?}"
        );
    }

    #[tokio::test]
    async fn a_dead_active_account_with_no_viable_target_holds_silently() {
        // Emergency-swap with nowhere to go: a dead active account whose only other
        // account is also unavailable holds (`ActiveDeadNoTarget`) without thrashing
        // — and silently, because `credential_dead` already fired on the transition.
        let mut daemon = lifecycle_daemon().await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;

        // No other account has a reading → no viable target.
        let readings = vec![None, None];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::ActiveDeadNoTarget);
        assert!(
            events.is_empty(),
            "the stuck dead-active state re-signals nothing: {events:?}"
        );
        assert_eq!(daemon.state.active, Some(0), "no swap with no target");
    }

    #[tokio::test]
    async fn m_consecutive_live_polls_recover_a_quarantined_account_and_signal_once() {
        // Spontaneous-revival auto-recovery (no re-login): a dead ACTIVE account whose
        // own token starts answering again un-quarantines after M consecutive live
        // polls, emitting exactly one `credential_restored` on the dead→alive edge. (A
        // re-login takes the immediate #107 path in reconcile_canonical_change instead —
        // see `a_relogin_un_quarantines_a_dead_account_immediately_on_restash`.)
        let mut daemon = lifecycle_daemon().await;
        daemon.state.health[0].quarantined = true;
        let mut events = Vec::new();

        // The first live poll while quarantined is a recovery PROBE — still dead,
        // and silent (below `monitor_recovery_m` = 2).
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(daemon.state.health[0].quarantined);
        assert_eq!(daemon.state.health[0].recovery_successes, 1);
        assert!(events.is_empty());

        // The 2nd consecutive live reaches the threshold → RESTORED (one event).
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(!daemon.state.health[0].quarantined);
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the probe resets on restore"
        );
        assert_eq!(
            events,
            vec![Event::CredentialRestored {
                account: "work".to_owned(),
            }]
        );

        // A later live on the now-healthy account emits nothing (edge-triggered).
        events.clear();
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn a_401_mid_recovery_resets_the_probe_so_recovery_must_restart() {
        // The recovery streak is consecutive: a 401 partway through breaks it, so a
        // single later live is NOT enough — a full M=2 fresh live polls are required.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.health[0].quarantined = true;
        let mut events = Vec::new();

        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events); // probe at 1
        assert_eq!(daemon.state.health[0].recovery_successes, 1);
        // A 401 mid-recovery breaks the streak (and is silent — already dead).
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the 401 reset the probe"
        );
        assert!(daemon.state.health[0].quarantined);
        assert!(events.is_empty());

        // One live after the reset is not enough; the second crosses the threshold.
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(
            daemon.state.health[0].quarantined,
            "one live after a reset is not enough"
        );
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(!daemon.state.health[0].quarantined);
        assert_eq!(
            events,
            vec![Event::CredentialRestored {
                account: "work".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn a_quarantined_account_surfaces_a_durable_needs_relogin_status() {
        // Signal — the durable status: a dead account is reported `quarantined` in
        // the `status` snapshot and on the wire, carrying a stable handle but no
        // token and no email (#15).
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10); // active holds
        let mut daemon = lifecycle_daemon_with(poller, tunables(95, 80, 0)).await;
        daemon.state.health[1].quarantined = true; // `spare` is dead

        let outcome = daemon.tick().await;

        let spare = &outcome.snapshot.accounts[1];
        assert_eq!(spare.label, "spare");
        assert!(
            spare.quarantined,
            "the dead account carries a durable status"
        );
        // The wire projection carries the flag but never a secret. A genuinely dead
        // account (quarantined, NOT mid-recovery) projects `recovering: false` (#109).
        assert!(!spare.recovering, "a dead account is not yet recovering");
        let json = serde_json::to_string(&status_response(&outcome.snapshot)).unwrap();
        assert!(json.contains(r#""quarantined":true"#), "got {json}");
        assert!(json.contains(r#""recovering":false"#), "got {json}");
        assert!(!json.contains('@'), "no email on the wire: {json}");
        assert!(!json.to_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn a_mid_recovery_account_surfaces_recovering_on_the_wire() {
        // Issue #109: a quarantined account whose credential is answering again —
        // `recovery_successes > 0` but below the un-quarantine threshold — is reported
        // `recovering` in the snapshot and on the wire, a refinement of `quarantined`
        // (still true) that lets `status` soften `needs re-login` to `recovering`.
        // Non-secret like every other status field (#15). Built through the real
        // `note_poll_outcome` → `snapshot` derivation, not a hand-set flag.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true; // `work` is dead…

        // …but its OWN token answers one live probe: still quarantined (below
        // monitor_recovery_m = 2), now mid-recovery.
        let mut events = Vec::new();
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(daemon.state.health[0].quarantined);
        assert_eq!(daemon.state.health[0].recovery_successes, 1);

        // The snapshot derives `recovering` from that health; the healthy spare does not.
        let readings = vec![
            Some(live(0.10, 0.10).unwrap()),
            Some(live(0.20, 0.20).unwrap()),
        ];
        let snapshot = daemon.snapshot(Some(0), &readings, 0);
        let work = &snapshot.accounts[0];
        assert_eq!(work.label, "work");
        assert!(
            work.quarantined && work.recovering,
            "a healing account is quarantined AND recovering"
        );
        assert!(
            !snapshot.accounts[1].recovering,
            "the healthy spare is not recovering"
        );

        // The wire carries the derived flag but never a secret.
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(json.contains(r#""recovering":true"#), "got {json}");
        assert!(!json.contains('@'), "no email on the wire: {json}");
        assert!(!json.to_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn a_dead_spare_is_never_polled_so_it_cannot_spuriously_recover() {
        // The recovery precondition, enforced structurally: a quarantined NON-active
        // account is skipped in polling, so it accrues no recovery successes and can
        // never un-quarantine on its own. It can only recover by first becoming active
        // — which happens only via the operator's re-login (the #13 re-stash, covered
        // by the next test). Without that, even an account whose token WOULD poll live
        // stays dead across ticks.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10) // active, holds
            .ok("u-B", 0.10, 0.10); // WOULD be live — but the dead spare is never polled
        let mut daemon = lifecycle_daemon_with(poller, tunables(95, 80, 0)).await;
        daemon.state.health[1].quarantined = true; // `spare` died in a prior episode

        for _ in 0..3 {
            let outcome = daemon.tick().await;
            assert!(
                !outcome
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::CredentialRestored { .. })),
                "a never-polled spare must not recover: {:?}",
                outcome.events
            );
        }

        assert!(
            daemon.state.health[1].quarantined,
            "still dead — never polled"
        );
        assert_eq!(daemon.state.health[1].recovery_successes, 0);
        assert_eq!(daemon.state.health[1].consec_401, 0);
    }

    #[tokio::test]
    async fn a_relogin_un_quarantines_a_dead_account_immediately_on_restash() {
        // Issue #107 (AC #1, #2, #4): the full re-login recovery path end-to-end,
        // exercising the #13↔#42 seam. A dead account (quarantined, already
        // emergency-swapped away so the spare is active) is re-logged-in by the
        // operator. The #13 canonical-change re-stash now un-quarantines it ON THE SPOT
        // — `status` stops lying on the NEXT tick, with NO M-poll delay — emitting
        // exactly one `credential_restored` on the dead→alive edge. Distinct from the
        // spontaneous-revival path
        // (`m_consecutive_live_polls_recover_a_quarantined_account_and_signal_once`),
        // which still needs M live polls because no re-login event marks the token fresh.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await; // `spare` is active post-emergency-swap
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"), // the OLD dead token
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        // The post-emergency-swap state: `work` is dead and parked off the active slot.
        daemon.state.active = Some(1);
        daemon.state.health[0].quarantined = true;

        // Tick 1 primes the canonical watch on `spare`; the dead `work` is skipped and
        // stays dead — no re-login has happened yet.
        let first = daemon.tick().await;
        assert!(!first
            .events
            .iter()
            .any(|e| matches!(e, Event::ReStash { .. } | Event::CredentialRestored { .. })));
        assert!(daemon.state.health[0].quarantined);

        // The operator `claude /login`s back into `work`: the canonical becomes its
        // fresh token and the display switches to it.
        daemon.store.write(&cred(b"A-reauthed")).await.unwrap();
        crate::claude_state::write_oauth_account(&json, &oauth("u-A")).unwrap();

        // Tick 2 detects the change, re-stashes `work`, re-resolves it active, AND
        // un-quarantines it immediately — no M-poll wait (#107). The same-tick poll that
        // runs after the re-stash sees an already-healthy account, so it does NOT emit a
        // second restore (edge-triggered, exactly once).
        let second = daemon.tick().await;
        assert!(
            second
                .events
                .iter()
                .any(|e| matches!(e, Event::ReStash { account } if account == "work")),
            "the re-login re-stashes work: {:?}",
            second.events
        );
        assert!(
            !daemon.state.health[0].quarantined,
            "the re-login un-quarantines work on the spot — no M-poll delay (#107)"
        );
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "an immediate restore leaves no recovery probe pending"
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "the re-logged-in account is active again"
        );
        assert_eq!(
            second
                .events
                .iter()
                .filter(|e| matches!(e, Event::CredentialRestored { account } if account == "work"))
                .count(),
            1,
            "exactly one credential_restored on the un-quarantine edge: {:?}",
            second.events
        );

        // Tick 3: `work` is healthy and active; no canonical change and no quarantine →
        // no further restore (the edge does not re-fire on an already-alive account).
        let third = daemon.tick().await;
        assert!(!daemon.state.health[0].quarantined);
        assert!(
            !third
                .events
                .iter()
                .any(|e| matches!(e, Event::CredentialRestored { .. })),
            "no repeat restore on an already-healthy account: {:?}",
            third.events
        );
    }

    #[tokio::test]
    async fn the_dead_and_restored_edges_re_arm_across_episodes() {
        // Edge-trigger re-arm (AC #5): a full dead→restored→dead cycle emits
        // credential_dead on EACH death edge and credential_restored on the recovery
        // edge — never stuck, never doubled. Proves the signals are per-transition,
        // not one-shot-per-process.
        let mut daemon = lifecycle_daemon().await;
        let mut events = Vec::new();

        // Episode 1 — death: 3 consecutive 401s.
        for _ in 0..3 {
            daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        }
        // Recovery: 2 consecutive live polls.
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        // Episode 2 — death again: the streak re-armed, so 3 fresh 401s re-quarantine.
        for _ in 0..3 {
            daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        }

        let deaths = events
            .iter()
            .filter(|e| matches!(e, Event::CredentialDead { .. }))
            .count();
        let restores = events
            .iter()
            .filter(|e| matches!(e, Event::CredentialRestored { .. }))
            .count();
        assert_eq!(deaths, 2, "one credential_dead per death edge: {events:?}");
        assert_eq!(
            restores, 1,
            "one credential_restored per recovery edge: {events:?}"
        );
        assert!(daemon.state.health[0].quarantined, "ends dead in episode 2");
    }

    // --- redaction METER (issue #15) ---------------------------------------
    //
    // The whole-corpus output-redaction gate. It drives the poll→decide→swap loop
    // body ([`Daemon::tick`]) across fault-injected scenarios with KNOWN secrets
    // seeded into every daemon input (the canonical store, the stashes, and
    // `~/.claude.json`), harvests EVERY operator-facing channel into one corpus,
    // and asserts — via [`crate::redaction::meter`] — that no token, no
    // credential-blob fingerprint, and no email surfaces anywhere. The meter
    // engine and its own non-vacuity proofs (each leak class planted and caught)
    // live in `crate::redaction`; this is the driver that feeds it real output.

    /// An `oauthAccount` carrying a chosen `uuid` and the secret `email`.
    fn meter_oauth(uuid: &str, email: &str) -> OauthAccount {
        OauthAccount::from_object_bytes(
            format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{email}"}}"#).as_bytes(),
        )
        .unwrap()
    }

    /// A stash holding the secret `blob` + an identity carrying the secret `email`.
    fn meter_stashed(blob: &[u8], uuid: &str, email: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(blob),
            oauth_account: meter_oauth(uuid, email),
        }
    }

    /// The MAXIMAL-mix #120 auth subset the offline `list` view trails, for the
    /// redaction-meter corpus: a future expiry + `refreshed`, a past expiry + `dead`
    /// (the `claude /login` cue), and a both-unavailable row — so the metered output
    /// covers every auth-tag shape. `now` is the corpus clock; `* 1000` lifts seconds to
    /// the epoch milliseconds `expires_at_ms` carries.
    fn meter_auth_subset(now: i64) -> Vec<crate::cli::AuthSubset> {
        vec![
            crate::cli::AuthSubset {
                expires_at_ms: Some((now + 7_200) * 1000),
                last_refresh: Some(RefreshEventOutcome::Refreshed),
            },
            crate::cli::AuthSubset {
                expires_at_ms: Some((now - 3_600) * 1000),
                last_refresh: Some(RefreshEventOutcome::Dead),
            },
            crate::cli::AuthSubset {
                expires_at_ms: None,
                last_refresh: None,
            },
        ]
    }

    /// A daemon whose every credential input carries the fixture's secrets: the
    /// canonical store and each per-account stash hold the secret blob, and each
    /// stashed identity (plus `~/.claude.json`) carries the secret email. Returns
    /// the daemon and the tempdir guard that keeps `~/.claude.json` alive.
    ///
    /// `~/.claude.json` is Claude Code's OWN state file — it legitimately holds the
    /// email — and is deliberately NOT one of the harvested output channels.
    async fn meter_daemon(
        secrets: &crate::redaction::meter::Secrets,
        accounts: &[(&str, &str)],
        poller: FakeRosterPoller,
        tun: &Tunables,
    ) -> (FakeDaemon, tempfile::TempDir) {
        let blob = secrets.blob();
        let email = secrets.email();

        let roster: Vec<Account> = accounts
            .iter()
            .map(|(uuid, label)| account(uuid, label))
            .collect();

        let store = FakeCredentialStore::empty();
        store.write(&cred(blob)).await.unwrap();
        let stash = FakeAccountStash::empty();
        for (uuid, _) in accounts {
            stash
                .write(
                    &format!("Sessiometer/{uuid}"),
                    &meter_stashed(blob, uuid, email),
                )
                .await
                .unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join(".claude.json");
        std::fs::write(
            &json,
            format!(
                r#"{{"numStartups":1,"oauthAccount":{{"accountUuid":"{}","emailAddress":"{email}"}}}}"#,
                accounts[0].0
            ),
        )
        .unwrap();

        let daemon = Daemon::new(roster, poller, store, stash, FakeClock::frozen(), json, tun);
        (daemon, dir)
    }

    /// Append every operator-facing channel of one tick's outcome to `corpus`,
    /// sourced from the EXACT canonical surfaces production uses: the single event-log
    /// surface ([`Event::to_log_line`]), the single diagnostic surface
    /// ([`Diagnostic::to_log_line`], issue #77), the UDS wire ([`status_response`] +
    /// [`control_reply`]), the `status` text ([`crate::cli::render_status`]), and the
    /// foreground swap echo ([`swap_report`]).
    fn harvest_channels(outcome: &TickOutcome, corpus: &mut String) {
        // A fixed wall-clock stamp keeps the log lines deterministic; the value is
        // a non-secret timestamp regardless.
        let ts = std::time::UNIX_EPOCH + Duration::from_secs(1_782_777_600);
        for event in &outcome.events {
            corpus.push_str(&event.to_log_line(ts));
            corpus.push('\n');
        }
        // The diagnostic channel (issue #77) — the per-poll lines carry an account
        // HANDLE, so they must clear the same #15 bar as the event log. Harvested
        // through the SAME `to_log_line` surface production renders to stderr.
        for diagnostic in &outcome.diagnostics {
            corpus.push_str(&diagnostic.to_log_line(ts));
            corpus.push('\n');
        }
        let response = status_response(&outcome.snapshot);
        corpus.push_str(&serde_json::to_string(&response).unwrap());
        corpus.push('\n');
        corpus.push_str(&control_reply(r#"{"cmd":"status"}"#, &outcome.snapshot, true).0);
        corpus.push('\n');
        // Scan the FULL table (`cols: None` → no width degradation), the maximal
        // text surface; the fixed `now` keeps "resets in" deterministic (issue #72).
        // Scan it BOTH uncolored and color-on (issue #73): the ANSI urgency overlay
        // must carry no secret either — it adds only `\x1b[3Xm`…`\x1b[0m`, never a
        // token or email.
        corpus.push_str(&crate::cli::render_status(
            &response,
            1_782_777_600,
            None,
            false,
        ));
        corpus.push_str(&crate::cli::render_status(
            &response,
            1_782_777_600,
            None,
            true,
        ));
        if let Some(report) = swap_report(outcome) {
            corpus.push_str(&report);
            corpus.push('\n');
        }
    }

    /// One representative value of EVERY [`Error`] variant — the error-message
    /// channel. Each carries only structural fields (paths, counts, codes, static
    /// field/op names); none can carry a token or email by construction, and the
    /// METER confirms the Display format strings hold to that.
    fn every_error_variant() -> Vec<Error> {
        vec![
            Error::Unimplemented("usage polling (#5)"),
            Error::UnknownCommand("bogus".to_owned()),
            Error::HomeUnresolved,
            Error::ForeignOwnership(PathBuf::from("/home/op/.config/sessiometer")),
            Error::CredentialNotFound,
            Error::CredentialAmbiguous { count: 2 },
            Error::KeychainLocked { op: "read" },
            Error::Keychain {
                op: "write",
                code: 1,
            },
            Error::ConfigNotFound {
                path: PathBuf::from("/home/op/.config/sessiometer/config.toml"),
            },
            Error::RosterEmpty,
            Error::ConfigParse("expected `=` at line 3".to_owned()),
            Error::ConfigInvalid("session_trigger must be in 50..=99, got 120".to_owned()),
            Error::ConfigTargetMaxAboveTrigger {
                target_max_usage: 95,
                trigger: 90,
            },
            Error::ClaudeStateNotFound {
                path: PathBuf::from("/home/op/.claude.json"),
            },
            Error::ClaudeStateParse {
                line: 5,
                column: 12,
            },
            Error::OauthAccountMissing,
            Error::OauthAccountFieldMissing {
                field: "accountUuid",
            },
            Error::RotationLabelRequired { verb: "disable" },
            Error::AccountLabelNotFound {
                label: "work".to_owned(),
            },
            Error::StashIncomplete {
                service: "Sessiometer/11111111-1111-1111-1111-111111111111".to_owned(),
            },
            Error::UsageTokenUnreadable,
            Error::UsageTransient {
                status: 0,
                retry_after: None,
            },
            Error::UsageRateLimited {
                status: 429,
                retry_after: None,
            },
            Error::UsageRejected { status: 400 },
            Error::UsageUnauthorized,
            Error::UsageScopeMissing,
            Error::UsageParse("no session (five_hour) dimension".to_owned()),
            Error::AlreadyRunning,
            Error::DaemonNotRunning,
            Error::SwapLockBusy,
            Error::Io(std::io::Error::other("boom")),
        ]
    }

    #[tokio::test]
    async fn redaction_meter_emits_no_secret_on_any_channel_across_the_full_loop() {
        use crate::observability::LoginEventOutcome;
        use crate::redaction::meter::{assert_clean, Secrets};

        let secrets = Secrets::meter_fixture();
        let mut corpus = String::new();

        // Recognizable, LOW-entropy uuids/labels: only the label reaches the
        // log/status/UDS channels; the uuid reaches only the `list` view. Keeping
        // them low-entropy means the entropy backstop fires only on a genuine
        // secret leak, never on the test scaffolding itself.
        const A: (&str, &str) = ("11111111-1111-1111-1111-111111111111", "work");
        const B: (&str, &str) = ("22222222-2222-2222-2222-222222222222", "spare");
        const C: (&str, &str) = ("33333333-3333-3333-3333-333333333333", "backup");

        // Scenario 1 — a swap: Event::Swap, the snapshot, and the foreground echo.
        {
            let poller = FakeRosterPoller::new()
                .ok(A.0, 0.97, 0.40) // active, over the session trigger
                .ok(B.0, 0.10, 0.20); // the (only) viable target
            let tun = tunables(95, 80, 0);
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B], poller, &tun).await;
            // The swap lands on the warm-up-completing staggered tick (#80).
            let outcome = warmed_tick(&mut daemon).await;
            assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
            harvest_channels(&outcome, &mut corpus);
        }

        // Scenario 2 — the all-exhausted terminal state: Event::AllExhausted with a
        // rendered `resets_at` (every account weekly-exhausted, no viable target).
        {
            const A_RESET: i64 = 1_782_777_600;
            const B_RESET: i64 = 1_782_496_800; // soonest -> the held account
            const C_RESET: i64 = 1_782_864_000;
            let poller = FakeRosterPoller::new()
                .ok_resets(A.0, 0.50, 0.99, A_RESET)
                .ok_resets(B.0, 0.50, 0.99, B_RESET)
                .ok_resets(C.0, 0.50, 0.99, C_RESET);
            let tun = tunables_floor_off(95, 0);
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B, C], poller, &tun).await;
            // The terminal verdict is reached on the warm-up-completing tick (#80),
            // once every account's exhaustion is known.
            let outcome = warmed_tick(&mut daemon).await;
            assert_eq!(outcome.action, TickAction::NoViableTarget);
            harvest_channels(&outcome, &mut corpus);
        }

        // Scenario 3a — poll-rejection fault injection: a 401 (active) and a 403
        // each emit their poll-outcome event in one tick. Account B's poll hits a
        // per-account lock, which is now silent (#13) and contributes no event.
        {
            let poller = FakeRosterPoller::new()
                .unauthorized(A.0) // monitor_401
                .keychain_locked(B.0) // silent per-account (#13)
                .scope_missing(C.0); // usage_scope_fail (403)
            let tun = tunables(95, 80, 0);
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B, C], poller, &tun).await;
            // One poll per staggered tick (#80), interleaved (#366): work (401), the
            // silent per-account lock on spare, work AGAIN (401), then backup (403). Four
            // ticks cover the full interleaved schedule `[work, spare, work, backup]`, so
            // both the monitor_401 and usage_scope_fail (403) channels are exercised. The
            // active account's reading is unavailable every tick → SkippedActiveUnavailable.
            for _ in 0..4 {
                let outcome = daemon.tick().await;
                assert_eq!(outcome.action, TickAction::SkippedActiveUnavailable);
                harvest_channels(&outcome, &mut corpus);
            }
        }

        // Scenario 3b — a globally LOCKED keychain (#13): the top-of-tick canonical
        // read fails, the whole cycle defers, and the accountless
        // keychain_locked_wait event plus the all-absent status snapshot are
        // harvested — proving the locked-path channels leak nothing either.
        {
            let poller = FakeRosterPoller::new();
            let tun = tunables(95, 80, 0);
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B, C], poller, &tun).await;
            daemon.store.set_locked(true);
            let outcome = daemon.tick().await;
            assert_eq!(outcome.action, TickAction::KeychainLocked);
            harvest_channels(&outcome, &mut corpus);
        }

        // Scenario 4 — the quarantine lifecycle (#42): a single 401 on the active
        // account (threshold 1) quarantines it — the #42 `credential_dead` EDGE, which is
        // NON-terminal (issue #427: an access-token 401-streak needs a refresh, not a
        // re-login) — and triggers an emergency swap in one tick, so `credential_dead`,
        // `emergency_swap`, AND the durable `quarantined` status (snapshot + wire + the
        // 🟠 degraded rollup text, #427) are all harvested at once.
        {
            let poller = FakeRosterPoller::new()
                .unauthorized(A.0) // active → 401 → dead at threshold 1
                .ok(B.0, 0.10, 0.20); // the viable escape target
            let tun = Tunables {
                monitor_401_n: 1,
                ..tunables(95, 80, 0)
            };
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B], poller, &tun).await;
            // The dead active is declared on the first staggered tick (#80) — emitting
            // `credential_dead` and the durable quarantine status — but the escape
            // target is not yet known (the spare polls only on the next tick), so the
            // emergency swap completes one tick later. Harvest both ticks.
            let dead = daemon.tick().await;
            assert_eq!(dead.action, TickAction::ActiveDeadNoTarget);
            harvest_channels(&dead, &mut corpus);
            let escaped = daemon.tick().await;
            assert_eq!(
                escaped.action,
                TickAction::EmergencySwapped { from: 0, to: 1 }
            );
            harvest_channels(&escaped, &mut corpus);
        }

        // Scenario 5 — auto-recovery (#42): a re-logged-in account polls live and,
        // at `monitor_recovery_m` = 1, un-quarantines — harvesting the
        // `credential_restored` line through the real daemon path.
        {
            let poller = FakeRosterPoller::new()
                .ok(A.0, 0.10, 0.20)
                .ok(B.0, 0.10, 0.20);
            let tun = Tunables {
                monitor_recovery_m: 1,
                ..tunables(95, 80, 0)
            };
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B], poller, &tun).await;
            daemon.state.active = Some(0);
            daemon.state.health[0].quarantined = true; // dead, now being re-probed
            let outcome = daemon.tick().await;
            assert_eq!(outcome.action, TickAction::Held);
            harvest_channels(&outcome, &mut corpus);
        }

        // Channel — the offline `list` roster view (label, full uuid) ENRICHED with the
        // #120 auth subset: each row's `expiresAt`-derived freshness + last-persisted
        // refresh outcome. The auth fields are a timestamp and a bare enum token, so they
        // must clear the same #15 bar as the rest of the view; `meter_auth_subset` is the
        // maximal mix (future + past expiry, the `dead`/`claude /login` cue, an empty
        // subset) harvested through the SAME `render_roster` production renders. `NOW` is
        // the fixed corpus clock.
        const NOW: i64 = 1_782_777_600;
        let roster: Vec<Account> = [A, B, C]
            .iter()
            .map(|(uuid, label)| account(uuid, label))
            .collect();
        corpus.push_str(&crate::cli::render_roster(
            &roster,
            &meter_auth_subset(NOW),
            NOW,
        ));

        // Channel — the diagnostic lifecycle Start summary (issue #77). The per-poll /
        // per-tick diagnostic lines are harvested per-cycle by `harvest_channels`
        // above; Start is emitted only at process start (by `cli::run`), so plant a
        // representative one here. It carries counts/percentages only — no handle.
        corpus.push_str(
            &Diagnostic::Start {
                accounts: 3,
                poll_secs: 30,
                target_max_usage: 70,
                session_trigger: 90,
                weekly_trigger: 98,
                monitor_401_n: 5,
                monitor_recovery_m: 4,
            }
            .to_log_line(std::time::UNIX_EPOCH + Duration::from_secs(1_782_777_600)),
        );
        corpus.push('\n');

        // Channel — the per-cycle refresh event (issue #106), emitted only in the run-loop
        // idle path; like the Start summary above it is planted here rather than tick-harvested.
        // Its fields are a handle / enum / timestamps, and the builder that maps a real cycle's
        // report into it is metered over a REAL secret in `refresh`'s engine test; here the
        // rendered LINE joins the all-channels corpus so the clean verdict below covers it too.
        corpus.push_str(
            &Event::Refresh {
                account: B.1.to_owned(),
                outcome: RefreshEventOutcome::Refreshed,
                expires_before: Some(1_782_777_600_000),
                expires_after: Some(1_782_784_800_000),
                refresh_token_rotated: true, // the #279 field joins the #15 all-channels corpus
                reason: None,
                backoff_secs: None,
            }
            .to_log_line(std::time::UNIX_EPOCH + Duration::from_secs(1_782_777_600)),
        );
        corpus.push('\n');

        // Channel — the `login` verb's redacted audit event (issue #135), emitted only by the
        // one-shot `login` command, so (like the refresh line above) it is planted here rather than
        // tick-harvested. `account` is the operator HANDLE (a label) — the SAME #15 bar as every
        // other channel; the onboarded form carries a handle, the cancelled form is accountless
        // (the omit branch), so both shapes of the line join the all-channels clean verdict below.
        corpus.push_str(
            &Event::Login {
                account: Some(A.1.to_owned()),
                outcome: LoginEventOutcome::Onboarded,
            }
            .to_log_line(std::time::UNIX_EPOCH + Duration::from_secs(1_782_777_600)),
        );
        corpus.push('\n');
        corpus.push_str(
            &Event::Login {
                account: None,
                outcome: LoginEventOutcome::Cancelled,
            }
            .to_log_line(std::time::UNIX_EPOCH + Duration::from_secs(1_782_777_600)),
        );
        corpus.push('\n');

        // Channel — the `capture` command's redacted audit event (issue #359), emitted only by the
        // daemon-routed capture path, so (like the login line above) it is planted here rather than
        // tick-harvested. `account` is the operator HANDLE (a label, or the account uuid when the
        // label is omitted) — the SAME #15 bar as every other channel; a captured account carries a
        // handle, while a pre-identity failure with no label hint (a locked keychain) is accountless
        // (the omit branch), so BOTH shapes of the line join the all-channels clean verdict below.
        corpus.push_str(
            &Event::Capture {
                account: Some(A.1.to_owned()),
                outcome: CaptureEventOutcome::Captured,
            }
            .to_log_line(std::time::UNIX_EPOCH + Duration::from_secs(1_782_777_600)),
        );
        corpus.push('\n');
        corpus.push_str(
            &Event::Capture {
                account: None,
                outcome: CaptureEventOutcome::KeychainLocked,
            }
            .to_log_line(std::time::UNIX_EPOCH + Duration::from_secs(1_782_777_600)),
        );
        corpus.push('\n');

        // Channel — the UDS error replies (malformed request / unknown command) and
        // the `manual-swapped` ack / unauthorized replies (#64), all secret-free.
        corpus.push_str(&control_reply("not json", &StatusSnapshot::default(), true).0);
        corpus.push('\n');
        corpus.push_str(&control_reply(r#"{"cmd":"nope"}"#, &StatusSnapshot::default(), true).0);
        corpus.push('\n');
        corpus.push_str(
            &control_reply(
                r#"{"cmd":"manual-swapped"}"#,
                &StatusSnapshot::default(),
                true,
            )
            .0,
        );
        corpus.push('\n');
        corpus.push_str(
            &control_reply(
                r#"{"cmd":"manual-swapped"}"#,
                &StatusSnapshot::default(),
                false,
            )
            .0,
        );
        corpus.push('\n');

        // Channel — the `restored` control command (issue #275): the request line (which carries
        // an account uuid) AND every reply — the authenticated ack, the unauthorized refusal, and
        // the malformed-missing-uuid refusal — proving the new channel leaks no secret in EITHER
        // direction. The uuid is the LOW-entropy fixture uuid, so it never trips the entropy
        // backstop on its own; a future change echoing a real token/email into the reply would.
        let restored_request = format!(r#"{{"cmd":"restored","uuid":"{}"}}"#, A.0);
        corpus.push_str(&restored_request);
        corpus.push('\n');
        corpus.push_str(&control_reply(&restored_request, &StatusSnapshot::default(), true).0);
        corpus.push('\n');
        corpus.push_str(&control_reply(&restored_request, &StatusSnapshot::default(), false).0);
        corpus.push('\n');
        corpus
            .push_str(&control_reply(r#"{"cmd":"restored"}"#, &StatusSnapshot::default(), true).0);
        corpus.push('\n');

        // Channel — the `swap` control ack (issue #167): both wire shapes — a completed swap's two
        // labels, and a redacted rejection reason — carry NO secret (only a machine `result` tag and
        // non-secret roster labels, #15). The labels are the fixture labels already in the corpus, so
        // this stays non-vacuous while proving the new channel leaks nothing in either shape.
        corpus.push_str(
            &serde_json::to_string(&SwapAck::Accepted {
                from: A.1.to_owned(),
                to: B.1.to_owned(),
            })
            .unwrap(),
        );
        corpus.push('\n');
        corpus.push_str(
            &serde_json::to_string(&SwapAck::Rejected {
                reason: SwapRejection::KeychainLocked,
            })
            .unwrap(),
        );
        corpus.push('\n');

        // Channel — the `capture` control ack (issue #359): all three wire shapes — a captured
        // account's label + count, a refreshed account's label + count, and a redacted rejection
        // reason — carry NO secret (only a machine `result` tag, a non-secret roster label, and a
        // count, #15). The labels are the fixture labels already in the corpus, so this stays
        // non-vacuous while proving the new channel leaks nothing in any of its shapes.
        corpus.push_str(
            &serde_json::to_string(&CaptureAck::Captured {
                label: A.1.to_owned(),
                count: 2,
            })
            .unwrap(),
        );
        corpus.push('\n');
        corpus.push_str(
            &serde_json::to_string(&CaptureAck::Refreshed {
                label: B.1.to_owned(),
                count: 2,
            })
            .unwrap(),
        );
        corpus.push('\n');
        corpus.push_str(
            &serde_json::to_string(&CaptureAck::Rejected {
                reason: CaptureRejection::KeychainLocked,
            })
            .unwrap(),
        );
        corpus.push('\n');

        // Channel — every error message Display.
        for err in every_error_variant() {
            corpus.push_str(&err.to_string());
            corpus.push('\n');
        }

        // Cardinality: a gate that passes on an empty/degraded corpus is no
        // evidence (issue #15). Prove every channel actually contributed its
        // expected non-secret content before trusting the clean verdict.
        assert!(
            corpus.contains("event=swap from=work to=spare"),
            "log channel: swap event missing"
        );
        assert!(
            corpus.contains("event=all_exhausted hold=spare"),
            "log channel: all_exhausted event missing"
        );
        assert!(
            corpus.contains("event=monitor_401 account=work"),
            "log channel: 401 event missing"
        );
        assert!(
            corpus.contains("event=keychain_locked_wait"),
            "log channel: keychain-lock event missing"
        );
        assert!(
            corpus.contains("event=usage_scope_fail account=backup"),
            "log channel: 403 event missing"
        );
        // #42 lifecycle channels: the three edge-triggered events plus the durable
        // quarantine status, on both the wire and the rendered text.
        assert!(
            corpus.contains("event=credential_dead account=work"),
            "log channel: credential_dead event missing"
        );
        assert!(
            corpus.contains("event=emergency_swap from=work to=spare"),
            "log channel: emergency_swap event missing"
        );
        assert!(
            corpus.contains("event=credential_restored account=work"),
            "log channel: credential_restored event missing"
        );
        assert!(
            corpus.contains("event=refresh account=spare outcome=refreshed"),
            "log channel: refresh event missing"
        );
        // The `login` verb channel (issue #135): the handle-carrying onboarded line AND the
        // accountless cancelled line both contributed, so the clean verdict below is non-vacuous
        // for the login event on BOTH its shapes.
        assert!(
            corpus.contains("event=login account=work outcome=onboarded"),
            "log channel: login event missing"
        );
        assert!(
            corpus.contains("event=login outcome=cancelled"),
            "log channel: accountless (cancelled) login event missing"
        );
        // The `capture` command channel (issue #359): the handle-carrying captured line AND the
        // accountless locked-keychain line both contributed, so the clean verdict below is
        // non-vacuous for the capture event on BOTH its shapes.
        assert!(
            corpus.contains("event=capture account=work outcome=captured"),
            "log channel: capture event missing"
        );
        assert!(
            corpus.contains("event=capture outcome=keychain_locked"),
            "log channel: accountless (locked-keychain) capture event missing"
        );
        assert!(
            corpus.contains(r#""quarantined":true"#),
            "UDS channel: quarantine status missing"
        );
        assert!(
            // The status-TEXT rendering of a quarantined credential (#119, #427): a bare
            // 401-streak quarantine is the NON-TERMINAL 🟠 `Degraded` rollup glyph plus the
            // needs-refresh cue — NOT the terminal 🔴 `claude /login`, which #427 reserves for
            // a PROVEN refresh-token death. Unique to the text channel — the wire carries the
            // verdict as the `"auth":"degraded"` enum (issue #143 renamed the key `health` →
            // `auth`), not this operator-facing command — so it proves the status-text channel
            // contributed (a non-vacuous #15 gate).
            corpus.contains("🟠 degraded — run 'sessiometer poke'"),
            "status-text channel: degraded-credential cue missing"
        );
        assert!(
            corpus.contains(r#""session_pct":97"#),
            "UDS channel: status wire missing"
        );
        assert!(
            // `97%` (with the percent sigil) is unique to the status-TEXT table —
            // the UDS wire renders the same reading as `"session_pct":97` (issue #72
            // reformatted the text into an aligned column table).
            corpus.contains("97%"),
            "status-text channel missing"
        );
        assert!(
            corpus.contains("swapped off work onto spare"),
            "foreground channel: swap report missing"
        );
        assert!(
            // The `list` view now shows the FULL account_uuid (issue #69), dropping
            // the former `Sessiometer/<uuid>` keychain-name column; this full uuid
            // is emitted by no other channel, so it proves the roster view ran.
            corpus.contains("11111111-1111-1111-1111-111111111111"),
            "list channel: roster view missing"
        );
        assert!(
            corpus.contains("daemon not running"),
            "error channel missing"
        );
        // Diagnostic channel (issue #77): the per-poll handle line, the per-tick
        // decision line, and the lifecycle Start summary each contributed — so the
        // clean verdict below is non-vacuous for the new channel too.
        assert!(
            corpus.contains("diag=poll account=work"),
            "diagnostic channel: per-poll outcome missing"
        );
        assert!(
            corpus.contains("diag=tick decision="),
            "diagnostic channel: per-tick decision missing"
        );
        assert!(
            corpus.contains("diag=start accounts=3"),
            "diagnostic channel: lifecycle start summary missing"
        );
        // The `restored` control channel (#275): the request line is unique to this channel
        // (`{"cmd":"restored"…}` appears on no other), so this keeps the clean verdict below
        // non-vacuous for the new channel — a #15 gate that never saw it would prove nothing.
        assert!(
            corpus.contains(r#"{"cmd":"restored","uuid":"#),
            "control channel: restored request missing"
        );
        // The `swap` ack channel (#167): the `accepted` result tag is unique to this channel, so it
        // keeps the clean verdict below non-vacuous for the new channel.
        assert!(
            corpus.contains(r#""result":"accepted""#),
            "control channel: swap ack missing"
        );
        // The `capture` ack channel (#359): the `captured` result tag is unique to this channel, so
        // it keeps the clean verdict below non-vacuous for the new channel.
        assert!(
            corpus.contains(r#""result":"captured""#),
            "control channel: capture ack missing"
        );
        assert!(
            corpus.len() > 800,
            "corpus implausibly small ({} bytes) — channels not captured",
            corpus.len()
        );

        // The METER: no token prefix, no known token, no blob fingerprint (leading
        // bytes or sha256), no email shape, and no high-entropy run — on ANY of the
        // channels above.
        assert_clean(&corpus, &secrets);
    }

    /// The 0.1.0 "done-when" acceptance, driven end-to-end through the four seams
    /// (the injected `UsageSource` via [`FakeRosterPoller`], [`FakeCredentialStore`],
    /// [`FakeAccountStash`], [`FakeClock`]) so it burns no real quota, touches no
    /// keychain, and runs in zero real time (issue #14). One hermetic run proves the
    /// whole loop that the smaller unit tests cover only in pieces:
    ///
    ///   - **reconcile-on-start (#13):** a deliberate canonical≠oauth mismatch — the
    ///     canonical holds B's token while `~/.claude.json` still DISPLAYS A (a torn
    ///     post-swap crash) — is healed before the first poll.
    ///   - **threshold → pick-viable → swap → propagate:** the active account, over
    ///     its session trigger, swaps to a VIABLE target (never the weekly-exhausted
    ///     distractor C), and the promoted credential propagates to BOTH the canonical
    ///     keychain item AND the `~/.claude.json` display.
    ///   - **B→A→B without oscillation (#10):** with A and B both hovering over the
    ///     trigger, the post-swap cooldown bounds the ping-pong — a re-swap inside the
    ///     window is refused (never A→B→A), and only past the window does the loop swap
    ///     back, completing a B→A→B cycle. No manual step at any point.
    ///   - **every event surfaced (#9) + nothing leaked (#15):** each cycle's output on
    ///     every operator channel (log / status / UDS / error / list) is harvested and
    ///     run through the redaction METER as a CI gate over the whole acceptance flow.
    ///
    /// The documented MANUAL counterpart — the same acceptance against real accounts,
    /// gated on the #16 H0–H3 checks — lives in `build/smoke-test.md`; it is documented,
    /// not run here, so this path stays hermetic and burns no real quota.
    #[tokio::test]
    async fn e2e_acceptance_full_loop_swaps_propagates_and_reconciles_without_oscillation_or_leak()
    {
        use crate::redaction::meter::{assert_clean, Secrets};

        // Low-entropy uuids/labels: only labels reach the harvested channels and only
        // uuids reach the `list` view, so the METER's entropy backstop fires solely on
        // a genuine secret leak, never on this scaffolding (as the meter test above).
        const A: (&str, &str) = ("11111111-1111-1111-1111-111111111111", "work");
        const B: (&str, &str) = ("22222222-2222-2222-2222-222222222222", "spare");
        const C: (&str, &str) = ("33333333-3333-3333-3333-333333333333", "backup");

        // Three DISTINCT secret blobs — distinct so a swap visibly MOVES the canonical
        // token (propagation is observable) and so token↔account resolution stays
        // unambiguous. Each carries `sk-ant-…` bearers the METER would catch on any
        // channel. A reuses the fixture blob (exercising the blob/known-token detectors
        // too); B and C are their own secrets, with C's never reaching the canonical.
        let secrets = Secrets::meter_fixture();
        let email = secrets.email();
        let a_blob = secrets.blob().to_vec();
        let b_blob = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-SPARE00SECRET00ACCESS00qR7sT3uV5wX9yZ","refreshToken":"sk-ant-ort-SPARE00SECRET00REFRESH00eF6gH8iJ0kL2mN","expiresAt":1782777600}}"#.to_vec();
        let c_blob = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-BACKUP0SECRET0ACCESS0sV1wY3zA5bC7dE","refreshToken":"sk-ant-ort-BACKUP0SECRET0REFRESH0iK2lM4nO6pQ8rS","expiresAt":1782777600}}"#.to_vec();

        // Roster: A (index 0), B (index 1), C (index 2 — the non-viable distractor).
        let roster: Vec<Account> = [A, B, C]
            .iter()
            .map(|(uuid, label)| account(uuid, label))
            .collect();

        // Each account's stash holds its OWN secret blob + a secret-bearing identity.
        let stash = FakeAccountStash::empty();
        for (id, blob) in [(A, &a_blob), (B, &b_blob), (C, &c_blob)] {
            stash
                .write(
                    &format!("Sessiometer/{}", id.0),
                    &meter_stashed(blob, id.0, email),
                )
                .await
                .unwrap();
        }

        // The canonical item holds B's token — so the active account resolves to B …
        let store = FakeCredentialStore::empty();
        store.write(&cred(&b_blob)).await.unwrap();
        // … while `~/.claude.json` still DISPLAYS A: the deliberate canonical≠oauth
        // mismatch (a torn post-swap crash, #13) that reconcile-on-start must heal.
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join(".claude.json");
        std::fs::write(
            &json,
            format!(
                r#"{{"numStartups":1,"oauthAccount":{{"accountUuid":"{}","emailAddress":"{email}"}}}}"#,
                A.0
            ),
        )
        .unwrap();

        // B (active) is over the 95 session trigger; A is session-FRESH (0.20, well
        // under the trigger) with low weekly, so A is the viable target. C is
        // WEEKLY-EXHAUSTED (0.99 ≥ the 0.98 weekly trigger) → never a viable target. A
        // correct loop must SELECT the session-fresh A and EXCLUDE C. (The always-on
        // session gate makes a session-saturated account a non-viable target — see
        // `pick_target_excludes_session_saturated_accounts` — so the destination must be
        // a genuinely fresh account, not a second near-exhausted one.)
        const C_RESET: i64 = 1_900_000_000; // far future; C is excluded regardless
        let poller = FakeRosterPoller::new()
            .ok(A.0, 0.20, 0.20)
            .ok(B.0, 0.96, 0.20)
            .ok_resets(C.0, 0.50, 0.99, C_RESET);
        // Floor OFF (the #10 default); cooldown 100 s; session trigger 95; no jitter, so
        // every draw is deterministic.
        let tun = tunables_floor_off(95, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json.clone(),
            &tun,
        );

        // --- reconcile-on-start: heal the canonical≠oauth mismatch -------------
        assert_eq!(
            displayed_uuid(&json).as_deref(),
            Some(A.0),
            "precondition: the display starts STALE (shows A while the canonical holds B)"
        );
        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(
            displayed_uuid(&json).as_deref(),
            Some(B.0),
            "reconcile must heal the display to the account the canonical actually holds (B)"
        );

        let mut corpus = String::new();

        // --- B → A: the active account (B), over its trigger, swaps to a viable
        // target. C (weekly-exhausted) is excluded; A is selected. The promoted
        // credential propagates to BOTH the canonical item and the display. The swap
        // lands on the warm-up-completing staggered tick (#80) — once the round-robin
        // has polled all three accounts and the last-known set is complete. -----------
        let outcome = warmed_tick(&mut daemon).await;
        assert_eq!(
            outcome.action,
            TickAction::Swapped { from: 1, to: 0 },
            "B (active, over trigger) must swap to the viable A, never the exhausted C"
        );
        assert!(
            daemon.store.read().await.unwrap().matches(&cred(&a_blob)),
            "propagate: the canonical item now holds A's token"
        );
        assert_eq!(
            displayed_uuid(&json).as_deref(),
            Some(A.0),
            "propagate: the display now shows A"
        );
        assert_eq!(daemon.state.active, Some(0), "the cached active is now A");
        assert!(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, Event::Swap { .. })),
            "the swap must surface a structured event (#9)"
        );
        harvest_channels(&outcome, &mut corpus);

        // --- no oscillation: A is now active and session-FRESH (0.20, under the 95
        // trigger), so `swap::decide` returns Hold on every later tick BEFORE `pick_target`
        // is ever consulted — the loop is stable and cannot revisit B, and NO swap-back
        // fires even well past the 100 s cooldown. (This full-loop acceptance test covers
        // the healthy swap + stable hold; the always-on session gate itself is guarded
        // directly — on the fails-if-reverted path — by
        // `pick_target_excludes_session_saturated_accounts` and
        // `two_session_saturated_accounts_hold_the_gate_prevents_oscillation`.) ---------
        for step in 1..=8u64 {
            daemon.clock.advance(Duration::from_secs(20)); // +20s .. +160s (past cooldown)
            let outcome = daemon.tick().await;
            assert_eq!(
                outcome.action,
                TickAction::Held,
                "A is session-fresh and stable: tick {step} must HOLD, never oscillate"
            );
            assert!(
                daemon.store.read().await.unwrap().matches(&cred(&a_blob)),
                "no oscillation: the canonical still holds A's token"
            );
            harvest_channels(&outcome, &mut corpus);
        }

        // --- the remaining operator channels: the offline `list` view, the UDS error
        // replies, and every Error Display — all secret-free by construction. -------
        // The `list` view is ENRICHED with the #120 auth subset (expiry freshness + last
        // refresh outcome); `meter_auth_subset` is the maximal mix so the metered corpus
        // covers a future expiry, a past expiry + the `dead`/`claude /login` cue, and an
        // empty subset.
        const NOW: i64 = 1_782_777_600;
        corpus.push_str(&crate::cli::render_roster(
            &[account(A.0, A.1), account(B.0, B.1), account(C.0, C.1)],
            &meter_auth_subset(NOW),
            NOW,
        ));
        corpus.push('\n');
        corpus.push_str(&control_reply("not json", &StatusSnapshot::default(), true).0);
        corpus.push('\n');
        corpus.push_str(&control_reply(r#"{"cmd":"nope"}"#, &StatusSnapshot::default(), true).0);
        corpus.push('\n');
        corpus.push_str(
            &control_reply(
                r#"{"cmd":"manual-swapped"}"#,
                &StatusSnapshot::default(),
                true,
            )
            .0,
        );
        corpus.push('\n');
        for err in every_error_variant() {
            corpus.push_str(&err.to_string());
            corpus.push('\n');
        }

        // Cardinality (issue #15): a gate that passes on an empty corpus is no
        // evidence. Prove the loop actually surfaced each swap on a real channel before
        // trusting the clean verdict.
        assert!(
            corpus.contains("event=swap from=spare to=work"),
            "log channel: the B→A swap event is missing"
        );
        assert!(
            corpus.contains(r#""session_pct":96"#),
            "UDS channel: the status wire is missing"
        );
        assert!(
            // `96%` (with the percent sigil) is unique to the status-TEXT table —
            // the UDS wire renders the same reading as `"session_pct":96` (issue #72
            // reformatted the text into an aligned column table).
            corpus.contains("96%"),
            "status-text channel is missing"
        );
        assert!(
            corpus.contains("swapped off spare onto work"),
            "foreground channel: the B→A swap report is missing"
        );
        assert!(
            corpus.len() > 800,
            "corpus implausibly small ({} bytes) — channels not captured",
            corpus.len()
        );

        // The METER gate (#15): no token prefix, known token, blob fingerprint (leading
        // bytes or sha256), email shape, or high-entropy run leaked onto ANY channel
        // across the whole acceptance loop.
        assert_clean(&corpus, &secrets);
    }

    // --- Usage-sample collector (issue #156) ------------------------------

    /// A successful poll writes EXACTLY ONE sample carrying the redacted handle and
    /// every projected field — including the widened `severity` — with no secret.
    #[test]
    fn collector_writes_one_redacted_sample_per_successful_poll() {
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");
        let reading = Ok(PolledReading {
            usage: Usage {
                session: 0.42,
                weekly: 0.88,
                weekly_resets_at: Some(1_700_600_000),
                session_resets_at: Some(1_700_003_600),
            },
            severity: Some("critical".to_owned()),
        });

        append_sample_for_poll(&samples_path, "work", &reading, 1_700_000_000);

        let samples = crate::usage_store::read_samples(&samples_path).unwrap();
        assert_eq!(samples.len(), 1, "exactly one sample per successful poll");
        let s = &samples[0];
        assert_eq!(s.ts, 1_700_000_000);
        assert_eq!(s.provider, USAGE_PROVIDER);
        assert_eq!(s.acct, "work", "the redacted handle, verbatim");
        assert!((s.session - 0.42).abs() < 1e-9);
        assert!((s.weekly - 0.88).abs() < 1e-9);
        assert_eq!(s.session_resets_at, Some(1_700_003_600));
        assert_eq!(s.weekly_resets_at, Some(1_700_600_000));
        assert_eq!(s.severity.as_deref(), Some("critical"), "severity retained");
        assert_eq!(s.spend, None, "no spend producer yet (forward slot)");

        // Redaction: the persisted line carries no email/token shape (issue #15).
        let raw = std::fs::read_to_string(&samples_path).unwrap();
        assert!(!raw.contains('@'), "no email may reach the store: {raw}");
        assert!(
            !raw.contains("sk-ant"),
            "no token may reach the store: {raw}"
        );
    }

    /// A reading whose optional `severity` is absent still yields a valid sample
    /// (issue #156 AC: valid when the optional field is absent).
    #[test]
    fn collector_sample_is_valid_without_severity() {
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");
        let reading = Ok(PolledReading {
            usage: Usage {
                session: 0.1,
                weekly: 0.2,
                weekly_resets_at: None,
                session_resets_at: None,
            },
            severity: None,
        });

        append_sample_for_poll(&samples_path, "spare", &reading, 42);

        let samples = crate::usage_store::read_samples(&samples_path).unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].severity, None);
        assert_eq!(samples[0].acct, "spare");
    }

    /// GAP HONESTY: a poll that yields NO reading (`Err`) records NOTHING — a gap is
    /// absent, never a fabricated zero/healthy sample (issue #157 reads a missing
    /// sample as UNKNOWN). A pre-existing sample file is left untouched.
    #[test]
    fn collector_records_nothing_for_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");

        // A 401 / offline / API-error poll writes no sample (and no file).
        let gap: Result<PolledReading> = Err(Error::UsageUnauthorized);
        append_sample_for_poll(&samples_path, "work", &gap, 100);
        assert!(
            crate::usage_store::read_samples(&samples_path)
                .unwrap()
                .is_empty(),
            "a gap writes no sample"
        );

        // A gap AFTER a real sample adds nothing — the earlier reading stands alone.
        let ok = Ok(PolledReading {
            usage: Usage {
                session: 0.3,
                weekly: 0.4,
                weekly_resets_at: None,
                session_resets_at: None,
            },
            severity: None,
        });
        append_sample_for_poll(&samples_path, "work", &ok, 200);
        let transient: Result<PolledReading> = Err(Error::UsageTransient {
            status: 0,
            retry_after: None,
        });
        append_sample_for_poll(&samples_path, "work", &transient, 300);
        let samples = crate::usage_store::read_samples(&samples_path).unwrap();
        assert_eq!(
            samples.len(),
            1,
            "only the successful poll produced a sample"
        );
        assert_eq!(samples[0].ts, 200);
    }

    /// FAIL-OPEN: a store-write error is swallowed — the collector returns normally
    /// (the poll/swap loop continues, the daemon stays up) and leaves no file. Here
    /// the parent directory does not exist, so the append cannot open its file.
    #[test]
    fn collector_swallows_a_store_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        // Parent subdir intentionally absent → the private-file open fails (ENOENT).
        let unwritable = dir
            .path()
            .join("missing-subdir")
            .join("usage-samples.jsonl");
        let reading = Ok(PolledReading {
            usage: Usage {
                session: 0.5,
                weekly: 0.6,
                weekly_resets_at: None,
                session_resets_at: None,
            },
            severity: Some("warning".to_owned()),
        });

        // Must NOT panic / unwind: reaching the assertion below proves the write
        // error was swallowed and control returned to the (simulated) poll loop.
        append_sample_for_poll(&unwritable, "work", &reading, 7);

        assert!(!unwritable.exists(), "no partial file left behind");
    }

    /// ISOLATION (issue #315): the hermetic `FakeDaemon` default wires NO usage-sample path,
    /// so ticking it records nothing to the developer's real store. `record_usage_sample`
    /// short-circuits on the `None` path, so a `cargo test` run can never append a sample to
    /// `~/Library/Application Support/sessiometer/usage-samples.jsonl`. This is the regression
    /// guard: defaulting the field to a real path in `new` would reintroduce the defect and
    /// this test would fail. It asserts the `None` field default — the structural guarantee that
    /// makes a real-store write impossible — not an observed absence-of-write (the real store
    /// can't be inspected without racing a concurrent writer); the companion
    /// `tick_appends_a_usage_sample_to_the_injected_path` proves the injected path is honored.
    #[tokio::test]
    async fn tick_records_no_usage_sample_without_an_injected_path() {
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.11, 0.10)
                .ok("u-B", 0.22, 0.10)
                .ok("u-C", 0.33, 0.10),
        )
        .await;
        // The hermetic default holds no store path — the collector is inert.
        assert!(
            daemon.usage_samples_path.is_none(),
            "a FakeDaemon must not wire a usage-sample path (issue #315 isolation)",
        );
        // Ticking never resolves one, so the real store stays untouched across the suite.
        let _ = warmed_tick(&mut daemon).await;
        assert!(
            daemon.usage_samples_path.is_none(),
            "ticking must not resolve a real store path (issue #315)",
        );
    }

    /// SEAM HONORED (issue #156/#315): with the path injected via `with_usage_samples`, a
    /// successful poll appends its per-poll sample to THAT path — proving the production
    /// collector still records per poll while the injected seam keeps every write inside a
    /// test-owned temp dir (never the real support dir). Each sample carries a redacted roster
    /// handle, so nothing escapes to the real store even in the wired case.
    #[tokio::test]
    async fn tick_appends_a_usage_sample_to_the_injected_path() {
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.11, 0.10)
                .ok("u-B", 0.22, 0.10)
                .ok("u-C", 0.33, 0.10),
        )
        .await
        .with_usage_samples(samples_path.clone());

        let _ = warmed_tick(&mut daemon).await;

        let samples = crate::usage_store::read_samples(&samples_path).unwrap();
        assert!(
            !samples.is_empty(),
            "the injected path must receive the per-poll sample(s)",
        );
        // Every sample carries a redacted roster handle — the write lands in the temp store,
        // never the real support dir.
        for s in &samples {
            assert!(
                ["work", "spare", "backup"].contains(&s.acct.as_str()),
                "unexpected acct handle in the injected store: {}",
                s.acct,
            );
        }
    }

    // --- Usage-stats store maintenance events (issue #161) ------------------

    /// Tight cadences so the hermetic tests drive re-emit / roll windows deterministically.
    fn test_cadence() -> StatsCadence {
        StatsCadence {
            gap_reemit_secs: 3_600,
            roll_cadence_secs: 3_600,
        }
    }

    /// A no-reading poll emits ONE redacted `usage_gap` (handle + streak-start), then is
    /// RATE-LIMITED: a second gap inside the re-emit window is suppressed; a later one
    /// re-emits with `since` STILL fixed at the streak start. No PII on any line.
    #[test]
    fn stats_gap_emits_a_rate_limited_redacted_gap_event() {
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");
        let rollup_path = dir.path().join("usage-rollup.json");
        let policy = RetentionPolicy::default();
        let cadence = test_cadence();
        let mut state = StatsState::default(); // empty store → the roll folds nothing (no rollup noise)

        // First gap of a streak → one UsageGap, since = now, handle-only.
        let e1 = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            true,
            1_000_000,
            &policy,
            &mut state,
            &cadence,
        );
        assert_eq!(e1.len(), 1, "the first gap of a streak emits");
        match &e1[0] {
            Event::UsageGap { account, since } => {
                assert_eq!(account, "work", "the redacted handle");
                assert_eq!(*since, 1_000_000, "since = streak start");
            }
            other => panic!("expected UsageGap, got {other:?}"),
        }

        // A second gap 100s later — inside the 3600s window → suppressed.
        let e2 = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            true,
            1_000_100,
            &policy,
            &mut state,
            &cadence,
        );
        assert!(e2.is_empty(), "rate-limited inside the re-emit window");

        // A third gap past the window → re-emit, `since` STILL the streak start.
        let e3 = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            true,
            1_003_600,
            &policy,
            &mut state,
            &cadence,
        );
        assert_eq!(e3.len(), 1, "re-emits past the window");
        match &e3[0] {
            Event::UsageGap { since, .. } => {
                assert_eq!(*since, 1_000_000, "since is fixed across the streak")
            }
            other => panic!("expected UsageGap, got {other:?}"),
        }

        // NO PII on the rendered gap line — handle + timestamp only.
        let line = e3[0].to_log_line(std::time::UNIX_EPOCH);
        assert!(line.contains("event=usage_gap acct=work"), "got: {line}");
        assert!(!line.contains('@'), "no email: {line}");
        assert!(!line.contains("sk-ant"), "no token: {line}");
    }

    /// A reading CLEARS the account's gap streak, so a later gap starts fresh (`since` = the
    /// new gap's time) rather than re-using the old streak start.
    #[test]
    fn stats_reading_clears_the_gap_streak() {
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");
        let rollup_path = dir.path().join("usage-rollup.json");
        let policy = RetentionPolicy::default();
        let cadence = test_cadence();
        let mut state = StatsState::default();

        // A gap opens a streak.
        stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            true,
            100,
            &policy,
            &mut state,
            &cadence,
        );
        assert!(state.gap_state.contains_key("work"), "streak recorded");

        // A reading clears it and emits no gap.
        let reading = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            false,
            200,
            &policy,
            &mut state,
            &cadence,
        );
        assert!(
            !state.gap_state.contains_key("work"),
            "reading clears the streak"
        );
        assert!(
            reading.iter().all(|e| !matches!(e, Event::UsageGap { .. })),
            "a reading emits no gap"
        );

        // A later gap is a NEW streak.
        let e = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            true,
            5_000,
            &policy,
            &mut state,
            &cadence,
        );
        match &e[0] {
            Event::UsageGap { since, .. } => assert_eq!(*since, 5_000, "fresh streak start"),
            other => panic!("expected UsageGap, got {other:?}"),
        }
    }

    /// A reading poll whose cadence-gated `compact_and_roll` folds aged samples emits ONE
    /// redacted `usage_rollup` (store-global: integers only, NO account handle). No PII.
    #[test]
    fn stats_rollup_emits_a_redacted_event_when_a_pass_folds_samples() {
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");
        let rollup_path = dir.path().join("usage-rollup.json");
        let policy = RetentionPolicy::default();
        let cadence = test_cadence();
        let mut state = StatsState::default(); // first pass is due
        let now = 200 * 86_400;
        let aged = 10 * 86_400; // one aged-out day, past the 14d raw window
        for k in 0..3 {
            append_sample(
                &samples_path,
                &Sample::new(aged + k * 600, "claude", "work", 0.5, 0.6),
            )
            .unwrap();
        }

        let events = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            false,
            now,
            &policy,
            &mut state,
            &cadence,
        );
        let rollups: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, Event::UsageRollup { .. }))
            .collect();
        assert_eq!(rollups.len(), 1, "one rollup when a pass folds samples");
        match rollups[0] {
            Event::UsageRollup {
                rolled_through,
                raw_lines,
            } => {
                assert_eq!(*raw_lines, 3, "folded the 3 aged samples");
                assert_eq!(
                    *rolled_through,
                    aged + 2 * 600,
                    "watermark = newest folded ts"
                );
            }
            _ => unreachable!(),
        }
        assert!(
            events.iter().all(|e| !matches!(e, Event::UsageGap { .. })),
            "a reading poll emits no gap"
        );
        assert!(
            state.last_roll.is_some(),
            "the roll advanced the cadence anchor"
        );

        // NO PII: store-global, integers only — no handle, no email/token.
        let line = rollups[0].to_log_line(std::time::UNIX_EPOCH);
        assert!(line.contains("event=usage_rollup"), "got: {line}");
        assert!(!line.contains("acct="), "rollup carries no handle: {line}");
        assert!(!line.contains('@'), "no email: {line}");
    }

    /// The roll is CADENCE-GATED: after a pass, a second poll inside the window runs no
    /// compaction (aged samples added meanwhile stay raw); a poll past the window rolls them.
    #[test]
    fn stats_rollup_is_cadence_gated() {
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");
        let rollup_path = dir.path().join("usage-rollup.json");
        let policy = RetentionPolicy::default();
        let cadence = test_cadence();
        let mut state = StatsState::default();
        let now = 200 * 86_400;

        // Day-10 samples → the first pass (due) rolls them.
        for k in 0..3 {
            append_sample(
                &samples_path,
                &Sample::new(10 * 86_400 + k * 600, "claude", "work", 0.5, 0.6),
            )
            .unwrap();
        }
        let e1 = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            false,
            now,
            &policy,
            &mut state,
            &cadence,
        );
        assert!(
            e1.iter().any(|e| matches!(e, Event::UsageRollup { .. })),
            "the first pass rolls"
        );

        // A different aged day's samples appended AFTER the first pass.
        for k in 0..3 {
            append_sample(
                &samples_path,
                &Sample::new(11 * 86_400 + k * 600, "claude", "work", 0.4, 0.5),
            )
            .unwrap();
        }
        // Inside the roll window → no compaction, so no rollup and the new samples stay raw.
        let e2 = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            false,
            now + 100,
            &policy,
            &mut state,
            &cadence,
        );
        assert!(
            e2.iter().all(|e| !matches!(e, Event::UsageRollup { .. })),
            "cadence-gated: no roll inside the window"
        );

        // Past the window → rolls the new day.
        let e3 = stats_events_for_poll(
            &StorePaths {
                samples: &samples_path,
                rollup: &rollup_path,
            },
            "work",
            false,
            now + 3_600,
            &policy,
            &mut state,
            &cadence,
        );
        assert!(
            e3.iter().any(|e| matches!(e, Event::UsageRollup { .. })),
            "rolls again past the cadence"
        );
    }
}
