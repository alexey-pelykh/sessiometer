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
//!    entry in a staggered round-robin schedule (the active account first, then each
//!    enabled non-active in turn) — through the canonical credential when it is the
//!    active account (freshest token) or its stash otherwise. Spreading a cycle's N
//!    polls across N sub-intervals (≈`poll_secs / N` apart) keeps each request in its
//!    own rate-limit window: the usage endpoint is source-scoped and serves ~one
//!    request per short window, so the former poll-of-all BURST had all-but-one
//!    `429`-fail at the CDN edge. The polled account's reading updates its slot in the
//!    carried `last_readings`; a failed poll clears it. A `429` / `5xx` backs the
//!    WHOLE loop off (issue #76) — rate-limiting is endpoint-global.
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
//! ## Lifecycle (the run loop, [`run_loop`])
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
//! swap-target session floor is an opt-in reserve on top (off by default). When EVERY
//! account is weekly-exhausted there is no viable target
//! ([`TickAction::NoViableTarget`], #11): the loop enters the all-exhausted
//! terminal state — it HOLDS (no swap, so no thrash) and emits a single
//! edge-triggered `all_exhausted` event naming the least-bad account (the soonest
//! weekly `resets_at`), which now fills the event log's `resets_at=` field.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use tokio::signal::unix::{signal, Signal, SignalKind};

use crate::claude_state;
use crate::config::{Account, Config, Tunables};
use crate::error::{Error, Result};
use crate::keychain::{
    CanonicalChange, CanonicalWatch, Credential, CredentialStore, RealCredentialStore,
};
use crate::observability::{
    CredentialHealth, DecisionClass, Diagnostic, DiagnosticLog, Event, EventLog, PollClass,
    RefreshEventOutcome, SwapReason,
};
use crate::refresh::{RefreshOutcome, RefreshReport};
use crate::refresh_tick::{RealRefreshEngine, RefreshEngine, RefreshObservation, SweepOutcome};
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};
use crate::swap::{self, SwapDecision};
use crate::timing::{Jitter, SplitMix64, Strategy};
use crate::usage::{CurlTransport, PolledReading, RealUsageSource, Usage, UsageSource};
use crate::usage_store::{append_sample, compact_and_roll, RetentionPolicy, Sample};

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
/// Per-cycle clamp bounds for the cooldown draw, in seconds (config range).
const COOLDOWN_SECS_LO: f64 = 0.0;
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
/// larger server-advised `Retry-After` still overrides this (honoured as a MINIMUM).
const POLL_BACKOFF_CAP: Duration = Duration::from_secs(3600);
/// Upper bound (seconds) on the jittered start-up delay (issue #76). Before its
/// FIRST poll the daemon waits a uniform `[0, this)` so that repeated restarts of
/// the same config — and the N accounts within a cycle — do not synchronize an
/// immediate burst of usage requests. Small enough to stay responsive on launch.
const STARTUP_DELAY_CAP: f64 = 30.0;

/// Time seam: the daemon reads "now" and sleeps until the next poll through
/// this, so a fake can drive time and make the loop run instantly in tests.
pub(crate) trait Clock {
    /// The current instant.
    fn now(&self) -> Instant;
    /// Sleep for `interval` — the (jittered) wait until the next poll, computed
    /// per cycle by the daemon (issue #38). The clock no longer owns the
    /// interval; it just sleeps the duration it is handed.
    async fn tick(&self, interval: Duration);
}

/// Real clock: monotonic `Instant::now` and a Tokio sleep of the handed interval.
#[derive(Default)]
pub(crate) struct RealClock;

impl RealClock {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    async fn tick(&self, interval: Duration) {
        tokio::time::sleep(interval).await;
    }
}

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

/// Record ONE usage sample for a poll (issue #156), fail-open.
///
/// The poll-loop entry point: resolve the store path, then delegate to
/// [`append_sample_for_poll`]. Splitting path resolution out keeps that core
/// hermetically testable with an injected path + clock. A path-resolution failure is
/// logged and swallowed — usage sampling is telemetry and must never break the
/// poll/swap loop (see [`append_sample_for_poll`] for the full fail-open contract).
fn record_usage_sample(account_label: &str, polled: &Result<PolledReading>) {
    let samples_path = match crate::paths::usage_samples() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("sessiometer: usage-sample path unavailable, skipped: {err}");
            return;
        }
    };
    append_sample_for_poll(&samples_path, account_label, polled, wall_clock_now_secs());
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

/// A side effect a served control connection asks the run loop to apply after the
/// reply is sent. `status` produces none (a pure read); the only variant today is
/// the manual-hold signal (issue #64). Returned by [`Control::serve`] so the
/// mutation lands on the daemon's decision state in the run loop, where `&mut
/// Daemon` is available — `serve` itself only borrows the read-only snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlSignal {
    /// A manual `use` swap committed and notified the daemon (issue #64). The run
    /// loop adopts it ([`Daemon::adopt_manual_swap`]): arm the post-swap cooldown
    /// (#10) so the very next poll does not immediately revert the operator's
    /// choice, and re-resolve the active account from the canonical item. A
    /// cooldown-only signal — it carries no credential and no write target, and
    /// never becomes a write command.
    ManualSwapped,
    /// A roster write on disk (`capture` / `login` / `remove`) committed and notified
    /// the daemon (issue #139). The run loop reloads it
    /// ([`Daemon::adopt_roster_reload`]): re-read `config.toml` and reconcile the
    /// in-memory roster (add onboarded/relogged-in accounts, drop removed ones),
    /// preserving per-account health/decision state for accounts that persist. Like
    /// [`ManualSwapped`](ControlSignal::ManualSwapped) it carries no payload — the
    /// authoritative new roster is the on-disk `config.toml`, re-read from scratch — so
    /// a duplicate or out-of-order notification at worst re-reads an unchanged file.
    RosterReloadRequested,
}

/// Control seam: serve control-socket connections. The production impl
/// ([`UnixControl`]) accepts on a `UnixListener`; the run loop's idle select
/// drives it between polls. The test no-op never resolves, so it never wins the
/// select. A served connection may return a [`ControlSignal`] for the run loop to
/// apply (`None` for a pure `status` read).
pub(crate) trait Control {
    /// Serve at most one control connection from `snapshot`, then resolve to any
    /// [`ControlSignal`] the exchange produced (`None` if none).
    async fn serve(&self, snapshot: &StatusSnapshot) -> Option<ControlSignal>;
}

/// Periodic-refresh seam (issue #105): the run loop drives the in-daemon isolated-refresh
/// tick from its idle path, off the poll→usage→swap seam. The production impl
/// ([`crate::refresh_tick::RefreshTick`]) keeps PARKED accounts' stored tokens fresh through
/// the #102 engine — and is wholly inert when the feature is off: its `until_due` never
/// resolves, so a feature-off daemon (or a hermetic test wired with a no-op ticker) behaves
/// exactly as it did before #105.
///
/// Two methods so the run loop can serve the control socket WHILE waiting for the tick to
/// fall due, yet protect an in-flight sweep from being cancelled by a control read (only
/// shutdown interrupts a sweep): [`until_due`](RefreshTicker::until_due) is the wait;
/// [`sweep`](RefreshTicker::sweep) is the bounded work.
pub(crate) trait RefreshTicker {
    /// Resolve when a refresh sweep is due (the ticker's own cadence/idle gating, on its own
    /// [`Clock`] seam). MUST never resolve when the feature is disabled, so it never wins the
    /// idle select and adds no clock activity. Re-armable: the run loop awaits it afresh each
    /// idle iteration, and a control read between waits simply restarts it.
    async fn until_due(&mut self);
    /// Run ONE refresh sweep over the due parked accounts, EXCLUDING the `excluded` uuids
    /// (the active account + the imminent swap target the daemon supplies). `quarantined` is
    /// the daemon's currently-dead ("needs re-login") set: those accounts are refreshed even
    /// when not near expiry, and a successful one is reported for RESTORE (issue #106).
    /// Records the sweep for cadence gating. Per-account failures are non-fatal (the engine
    /// Caller contract). Returns the per-cycle [`SweepOutcome`] for the daemon to emit + apply.
    async fn sweep(&mut self, excluded: &[String], quarantined: &[String]) -> SweepOutcome;
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

/// Production control: accept one client at a time on the bound socket and answer
/// from the latest snapshot.
pub(crate) struct UnixControl {
    listener: UnixListener,
}

impl UnixControl {
    pub(crate) fn new(listener: UnixListener) -> Self {
        Self { listener }
    }
}

impl Control for UnixControl {
    async fn serve(&self, snapshot: &StatusSnapshot) -> Option<ControlSignal> {
        match self.listener.accept().await {
            Ok((stream, _addr)) => {
                // Authenticate the peer as the SAME local user (issue #64): a
                // state-affecting command (`manual-swapped`) is honored only from
                // our own uid. The socket is already `0600` in a `0700` dir, so
                // this is defense-in-depth — but the manual-hold receive path must
                // be authenticated, never trust-by-reachability. Peer creds are read
                // from the real fd here; `serve_control` takes the verdict as a
                // plain bool so it stays testable over an in-memory duplex.
                let peer_authenticated = peer_is_same_user(&stream);
                // Best-effort: a malformed or disconnected client must never crash
                // the daemon — drop the exchange (the reply carries nothing secret).
                serve_control(stream, snapshot, peer_authenticated)
                    .await
                    .unwrap_or(None)
            }
            Err(_) => None,
        }
    }
}

/// Whether the peer connected on `stream` is the same local user as this process
/// (issue #64). Reads the peer's effective uid via `getpeereid(2)` (the portable
/// BSD/macOS peer-credential call for a Unix-domain socket) and compares it to our
/// own `getuid()`. Any failure to read the credential is treated as NOT
/// authenticated — fail closed. Used to gate the state-affecting `manual-swapped`
/// command; the non-secret `status` read is not gated.
fn peer_is_same_user(stream: &tokio::net::UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;

    let mut euid: libc::uid_t = 0;
    let mut egid: libc::gid_t = 0;
    // SAFETY: `getpeereid` takes a valid connected-socket fd (owned by `stream`,
    // which outlives the call) and two out-pointers to stack locals it fills only
    // on success (rc == 0). No other preconditions.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut euid, &mut egid) };
    // SAFETY: `getuid` cannot fail and has no preconditions.
    rc == 0 && euid == unsafe { libc::getuid() }
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
}

/// The latest per-account reading the daemon exposes — over the control socket
/// and in the event log. Non-secret by construction: a handle (label), the active
/// flag, and percentages — never a token or email (issue #15).
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusSnapshot {
    pub(crate) accounts: Vec<AccountReading>,
    /// The next swap candidate as of this cycle (issue #88): who [`pick_target`]
    /// would rotate the active session to, or why there is no candidate. Computed
    /// daemon-side ([`Daemon::next_swap`]); [`status_response`] copies it straight
    /// onto the wire. `None` only when there is no active anchor to swap from.
    pub(crate) next_swap: Option<NextSwap>,
    /// Whether the periodic isolated-refresh tick is enabled in config (`[refresh].enabled`,
    /// issue #105) — copied from [`Daemon::refresh_enabled`] at build. Carried to the wire so
    /// the thin `status` client can surface the issue-#138 advisory (with the tick OFF,
    /// non-active accounts get no maintenance). `false` by `Default` (an all-defaults snapshot
    /// reads as tick-off), matching the opt-in default.
    pub(crate) refresh_enabled: bool,
}

/// The non-secret refresh-health inputs `status` surfaces in `--json` (issue #119): the
/// daemon's reduced projection of the refresh observations its per-account health state
/// carries — whether the last refresh kept the credential alive, whether CC rotated the
/// refresh-token VALUE, and the consecutive-failure streak. `None` (the whole struct) until
/// the refresh engine has observed the account at least once (e.g. the `[refresh]` feature
/// is off, or the account has not yet been swept). Every field is a boolean / count — never
/// a token or expiry (the #15 discipline). Derives `Deserialize` so the `status` client can
/// read it back; `#[serde(default)]` on the carrying field handles a pre-#119 daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RefreshHealth {
    /// Whether the LAST observed refresh kept the credential ALIVE (`refreshed` /
    /// `no_change`), as opposed to a `dead` (refresh token cleared) or `error` (cycle
    /// failed) outcome.
    pub(crate) last_ok: bool,
    /// Whether CC ROTATED the refresh-token value on the last refresh (the AC-3 durability
    /// signal) — the boolean only, never either token value. Named `rotated` (not
    /// `token_rotated`) so the `--json` field carries no `token` substring that a coarse
    /// #15 leak-proxy (`!contains("token")`) could false-positive on.
    pub(crate) rotated: bool,
    /// Consecutive refresh FAILURES (`dead` / `error` outcomes), reset to 0 by the next
    /// alive refresh — the rollup's at-risk input.
    pub(crate) consecutive_failures: u32,
}

/// One account's latest reading.
#[derive(Debug, Clone, Default)]
pub(crate) struct AccountReading {
    pub(crate) label: String,
    pub(crate) active: bool,
    /// Whether the account is in the rotation (issue #36) — surfaced so `status`
    /// can mark a parked account. A disabled account is shown but never swapped to.
    pub(crate) enabled: bool,
    /// Whether the account is QUARANTINED — its credential is dead and needs a
    /// re-login (issue #42). The durable "needs re-login" status `status` surfaces;
    /// non-secret (a plain flag on the account's handle).
    pub(crate) quarantined: bool,
    /// Whether a quarantined account is mid-RECOVERY — its credential is currently
    /// answering again (`quarantined && recovery_successes > 0`), climbing toward the
    /// un-quarantine threshold on the spontaneous-revival path (issue #109). A refinement
    /// of `quarantined` (always implies it), surfaced so `status` can render `recovering`
    /// instead of the alarming `needs re-login` for a healing account. Derived from the
    /// health counter (where it lives); non-secret — a plain flag, no raw count exposed.
    pub(crate) recovering: bool,
    /// Whether the account's WEEKLY window is EXHAUSTED — `weekly >= weekly_trigger`
    /// (the base, un-jittered threshold; issue #11/#37), the daemon's own viability
    /// verdict. When true the account is blocked until its weekly reset, so `status`
    /// keys its "resets in" off the weekly reset rather than the sooner session
    /// reset (issue #72). Precomputed here (where the threshold lives) so the wire
    /// projection stays threshold-free; `false` when the last poll failed.
    pub(crate) weekly_exhausted: bool,
    pub(crate) usage: Option<Usage>,
    /// The stored access-token `expiresAt` as epoch SECONDS (issue #119), or `None` until
    /// the refresh engine has observed this account's stash. An absolute instant (not a
    /// relative duration, like `session_resets_at`) carried RAW on the wire, from which a
    /// consumer (`--json` | `jq`) can derive an "expires in" against its own clock; the lean
    /// text view projects only the rollup glyph, not a clock cell. Non-secret — a timestamp.
    pub(crate) access_expires_at: Option<i64>,
    /// The non-secret refresh-health inputs (issue #119), or `None` until a refresh has been
    /// observed. The rollup's at-risk / dead inputs plus the `--json` durability signal.
    pub(crate) refresh_health: Option<RefreshHealth>,
    /// The daemon-computed 4-state credential-health rollup (issue #119) — the verdict the
    /// thin `status` client projects to a glyph. Computed in [`Daemon::snapshot`] from this
    /// account's health state and the wall clock.
    pub(crate) health: CredentialHealth,
}

/// The control socket's `status` reply — handles + percentages + the forward-looking
/// `next_swap` candidate, and nothing else (issue #15: never a token or email).
/// Derives both `Serialize` (the daemon writes it) and `Deserialize` (the `status`
/// client reads it), so this one definition is the whole wire contract. The durable,
/// timestamped swap HISTORY remains the event-log view (#9), not `status`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StatusResponse {
    pub(crate) accounts: Vec<AccountStatusLine>,
    /// The next swap candidate (issue #88), or `null` when there is no active anchor
    /// to swap from. `#[serde(default)]` per the added-field convention (cf.
    /// `session_resets_at`): a pre-#88 daemon that omits the field decodes to `None`.
    #[serde(default)]
    pub(crate) next_swap: Option<NextSwap>,
    /// Whether the daemon's periodic isolated-refresh tick is enabled (`[refresh].enabled`,
    /// issue #105). `Some(false)` is the ONLY value that arms the issue-#138 discoverability
    /// advisory (paired with ≥1 unhealthy/unverified non-active account); `Some(true)`
    /// suppresses it. `Option` + `#[serde(default)]` per the added-field convention (cf.
    /// `auth`): a pre-#138 daemon that omits the field decodes to `None`, which the client
    /// treats as "unknown → suppress" rather than mis-firing a stale advisory against an old
    /// daemon. Non-secret — a plain flag.
    #[serde(default)]
    pub(crate) refresh_enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AccountStatusLine {
    /// The operator-chosen handle (label) — never the email (issue #15).
    pub(crate) label: String,
    pub(crate) active: bool,
    /// Whether the account is in the rotation (issue #36); `false` for a parked
    /// account, which `status` marks. Non-secret — a plain flag.
    pub(crate) enabled: bool,
    /// Whether the account is QUARANTINED — its credential is dead and needs a
    /// re-login (issue #42). The durable "needs re-login" status; `false` for a
    /// healthy account. Non-secret — a plain flag.
    pub(crate) quarantined: bool,
    /// Whether a quarantined account is mid-RECOVERY — its credential is answering
    /// again and climbing toward un-quarantine (issue #109). Refines `quarantined`
    /// (true only when it is): lets `status` render `recovering` instead of the
    /// alarming `needs re-login` for a healing account, so an operator does not swap
    /// away from a recovering — and often healthier — account. Non-secret — a derived
    /// flag, no raw count. `#[serde(default)]` per the added-field convention (cf.
    /// `session_resets_at`): a pre-#109 daemon that omits it decodes to `false`.
    #[serde(default)]
    pub(crate) recovering: bool,
    /// Last-polled session-window usage percent (`0..=100`); `null` if the last
    /// poll for this account failed (never a fabricated `0`).
    pub(crate) session_pct: Option<u8>,
    /// Last-polled weekly-window usage percent (`0..=100`).
    pub(crate) weekly_pct: Option<u8>,
    /// Epoch seconds at which the rolling 5-hour SESSION window resets, or `null`
    /// when the last poll failed or the API supplied no parseable timestamp.
    /// Carried so the client can render a per-account "resets in" (issue #72); an
    /// absolute instant (not a relative duration), so the client computes the
    /// freshest delta against its own clock at print time. Non-secret — an integer.
    #[serde(default)]
    pub(crate) session_resets_at: Option<i64>,
    /// Epoch seconds at which the WEEKLY window resets (see `session_resets_at`).
    /// `null` when unknown. Non-secret — an integer.
    #[serde(default)]
    pub(crate) weekly_resets_at: Option<i64>,
    /// Whether the account's WEEKLY window is exhausted (`weekly >= weekly_trigger`),
    /// the daemon's own viability verdict (issue #11/#37). The client keys "resets
    /// in" off this: a weekly-exhausted account is blocked until the WEEKLY reset,
    /// otherwise the sooner SESSION reset governs (issue #72). Non-secret — a flag.
    #[serde(default)]
    pub(crate) weekly_exhausted: bool,
    /// The stored access-token `expiresAt` as epoch SECONDS (issue #119), or `null` until
    /// this account has been polled (issue #141) — sourced from the refresh sweep when
    /// `[refresh]` is on, otherwise from the poll path, so it is populated in the default
    /// config too. An absolute instant (not a relative duration, like `session_resets_at`)
    /// carried RAW for a consumer (`--json` | `jq`) to derive an "expires in" against its
    /// own clock; the lean text view projects only the rollup glyph, not a clock cell.
    /// Non-secret — a timestamp, never the token. `#[serde(default)]` per the added-field
    /// convention: a pre-#119 daemon that omits it decodes to `None`.
    #[serde(default)]
    pub(crate) access_expires_at: Option<i64>,
    /// The non-secret refresh-health inputs (issue #119) — last refresh ok? token rotated?
    /// consecutive failures — or `null` until a refresh has been observed (e.g. `[refresh]`
    /// off). The `--json` durability signal; also feeds the daemon's rollup. `#[serde(default)]`:
    /// a pre-#119 daemon omits it → `None`.
    #[serde(default)]
    pub(crate) refresh_health: Option<RefreshHealth>,
    /// The daemon-computed 4-state credential-auth rollup (issue #119): the verdict the
    /// thin read-only client projects to a glyph (🟢/🟡/🟠/🔴/⚪) under the `AUTH` column.
    /// Serialized on the `--json` wire as **`auth`** (issue #143 — the field reports the
    /// credential-AUTH standing, not a vague "health"; renamed while pre-release, no stable
    /// `--json` consumers yet); the Rust field keeps the name `health` to localize the
    /// rename to the wire key. `Option` for backward compatibility — `#[serde(default)]`
    /// makes a pre-#119 daemon (which omits the field) decode to `None`, and the client then
    /// FALLS BACK to the legacy quarantine-based text rather than mis-reading a defaulted
    /// `healthy` over a dead account.
    #[serde(default, rename = "auth")]
    pub(crate) health: Option<CredentialHealth>,
}

/// The next swap candidate shown by `status` (issue #88): who the daemon would
/// rotate the active session TO if a swap fired right now. DERIVED state —
/// recomputed each cycle from the latest readings — so, unlike the dropped in-process
/// `last_swap` (#8), it survives a daemon restart by construction and never reads
/// `none` merely because the process is young. Non-secret by construction: a roster
/// label or a bare reason, never a token or email (issue #15). One serializable type
/// for both [`StatusSnapshot`] (built each cycle) and [`StatusResponse`] (the wire),
/// mirroring the redaction posture of the now-removed `LastSwapLine`. Internally
/// tagged (`state`), so the three cases stay one self-describing field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum NextSwap {
    /// A viable target exists — [`pick_target`]'s choice, by roster label.
    Target { to: String },
    /// No sound swap destination — [`pick_target`] picked nothing AND this is not the
    /// post-restart all-unpolled moment (`AwaitingData`). Reached when at least one
    /// *live* (enabled, non-quarantined) other account has already been polled and none
    /// qualifies (weekly-exhausted, or over the opt-in session floor) — even while other
    /// live accounts are still unpolled (the staggered-warm-up #80 mixed case) — or when
    /// there is no live other account at all (every other disabled #36 or quarantined #42,
    /// its reading masked away by `decision_readings`, or there is simply no other account).
    NoViableTarget,
    /// No reading yet for any *live* (enabled, non-quarantined) other account — the
    /// post-restart moment, before the staggered poll loop (#80) has read the rotation.
    /// Kept distinct from `NoViableTarget` because it is exactly the moment an operator
    /// checks `status`; a quarantined account's masked-away reading does NOT count here
    /// (its data needs a re-login, not a poll).
    AwaitingData,
}

/// The `{"cmd": "..."}` control request.
#[derive(Deserialize)]
struct ControlRequest {
    cmd: String,
}

/// Project a [`StatusSnapshot`] into the wire [`StatusResponse`]. Sourced solely
/// from non-secret fields, so it can never carry a token or email (issue #15).
fn status_response(snapshot: &StatusSnapshot) -> StatusResponse {
    StatusResponse {
        accounts: snapshot
            .accounts
            .iter()
            .map(|account| AccountStatusLine {
                label: account.label.clone(),
                active: account.active,
                enabled: account.enabled,
                quarantined: account.quarantined,
                recovering: account.recovering,
                session_pct: account.usage.map(|u| to_pct(u.session)),
                weekly_pct: account.usage.map(|u| to_pct(u.weekly)),
                session_resets_at: account.usage.and_then(|u| u.session_resets_at),
                weekly_resets_at: account.usage.and_then(|u| u.weekly_resets_at),
                weekly_exhausted: account.weekly_exhausted,
                // The credential clocks + the daemon-computed rollup (issue #119), already
                // resolved at snapshot build; `health` is wrapped `Some` since a current
                // daemon always sends a verdict (the `Option` is purely pre-#119 wire compat).
                access_expires_at: account.access_expires_at,
                refresh_health: account.refresh_health,
                health: Some(account.health),
            })
            .collect(),
        // Already computed at snapshot build (issue #88); copy it to the wire.
        next_swap: snapshot.next_swap.clone(),
        // The config `[refresh].enabled` (#105) for the #138 advisory; wrapped `Some` since a
        // current daemon always knows it (the `Option` is purely pre-#138 wire compat, mirroring
        // `health`).
        refresh_enabled: Some(snapshot.refresh_enabled),
    }
}

/// A usage fraction in `[0.0, 1.0]` as a rounded, clamped `0..=100` percent.
fn to_pct(fraction: f64) -> u8 {
    (fraction * 100.0).round().clamp(0.0, 100.0) as u8
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

/// Fold an access-token `expiresAt` from CC's native epoch MILLISECONDS to the epoch
/// SECONDS the credential rollup and the `status` wire speak (issue #141 must-carry: the
/// blob is ms, the wire/rollup are s — a missed conversion misfires the operator clock by
/// 1000×). Integer division truncates any sub-second remainder, immaterial for a
/// token-lifetime clock and matching the refresh fold's `ms / 1000`
/// ([`Daemon::apply_refresh_observation`]). A pure function so the boundary is unit-tested.
fn millis_to_secs(ms: i64) -> i64 {
    ms / 1000
}

/// The daemon-side credential-health rollup (issue #119, extended by #137) — a PURE function
/// of one account's health inputs, its fresh-reading liveness signal, and the wall clock, so
/// it is unit-tested directly and computed identically for the display snapshot and the
/// transition-event diff. The thin `status` client just projects the returned verdict to a
/// glyph.
///
/// A SEVERITY ladder (most-severe wins), matching the issue's 🟢→🟡→🟠→🔴 ordering, plus a
/// distinct ⚪ `Unknown` for the no-evidence case (#137):
/// - **Dead** — `quarantined` (the #42 401-streak verdict) OR the last refresh outcome was
///   `Dead` (the refresh token was cleared in place). Both genuinely need `claude /login`;
///   surfacing a refresh-detected death as 🔴 too is more honest than hiding it (this is a
///   DISPLAY rollup — it never flips the quarantine machinery).
/// - **AtRisk** — the refresh safety-net is failing (`consecutive_refresh_failures > 0`):
///   a streak of `Error` cycles means the mechanism that prevents staleness/death is
///   struggling, so the account trends toward dead even while its token may still work.
/// - **Stale** — the stored REFRESH-sourced access token has EXPIRED (`access_expires_at <=
///   now_secs`) but the refresh token is still valid (not dead, not failing): a transient
///   window the next refresh recovers. Keys off `access_expires_at` ONLY (never the
///   poll-sourced clock), so an idle account's naturally-lapsed stashed expiry never
///   false-🟠s (#141/#137).
/// - **Healthy** — a POSITIVE liveness signal exists: a fresh successful usage reading
///   (`has_fresh_reading`), OR refresh telemetry, OR a (future) refresh-sourced expiry.
/// - **Unknown** — none of the above AND no positive liveness signal (#137): a non-active
///   account never successfully polled, `[refresh]` off, no/unknown `access_expires_at`.
///   Absence of a NEGATIVE signal is not health; the daemon reports "unverified" rather than
///   a false 🟢 that would jump straight to 🔴 the moment the 401-streak quarantines it.
///
/// `has_fresh_reading` is this account's masked [`decision_readings`](Daemon::decision_readings)
/// entry being `Some` — a SUCCESSFUL poll against the live API (the strongest liveness proof),
/// `None` for a failed poll or an out-of-rotation account. Deliberately NOT `poll_expires_at`:
/// that clock is written on every poll ATTEMPT (even a 401 against a readable-but-revoked
/// stash), so it cannot distinguish alive from the exact lapsed-credential bug #137 fixes; it
/// stays the display clock only (`--json`, via [`Daemon::snapshot`]'s `.or()` fallback).
fn credential_health(
    quarantined: bool,
    last_refresh_outcome: Option<RefreshEventOutcome>,
    consecutive_refresh_failures: u32,
    access_expires_at: Option<i64>,
    has_fresh_reading: bool,
    now_secs: i64,
) -> CredentialHealth {
    if quarantined || last_refresh_outcome == Some(RefreshEventOutcome::Dead) {
        CredentialHealth::Dead
    } else if consecutive_refresh_failures > 0 {
        CredentialHealth::AtRisk
    } else if access_expires_at.is_some_and(|expires_at| expires_at <= now_secs) {
        CredentialHealth::Stale
    } else if has_fresh_reading || last_refresh_outcome.is_some() || access_expires_at.is_some() {
        CredentialHealth::Healthy
    } else {
        CredentialHealth::Unknown
    }
}

/// Reduce one account's stored refresh observations into the non-secret [`RefreshHealth`]
/// the wire surfaces (issue #119), or `None` when no refresh has been observed yet. `last_ok`
/// collapses the full outcome to alive-vs-not (`Refreshed` / `NoChange` ⇒ ok; `Dead` /
/// `Error` ⇒ not), the rollup's finer `Dead`-vs-`Error` distinction having already been
/// applied by [`credential_health`].
fn refresh_health_view(health: &AccountHealth) -> Option<RefreshHealth> {
    let outcome = health.last_refresh_outcome?;
    Some(RefreshHealth {
        last_ok: matches!(
            outcome,
            RefreshEventOutcome::Refreshed
                | RefreshEventOutcome::RefreshedNotReStashed
                | RefreshEventOutcome::NoChange
        ),
        rotated: health.refresh_token_rotated.unwrap_or(false),
        consecutive_failures: health.consecutive_refresh_failures,
    })
}

/// Build the one-line reply to a control request line, plus any [`ControlSignal`]
/// the run loop must apply afterward. Pure (no I/O, no clock), so the
/// request→(reply, signal) mapping is unit-testable; `peer_authenticated` is
/// passed in (computed from the real fd by the caller) rather than read here, for
/// the same testability reason `in_cooldown` is a parameter elsewhere.
///
/// `status` is a non-secret read, answered for any peer. `manual-swapped` (issue
/// #64) is state-affecting, so it is honored ONLY for an authenticated same-user
/// peer; an unauthenticated one gets an error and produces NO signal (the cooldown
/// is never armed by a stranger).
fn control_reply(
    line: &str,
    snapshot: &StatusSnapshot,
    peer_authenticated: bool,
) -> (String, Option<ControlSignal>) {
    match serde_json::from_str::<ControlRequest>(line) {
        Ok(request) if request.cmd == "status" => (
            serde_json::to_string(&status_response(snapshot))
                .unwrap_or_else(|_| r#"{"error":"encode failed"}"#.to_owned()),
            None,
        ),
        Ok(request) if request.cmd == "manual-swapped" => {
            if peer_authenticated {
                (
                    r#"{"ok":true}"#.to_owned(),
                    Some(ControlSignal::ManualSwapped),
                )
            } else {
                (r#"{"error":"unauthorized"}"#.to_owned(), None)
            }
        }
        // `roster-reload` (issue #139) is state-affecting — it makes the daemon adopt a
        // new on-disk roster — so, like `manual-swapped`, it is honored ONLY for an
        // authenticated same-user peer; an unauthenticated one gets an error and
        // produces NO signal (a stranger can never make the daemon re-read its config).
        Ok(request) if request.cmd == "roster-reload" => {
            if peer_authenticated {
                (
                    r#"{"ok":true}"#.to_owned(),
                    Some(ControlSignal::RosterReloadRequested),
                )
            } else {
                (r#"{"error":"unauthorized"}"#.to_owned(), None)
            }
        }
        Ok(_) => (r#"{"error":"unknown command"}"#.to_owned(), None),
        Err(_) => (r#"{"error":"malformed request"}"#.to_owned(), None),
    }
}

/// Upper bound on a single control-socket request line. A control request is one
/// short JSON command (`{"cmd":"status"}` / `{"cmd":"manual-swapped"}`); capping the
/// read keeps a misbehaving same-uid client from growing the daemon's buffer without
/// bound (issue #64 — the receive path must be BOUNDED).
const MAX_CONTROL_LINE_BYTES: u64 = 8 * 1024;

/// Upper bound on one whole control exchange (read request + write reply). Mirrors
/// the `use`-side `CONTROL_SOCKET_TIMEOUT` so a peer that never completes its line
/// cannot hold the serve arm; the run-loop select also drops this future at the next
/// poll tick, so this is the tighter, dedicated time bound (issue #64).
const CONTROL_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(2);

/// Serve one control exchange: read one newline-delimited JSON request and write
/// one newline-delimited JSON reply, returning any [`ControlSignal`] the request
/// produced. Generic over the stream so it is testable over an in-memory duplex
/// without binding a real socket; `peer_authenticated` is the caller's
/// peer-credential verdict (issue #64), gating the state-affecting commands. The
/// receive path is BOUNDED in space (the read is capped at [`MAX_CONTROL_LINE_BYTES`])
/// and in time (the exchange is wrapped in [`CONTROL_EXCHANGE_TIMEOUT`]).
async fn serve_control<RW>(
    stream: RW,
    snapshot: &StatusSnapshot,
    peer_authenticated: bool,
) -> Result<Option<ControlSignal>>
where
    RW: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

    let exchange = async {
        // Cap the request read: a control request is one short line, so a peer that
        // streams more — or never sends a newline — is bounded here (EOF at the
        // limit) instead of growing `line` without limit.
        let mut buffered = tokio::io::BufReader::new(stream);
        let mut line = String::new();
        (&mut buffered)
            .take(MAX_CONTROL_LINE_BYTES)
            .read_line(&mut line)
            .await?;
        let (reply, signal) = control_reply(line.trim_end(), snapshot, peer_authenticated);
        buffered.write_all(reply.as_bytes()).await?;
        buffered.write_all(b"\n").await?;
        buffered.flush().await?;
        Ok::<_, Error>(signal)
    };
    // A peer that stalls mid-line must not hold the exchange open: time-box it and
    // drop on elapse. The reply carries nothing secret, so a dropped exchange is
    // harmless — the caller maps both a timeout and an error to "no signal".
    match tokio::time::timeout(CONTROL_EXCHANGE_TIMEOUT, exchange).await {
        Ok(result) => result,
        Err(_elapsed) => Ok(None),
    }
}

/// Upper bound on the client-side `roster-reload` notify exchange (issue #139) — the
/// CLI-verb counterpart of the server's [`CONTROL_EXCHANGE_TIMEOUT`]. Mirrors the
/// `use`-side manual-hold notify (#64): a missing / wedged daemon must never hang the
/// `capture` / `login` / `remove` verb, so the whole connect→send→ack exchange is
/// time-boxed and any failure degrades to a logged best-effort skip.
const ROSTER_RELOAD_NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);

/// Notify a running daemon that the on-disk roster changed (issue #139), so it
/// re-reads `config.toml` and reconciles its in-memory rotation WITHOUT a restart.
/// The CLI-verb counterpart of the daemon's `roster-reload` control handler
/// ([`control_reply`]); sends one newline-delimited `{"cmd":"roster-reload"}` request
/// and reads the one-line ack so the daemon has RECEIVED it before returning.
///
/// BEST-EFFORT by contract, exactly like the `use` manual-hold notify (#64): the
/// on-disk `config.toml` is authoritative (the write already succeeded), so a notify
/// failure — no daemon running (connect refused / socket absent), a timeout, an I/O
/// error — is for the CALLER to log and ignore, never fatal. Bounded by
/// [`ROSTER_RELOAD_NOTIFY_TIMEOUT`] so a missing / wedged daemon can never hang the
/// verb. Carries NO credential and NO write target — a pure reload signal (the daemon
/// re-reads the authoritative file itself).
pub(crate) async fn notify_roster_reload(socket: &Path) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let exchange = async {
        let stream = tokio::net::UnixStream::connect(socket).await?;
        let mut buffered = tokio::io::BufReader::new(stream);
        buffered.write_all(b"{\"cmd\":\"roster-reload\"}\n").await?;
        buffered.flush().await?;
        // Read the one-line ack so the daemon has processed the request before we
        // return; the content is irrelevant (any failure is non-fatal for the caller).
        let mut line = String::new();
        buffered.read_line(&mut line).await?;
        Ok::<(), Error>(())
    };
    tokio::time::timeout(ROSTER_RELOAD_NOTIFY_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "roster-reload notify timed out",
            ))
        })?
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
    /// other account is weekly-exhausted (or, with the opt-in session floor
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
    /// jittered poll interval (issue #38); `Some(d)` = an explicit wait that widens
    /// the gap between retries — either the locked-keychain back-off (issue #13,
    /// while the keychain stays locked) or the rate-limit / transient poll back-off
    /// (issue #76, while the usage endpoint keeps returning `429` / `5xx`).
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
#[derive(Default, Clone)]
struct AccountHealth {
    /// Consecutive non-scope 401s on this account's stored token. Incremented on a
    /// 401, reset to 0 on ANY non-401 outcome (success, 403, transient, locked). The
    /// `consecutive=` field of a `monitor_401` event while still healthy; reaching
    /// `monitor_401_n` declares the account DEAD ([`quarantined`](Self::quarantined)).
    consec_401: u32,
    /// Whether this account is QUARANTINED — its credential is dead (rejected
    /// `monitor_401_n` times in a row), so the daemon stops polling and selecting it
    /// for the rotation until the operator re-logs-in. The durable "needs re-login"
    /// status surfaced by `status` (issue #42), and the edge that fires the
    /// [`Event::CredentialDead`] / [`Event::CredentialRestored`] signals exactly once
    /// per transition.
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
    /// Consecutive cycles whose polls saw a rate-limit (`429`) or transient (`5xx`
    /// / network) failure and therefore backed off (issue #76). Drives the
    /// exponential widening of [`TickOutcome::next_wait`]: the wait is this cycle's
    /// jittered poll interval times `2^min(streak, POLL_BACKOFF_MAX_SHIFT)`, capped
    /// at [`POLL_BACKOFF_CAP`] (and never below a server-advised `Retry-After`).
    /// Reset to 0 on the first cycle whose polls are all clean, so a later
    /// throttling episode starts the climb afresh. Distinct from `lock_backoff`: the
    /// keychain lock short-circuits the whole tick (`locked_tick`), whereas this
    /// rides a tick that DID poll and was throttled.
    poll_backoff_streak: u32,
    /// Last-known usage reading per roster account (issue #80), indexed by roster
    /// position. The daemon polls ONE account per tick (round-robin, active first),
    /// so a decision is taken on the most recent reading of EACH account rather than
    /// a single-instant poll-of-all — one account's number may be ~a cycle older than
    /// another's. `None` until an account is first polled (or after a poll fails).
    /// Sized to the roster in [`Daemon::new`]. The decision/snapshot view masks an
    /// out-of-rotation (disabled / quarantined) non-active account back to `None`
    /// ([`decision_readings`](Daemon::decision_readings)), so stale carried data can
    /// never leak into [`pick_target`].
    last_readings: Vec<Option<Usage>>,
    /// The staggered poll schedule for the CURRENT cycle (issue #80): the roster
    /// indices to poll, in order — the active account first (its swap-away trigger is
    /// the most time-sensitive), then every enabled, non-quarantined non-active
    /// account. One entry is consumed per tick; when [`poll_pos`](Self::poll_pos)
    /// reaches its end the schedule is rebuilt for the next cycle (re-resolving active
    /// and re-reading rotation membership). Empty only for a degenerate roster (no
    /// active and nothing enabled), in which case a tick polls nothing.
    poll_schedule: Vec<usize>,
    /// Cursor into [`poll_schedule`](Self::poll_schedule): the position to poll this
    /// tick. Advances by one per tick and triggers a schedule rebuild on wrap, so the
    /// daemon walks active → spare → spare → … one account per sub-interval instead of
    /// bursting all at once (issue #80).
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
    /// Opt-in swap-target session guard (#10): `Some(fraction)` only swaps TO an
    /// account whose session usage is below it (`session_floor / 100`); `None` (the
    /// default) disables the guard, leaving target choice to the soonest-reset rule
    /// (issue #37) constrained by the always-on session gate (`session < session_trigger`)
    /// — which is what prevents session-saturated oscillation; the floor is a STRICTER
    /// reserve layered on top.
    session_floor: Option<f64>,
    /// Per-cycle post-swap cooldown strategy (issue #38; the #10 seam — see
    /// [`DecisionState`]): drawn + clamped to `0..=3600` s each cycle. Replaces
    /// the former fixed `cooldown` duration.
    cooldown_strategy: Strategy,
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
    /// ([`RealRefreshEngine`]) via [`with_refresh_engine`](Self::with_refresh_engine),
    /// gated on the same effective switch as the periodic tick; when `Some`, a usage 401
    /// attempts one isolated refresh + re-poll before it can quarantine the account.
    poll_refresh: Option<Box<dyn PollRefresh>>,
    /// The usage-stats store maintenance seam (issue #161), or `None` to disable it (the
    /// hermetic-test default — a test with no on-disk store wires nothing, so the collector's
    /// roll/gap emission is wholly inert and the ~existing `tick` tests are unaffected).
    /// Production wires the config-derived [`RetentionPolicy`] via
    /// [`with_stats`](Self::with_stats); when `Some`, each poll runs the cadence-gated
    /// `compact_and_roll` (emitting a redacted `usage_rollup` when a pass folds samples) and
    /// records a rate-limited redacted `usage_gap` on a no-reading poll. The append-per-poll
    /// collector (#156) runs regardless — this seam adds only the roll + gap-event layer.
    stats: Option<RetentionPolicy>,
    /// Whether the operator turned the periodic isolated-refresh tick ON in config
    /// (`[refresh].enabled`, issue #105) — the CONFIG value, NOT the effective switch (which
    /// also requires a resolvable `claude` binary). Carried onto the display snapshot so the
    /// thin `status` client can surface the issue-#138 discoverability advisory: with the tick
    /// OFF, non-active accounts get no maintenance and their credentials can silently lapse.
    /// `false` by default (the opt-in default, #105); production sets the real config value via
    /// [`with_refresh_enabled`](Self::with_refresh_enabled). Purely a display signal — never a
    /// swap/poll decision input.
    refresh_enabled: bool,
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
            session_floor: tunables.session_floor.map(|floor| f64::from(floor) / 100.0),
            cooldown_strategy: tunables.cooldown_strategy,
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
            // The periodic-refresh tick defaults OFF (opt-in, #105); production sets the real
            // `config.refresh.enabled` via `with_refresh_enabled`. Left false, the #138 advisory
            // stays inert (it also requires an unhealthy non-active account to fire).
            refresh_enabled: false,
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
    /// (gated on the same effective switch as the periodic tick — a resolvable `claude`
    /// binary); left unset, a test / feature-off daemon behaves exactly as before. Builder-
    /// style to mirror `with_swap_lock` / `with_config_path` and keep `new`'s args stable.
    pub(crate) fn with_refresh_engine(mut self, engine: Box<dyn PollRefresh>) -> Self {
        self.poll_refresh = Some(engine);
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
        // (issue #80): active first, then each enabled, non-quarantined non-active in
        // turn, one account per sub-interval, so the N requests of a cycle land in N
        // separate rate-limit windows instead of a single back-to-back burst (most of
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
        // Whether THIS poll was rate-limited (`429`) or a transient (`5xx` / network)
        // failure, with any server-advised `Retry-After` — the inputs to the poll
        // back-off (issue #76). Rate-limiting is an endpoint-global signal (one usage
        // endpoint for the whole roster), so the single account seeing it this tick
        // backs off the WHOLE loop, not just itself.
        let mut backed_off = false;
        let mut retry_after_floor: Option<Duration> = None;
        if let Some(i) = poll_idx {
            let polled = self.poller.poll(&self.roster[i], active == Some(i)).await;
            // Record ONE usage sample for this poll (issue #156): piggyback the
            // reading just fetched (no extra usage-API call), recording nothing on a
            // gap and swallowing any store error. Off the swap-decision path — a
            // sampling failure never perturbs the loop below.
            record_usage_sample(&self.roster[i].label, &polled);
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
                result = self.refresh_retry(i).await;
            }
            self.note_poll_outcome(i, &result, &mut events);
            diagnostics.push(Diagnostic::Poll {
                account: self.roster[i].label.clone(),
                outcome: diag_poll_class(&result),
            });
            if let Some(signal) = backoff_signal(&result) {
                backed_off = true;
                retry_after_floor = signal.retry_after;
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
        // Rate-limit / transient back-off (issue #76): a cycle whose polls saw a
        // `429` / `5xx` widens the next poll's spacing instead of re-polling at the
        // fixed interval; a fully-clean cycle resets the climb so a later episode
        // starts afresh.
        let next_wait = if backed_off {
            Some(self.note_poll_backoff(retry_after_floor))
        } else {
            self.state.poll_backoff_streak = 0;
            None
        };
        // The per-tick decision diagnostic (issue #77), with any back-off this tick
        // imposed — the decision class names what the loop did (swap / hold / skip /
        // all_exhausted / …); a `None` back-off omits the field.
        diagnostics.push(Diagnostic::Tick {
            decision: action.decision_class(),
            backoff_secs: next_wait.map(|wait| wait.as_secs()),
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
    /// each cycle. The schedule is the active account first, then every enabled,
    /// non-quarantined non-active account (see [`build_poll_schedule`](Self::build_poll_schedule));
    /// consuming one entry per tick spaces a cycle's N polls across N sub-intervals.
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

    /// Build this cycle's poll schedule (issue #80): the active account FIRST (its
    /// swap-away trigger is the most time-sensitive), then every enabled (#36),
    /// non-quarantined (#42) non-active account in roster order. The active account is
    /// always included even when disabled / quarantined (its swap-AWAY trigger must
    /// still fire and a dead active is re-probed), exactly as the former poll-all loop
    /// did; a disabled / quarantined non-active is excluded (never a swap target, and
    /// polling its dead token would waste a `curl`).
    fn build_poll_schedule(&self, active: Option<usize>) -> Vec<usize> {
        let mut schedule = Vec::with_capacity(self.roster.len());
        if let Some(a) = active {
            schedule.push(a);
        }
        for i in 0..self.roster.len() {
            if active == Some(i) {
                continue; // already first
            }
            if self.roster[i].enabled && !self.state.health[i].quarantined {
                schedule.push(i);
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

    /// The number of accounts in the current poll rotation (issue #80): the active
    /// account plus every enabled, non-quarantined non-active account — the schedule
    /// length, and the divisor that spreads a cycle's polls across the interval (see
    /// [`next_subinterval`](Self::next_subinterval)). At least 0; callers clamp to ≥ 1.
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
    ///   an operator re-login — never re-refreshed on every re-probe poll), and
    /// - this is the FIRST 401 of the current streak episode (`consec_401 == 0`).
    ///
    /// The last condition is the once-per-episode guard (AC-4, no refresh storm): a refresh
    /// spawns `claude -p` under the swap lock (seconds), so a persistently-401 account must
    /// refresh at most once per streak — the first 401 attempts the revive; the rest of the
    /// episode advances the streak directly.
    fn should_refresh_retry(&self, i: usize, result: &Result<Usage>) -> bool {
        self.poll_refresh.is_some()
            && matches!(classify_poll(result), PollOutcome::Unauthorized)
            && !self.state.health[i].quarantined
            && self.state.health[i].consec_401 == 0
    }

    /// Attempt one isolated refresh of account `i` (the #102 engine) and a single re-poll,
    /// returning the outcome [`note_poll_outcome`](Self::note_poll_outcome) then folds into
    /// the streak (issue #162). Only called when [`should_refresh_retry`](Self::should_refresh_retry)
    /// holds, so `poll_refresh` is `Some`.
    ///
    /// - Refresh reports **`Dead`** (the refresh token was cleared in place, `refresh.rs`) →
    ///   a genuine death: skip the re-poll and let the 401 stand so the streak advances.
    /// - Refresh ran otherwise (refreshed / no-change / even an engine error report) → the
    ///   account's STASH may now bear a fresh token, so re-poll THROUGH THE STASH
    ///   (`active = false`). Re-polling the stash is a liveness probe that never touches the
    ///   live canonical credential — the deliberate, safe path for the ACTIVE account too:
    ///   its 401 is the most urgent so it is NOT skipped, but confirming its liveness must
    ///   never mutate the credential its live session depends on. A stale-stash `Dead` for an
    ///   active account therefore only advances the streak by one (a later canonical re-poll
    ///   can still clear it), never an instant false-kill.
    /// - The refresh itself **errors** → "could not revive"; fail-safe by letting the 401
    ///   stand. A refresh failure never crashes the poll loop.
    async fn refresh_retry(&self, i: usize) -> Result<Usage> {
        let refreshed = match self.poll_refresh.as_ref() {
            Some(engine) => engine.refresh(&self.roster[i]).await,
            // Unreachable given the `should_refresh_retry` guard; treat as could-not-revive.
            None => return Err(Error::UsageUnauthorized),
        };
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
            self.session_floor,
            session_trigger,
            weekly_trigger,
        ) else {
            // No viable target — every other account is weekly-exhausted, session-
            // saturated (over the always-on session gate), or over the opt-in floor.
            // The all-exhausted TERMINAL state (issue #11): HOLD, do NOT swap (swapping
            // among exhausted accounts only thrashes), and emit ONE edge-triggered
            // signal naming the least-bad account by soonest weekly reset, so the
            // operator knows when relief arrives. (For the session-saturated case relief
            // actually arrives at the sooner SESSION reset; keying the hint off session
            // when the block is session-wide is a follow-up — the minimal gate keeps the
            // existing weekly-reset hint.) The active account is left exactly as is.
            // The signal is edge-triggered: emit only on ENTERING the state, so the
            // payload is computed once per episode, not every poll while it holds.
            if !self.state.signaled_all_exhausted {
                let (hold_idx, resets_at) = match soonest_weekly_reset(readings) {
                    Some((idx, at)) => (idx, Some(at)),
                    // No account reported a parseable weekly reset: fall back to the
                    // active account, timestamp omitted (forward-compatible).
                    None => (active_idx, None),
                };
                events.push(Event::AllExhausted {
                    hold: self.roster[hold_idx].label.clone(),
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
        // fresh one (active-first, then enabled non-quarantined) at the next cycle start.
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
            self.session_floor,
            // No always-on session gate on the emergency path: the active credential is
            // DEAD, so liveness beats optimality — escape to a live account even if it is
            // over the session trigger. (An opt-in `session_floor`, passed just above, is
            // still honored: a configured reserve is not breached even in the emergency;
            // with the default floor OFF, any live account qualifies.) This cannot
            // ping-pong: the dead active is quarantined (never a viable target), and once
            // a session-fresh target exists the normal path's session gate moves off the
            // saturated account cleanly.
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
    /// `session_floor` / triggers `pick_target` consumes). Uses the BASE (un-jittered)
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
        if let Some(target) = pick_target(
            active_idx,
            readings,
            &enabled,
            self.session_floor,
            self.session_trigger_base,
            self.weekly_trigger_base,
        ) {
            return Some(NextSwap::Target {
                to: self.roster[target].label.clone(),
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
                self.session_floor,
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
    fn apply_refresh_observation(&mut self, observation: &RefreshObservation) {
        let Some(idx) = self
            .roster
            .iter()
            .position(|a| a.account_uuid == observation.account_uuid)
        else {
            return;
        };
        let health = &mut self.state.health[idx];
        // ms → s at the boundary; the rollup/wire are uniform epoch seconds.
        health.access_expires_at = observation.expires_at_ms.map(|ms| ms / 1000);
        if let Some(delta) = observation.refresh {
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
        }
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

    /// Recompute every account's 4-state credential-health rollup (issue #119) against
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
                        // The credential clocks + the daemon-computed 4-state rollup (issue
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
    /// jittered interval divided by the rotation size, so the N accounts of a cycle
    /// are spaced ~`poll_secs / N` apart (≈40–45 s for a typical roster) and a full
    /// sweep still takes ~one `poll_secs`. Each sub-interval draws a fresh full
    /// interval (inheriting the #38 jitter decorrelation) before dividing. The divisor
    /// is clamped to ≥ 1 so a single-account roster simply waits the whole interval —
    /// there is nothing to stagger and no burst is possible.
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
    /// `Some(d)` → an explicit back-off duration — the locked-keychain back-off
    /// (issue #13) or the rate-limit / transient poll back-off (issue #76). Behind
    /// the [`Clock`] seam, so tests drive both paths deterministically.
    pub(crate) async fn wait_after_tick(&mut self, next_wait: Option<Duration>) {
        match next_wait {
            Some(backoff) => self.clock.tick(backoff).await,
            None => self.wait_for_next_poll().await,
        }
    }

    /// Fold a backed-off cycle (a `429` / `5xx` poll, issue #76) into the poll
    /// back-off and return the wait that WIDENS the next poll's spacing. The base is
    /// this cycle's freshly-drawn, jittered poll interval — so the back-off inherits
    /// the #38 decorrelation — multiplied by `2^min(streak, POLL_BACKOFF_MAX_SHIFT)`,
    /// then clamped to [`POLL_BACKOFF_CAP`]. The first
    /// backed-off cycle already waits ~2× the normal interval, so the effective
    /// spacing is WIDER than re-polling at the fixed cadence — the issue's core ask.
    /// A server-advised `Retry-After` is honoured as a MINIMUM: the wait is never
    /// shorter than it, even past the cap. Advances and stores the streak so the next
    /// consecutive backed-off cycle doubles again.
    fn note_poll_backoff(&mut self, retry_after: Option<Duration>) -> Duration {
        let streak = self.state.poll_backoff_streak.saturating_add(1);
        self.state.poll_backoff_streak = streak;
        let shift = streak.min(POLL_BACKOFF_MAX_SHIFT);
        let widened = self
            .next_poll_interval()
            .checked_mul(1u32 << shift)
            .unwrap_or(POLL_BACKOFF_CAP)
            .min(POLL_BACKOFF_CAP);
        match retry_after {
            Some(ra) => widened.max(ra),
            None => widened,
        }
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
/// (rate-limited) or a `5xx` / network transient. Carries the server-advised
/// `Retry-After` the response supplied, if any.
struct BackoffSignal {
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
        Err(Error::UsageRateLimited { retry_after, .. })
        | Err(Error::UsageTransient { retry_after, .. }) => Some(BackoffSignal {
            retry_after: *retry_after,
        }),
        _ => None,
    }
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
/// predicate on BOTH dimensions. It is always-on, distinct from the opt-in `floor`
/// (#10) — a STRICTER reserve layered on top (effective ceiling
/// `min(session_trigger, floor)`). The disabled exclusion (#36): a parked account
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
    readings
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != active)
        .filter(|&(i, _)| enabled[i])
        .filter_map(|(i, reading)| reading.map(|usage| (i, usage)))
        .filter(|&(_, usage)| usage.weekly < weekly_trigger)
        // Always-on session anti-thrash gate: exclude a target at/above the session
        // trigger — it would immediately re-trip [`swap::decide`]'s session dimension
        // and thrash (the exact mirror of the weekly filter above). Distinct from the
        // opt-in `floor` below, which tightens this ceiling further when set.
        .filter(|&(_, usage)| usage.session < session_trigger)
        .filter(|&(_, usage)| floor.is_none_or(|f| usage.session < f))
        // Soonest weekly reset (issue #37). The key sorts a known reset ahead of an
        // unknown one (`false` < `true`), then by the reset epoch ascending;
        // `min_by_key` keeps the first of equal keys, so an exact tie — or an
        // all-unknown field — falls to the earliest roster index, matching
        // [`soonest_weekly_reset`]'s tie-break (#11).
        .min_by_key(|&(_, usage)| match usage.weekly_resets_at {
            Some(resets_at) => (false, resets_at),
            None => (true, i64::MAX),
        })
        .map(|(i, _)| i)
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

/// The console line for a swap this cycle, or `None` for any non-swap outcome.
/// Surfaced to the operator watching the foreground `run` (issue #8) — the file
/// event log records every cycle separately. Both swap kinds echo: a normal swap
/// and the #42 emergency swap away from a dead active credential (the latter named
/// distinctly, since it means a credential just died and the daemon force-rotated).
/// Sourced solely from labels, so it can never carry a token or email (issue #15).
fn swap_report(outcome: &TickOutcome) -> Option<String> {
    match outcome.action {
        TickAction::Swapped { from, to } => Some(format!(
            // `off <from> onto <to>` rather than `<from> → <to>` (issue #89): the
            // bare arrow reads ambiguously, but `to` is the account just made
            // active (swapped ONTO) and `from` the one swapped OFF — spell it out
            // so the operator can never misread the direction.
            "swapped off {} onto {}",
            label_at(&outcome.snapshot, from),
            label_at(&outcome.snapshot, to),
        )),
        TickAction::EmergencySwapped { from, to } => Some(format!(
            // Same off/onto phrasing (#89), still named distinctly — the trailing
            // cause tells the operator a credential just died and forced this.
            "emergency-swapped off {} onto {} (dead credential)",
            label_at(&outcome.snapshot, from),
            label_at(&outcome.snapshot, to),
        )),
        _ => None,
    }
}

/// The label of the roster account at `index` in `snapshot`, or `"?"` if out of
/// range. A swap's indices are always valid, but the long-running daemon must
/// never panic on a display path, so this stays total.
fn label_at(snapshot: &StatusSnapshot, index: usize) -> &str {
    snapshot
        .accounts
        .get(index)
        .map_or("?", |account| account.label.as_str())
}

/// Drive the poll loop until shutdown.
///
/// Reconcile-on-start, then forever: tick, log (the event log `log` AND the
/// operator-facing diagnostic channel `diag`, issue #77), and idle until the next
/// poll — meanwhile serving control requests and watching for shutdown. Shutdown is
/// observed only HERE (between ticks), never mid-tick: a swap inside [`Daemon::tick`]
/// always runs to completion, so a shutdown can never tear a swap
/// (complete-or-abort; #6 is no-half-swap). The lifecycle markers (`diag=start` /
/// `diag=stop`) bracket this call in [`crate::cli`], which owns the process
/// lifecycle; this loop emits only the per-tick diagnostics.
pub(crate) async fn run_loop<P, C, S, K, Sh, Ctl, R, LW, W>(
    daemon: &mut Daemon<P, C, S, K>,
    log: &mut EventLog,
    diag: &mut DiagnosticLog<W>,
    shutdown: &mut Sh,
    control: &Ctl,
    refresh: &mut R,
    login_watch: &mut LW,
) -> Result<()>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
    Sh: Shutdown,
    Ctl: Control,
    R: RefreshTicker,
    LW: ExternalLoginWatch,
    W: Write,
{
    // Reconcile-on-start is best-effort: a failure is logged and the loop still
    // starts — the next swap re-establishes consistency anyway.
    if let Err(err) = daemon.reconcile_on_start().await {
        eprintln!("sessiometer: reconcile-on-start skipped: {err}");
    }

    /// How the idle-until-next-tick wait ended. Scoping the wait future to one
    /// block lets it (and its `&mut Daemon` borrow) drop before the run loop
    /// applies a `ManualSwapped` adoption, which needs its own `&mut Daemon`.
    enum Idle {
        /// SIGINT / SIGTERM observed — exit the loop cleanly.
        Shutdown,
        /// The poll interval (or a back-off wait — #13 locked-keychain or #76
        /// rate-limit) elapsed — re-tick.
        Elapsed,
        /// A manual `use` swap notified the daemon (#64) — adopt it, then re-tick.
        ManualSwapped,
        /// A roster write (`capture` / `login` / `remove`) notified the daemon (#139)
        /// — reload + reconcile the in-memory roster, then re-tick.
        RosterReloadRequested,
        /// The external-login watch (#140) saw the canonical credential change out-of-band
        /// during the idle (a manual `claude /login`) — re-tick NOW, off the usage-poll cadence,
        /// so the next `tick`'s `reconcile_canonical_change` re-stashes / re-resolves / surfaces
        /// it within the watch cadence instead of up to a full poll interval later.
        ExternalLoginDetected,
    }

    // De-burst start-up (issue #76): wait a small jittered delay before the FIRST
    // poll, so repeated restarts of the same config do not synchronize an immediate
    // burst of usage requests. Behind the Clock seam, so tests pass through it
    // instantly. Shutdown-responsive (like the per-cycle idle below): a SIGINT /
    // SIGTERM during the delay exits cleanly rather than being deferred for up to
    // STARTUP_DELAY_CAP. No control serving here — there is no snapshot to answer
    // from until the first tick.
    let startup_delay = daemon.startup_delay();
    tokio::select! {
        biased;
        _ = shutdown.requested() => return Ok(()),
        _ = daemon.clock.tick(startup_delay) => {}
    }

    loop {
        let outcome = daemon.tick().await;
        // Best-effort logging (issue #9): emit each event the tick produced. A
        // write failure must not kill the daemon, and one failed event must not
        // drop the rest of the tick's events — so log and continue, never return.
        for event in &outcome.events {
            if let Err(err) = log.emit(event) {
                eprintln!("sessiometer: event log write failed: {err}");
            }
        }
        // Operator-facing diagnostics (issue #77): emit each to the diagnostic
        // channel, which DROPS them unless `-v`/`--verbose` was passed. Per-poll
        // outcomes, the per-tick decision, and any all-exhausted-leave edge — the
        // run-debugging detail the edge-triggered event log deliberately omits.
        for diagnostic in &outcome.diagnostics {
            diag.emit(diagnostic);
        }
        // Echo a swap to the operator watching the foreground process (issue #8).
        // The file event log (above) records every cycle; the console gets just
        // swaps, sourced solely from labels (issue #15).
        if let Some(report) = swap_report(&outcome) {
            eprintln!("sessiometer: {report}");
        }
        // The wait this tick requested — an explicit back-off overrides the normal
        // interval (locked-keychain #13, or rate-limit / transient #76); captured
        // before the snapshot is moved.
        let next_wait = outcome.next_wait;
        // The snapshot the control socket answers from until the next poll.
        let snapshot = outcome.snapshot;

        // The accounts the periodic refresh tick (#105) must not touch this idle period —
        // the active account and the imminent swap target — and the quarantined ("needs
        // re-login") accounts it SHOULD attempt for the RESTORE path (issue #106). Both are
        // computed from the POST-tick state HERE, before the idle borrows `&mut daemon`; the
        // tick owns its own roster copy + clock, so the sweep below needs nothing from it.
        let refresh_excluded = daemon.refresh_exclusions();
        let refresh_quarantined = daemon.refresh_quarantined();
        // Accounts the sweep proved still refreshable (issue #106): collected inside the idle
        // loop (where `&mut daemon` is held by `wait`) and un-quarantined AFTER it, when
        // `&mut daemon` is free again — the same post-idle pattern as the manual-swap adoption.
        let mut refresh_restored: Vec<String> = Vec::new();
        // The credential-clock observations the sweep read (issue #119): collected here and
        // folded into the per-account health state AFTER the idle block (the fold needs
        // `&mut daemon`), exactly like the restores above.
        let mut refresh_observations: Vec<RefreshObservation> = Vec::new();

        // The canonical the daemon last COMMITTED to its watch (issue #140), snapshotted HERE —
        // before the idle borrows `&mut daemon` — so the external-login watch arm can tell an
        // out-of-band write it reads DURING the idle from the daemon's own last state, without
        // needing `&mut daemon` mid-idle. The daemon's own writes (a swap) commit the watch, so
        // this baseline already reflects them and they are never mis-seen as external.
        let canonical_baseline = daemon.canonical_baseline();

        // Idle until the next tick is due, serving control requests and watching
        // for shutdown. A swap (if any) already completed inside `tick`, so a
        // shutdown observed here aborts cleanly before the next tick — no half-swap.
        // The wait future borrows `&mut daemon`, so it is scoped to this block and
        // dropped before any post-idle mutation (the manual-swap adoption) runs.
        let idle = {
            let wait = daemon.wait_after_tick(next_wait);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.requested() => break Idle::Shutdown,
                    // A served control connection may carry a signal (#64): a
                    // `manual-swapped` breaks the idle to adopt it; a `status` read
                    // (None) just continues serving until the wait elapses.
                    signal = control.serve(&snapshot) => match signal {
                        Some(ControlSignal::ManualSwapped) => break Idle::ManualSwapped,
                        // A `roster-reload` (#139) breaks the idle to reconcile the
                        // in-memory roster to the freshly-written config; a `status`
                        // read (None) just continues serving until the wait elapses.
                        Some(ControlSignal::RosterReloadRequested) => {
                            break Idle::RosterReloadRequested
                        }
                        None => continue,
                    },
                    // The periodic isolated-refresh tick (issue #105), in the idle path off
                    // the poll→usage→swap seam. `until_due` resolves only when a refresh is
                    // due — and NEVER when the feature is off (the no-op ticker) — so this arm
                    // is inert by default. When it fires, run the sweep under a NESTED select
                    // so ONLY a shutdown can interrupt it: a control read must not cancel an
                    // in-flight refresh (the swap-lock-holding engine is cancel-safe, but a
                    // status query should neither forfeit a token nor be able to starve the
                    // sweep). `wait` is pinned OUTSIDE this loop, so a sweep does not reset the
                    // poll cadence; after it the loop idles on until the wait elapses.
                    () = refresh.until_due() => {
                        tokio::select! {
                            biased;
                            _ = shutdown.requested() => break Idle::Shutdown,
                            sweep = refresh.sweep(&refresh_excluded, &refresh_quarantined) => {
                                // Emit each per-cycle refresh event (issue #106) to the event
                                // log — the SAME best-effort path the tick's events ride; `log`
                                // is not borrowed by `wait`, so it is free to use here. The
                                // RESTORES are deferred: un-quarantining mutates the health
                                // machine (needs `&mut daemon`, held by `wait`), so they are
                                // collected here and applied after the idle block.
                                for event in &sweep.events {
                                    if let Err(err) = log.emit(event) {
                                        eprintln!("sessiometer: event log write failed: {err}");
                                    }
                                }
                                refresh_restored.extend(sweep.restored);
                                // The #119 credential-clock observations, deferred like the
                                // restores: folding them mutates the health machine.
                                refresh_observations.extend(sweep.observations);
                            }
                        }
                    }
                    // The external-login watch (issue #140): a dedicated SHORT-cadence, LOCAL
                    // (no-network) probe of the canonical credential, DECOUPLED from the
                    // usage-poll cadence, so a manual `claude /login` on the active account is
                    // reflected within the watch cadence, not up to a full poll interval. The
                    // probe reads the canonical via the watch's OWN store (the daemon's is
                    // borrowed by `wait`) and compares against the pre-idle committed baseline;
                    // a difference is an out-of-band write since the last tick → break to
                    // re-tick, so `tick`'s `reconcile_canonical_change` does the authoritative
                    // re-stash / re-resolve / surface. Fail-safe: an unreadable / locked /
                    // absent probe (`None`), or a byte-identical read, or no baseline yet,
                    // detects nothing and keeps idling — the loop never stalls. `wait` is pinned
                    // OUTSIDE this loop, so a probe does not reset the poll cadence.
                    () = login_watch.until_due() => {
                        if let Some(current) = login_watch.read_canonical().await {
                            if canonical_baseline
                                .as_ref()
                                .is_some_and(|base| !base.matches(&current))
                            {
                                break Idle::ExternalLoginDetected;
                            }
                        }
                    }
                    _ = &mut wait => break Idle::Elapsed,
                }
            }
        };
        // Apply the RESTORES the sweep reported (issue #106), now that the idle block has
        // dropped its `&mut daemon` borrow: un-quarantine each recovered account and log its
        // edge-triggered `credential_restored`. Applied on every idle exit (shutdown included
        // — the restore genuinely happened, so the log record is honest; the durable effect is
        // the re-stashed fresh token, which persists regardless of the in-memory flip).
        for uuid in &refresh_restored {
            if let Some(event) = daemon.apply_refresh_restore(uuid) {
                if let Err(err) = log.emit(&event) {
                    eprintln!("sessiometer: event log write failed: {err}");
                }
            }
        }
        // Fold the sweep's credential-clock observations into the health state (issue #119),
        // now that `&mut daemon` is free, BEFORE diffing the rollup so a transition reflects
        // this cycle's refresh.
        for observation in &refresh_observations {
            daemon.apply_refresh_observation(observation);
        }
        // Diff every account's 4-state rollup against its last-emitted verdict and log one
        // edge-triggered `credential_health` per CHANGE (issue #119, AC-3). Runs EVERY
        // iteration — not only on a sweep — so a time-driven transition (the access token
        // crossing its expiry) and a quarantine-driven one (the #42 path, even with the
        // refresh feature OFF) are both caught; the first computation per account seeds the
        // baseline silently. Best-effort like every other log emission here.
        for event in daemon.note_health_transitions(wall_clock_now_secs()) {
            if let Err(err) = log.emit(&event) {
                eprintln!("sessiometer: event log write failed: {err}");
            }
        }
        match idle {
            Idle::Shutdown => return Ok(()),
            // Adopt the manual `use` swap (#64) — arm the cooldown so the next tick
            // holds on the operator's choice, and re-resolve active from the
            // canonical — BEFORE looping back to re-tick.
            Idle::ManualSwapped => daemon.adopt_manual_swap().await,
            // Reload + reconcile the in-memory roster to the freshly-written
            // `config.toml` (#139) — the onboarded / relogged-in / removed account is
            // adopted into the live rotation — BEFORE looping back to re-tick.
            Idle::RosterReloadRequested => daemon.adopt_roster_reload().await,
            // The external-login watch (#140) detected an out-of-band canonical change: just
            // re-tick — the next `tick` reads the canonical and its `reconcile_canonical_change`
            // does the authoritative re-stash / re-resolve / surface (no pre-tick adoption
            // needed, unlike a manual swap or a roster reload).
            Idle::ExternalLoginDetected => {}
            Idle::Elapsed => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_state::OauthAccount;
    use crate::config::Tunables;
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
        async fn serve(&self, _snapshot: &StatusSnapshot) -> Option<ControlSignal> {
            std::future::pending().await
        }
    }

    /// The inert [`RefreshTicker`] for the hermetic run-loop tests (issue #105): never due,
    /// sweeps nothing — so the periodic-refresh arm never wins the idle select and these
    /// tests behave exactly as they did before #105. Production wires the real
    /// [`crate::refresh_tick::RefreshTick`] (disabled by default).
    struct NoopRefreshTicker;

    impl RefreshTicker for NoopRefreshTicker {
        async fn until_due(&mut self) {
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
    }

    impl OnceRefreshTicker {
        fn new() -> Self {
            Self {
                fired: Cell::new(false),
                swept: RefCell::new(Vec::new()),
                swept_quarantined: RefCell::new(Vec::new()),
                outcome: RefCell::new(SweepOutcome::default()),
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
        async fn until_due(&mut self) {
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
        async fn until_due(&mut self) {
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
        async fn serve(&self, _snapshot: &StatusSnapshot) -> Option<ControlSignal> {
            if self.fired.replace(true) {
                std::future::pending().await
            } else {
                Some(ControlSignal::ManualSwapped)
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
        async fn serve(&self, _snapshot: &StatusSnapshot) -> Option<ControlSignal> {
            if self.fired.replace(true) {
                std::future::pending().await
            } else {
                Some(ControlSignal::RosterReloadRequested)
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
            // Most daemon tests opt the floor IN (the pre-#10 behavior they were
            // written against); `tunables_floor_off` covers the new default.
            session_floor: Some(floor),
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

    /// Tunables with the session-floor guard OFF — the #10 default. The floor is
    /// the only field that differs from [`tunables`], so the rest is reused.
    fn tunables_floor_off(trigger: u8, cooldown: u64) -> Tunables {
        Tunables {
            session_floor: None,
            ..tunables(trigger, 0, cooldown)
        }
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
        // Warm-up latches within one full cycle — at most one tick per in-rotation
        // account, so never more than the roster size. Bound the loop accordingly (+1
        // slack) so a misuse on a roster that can NEVER warm up — no identifiable
        // active AND nothing enabled, i.e. an empty schedule whose `note_polled` never
        // fires — fails LOUDLY here instead of hanging the test forever.
        let max_ticks = daemon.roster.len() + 1;
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
        // The opt-in session_floor (#10) is a STRICTER reserve layered on the always-on
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
        // with the session floor OFF and ample session headroom — swapping there
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

    // --- rate-limit / transient poll back-off (issue #76) ------------------

    /// A single-account ('u-A', active) daemon with the fixed 60 s poll interval —
    /// the seam the poll back-off tests read `tick().next_wait` off (frozen clock,
    /// no jitter, so the back-off is `60 s × 2^streak`). Returns the tempdir guard so
    /// the caller keeps the displayed `~/.claude.json` alive for the daemon's life.
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
        // AC: sustained 429 WIDENS the effective poll spacing rather than re-polling
        // at the fixed interval. The first backed-off cycle already waits 2× the 60 s
        // interval, and each consecutive 429 doubles it.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;

        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::SkippedActiveUnavailable);
        assert_eq!(first.next_wait, Some(Duration::from_secs(120)));
        // Diagnostic channel (#77): the poll surfaces as the `rate_limited` class —
        // NOT a generic transient — and the per-tick decision carries the back-off in
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
                },
            ],
        );
        assert_eq!(
            daemon.tick().await.next_wait,
            Some(Duration::from_secs(240))
        );
        assert_eq!(
            daemon.tick().await.next_wait,
            Some(Duration::from_secs(480))
        );
    }

    #[tokio::test]
    async fn the_rate_limit_back_off_doubles_then_caps() {
        // The back-off grows exponentially from the interval and saturates at the cap,
        // so sustained throttling settles at one poll per hour rather than growing
        // without bound — mirroring the locked-keychain back-off shape.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(daemon.tick().await.next_wait.unwrap());
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
        // AC: Retry-After is honoured as a MINIMUM. When it exceeds the exponential
        // back-off it wins; when it is smaller, the larger exponential governs but the
        // wait is never below Retry-After.
        // Larger than the 120 s first-cycle exponential → Retry-After (600 s) wins.
        let (_d1, mut bigger) = rate_limit_daemon(
            FakeRosterPoller::new().rate_limited("u-A", Some(Duration::from_secs(600))),
        )
        .await;
        assert_eq!(
            bigger.tick().await.next_wait,
            Some(Duration::from_secs(600))
        );

        // Smaller than the exponential → the 120 s exponential governs (and is ≥ 10 s).
        let (_d2, mut smaller) = rate_limit_daemon(
            FakeRosterPoller::new().rate_limited("u-A", Some(Duration::from_secs(10))),
        )
        .await;
        assert_eq!(
            smaller.tick().await.next_wait,
            Some(Duration::from_secs(120))
        );
    }

    #[tokio::test]
    async fn retry_after_overrides_the_cap_when_larger() {
        // AC: Retry-After is a minimum even past POLL_BACKOFF_CAP — a server asking
        // for 2 h is obeyed though the exponential ceiling is 1 h.
        let two_hours = Duration::from_secs(7200);
        assert!(two_hours > POLL_BACKOFF_CAP);
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", Some(two_hours))).await;
        assert_eq!(daemon.tick().await.next_wait, Some(two_hours));
    }

    #[tokio::test]
    async fn a_clean_cycle_resets_the_rate_limit_back_off() {
        // Once polls succeed again the back-off clears (next_wait None → normal
        // interval) and the streak resets, so a LATER 429 restarts at 2× — not where
        // the prior episode left off.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        assert_eq!(
            daemon.tick().await.next_wait,
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            daemon.tick().await.next_wait,
            Some(Duration::from_secs(240))
        );

        // A clean poll clears the back-off and resets the streak.
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        assert_eq!(daemon.tick().await.next_wait, None);

        // A later 429 restarts the climb at the base multiplier, not at 480.
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        assert_eq!(
            daemon.tick().await.next_wait,
            Some(Duration::from_secs(120))
        );
    }

    #[tokio::test]
    async fn only_throttling_outcomes_trigger_the_back_off() {
        // Back-off is scoped to 429 (rate-limit) and 5xx / network (transient). A 403
        // (scope) and a 401 (unauthorized) authenticate-or-reject the token but are not
        // endpoint throttling — neither backs off; a transient does.
        let (_d1, mut scope) =
            rate_limit_daemon(FakeRosterPoller::new().scope_missing("u-A")).await;
        assert_eq!(scope.tick().await.next_wait, None);

        let (_d2, mut unauth) =
            rate_limit_daemon(FakeRosterPoller::new().unauthorized("u-A")).await;
        assert_eq!(unauth.tick().await.next_wait, None);

        let (_d3, mut transient) = rate_limit_daemon(FakeRosterPoller::new().failing("u-A")).await;
        assert_eq!(
            transient.tick().await.next_wait,
            Some(Duration::from_secs(120))
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
    /// canonical holding `work`'s — for the endpoint-global back-off tests (issue
    /// #76), where a poll outcome on a NON-active account must steer the whole loop.
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
    async fn a_non_active_rate_limit_backs_off_the_whole_loop() {
        // AC: rate-limiting is endpoint-global — there is ONE usage endpoint for the
        // whole roster, so a `429` on ANY polled account widens the next poll's
        // spacing for the entire loop. Here the active `work` polls clean and holds
        // (under its trigger), while the non-active `spare` is throttled: the loop
        // still backs off (2× the 60 s interval). Were the back-off scoped only to an
        // unavailable ACTIVE account, this cycle's `next_wait` would be `None`.
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .rate_limited("u-B", None),
        )
        .await;

        // Tick 1 polls the ACTIVE A (clean) — no back-off from this account.
        let first = daemon.tick().await;
        assert_eq!(first.next_wait, None);
        // Tick 2 polls the NON-active B (the next round-robin entry), which is
        // rate-limited — and the WHOLE loop backs off (2× the 60 s interval), even
        // though B is not the active account: rate-limiting is endpoint-global.
        let second = daemon.tick().await;
        assert_eq!(second.next_wait, Some(Duration::from_secs(120)));
    }

    #[tokio::test]
    async fn a_throttled_non_active_accounts_retry_after_governs_the_global_back_off() {
        // The staggered loop (issue #80) polls ONE account per tick, so the former
        // same-cycle fold across accounts no longer applies — but the back-off is still
        // endpoint-global and still honours the throttled account's `Retry-After`,
        // whichever account in the rotation hits it. Here the active A polls clean (no
        // back-off) and the non-active B carries a `Retry-After` of 300 s on its
        // round-robin tick; 300 s > the 120 s first-cycle exponential, so B's hint
        // governs the WHOLE loop's wait even though B is not the active account.
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .rate_limited("u-B", Some(Duration::from_secs(300))),
        )
        .await;
        // Tick 1: active A, clean — no back-off.
        assert_eq!(daemon.tick().await.next_wait, None);
        // Tick 2: non-active B, throttled with Retry-After 300 → the loop waits 300 s.
        assert_eq!(
            daemon.tick().await.next_wait,
            Some(Duration::from_secs(300))
        );
    }

    // --- #80 staggered round-robin poll scheduling -------------------------
    //
    // The cycle no longer bursts every account in one tick. Each tick polls ONE
    // account from a round-robin schedule — the active first (its swap-away trigger
    // is the most time-sensitive, so it is polled every cycle), then the enabled,
    // non-quarantined non-actives — carrying the rest at their last-known reading.
    // The swap-away decision HOLDS until a warm-up cycle has polled everyone once,
    // and the per-poll wait is the full interval spread across the rotation. Every
    // seam is hermetic — no real clock or network (AC #4).

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
        // identifiable: exactly one new reading lands per tick, in round-robin order
        // (active `work` first, then `spare`, then `backup`).
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
            vec![Some(0.11), Some(0.22), Some(0.33)],
            "tick 3 completes the cycle — every account now carried at its last reading"
        );
    }

    #[tokio::test]
    async fn the_poll_schedule_leads_with_the_active_then_round_robins_and_wraps() {
        // AC #1/#2: the schedule is the active account FIRST (polled every cycle),
        // then the enabled non-quarantined non-actives in roster order; the cursor
        // advances one entry per tick and wraps at the cycle boundary.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;

        // With `spare` (index 1) active, the schedule leads with it, then the others.
        assert_eq!(daemon.build_poll_schedule(Some(1)), vec![1, 0, 2]);

        // Driving the cursor a full cycle plus one yields the wrap back to the lead.
        let polled: Vec<usize> = (0..4)
            .map(|_| daemon.next_poll_index(Some(1)).unwrap())
            .collect();
        assert_eq!(
            polled,
            vec![1, 0, 2, 1],
            "active-first, then round-robin, then wrap to the lead"
        );
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
        // warm-up cycle and fires only on the full last-known set. Active `work` is
        // over its trigger from tick 1, yet the swap waits for the third (final) tick.
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
            TickAction::Swapped { from: 0, to: 1 },
            "tick 3: warm-up complete → the swap fires on the full last-known set"
        );
        assert!(daemon.state.warmed_up);
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
        // The floor-driven no-viable-target path emits one all_exhausted event.
        // No reading carried a weekly reset here, so #11 falls back to the active
        // handle with `resets_at` omitted (the soonest-reset path is covered by the
        // all-weekly-exhausted test below).
        assert_eq!(
            outcome.events,
            vec![Event::AllExhausted {
                hold: "work".to_owned(),
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
        // Floor OFF (the #10 default); weekly_trigger 98 via the tunables helper, so
        // the swap-away fires on the weekly dimension and every target is excluded.
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
            // A viable candidate rides the wire as a label (#88).
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
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
    fn credential_health_rolls_up_the_four_states_by_severity() {
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

        // Dead — quarantined (the #42 401-streak verdict)…
        assert_eq!(
            credential_health(true, None, 0, None, false, NOW),
            CredentialHealth::Dead
        );
        // …or the refresh token was cleared in place (a refresh-detected death), surfaced
        // as 🔴 too rather than hidden — this is a DISPLAY rollup, it never quarantines.
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

        // Severity ladder (Dead > AtRisk > Stale > Healthy): a quarantined account whose
        // token is ALSO expired and whose refresh is ALSO failing still reads Dead; an
        // at-risk account whose token is ALSO expired reads AtRisk, not Stale. A fresh
        // reading NEVER masks a negative signal — even `has_fresh_reading = true` stays
        // Dead / AtRisk here.
        assert_eq!(
            credential_health(true, Some(Error), 3, Some(NOW - 10), true, NOW),
            CredentialHealth::Dead
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

        // A negative signal always overrides missing evidence — quarantine ⇒ Dead, not
        // Unknown, even with no other input.
        assert_eq!(
            credential_health(true, None, 0, None, false, NOW),
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
        // `RefreshDelta` lives in `refresh_tick` and is not imported at module scope (only
        // the daemon's fold consumes it); name it in full here.
        use crate::refresh_tick::RefreshDelta;
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
        // state — and only for the account that changed.
        daemon.state.health[0].quarantined = true; // → Dead
        assert_eq!(
            daemon.note_health_transitions(NOW),
            vec![Event::CredentialHealth {
                account: "work".to_owned(),
                state: CredentialHealth::Dead,
            }]
        );

        // No change ⇒ no event (edge-triggered, not level-triggered).
        assert!(daemon.note_health_transitions(NOW).is_empty());

        // Un-quarantine WITHOUT any new evidence ⇒ back to Unknown, NOT a false Healthy
        // (#137): clearing the dead flag does not prove the credential is alive.
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
        let signal = serve_control(server, &snapshot, false).await.unwrap();
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
            .unwrap();
        assert!(signal.is_none(), "an unknown command produces no signal");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("unknown command"), "got {reply:?}");
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
            .unwrap();
        assert!(signal.is_none(), "an oversized request produces no signal");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(
            reply.contains("malformed"),
            "an over-long request is bounded and rejected: {reply:?}"
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

    // --- next_swap candidate (issue #88) + swap report ---------------------

    #[test]
    fn status_response_projects_the_next_swap_candidate_and_drops_last_swap() {
        // A viable candidate projects as a label (#88), never a token/email (#15).
        let target = StatusSnapshot {
            refresh_enabled: false,
            accounts: vec![],
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
            }),
        };
        let json = serde_json::to_string(&status_response(&target)).unwrap();
        assert!(json.contains("\"next_swap\":"), "got {json}");
        assert!(json.contains("\"state\":\"target\""), "got {json}");
        assert!(json.contains("\"to\":\"spare\""), "got {json}");
        // #15: a label only — never an email or token sigil.
        assert!(!json.contains('@'));
        assert!(!json.to_lowercase().contains("token"));

        // The two no-candidate verdicts project as bare reasons (no label at all), so
        // the client can tell `no viable target` from `awaiting usage data`.
        let no_target = StatusSnapshot {
            refresh_enabled: false,
            accounts: vec![],
            next_swap: Some(NextSwap::NoViableTarget),
        };
        assert!(serde_json::to_string(&status_response(&no_target))
            .unwrap()
            .contains("\"next_swap\":{\"state\":\"no_viable_target\"}"));
        let awaiting = StatusSnapshot {
            refresh_enabled: false,
            accounts: vec![],
            next_swap: Some(NextSwap::AwaitingData),
        };
        assert!(serde_json::to_string(&status_response(&awaiting))
            .unwrap()
            .contains("\"next_swap\":{\"state\":\"awaiting_data\"}"));

        // No anchor → null candidate; and the dropped `last_swap` field never appears.
        let none = StatusSnapshot {
            refresh_enabled: false,
            accounts: vec![],
            next_swap: None,
        };
        let json = serde_json::to_string(&status_response(&none)).unwrap();
        assert!(json.contains("\"next_swap\":null"), "got {json}");
        assert!(!json.contains("last_swap"), "got {json}");
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
        let tun = tunables(95, 80, 0); // session floor 0.80
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
        // harness (work=0, spare=1, backup=2; session_floor 0.80, weekly_trigger_base
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

        // Viable target → the choice mapped to a label. spare and backup are both under
        // the floor and weekly-viable; with no known reset the tie falls to the earliest
        // roster index (spare), mirroring `pick_target`.
        assert_eq!(
            daemon.next_swap(
                Some(0),
                &[usage(0.97, 0.40), usage(0.10, 0.10), usage(0.20, 0.10)]
            ),
            Some(NextSwap::Target {
                to: "spare".to_owned()
            }),
        );

        // Readings in hand but none viable (both over the 0.80 session floor) → a
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
        // not per poll. The staggered loop (#80) polls ONE account per tick in
        // round-robin (active A first, then B, then C), so a full sweep of the
        // 3-account roster takes three ticks; running FOUR ticks polls A twice
        // (ticks 1 and 4) — proving the per-account 401 streak climbs 1 → 2 across
        // its own re-polls — with B's (silent) lock on tick 2 and C's 403 on tick 3
        // in between, demonstrating `note_poll_outcome` is wired into the loop and
        // serialized.
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
        // staggered ticks (A, B, C, A).
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

        // Across the four staggered ticks (#80), A 401s twice (ticks 1, 4) and C 403s
        // once (tick 3) → three event lines, each stamped, none carrying secret
        // material (handles only — never a token or email). The locked account B
        // contributes nothing per-account (#13).
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
                    refresh_token_rotated: false,
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
            RefreshOutcome::Error, // unused: `hard_error` short-circuits before the report
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
            Error::ConfigFloorAboveTrigger {
                floor: 95,
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
            // One poll per staggered tick (#80): work (401), then the silent
            // per-account lock on spare, then backup (403). Harvest the whole rotation
            // so both the monitor_401 and usage_scope_fail channels are exercised. The
            // active account's reading is unavailable every tick → SkippedActiveUnavailable.
            for _ in 0..3 {
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

        // Scenario 4 — the dead-credential lifecycle (#42): a single 401 on the
        // active account (threshold 1) declares it DEAD and triggers an emergency
        // swap in one tick, so `credential_dead`, `emergency_swap`, AND the durable
        // `quarantined` status (snapshot + wire + text) are all harvested at once.
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
                session_floor: Some(70),
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
        assert!(
            corpus.contains(r#""quarantined":true"#),
            "UDS channel: quarantine status missing"
        );
        assert!(
            // The status-TEXT rendering of a dead credential (#119): the 🔴 rollup glyph
            // plus the actionable `claude /login` cue (AC-1) now stand in for the pre-rollup
            // `needs re-login` tag. Unique to the text channel — the wire carries the verdict
            // as the `"auth":"dead"` enum (issue #143 renamed the key `health` → `auth`), not
            // this operator-facing command — so it proves the status-text channel contributed
            // (a non-vacuous #15 gate).
            corpus.contains("🔴 claude /login"),
            "status-text channel: dead-credential cue missing"
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
