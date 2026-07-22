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
//!    carried per-account readings; a failed poll clears it. A `429` / `5xx` backs off
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
//! swap-target `target_max_session_usage` reserve (#398) is a default-on ceiling on top: a
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
use crate::landing;
use crate::observability::{
    BackoffClass, BlindVelocity, CanonicalLiveness, CaptureEventOutcome, CredentialHealth,
    DecisionClass, Diagnostic, DiagnosticLog, Event, EventLog, KeepWarmTrigger, PollClass,
    RefreshEventOutcome, SwapProjection, SwapReason,
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
    credential_health, refresh_health_view, to_pct, to_pct_exact, versioned_status_response,
    AccountReading, AccountStatusLine, BlindActive, BlindPreemptSwap, CanonicalScrub,
    LandingOvershoot, NextSwap, NextSwapReason, NoTargetCause, SchemaVersion, StatusResponse,
    StatusSnapshot, VersionedStatus, STATUS_SCHEMA_VERSION,
};
// `status_response` (the payload projection) and `RefreshHealth` are named only by the in-module
// tests — production reaches the wire through `versioned_status_response` (issue #164) and builds
// the health view through `refresh_health_view` without naming the type. Re-export test-scoped so
// `use super::*` resolves them while a non-test build sees no unused re-export.
#[cfg(test)]
pub(crate) use snapshot::{status_response, RefreshHealth};

mod socket;

pub(crate) use socket::{
    notify_restored, notify_roster_reload, request_swap, write_capture_ack, write_config_set_ack,
    write_swap_ack, CaptureAck, CaptureCommand, CaptureRejection, ConfigSetAck, ConfigSetCommand,
    ConfigSetEffect, ConfigSetRejection, Control, ControlSignal, ControlYield, SwapAck,
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
    config_get_reply, control_reply, encode_heartbeat_frame, encode_snapshot_frame,
    parse_watch_frame, serve_control, serve_stats, serve_watch, ServeOutcome, StatsRequest,
    WatchFrame, MAX_CONTROL_LINE_BYTES,
};

mod run_loop;

pub(crate) use run_loop::run_loop;
// `swap_report` / `unrecoverable_report` are exercised only by the in-module run-loop tests
// (production calls them inside `run_loop`); re-export test-scoped so `use super::*` resolves them
// while a non-test build sees no unused re-export.
#[cfg(test)]
pub(crate) use run_loop::{swap_report, unrecoverable_report};

// The two pure, `&self`-free function families split out by the God-module decomposition (issue
// #637 step 1, issue #656): swap-TARGET selection and poll/failure CLASSIFICATION. Both are
// re-exported here, so every call site — in this module, in the sibling submodules that reach them
// through `use super::*`, and in every intra-doc link — resolves unmodified.
mod selection;

pub(crate) use selection::{
    pick_target, pick_target_ranked, pick_target_with_reason_ranked, SelectionTiebreak,
};
// `pick_target_with_reason` (the un-jittered, velocity-blind projection) is named only by the
// selection unit tests, which moved alongside it; production reaches selection through
// `pick_target` / the `_ranked` pair. This module's doc links reference it by its
// `selection::pick_target_with_reason` path rather than re-exporting it, so a non-test build sees
// no unused re-export.

mod classify;

pub(crate) use classify::{
    backoff_signal, classify_capture_failure, classify_config_set_failure, classify_poll,
    classify_swap_failure, diag_poll_class, usage_velocity, PollOutcome,
};

// The seam traits + their production impls, split out of this file by the God-module decomposition
// (issue #637 step 2, issue #657): the shutdown / poll / external-login / poll-refresh / keep-warm
// trait seams the generic `Daemon<P, C, S, K>` bounds resolve against, plus the concrete `flock`
// `InstanceLock`, all reached by the production wiring (cli / use_account / service). Re-exported
// here, so every call site — in this module, in the sibling submodules that reach them through
// `use super::*`, and in every intra-doc link — resolves unmodified.
mod seams;

pub(crate) use seams::{
    ExternalLoginWatch, ExternalLoginWatcher, InstanceLock, KeepWarm, PollRefresh,
    RealKeepWarmEngine, RealRosterPoller, RealShutdown, RosterPoller, Shutdown,
};
// `KeepWarmMint` (the `KeepWarm` trait's boxed-future return alias) is named outside `seams` only by
// the in-module `SeamKeepWarm` keep-warm fake; re-export test-scoped so `mod tests`'s `use super::*`
// resolves it while a non-test build sees no unused re-export.
#[cfg(test)]
pub(crate) use seams::KeepWarmMint;

// The plain value/record types the decision core carries between ticks — the tick verdicts, the
// retained anchors/records, the tick-local back-off, the socket-swap verdict — plus the pure
// `blind_active_view` projection over them, split out of this file by the God-module decomposition
// (issue #637 step 3, issue #658). Re-exported here, so every call site — in this module, in the
// sibling submodules that reach them through `use super::*`, and in every intra-doc link — resolves
// unmodified. The state machine that MUTATES them stays here, as do `AnchorArmInputs` (the
// `blind_active_view` argument bundle), `LandingOvershootRecord`, and BOTH `recent_*_view`
// projections — including `recent_blind_preempt_swap_view`, now separated from the
// `BlindPreemptSwapRecord` it projects.
mod records;

// `TickAction` / `TickOutcome` were ALREADY `pub(crate)` before the split — `observability`
// intra-doc-links `crate::daemon::TickAction` and `run_loop` consumes both — so they keep that
// visibility exactly.
pub(crate) use records::{TickAction, TickOutcome};
// Everything else was daemon-PRIVATE, so it stays `pub(super)` in `records` and is re-exported here
// PRIVATELY — restoring the pre-move reach exactly rather than widening it crate-wide (the scheme is
// spelled out in `records`' module doc). `blind_active_view` additionally CANNOT be `pub(crate)`: it
// takes the daemon-private `AnchorArmInputs`, which would be a more-private type in a public
// interface.
use records::{
    blind_active_view, BlindAnchor, BlindPreemptSwapRecord, LastGood, LastSwap, ParkedLanding,
    SwapVerdict, TickBackoff, VelocityEma,
};

// The five responsibility clusters carved out of the single `impl Daemon<P, C, S, K>` block by the
// God-module decomposition (issue #637 step 4, issue #659). Each is an `impl super::Daemon<P, C, S,
// K>` repeating the SAME 4-param `where` bound verbatim, reaching this module's items through
// `use super::*` exactly as `run_loop` does — so the methods are a pure relocation, still inherent
// on `Daemon` and still called unqualified as `self.method(..)` from the decision core that stays
// here. Each carries its OWN `#[cfg(test)] mod tests` holding that cluster's tests, which reach the
// shared fakes/builders through `use crate::daemon::tests::*`. A method that was daemon-PRIVATE
// becomes `pub(super)` on the move — the minimum that restores its pre-move reach now that it sits
// one module down, the same scheme `records` used in step 3. Nothing is re-exported: these modules
// add no nameable items, only inherent methods.
mod canonical;
mod commands;
mod keep_warm;
mod refresh_fold;
mod snapshot_build;

/// Per-cycle clamp bounds for the swap-away trigger draw, in PERCENT — mirrors
/// config's `session_ceiling` range so a jittered draw can never escape it.
const SESSION_CEILING_PCT_LO: f64 = 50.0;
const SESSION_CEILING_PCT_HI: f64 = 99.0;
/// Per-cycle clamp bounds for the WEEKLY swap-away trigger draw, in PERCENT
/// (issue #41) — mirrors config's `weekly_ceiling` range. Its own constants
/// (numerically equal to the session bounds today) so the two triggers stay
/// independently bounded.
const WEEKLY_CEILING_PCT_LO: f64 = 50.0;
const WEEKLY_CEILING_PCT_HI: f64 = 99.0;
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
/// Ceiling on the rate-limit / transient poll back-off (issue #76) for a NON-active
/// (peer) account. Under sustained `429` / `5xx` the effective poll spacing grows
/// exponentially but settles here — one poll per hour, gentle on a throttling endpoint
/// without going fully dark. A server-advised `Retry-After` is honoured as a MINIMUM but
/// is itself clamped to this ceiling (issue #294), so this is the absolute maximum a
/// PEER's poll-backoff window can reach. The ACTIVE account is bounded MORE tightly by
/// [`ACTIVE_POLL_BACKOFF_CAP`] and honours `Retry-After` as an un-clamped floor instead
/// (issue #453).
const POLL_BACKOFF_CAP: Duration = Duration::from_secs(3600);
/// Ceiling on the ACTIVE account's OWN rate-limit / transient poll back-off (issue #453),
/// a compile-time constant fixed at 120 s. Much tighter than the peer
/// [`POLL_BACKOFF_CAP`]: a `429` on the active account blinds the very account being
/// consumed, so its self-backoff (the exponential arm, when the server gives NO
/// `Retry-After`) is clamped here to recover observability fast, rather than letting the
/// exponential climb toward an hour. Applies ONLY to the exponential arm — a server
/// `Retry-After` is an ABSOLUTE floor for the active account (never clamped by this or by
/// [`POLL_BACKOFF_CAP`]), so the daemon never re-polls before the server said it may
/// (issue #453 AC). Peers are unaffected — they keep the [`POLL_BACKOFF_CAP`] ceiling and
/// the #294 pathological-`Retry-After` clamp.
const ACTIVE_POLL_BACKOFF_CAP: Duration = Duration::from_secs(120);
/// Interim `T` for the #452 bounded-blindness preemptive-swap gate (ADR-0017): the active
/// account's retained pre-blind anchor must be stale beyond this before the gate turns eligible.
/// The always-on measurement band for the gate-premise SLI (issue #482, the
/// [`Event::BlindGateEligible`] no-viable-target falsifier) and the [`blind_active_view`] status
/// projection (#479). #452's SWAP action keys off the `session_blind_swap_secs` CONFIG tunable
/// instead (default-equal to this, so the two align out of the box); this const stays the SLI /
/// status band DELIBERATELY, so the config kill-switch disables the swap without blinding #484's
/// ratification SLIs. The interim value (300 s) is the one ADR-0017 names; measuring the SLI at
/// exactly it is what lets #484 confirm-or-refute the constant against production.
///
/// INTERIM / REVERSIBLE — stays so until the production ratification bar (issue #484, documented on
/// [`BLIND_GATE_RISK_BAND`] and shared by both constants) is met; promotion reads PRODUCTION SLIs
/// (#482 / #449 / #455), NOT a re-run of the #451 replay.
///
/// `pub(crate)` so the OFFLINE blind-arm projection-error SLI ([`crate::reliability`], issue #636)
/// scopes its scored population to the SAME first gate this arm applies — one `T`, referenced from
/// both, so the runtime arm and the offline readout that grades it cannot drift apart (the shared-
/// constant discipline [`crate::reliability`]'s `SLO_SWAP_P100_MAX` already uses with
/// [`crate::landing`]).
pub(crate) const BLIND_GATE_SECS: u64 = 300;
/// Interim `risk_band` for the #452 gate (ADR-0017), as a session-usage fraction: the retained
/// pre-blind anchor ([`crate::daemon`]'s `last_good`, #450) — plausibility-corrected to its window
/// high-water mark (#619) — must be at/over this for the gate to turn eligible. DISTINCT from — and
/// deliberately LOWER than — the reactive `session_ceiling` ([`Daemon::session_ceiling_base`], the
/// #449 `near_limit` band): the gate acts PREEMPTIVELY, on a stale anchor, before the account would
/// have tripped the reactive band. The always-on SLI (#482) / status (#479) measurement band;
/// #452's swap keys off the `session_blind_risk_band` CONFIG tunable (default-equal), so the action
/// threshold and this measurement band align out of the box, but a config kill-switch disables only
/// the action — the SLI keeps measuring.
///
/// INTERIM / REVERSIBLE, biased CONSERVATIVE (issue #484). ADR-0017 named the interim at 65 %; #484
/// biases it to the low end of a conservative **[60, 65] band** — the interim evolution the ADR
/// anticipated ("to be finalised"), not a rewrite. Why low: the #451 replay is FLAT (0 walls /
/// disasters / false-preempts) all the way from 68 % down to 50 %, so the data bounds only the
/// CEILING (≤ 68 % — above it the gate can't fire before S1's own pre-blind anchor) and leaves the
/// floor free. With the floor undetermined the swap-timing asymmetry decides it: firing LATE misses
/// the swap outright (unrecoverable), firing EARLY only spends a swap onto a still-recoverable
/// target — so more margin (a lower band) is the cheap-error direction. 60 % is that low end;
/// production evidence is required to move it UP.
///
/// PROMOTION BAR (interim → locked), shared with [`BLIND_GATE_SECS`], read off PRODUCTION SLIs — NOT
/// a re-run of the #451 replay (which only reproduces its own assumptions: linear velocity,
/// `t_fire = T` ignoring the ±1-poll-tick granularity, n=3 from one fleet). Holds interim until ALL:
/// (1) **≥ 5** distinct gated-eligible blind episodes (#482 [`Event::BlindGateEligible`],
/// `viable_target=true`) — 5 itself a conservative interim, above the replay's discredited n=3;
/// (2) **zero session walls** across them (no `reason=session` swap-out or blind recovery at/over
/// ~99 %, via #455's swap-out overshoot P100 SLO + #449's `blind_window` `session_at_recovery`);
/// (3) a **majority** classified `swap_necessary=true` (#482's post-recovery swap-necessity —
/// `session_at_recovery` climbed meaningfully above the stale anchor over its `anchor_age`). A
/// promotion commit cites those readings; absent them the band holds at 60 %.
const BLIND_GATE_RISK_BAND: f64 = 0.60;

/// Minimum velocity samples the #539 EMA must have blended before its rate is trusted for a
/// projective swap (ADR-0017). At `1` the EMA is a SINGLE freshly-seeded interval — an isolated
/// spike would project and fire on it, defeating the "damp single-interval spikes" purpose of the
/// smoothing; requiring `>= 2` means ≥ 2 intervals have blended, so a spike that did not persist has
/// already decayed back below the crossing (the "no-fire without SUSTAINED velocity" invariant).
/// A small compile-time interim (like [`BLIND_GATE_SECS`]): the operator-facing tunables are the
/// horizon / guard / α; this floor is an internal soundness bound on the smoothing, not a policy dial.
const MIN_VELOCITY_SAMPLES: u32 = 2;

/// Worst-case rate-inflation factor for the #584 BLIND velocity-projection report arm
/// ([`blind_velocity_projected_armed`]) — the multiplicative bias-HIGH bound applied to the retained
/// #539 EMA rate before the stale anchor is projected forward over the blind window. NOT a computed
/// confidence interval: the EMA holds only [`MIN_VELOCITY_SAMPLES`]-few blended intervals, so a `k·σ`
/// bound would be theatre (the seed/blend damps any early variance to noise) — this is an EMPIRICAL
/// worst-case rate multiplier instead.
///
/// Basis (recorded per issue #584 acceptance criteria): the 2026-07-17 incident band measured
/// 2.7–4.14 %/min on one climbing account; the worst-case-over-smoothed ratio is `4.14 / 2.7 ≈ 1.53`,
/// rounded UP to 1.75 to absorb few-sample EMA lag and the extra anchor staleness of a blind window that
/// runs 5–8× past the #538-validated `H ≈ 150 s` observed-arm envelope. Multiplicative, NOT additive:
/// the margin `(k − 1) × rate × blind_secs` then GROWS with the blind window — the uncertainty widens
/// exactly as the projection extends further beyond the validated envelope — whereas a fixed additive
/// margin would just re-lower the anchor band, the level-vs-level move issue #584 shows cannot work.
///
/// Biased HIGH by the swap-timing asymmetry (issue #484): a too-HIGH factor only over-reports DEGRADED
/// (a scary status line — this arm is REPORT-ONLY, it fires no swap), while a too-LOW one under-reports
/// and lets the burn stay invisible — the exact #584 failure.
///
/// INTERIM / REVERSIBLE, ratification-pending (issue #584 acceptance criteria / issue #583): promotion
/// reads the #583-uncensored blind-episode distribution — which now records the never-recovering and
/// swapped-away tails this single incident band cannot — NOT this one incident. A compile-time internal
/// soundness bound, like [`MIN_VELOCITY_SAMPLES`]; the operator-facing velocity dials stay the #539
/// horizon / guard / α.
const BLIND_VELOCITY_RATE_INFLATION: f64 = 1.75;

/// Trailing window over which the #582 circuit-breaker counts server-`Retry-After`-driven
/// preemptive swaps to detect a THROTTLE WALK — the roster-wide failure mode where the throttle
/// follows the ACTIVE ROLE rather than any one account, so each swap merely hands the `429` to the
/// next account (observed 2026-07-17: three distinct accounts each took an `RA:3600` within
/// minutes of taking the slot — at +12m, +4m22s and +2m43s — while their peers never got one).
///
/// Scaled at the pathological directive that motivates the path (`Retry-After: 3600`): the window
/// must span the throttle it is detecting, or the walk resets its own counter between hops and the
/// breaker never trips. The observed hop spacing (≤ 12m) sits well inside it.
///
/// A compile-time internal soundness bound, NOT an operator dial (the same standing as
/// [`BLIND_GATE_SECS`] / [`MIN_VELOCITY_SAMPLES`]): the kill-switch for the whole path remains the
/// existing `session_blind_swap_secs` tunable, so this adds no config surface (issue #582 AC).
const RETRY_AFTER_WALK_WINDOW: Duration = Duration::from_secs(3600);

/// How many server-`Retry-After`-driven preemptive swaps inside [`RETRY_AFTER_WALK_WINDOW`] trip
/// the #582 circuit-breaker: at/over this count the path HOLDS POSITION and alarms rather than
/// keep rotating, because holding a blind-but-low account beats walking a 3600 s throttle onto the
/// last good one.
///
/// `2` is the smallest count that can distinguish a WALK from a single unlucky throttle: one
/// server-throttled account is a local event (swap away from it), whereas a SECOND one — on the
/// account we just swapped TO — is the roster-wide pattern itself, and evidence the next hop would
/// only move the throttle again. So the breaker permits at most two hops per window, then stops.
const RETRY_AFTER_WALK_MAX: usize = 2;

/// The ADR-0017 preemptive-gate ARMING predicate on the retained pre-blind anchor: blind past the
/// interim [`BLIND_GATE_SECS`] (strict `>`) AND the anchor at/over the interim [`BLIND_GATE_RISK_BAND`]
/// (`>=`). The SINGLE source of truth for "the gate is armed", so [`blind_active_view`]'s
/// `auto_protection_degraded` projection (#479) and [`Daemon::note_blind_gate_eligibility`]'s
/// not-eligible guard (#482) cannot drift apart — both express THIS one predicate (the latter as its
/// negation) rather than re-encoding the two comparisons independently.
fn blind_gate_armed(blind_secs: u64, anchor_session: f64) -> bool {
    blind_secs > BLIND_GATE_SECS && anchor_session >= BLIND_GATE_RISK_BAND
}

/// The #584 BLIND velocity-projection report arm: does the active account's retained pre-blind anchor,
/// carried forward over the blind window at its (inflation-bounded) #539 EMA rate, PLAUSIBLY reach the
/// session trigger before the daemon sees it again? The THIRD `auto_protection_degraded` arm (issue
/// #584), and the first that mirrors NO decision arm — [`blind_gate_armed`] (anchor → the #452 swap) and
/// [`server_retry_after_holding`] (server directive → the #582 swap-away) each front a swap the daemon
/// actually fires, whereas this arm is PURE HONESTY: it reports a blind account that could burn to
/// exhaustion UNSEEN even though no swap path acts on it — the frozen anchor sits BELOW the swap band
/// (so [`blind_gate_armed`] cannot see the climb) and no directive holds it (so the #582 arm is silent
/// too). This is the exact 2026-07-17 episode issue #584 was filed about (anchor 0.29 climbing
/// 2.7–4.14 %/min, blind past the interim T, burning while `status` said "auto-protection OK").
///
/// REPORT-ONLY: it does NOT arm a swap. A too-high report only prints a scary status line, but ACTING on
/// a deliberately-inflated projection off a stale anchor could thrash a good target — a higher-confidence
/// decision than an honest status line needs (ADR-0017, issue #584). As a disjunct it can only move
/// `status` OK→DEGRADED, never the reverse, so it strictly strengthens the one-sided
/// never-"OK"-while-unprotected invariant.
///
/// Fires only when ALL hold, so a single spike never trips it:
/// - blind past the interim [`BLIND_GATE_SECS`] (strict `>`, the SAME floor the anchor + server arms
///   share — below it the blindness is presumed calm / self-resolving), AND
/// - a retained [`VelocityEma`] exists and is SUSTAINED (`>= MIN_VELOCITY_SAMPLES` blended intervals — a
///   single-interval spike has already decayed, the #539 invariant), AND
/// - `anchor + rate × BLIND_VELOCITY_RATE_INFLATION × blind_secs >= session_ceiling` — the anchor,
///   projected forward over the blind window at the bias-HIGH bound, plausibly reaches the trigger.
///
/// `session_ceiling` is the BASE (un-jittered) draw — the projection/preview trigger the SLI's
/// `pick_target` and `next_swap` already use — NOT the per-cycle jittered draw [`Daemon::velocity_swap`]
/// projects on: that jitter is a per-tick anti-herd artefact, meaningless over a multi-minute blind
/// horizon.
fn blind_velocity_projected_armed(
    blind_secs: u64,
    anchor_session: f64,
    velocity: Option<VelocityEma>,
    session_ceiling: f64,
) -> bool {
    if blind_secs <= BLIND_GATE_SECS {
        return false;
    }
    let Some(vel) = velocity else {
        return false;
    };
    if vel.samples < MIN_VELOCITY_SAMPLES {
        return false;
    }
    // `rate` is non-negative and finite BY CONSTRUCTION ([`Daemon::note_session_velocity`] resets the
    // slot to `None` on a zero interval or a session DROP and blends only non-negative instants), so this
    // pins that load-bearing dependency against a future refactor of the velocity fold: a NaN rate would
    // silently compare `< trigger` and read as a false OK — the very #584 failure mode this arm closes.
    debug_assert!(
        vel.rate.is_finite() && vel.rate >= 0.0,
        "velocity EMA rate must be finite and non-negative",
    );
    let projected = anchor_session + vel.rate * BLIND_VELOCITY_RATE_INFLATION * blind_secs as f64;
    projected >= session_ceiling
}

/// The RAW server-advised `Retry-After` (pre-cap, issue #295) STILL holding `health`'s account off
/// its usage poll at `at` (issue #582), or `None` when the account's back-off window is the daemon's
/// own self-capped exponential, has lapsed, or was never armed. The SINGLE source of truth for "the
/// server is still enforcing a directive on this account", so [`blind_swap`](Daemon::blind_swap)'s
/// server-directed arm (which decides the swap) and [`blind_active_view`]'s
/// `auto_protection_degraded` projection (which reports it to `status`) cannot drift apart — exactly
/// as [`blind_gate_armed`] is the shared predicate for the anchor arm.
///
/// Requires the window to still be OPEN (`poll_backoff_until > at`), not merely that a directive was
/// once seen: a lapsed window means the daemon may re-poll the account NOW, so it may be about to
/// recover on its own and a swap-away would be premature — only a directive the server is STILL
/// enforcing is evidence the blindness will persist. Cheap conservatism, not a coverage gap: the
/// pathological case this path exists for (`Retry-After: 3600`) holds its window open across the
/// whole episode, and a short directive that lapses simply re-polls and either recovers (clearing
/// the retained value) or re-arms. A pure read of `health`.
fn server_retry_after_holding(health: &AccountHealth, at: Instant) -> Option<Duration> {
    let retry_after = health.poll_backoff_retry_after?;
    let until = health.poll_backoff_until?;
    (until > at).then_some(retry_after)
}

/// How long (seconds) `status` NARRATES a #452 bounded-blindness preemptive swap (issue #479) after
/// it fires — the bounded window the retained [`BlindPreemptSwap`] notice is projected onto the wire
/// (`recent_blind_preempt_swap`). The notice is a transient, high-signal "the daemon just swapped you
/// off a blind account on a stale reading — undo with `use <from>` if it recovered" prompt, so it is
/// deliberately short: long enough for an operator glancing at `status` shortly after a swap to catch
/// it, short enough not to nag as stale chrome once the moment has passed (and any superseding swap
/// clears it sooner, via the target-still-active projection gate). Scaled at the gate window
/// [`BLIND_GATE_SECS`] — the same order of time the blindness episode that triggered the swap unfolds
/// over.
///
/// INTERIM — the display window is a UX dial, not a correctness bound, and no production evidence yet
/// fixes it; 300 s is a defensible starting scale (ratification-pending, issue #479). Distinct from
/// the swap gate's own thresholds; changing it only widens/narrows the narration window, never
/// whether a swap fires.
const BLIND_PREEMPT_NOTICE_SECS: u64 = 300;

/// The [`blind_active_view`] ANCHOR-arm inputs: the retained pre-blind anchor (`last_good`, #450) and
/// the active account's frozen per-window session high-water mark (#614). Bundled into one struct so
/// `blind_active_view` stays within the repo's 7-argument clippy bound (this repo never `#[allow]`s
/// `too_many_arguments`) once #619 adds the mark alongside the anchor. The mark plausibility-corrects
/// the anchor's session at the arm read (issue #619): a stale-low pre-blind reading is raised to the
/// mark so the projection tracks the swap the #452 gate actually fires rather than reporting a
/// false-"OK" off a reading that never happened.
#[derive(Clone, Copy)]
struct AnchorArmInputs {
    /// The retained pre-blind reading (`None` = a genuinely-unknown active with no anchor, #450).
    last_good: Option<LastGood>,
    /// The active account's session high-water mark for the anchor's window (`None` = never polled
    /// with a parseable `session_resets_at`, so no correction applies — the raw anchor stands).
    high_water: Option<swap::SessionHighWater>,
}

/// Project the retained #452 preemptive-swap record into the `status` wire narration (issue #479), or
/// `None` when there is no swap to narrate. PURE — a function of the record, the CURRENT active
/// account's label, and the monotonic clock — so it is unit-tested directly. `Some` only when BOTH:
/// - **still current** — the swap's TARGET (`to`) is STILL the active account. Any later swap (reactive
///   / emergency / manual `use` / external-login reconcile) moves the active away from `to`, so the
///   "swapped off X → Y" narration is stale and self-invalidates here — no scattered clear at every
///   swap site needed (the one clear in [`record_swap`](Daemon::record_swap) only covers same-active
///   supersession within the window; this covers the rest). A manual `use <from>` — the very undo the
///   notice names — moves active back to `from`, so it clears the hint at its most natural moment; AND
/// - **recent** — within the [`BLIND_PREEMPT_NOTICE_SECS`] window since the swap fired, measured on the
///   SAME monotonic clock the anchor / cooldown use.
///
/// Keeps `render_status` a pure function of the wire (the window + supersession are decided daemon-side
/// here), honoring the #169 UI-never-acts invariant.
fn recent_blind_preempt_swap_view(
    record: Option<&BlindPreemptSwapRecord>,
    active_label: Option<&str>,
    at: Instant,
) -> Option<BlindPreemptSwap> {
    let record = record?;
    if active_label != Some(record.to.as_str()) {
        return None;
    }
    if at.saturating_duration_since(record.at).as_secs() >= BLIND_PREEMPT_NOTICE_SECS {
        return None;
    }
    Some(BlindPreemptSwap {
        from_label: record.from.clone(),
        to_label: record.to.clone(),
        last_known_session_pct: record.last_known_session_pct,
    })
}

/// Project the retained runtime landing-overshoot record into the `status` wire notice (issue #613),
/// or `None` when there is none to surface or it has aged out. PURE — a function of the record and
/// the monotonic clock — so it is unit-tested directly. `Some` only while RECENT: within the
/// [`landing::LANDING_OVERSHOOT_NOTICE_SECS`] window since the overshoot was observed, on the SAME
/// monotonic clock the anchor / cooldown use. Unlike [`recent_blind_preempt_swap_view`] there is NO
/// "still current" gate — a landing overshoot is a point event about a PAST parked account, not a
/// standing property of the current active one, so only the recency window bounds it. Keeps
/// `render_status` a pure function of the wire (the window is decided daemon-side here), honoring the
/// #169 UI-never-acts invariant.
fn recent_landing_overshoot_view(
    record: Option<&LandingOvershootRecord>,
    at: Instant,
) -> Option<LandingOvershoot> {
    let record = record?;
    if at.saturating_duration_since(record.at).as_secs() >= landing::LANDING_OVERSHOOT_NOTICE_SECS {
        return None;
    }
    Some(LandingOvershoot {
        from_label: record.from_label.clone(),
        decision_pct: record.decision_pct,
        landing_pct: record.landing_pct,
    })
}

/// Upper bound (seconds) on the jittered start-up delay (issue #76). Before its
/// FIRST poll the daemon waits a uniform `[0, this)` so that repeated restarts of
/// the same config — and the N accounts within a cycle — do not synchronize an
/// immediate burst of usage requests. Small enough to stay responsive on launch.
const STARTUP_DELAY_CAP: f64 = 30.0;

/// Maximum autonomous scrubbed-canonical adopt-target recoveries (issue #467) within one
/// [`SCRUB_ADOPT_WINDOW`] before the daemon BACKS OFF. An isolated scrub is healed at once; only a
/// rapid re-scrub cluster — the canonical getting emptied again right after each adopt (a persistent
/// multi-session rotation churn) — trips the bound, after which the daemon holds and surfaces the
/// stuck state (the durable `canonical_recovery_exhausted` event + #469's status/menubar signal)
/// rather than thrashing a re-auth loop.
const SCRUB_ADOPT_MAX: u32 = 3;
/// The rolling window over which [`SCRUB_ADOPT_MAX`] autonomous adopts are counted (issue #467).
/// Sized comfortably above a few adopt→re-scrub cycles at the default 5-minute poll cadence
/// ([`crate::config`] `DEFAULT_POLL_SECS` = 300) so a genuine churn burst is contained within one
/// window, yet short enough that a fresh, isolated scrub an hour later opens a new episode and is
/// healed immediately. On expiry the counter resets and autonomous recovery resumes.
const SCRUB_ADOPT_WINDOW: Duration = Duration::from_secs(3600);

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
/// - **Redaction-clean**: `acct` is the account's roster `label` — never a token or a
///   credential-read email. Since #444/#447 an operator-*authored* label MAY be an
///   email (the capture prompt pre-fills the harvested address), so the store carries
///   it verbatim under the SAME provenance-scoped rule as the render/event channels:
///   an authored email label is permitted; an UNAUTHORED email (a stranger's address,
///   a blob spill) is not (issue #15, relaxed by #444; see
///   `redaction::meter::unauthored_emails`).
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

/// The result of one [`keep_warm_and_promote`](Daemon::keep_warm_and_promote) attempt: whether a
/// fresh token was PROMOTED to the canonical item, plus the cycle's non-secret classification
/// (issue #643). The reactive caller reads only [`promoted`](Self::promoted); the proactive caller
/// discards the result entirely; the `use`-activate recovery re-probe additionally folds
/// [`outcome`](Self::outcome) / [`token_rotated`](Self::token_rotated) into the credential-health
/// verdict via [`fold_recovery_outcome`](Daemon::fold_recovery_outcome). A dead / absent refresh
/// token (the `!has_live_refresh_token` short-circuit) maps to `outcome = Dead` — its absence IS the
/// dead signal — so the recovery path keeps an honest 🔴 without a doomed spawn.
struct KeepWarmPromote {
    /// Whether a fresh token was actually written to the canonical item this cycle.
    promoted: bool,
    /// The cycle's non-secret [`RefreshEventOutcome`] — `Dead` for the empty-refresh-token
    /// short-circuit, else the classified mint result (`Error` on a could-not-run).
    outcome: RefreshEventOutcome,
    /// Whether CC rotated the refresh token value this cycle (the AC-3 durability signal;
    /// `false` for the short-circuit / could-not-run).
    token_rotated: bool,
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

/// The hex length of the canonical refresh-token FINGERPRINT (issue #464) — a SHA-256 hex
/// PREFIX, never the token. 16 chars (64 bits) is ample to tell one credential's token from
/// another across polls (the rotation-interference signal #465 reads) while staying UNDER the
/// redaction meter's 20-char high-entropy-run backstop (`redaction::meter::ENTROPY_MIN_RUN`), so
/// the fingerprint reads CLEAN; it matches the existing 16-hex uuid-seed slice in
/// [`keep_warm_stagger_secs`].
const CANONICAL_FINGERPRINT_HEX: usize = 16;

/// A redaction-safe fingerprint of a canonical token: the first [`CANONICAL_FINGERPRINT_HEX`]
/// chars of its SHA-256 hex digest (issue #464). Identity WITHOUT the secret — it changes when
/// the canonical's token rotates (a refresh / swap / out-of-band `/login`), so the scrub-driving
/// rotation is measurable from the log, yet reveals nothing of the token (a one-way hash,
/// prefix-truncated). Hashing an INDIVIDUAL token (not the whole blob) keeps it distinct from the
/// meter's `BlobSha256` fingerprint, and the 16-char prefix stays under its high-entropy backstop,
/// so the value reads clean through the #15 meter. Reuses the hand-rolled [`crate::sha256`]
/// primitive (no new dependency — the CONTRIBUTING minimal-dependency line). A pure function,
/// unit-tested directly.
fn canonical_fingerprint(token: &[u8]) -> String {
    crate::sha256::sha256_hex(token)[..CANONICAL_FINGERPRINT_HEX].to_owned()
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

/// The most recent LOCAL landing overshoot, retained so `status` can surface it (issue #613). Set in
/// [`Daemon::note_landing_overshoot`] when a parked account's live reading crosses the SLO ceiling
/// within its [`ParkedLanding`] window; projected onto the wire ([`LandingOvershoot`]) by
/// [`recent_landing_overshoot_view`] while RECENT (within [`landing::LANDING_OVERSHOOT_NOTICE_SECS`]).
/// A single latest-wins slot (a fresh overshoot supersedes an older notice), the runtime mirror of
/// the offline #595 landing SLI's breach. DEDICATED — kept SEPARATE from the per-account
/// [`ParkedLanding`] arming, exactly as [`BlindPreemptSwapRecord`] is kept separate from the cooldown
/// primitive. Process-local: never serialized. Non-secret — one operator handle + two `u8`s + a
/// monotonic [`Instant`], never a token or email (issue #15).
#[derive(Debug, Clone, PartialEq)]
struct LandingOvershootRecord {
    /// The label of the parked account that overshot — the one swapped AWAY FROM, observed climbing
    /// past the ceiling. Never the email (issue #15).
    from_label: String,
    /// The session % the swap fired on (the parked [`ParkedLanding::decision_pct`]).
    decision_pct: u8,
    /// The session % the parked account actually LANDED at — the live peak that crossed the ceiling.
    landing_pct: u8,
    /// When the overshoot was observed — monotonic ([`Instant`]), so the
    /// [`landing::LANDING_OVERSHOOT_NOTICE_SECS`] window is measured against the SAME clock as the
    /// anchor / cooldown. Process-local: never serialized.
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
    /// converted at the fold), AND — for the ACTIVE account, which the parked sweep excludes —
    /// reconciled to the freshly-PROMOTED canonical token on each keep-warm promote (issue #477,
    /// [`promote_canonical`](Daemon::promote_canonical)), so the rollup judges staleness off the
    /// live canonical the sessions read rather than the un-restashed (deliberately stale) stash.
    /// `None` until the refresh engine has observed this account (e.g. `[refresh]` is off). The
    /// rollup's `Stale` (expired) input + the wire clock.
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
    /// POLL_BACKOFF_MAX_SHIFT)`, capped at [`POLL_BACKOFF_CAP`] for a peer or the tighter
    /// [`ACTIVE_POLL_BACKOFF_CAP`] for the active account (issue #453; never below a server
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
    /// The RAW server-advised `Retry-After` that armed this account's CURRENT back-off window
    /// (issue #582), or `None` when the window is the daemon's OWN self-capped exponential (the
    /// server sent no header, OR sent a ZERO one — which contributes nothing to the wait, so it is
    /// normalized to `None` here per the issue's `ra > 0` classification) or no window is armed at
    /// all. So a `Some` means exactly "a server directive is holding this account off, and it is
    /// the reason the window is as long as it is". Written and cleared in exact lockstep
    /// with [`poll_backoff_until`](Self::poll_backoff_until) by
    /// [`note_account_backoff`](Daemon::note_account_backoff), so the pair is never torn: a `Some`
    /// here always describes the window `poll_backoff_until` is currently holding open.
    ///
    /// RETAINED (rather than left riding the tick-local `TickBackoff` / the durable
    /// [`Event::UsageBackoff`]) because the #582 swap-away arm must read it at DECISION time, on a
    /// later tick than the throttled poll that armed it: the active account is polled only every
    /// ~2 sub-intervals (the #366 interleave) and is SKIPPED entirely while backing off, so the
    /// tick that decides is almost never the tick that observed the `429`. `Option<Duration>`, not
    /// a bool: the raw value is what the swap-away path reports (`retry_after_secs=`), and it is
    /// the pre-cap server directive (#295) — never the clamped effective wait.
    ///
    /// Diagnostic / decision state ONLY. It arms no back-off and floors no wait — the #453
    /// absolute-floor arithmetic in `note_account_backoff` reads `signal.retry_after` directly and
    /// is untouched by this field's existence.
    poll_backoff_retry_after: Option<Duration>,
    /// The monotonic-[`Clock`] instant before which this NON-active account's poll is SKIPPED
    /// because it is OUT OF ROTATION — weekly- or session-exhausted (issue #537). Armed by a
    /// SUCCESSFUL poll whose reading is out of rotation
    /// ([`note_exhausted_poll`](Daemon::note_exhausted_poll)) to
    /// `now + min(exhausted_poll_secs, max(soonest_applicable_resets_at - now, poll_secs))`,
    /// and cleared when a later poll reads the account viable again OR when it becomes the
    /// active account (which is exempt). While [`Daemon::tick`] sees `now < exhausted_poll_until`
    /// (and the account is NOT active) it skips this account's poll — an exhausted peer's usage
    /// number cannot change until its server-side window resets, so re-polling it every
    /// `poll_secs` is a wasted request (see
    /// [`exhausted_slow_polling`](Daemon::exhausted_slow_polling)).
    ///
    /// DELIBERATELY SEPARATE from [`poll_backoff_until`](Self::poll_backoff_until) (ADR-0019):
    /// that back-off is armed by a 429/5xx and CLEARED on any success, but an exhausted account
    /// is an HTTP-200 SUCCESS — the very poll that reads exhaustion would clear a shared window —
    /// and it drives the 429 `UsageBackoff` events + streak, so overloading it would fire
    /// spurious rate-limit signals. `None` until the first out-of-rotation reading; on the same
    /// monotonic clock as `poll_backoff_until`.
    exhausted_poll_until: Option<Instant>,
}

/// One roster account's carried per-signal runtime state (issue #668, #637 step 5).
///
/// Bundles what were EIGHT parallel `Vec`s on [`DecisionState`], each indexed by roster position and
/// held in hand-maintained lockstep at every sizing/re-key site. That index-alignment was an
/// invariant no type enforced: a ninth per-account signal meant remembering [`Daemon::new`], both
/// arms of [`reconcile_roster`](Daemon::reconcile_roster), and the commit block — and forgetting one
/// still COMPILED, silently drifting a vec out of length/index sync with the roster (a projective
/// read would then index the wrong account or panic out of bounds). Bundled, the roster's runtime
/// state is ONE vec: a new signal is a new FIELD here, and every lockstep site updates it for free.
///
/// [`DecisionState::accounts`] holds one of these per roster account, in roster order. State NOT
/// indexed by roster position — the cooldown (#10), the canonical watch (#13), the tick counter, the
/// active account's [`last_good`](DecisionState::last_good) anchor (#450) — deliberately stays on
/// [`DecisionState`]: it is daemon-scoped, not account-scoped.
///
/// `Clone` (not `Copy`) because [`AccountHealth`] is: [`reconcile_roster`](Daemon::reconcile_roster)
/// clones a persisting account's slot into the rebuilt vec. `Default` is the freshly-onboarded
/// account's state — unpolled, no reading, healthy — so a new roster entry is `AccountRuntime::default()`.
#[derive(Default, Clone)]
struct AccountRuntime {
    /// Health carried across ticks (issue #42): the consecutive-401 streak (feeding the `monitor_401`
    /// log event and the dead-credential threshold), the quarantine flag, and the recovery-probe
    /// count. See [`AccountHealth`].
    health: AccountHealth,
    /// Last-known usage reading (issue #80). The daemon polls ONE account per tick (staggered, the
    /// active interleaved before each peer — #366), so a decision is taken on the most recent reading
    /// of EACH account rather than a single-instant poll-of-all — one account's number may be ~a cycle
    /// older than another's. `None` until the account is first polled (or after a poll fails). The
    /// decision/snapshot view masks an out-of-rotation (disabled / quarantined) non-active account back
    /// to `None` ([`decision_readings`](Daemon::decision_readings)), so stale carried data can never
    /// leak into [`pick_target`].
    last_reading: Option<Usage>,
    /// When [`last_reading`](Self::last_reading) was observed (issue #449) — the monotonic ([`Instant`])
    /// time of the reading now in that slot, `None` whenever the reading is `None` (never polled, or
    /// cleared by a failed poll). The per-account usage-velocity signal divides its delta by
    /// `now - last_reading_at` to normalize to %/min; keeping the timestamp BESIDE the reading (rather
    /// than folding it into the `Copy` decision [`Usage`]) leaves the swap-decision type lean and every
    /// reader of the reading unchanged. Process-local: never serialized (an [`Instant`] is meaningless
    /// across the socket).
    last_reading_at: Option<Instant>,
    /// The retained SESSION-velocity EMA (issue #539, ADR-0017) — the smoothed rate the #539
    /// velocity-projection preemptive trigger ([`Daemon::velocity_swap`]) projects from. `None` until
    /// the account has a usable two-reading interval, and RESET to `None` on a session-usage DROP (a
    /// 5 h reset / recovery). Updated in the poll fold by
    /// [`note_session_velocity`](Daemon::note_session_velocity) from the SAME `(prev, next, elapsed)`
    /// the durable [`Event::UsageVelocity`] uses. See [`VelocityEma`].
    session_velocity: Option<VelocityEma>,
    /// The ARMED landing watch for the runtime landing-overshoot signal (issue #613) — `Some` on the
    /// account the daemon just parked with a `reason=session` swap, until its post-swap window
    /// ([`ParkedLanding`]) closes (an overshoot fires, the window elapses, or the account goes active
    /// again). Armed in [`decide_action`](Daemon::decide_action), checked each poll by
    /// [`note_landing_overshoot`](Daemon::note_landing_overshoot). `None` when no landing window is
    /// open. See [`ParkedLanding`].
    parked_landing: Option<ParkedLanding>,
    /// The SESSION high-water mark within the current session window (issue #614) — the plausibility
    /// baseline that lets both swap arms recognize a stale / cache-lagged LOW `/oauth/usage` reading
    /// (usage is monotonic within a window, so a drop with an unchanged `session_resets_at` cannot be
    /// real). Folded from every SUCCESSFUL poll by [`swap::SessionHighWater::fold`] (a failed poll is
    /// blindness, not evidence the window rolled, so it leaves the mark untouched) and released
    /// automatically when the window rolls, so it can never pin an account across windows. `None`
    /// until the account is polled with a parseable `session_resets_at`. See [`swap::SessionHighWater`].
    session_high_water: Option<swap::SessionHighWater>,
    /// The pre-blind anchor (issue #583) — `Some` exactly while this account is inside a blind episode
    /// whose entry the daemon actually witnessed. Set on the live→blind ENTRY edge (which emits
    /// [`Event::BlindEnter`]), read and cleared on the blind→live EXIT edge (which emits
    /// [`Event::BlindExit`]), and touched by nothing else — no swap path, no active resolution — so
    /// neither censoring tail of `blind_window` can reach it. `None` for an account that is reading
    /// live, and for one whose blindness the daemon never saw begin (a first-ever poll that fails, or
    /// one already blind at startup): with no anchor there is no baseline to difference against, so no
    /// episode is claimed. See [`BlindAnchor`]; contrast the ACTIVE-scoped
    /// [`last_good`](DecisionState::last_good).
    blind_anchor: Option<BlindAnchor>,
    /// Whether this account has been polled at least once this run (issue #80). Drives the warm-up
    /// latch [`warmed_up`](DecisionState::warmed_up).
    polled_once: bool,
}

/// Per-account decision helpers (issue #669, the final step of the #637 `daemon.rs`
/// decomposition). These let a decision reason over ONE account's own bundled state through a
/// named predicate instead of index-reaching into the coordinator's [`DecisionState::accounts`]
/// vec and re-deriving the predicate inline at each site.
///
/// Deliberately NARROW — only PURE per-account reads belong here. The swap DECISIONS themselves
/// (the reactive / #539 velocity ([`Daemon::velocity_swap`]) / #452 blind ([`Daemon::blind_swap`])
/// / #42 emergency ([`Daemon::emergency_swap`]) arms and `pick_target` selection) stay on
/// [`Daemon`] by design: collectively they draw the seeded `rng` in a FIXTURE-PINNED order (session
/// ceiling → weekly ceiling → cooldown, plus the emergency / velocity draws), reach the roster and
/// the OTHER accounts to pick a target, and write daemon-scoped `state.signaled_*`. Relocating those
/// onto `AccountRuntime` would only re-thread the daemon's `rng` + roster + config back in as
/// parameters — muddying the draw-order guarantee for no reasoning gain — so #637 step 6 stops at
/// the pure reads. A long-but-documented [`decide_action`](Daemon::decide_action) is the accepted
/// outcome (#637's own verdict: "likely not worth it").
impl AccountRuntime {
    /// This account's retained session-velocity EMA (issue #539, ADR-0017), but ONLY once it is
    /// SUSTAINED — a [`VelocityEma`] blended from `>= MIN_VELOCITY_SAMPLES` intervals; `None` while
    /// the signal is absent (never polled, a first/failed poll, or reset by a window drop) OR still
    /// a single unblended sample. This is the ONE "is the velocity usable?" predicate every
    /// velocity-aware arm shares — the reactive ceiling derivation and the #539 projection
    /// ([`decide_action`](Daemon::decide_action) / [`Daemon::velocity_swap`]), the #540 near-limit
    /// fast-poll ([`Daemon::active_near_limit`]), and the #634 blind-window ingredient log
    /// ([`Daemon::blind_velocity_ingredients`]) — each of which previously re-encoded the same
    /// `samples >= MIN_VELOCITY_SAMPLES` gate inline, differing only in the UNSUSTAINED fallback
    /// (`0.0` rate / hold / no fast-poll / no ingredient — supplied by each caller, unchanged).
    /// Gating here keeps the "SUSTAINED, never a single-interval spike" invariant (issues #538 /
    /// #584 / #600) in one place. A pure read of retained state — no `rng`, no clock, no roster.
    fn sustained_session_velocity(&self) -> Option<VelocityEma> {
        self.session_velocity
            .filter(|v| v.samples >= MIN_VELOCITY_SAMPLES)
    }
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
    /// The most recent #452 preemptive swap-away, retained so `status` can narrate it (issue #479):
    /// `Some` from a successful [`Daemon::blind_swap`] until superseded / aged out. See
    /// [`BlindPreemptSwapRecord`]. `None` by default (no preemptive swap yet). DEDICATED, kept off the
    /// cooldown-bearing `last_swap` above.
    last_blind_preempt_swap: Option<BlindPreemptSwapRecord>,
    /// The most recent LOCAL landing overshoot, retained so `status` can surface it (issue #613):
    /// `Some` from [`Daemon::note_landing_overshoot`] until aged out of the notice window. See
    /// [`LandingOvershootRecord`]. `None` by default (no overshoot yet). A latest-wins slot; the
    /// runtime mirror of the offline #595 landing SLI. Carries a String label, so it survives a roster
    /// reindex untouched (unlike the per-account `parked_landing` below).
    last_landing_overshoot: Option<LandingOvershootRecord>,
    /// Monotonic-[`Clock`] instants of recent server-`Retry-After`-driven preemptive swaps (issue
    /// #582) — the #582 circuit-breaker's evidence that the throttle is WALKING the roster rather
    /// than afflicting one account. Appended by [`Daemon::blind_swap`] on each swap it fires away
    /// from a server-throttled active, and pruned to [`RETRY_AFTER_WALK_WINDOW`] on read, so the
    /// vector holds at most a handful of entries (one per swap, and the path swaps at most
    /// [`RETRY_AFTER_WALK_MAX`] times per window before it stops).
    ///
    /// Records EVERY server-throttled preemptive swap — including one the ratified #452 anchor
    /// band would have fired anyway — because the walk is a property of the THROTTLE moving, not
    /// of which arm authorised the move. Only the #582 arm ever CONSULTS the record, so a #452
    /// anchor-band swap is never blocked by it (its ratified behaviour is preserved exactly);
    /// counting those swaps only ever makes the newer, more speculative arm more conservative.
    retry_after_swaps: Vec<Instant>,
    /// Edge-trigger guard for the #582 reserve hold: set when the "would spend the LAST viable
    /// target" event is emitted, and cleared on any tick where the active is no longer blind, or
    /// on any swap. So the signal fires once per episode rather than on each of the ~240 ticks a
    /// 3600 s blind window spans. Mirrors the `signaled_all_exhausted` idiom.
    signaled_retry_after_reserve: bool,
    /// Edge-trigger guard for the #582 throttle-walk alarm, cleared exactly like
    /// [`signaled_retry_after_reserve`](Self::signaled_retry_after_reserve) above. Mirrors the
    /// `signaled_all_exhausted` idiom.
    signaled_retry_after_walk: bool,
    /// Per-account carried runtime state (issue #668), one slot per roster account, in roster order —
    /// health (#42), the last-known reading (#80) and its timestamp (#449), the session-velocity EMA
    /// (#539), the armed landing watch (#613), the session high-water mark (#614), the pre-blind
    /// anchor (#583), and the warm-up flag (#80). Sized to the roster in [`Daemon::new`] and re-keyed
    /// BY UUID (never by position) in [`reconcile_roster`](Daemon::reconcile_roster).
    ///
    /// ONE vec, not eight parallel ones: index `i` denotes roster account `i` for EVERY signal at
    /// once, so the roster/state length-and-index invariant is structural rather than hand-maintained
    /// across a growing set of sizing and re-key sites. See [`AccountRuntime`].
    accounts: Vec<AccountRuntime>,
    /// Edge-trigger guard for the all-exhausted signal (issue #11): set when an
    /// `all_exhausted` event is emitted, and cleared by [`Daemon::tick`] on any
    /// cycle that is NOT the no-viable-target state. So the signal fires exactly
    /// ONCE per all-exhausted episode — not once per poll while every account
    /// stays exhausted — and fires afresh if the state clears and is re-entered.
    signaled_all_exhausted: bool,
    /// Edge-trigger guard for the active-dead-no-target strand signal (issue #405):
    /// set when an `active_dead_no_target` event is emitted (the emergency path found
    /// no live target for a DEAD active), cleared by [`Daemon::tick`] on any cycle
    /// that is NOT that strand. So the signal fires exactly ONCE per strand episode —
    /// not once per emergency tick while every spare stays weekly-exhausted — and
    /// fires afresh if the strand clears and is re-entered. The strictly-worse sibling
    /// of `signaled_all_exhausted`, mirroring its edge-trigger idiom exactly.
    signaled_active_dead_no_target: bool,
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
    /// Edge-trigger guard for the canonical-scrub signal (issue #464): set when a
    /// `canonical_scrubbed` event is emitted (the shared `Claude Code-credentials` item was
    /// observed empty/scrubbed), cleared when the item is next observed LIVE (which emits the
    /// `canonical_restored` counterpart). So the scrub fires exactly ONCE per episode — not once
    /// per poll while the item stays scrubbed — and afresh if it recovers and is scrubbed again.
    /// Mirrors `signaled_all_exhausted`; UNLIKE it, a transient unreadable poll does NOT clear it
    /// (only a confirmed live read does), so a flaky read never fabricates a recovery.
    signaled_canonical_scrubbed: bool,
    /// The PRIOR poll's canonical refresh-token FINGERPRINT (issue #475), retained across polls to
    /// detect a rotation-YANK: a Present→Present fingerprint change means the shared item ROTATED
    /// under any mid-flight sessions, which get a RECOVERABLE 401 → "Not logged in" (they re-read
    /// the still-live item on `continue`, no `/login`) — the frequent sibling of the rare,
    /// UNRECOVERABLE scrub `signaled_canonical_scrubbed` tracks (umbrella #463). A non-secret 16-hex
    /// SHA-256 prefix (issue #15), NEVER a token. Updated ONLY by [`note_canonical_liveness`](Daemon::note_canonical_liveness)
    /// — NOT committed by the daemon's OWN swap / keep-warm canonical writes, UNLIKE `canonical_watch`
    /// — so a self-authored rotation is still observed as a yank, keeping the diagnostic yank series
    /// the full canonical-rotation denominator #465 measures (and a reconcile-independent cross-check).
    /// `None` until the first Present observation seeds it, and reset to `None` on a scrub (so the next
    /// Present re-seeds without a false yank across the recovery edge).
    prev_canonical_fingerprint: Option<String>,
    /// Count of autonomous scrubbed-canonical adopt-target recoveries (issue #467) within the current
    /// churn window — the bound against a re-auth thrash loop when the canonical keeps getting
    /// re-scrubbed right after each adopt. Incremented only on a LANDED adopt; reset to `0` when
    /// [`scrub_adopt_window_start`](Self::scrub_adopt_window_start) is older than [`SCRUB_ADOPT_WINDOW`].
    /// Once it reaches [`SCRUB_ADOPT_MAX`] the daemon backs off (holds + surfaces) for the rest of the
    /// window. Default `0`.
    scrub_adopt_count: u32,
    /// When the current scrub-adopt churn window opened (issue #467): set on the FIRST adopt of an
    /// episode, used to age out [`scrub_adopt_count`](Self::scrub_adopt_count) after
    /// [`SCRUB_ADOPT_WINDOW`]. On the same monotonic clock as the tick's `at`. `None` between episodes.
    scrub_adopt_window_start: Option<Instant>,
    /// Edge-trigger guard for the scrub-adopt back-off signal (issue #467): set when a
    /// `canonical_recovery_exhausted` event is emitted (the adopt bound was hit), cleared when the
    /// churn window resets — so the back-off fires exactly ONCE per episode, not once per held tick
    /// while the canonical stays scrubbed. Mirrors `signaled_all_exhausted`.
    signaled_scrub_adopt_exhausted: bool,
    /// The ACTIVE account's retained pre-blind anchor (issue #450): its last
    /// SUCCESSFUL reading (`session` / `weekly` fractions) plus the monotonic time it
    /// was observed, kept ACROSS a `429` / `5xx` blindness window that clears
    /// `accounts[active].last_reading` to `None`. `None` until the
    /// active account is polled successfully once, and reset to `None` on every
    /// swap-away / active-loss so it always belongs to the CURRENT active account.
    /// Consumed by the bounded-blindness preemptive swap (issue #452, ADR-0017); the
    /// reactive `swap::decide` path never reads it, so keeping it separate leaves that
    /// path byte-for-byte unchanged. See [`LastGood`].
    last_good: Option<LastGood>,
    /// Edge-trigger latch for the #452 gate-premise SLI (issue #482): set once a
    /// [`Event::BlindGateEligible`] has fired for the
    /// CURRENT blind episode of the active account, so the SLI emits exactly ONCE per episode (the
    /// gate would swap once, ending the episode) rather than every held blind tick. Cleared when the
    /// active account regains a live reading (recovery) OR its anchor ([`last_good`](Self::last_good))
    /// is dropped (swap-away / active-loss) — the latch and the anchor share the blind-episode
    /// lifecycle, so [`note_blind_gate_eligibility`](Daemon::note_blind_gate_eligibility) manages both
    /// clears in one place and no `last_good` reset site has to touch it.
    blind_gate_signaled: bool,
    /// Whether the ACTIVE account's near-limit poll-coverage fast-poll is engaged (issue #540):
    /// the active's last reading — or the #539 velocity projection — is in the near-limit band, so
    /// [`next_subinterval`](Daemon::next_subinterval) tightens the poll sub-interval to
    /// [`near_limit_poll_secs`](Daemon::near_limit_poll_secs). Recomputed every tick from the
    /// post-decision state ([`near_limit_fast_poll_engaged`](Daemon::near_limit_fast_poll_engaged))
    /// so it always reflects the CURRENT active account and its freshest reading; the wait path
    /// reads this cached verdict rather than re-deriving it, and its `false → true` transition is
    /// the edge that emits the durable [`Event::NearLimitPollCoverage`] exactly once per band
    /// entry (its `true → false` transition clears silently — the band ends at a swap, which emits
    /// its own [`Event::Swap`], or at a below-band reading). Defaults `false` (below-band / not yet
    /// warmed / kill-switch), so a fresh daemon and every baseline test start on the normal cadence.
    near_limit_fast_poll: bool,
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

#[cfg(test)]
impl DecisionState {
    /// Test seam: the per-account carried readings, flattened into the positional vec the decision
    /// and preview entry points take — the read the pre-#668 parallel `last_readings` vec offered
    /// directly.
    fn readings(&self) -> Vec<Option<Usage>> {
        self.accounts.iter().map(|a| a.last_reading).collect()
    }

    /// Test seam: seed each account's carried reading positionally, leaving every OTHER
    /// [`AccountRuntime`] field untouched — the write the pre-#668 parallel `last_readings` vec
    /// offered directly, minus its ability to clobber the other signals.
    ///
    /// Panics on a length mismatch: [`accounts`](Self::accounts) is sized to the roster, so a
    /// differently-sized seed is a test bug. Pre-#668 the same assignment silently desynced ONE vec
    /// from the other seven — exactly the invariant this bundle removes — so the loud failure is the
    /// point.
    fn seed_readings(&mut self, readings: impl IntoIterator<Item = Option<Usage>>) {
        let readings: Vec<_> = readings.into_iter().collect();
        assert_eq!(
            readings.len(),
            self.accounts.len(),
            "a reading seed must cover exactly the roster",
        );
        for (account, reading) in self.accounts.iter_mut().zip(readings) {
            account.last_reading = reading;
        }
    }
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
    /// `50..=99` percent each cycle, then `/100` for the swap decision. Distinct from
    /// [`Self::session_ceiling_base`] (the un-jittered base it draws around), which the
    /// deterministic display paths key off instead.
    session_ceiling_strategy: Strategy,
    /// Per-cycle WEEKLY swap-away trigger strategy (issue #41): drawn + clamped to
    /// `50..=99` percent each cycle, then `/100` for the swap decision — the
    /// weekly-dimension counterpart of `session_ceiling_strategy`, independent of it.
    weekly_ceiling_strategy: Strategy,
    /// Base WEEKLY CEILING as a fraction (`weekly_ceiling / 100`), un-jittered — the configured
    /// not-cross line itself (issue #607 reframed this from a fire-AT trigger), and the SAME value
    /// the `use` pre-swap gate treats as "weekly exhausted" (issue #11/#37). Distinct from
    /// `weekly_ceiling_strategy` (the per-cycle JITTERED draw): the snapshot's `weekly_exhausted`
    /// verdict (issue #72) must be deterministic and match the user-facing viability rule, so it
    /// keys off this base, not a per-cycle draw.
    ///
    /// This is the RAW ceiling. Rotation decisions must NOT use it directly — they use
    /// [`Self::weekly_rotation_line`], which applies the tail margin. See that method for why.
    weekly_ceiling_base: f64,
    /// Base SESSION swap-away threshold as a fraction (`session_ceiling / 100`),
    /// un-jittered — the session-dimension counterpart of [`Self::weekly_ceiling_base`].
    /// The always-on session anti-thrash gate in [`pick_target`] keys off this on the
    /// deterministic display paths ([`Self::next_swap`], [`Self::refresh_exclusions`]),
    /// so the "next swap" candidate never flickers with per-cycle session-trigger
    /// jitter; the live swap path (`decide_action`) uses the per-cycle drawn trigger.
    session_ceiling_base: f64,
    /// Default-on swap-target session reserve (issue #398) as a fraction
    /// (`target_max_session_usage / 100`), always valued. The PROACTIVE swap path passes it as
    /// `Some(..)` to [`pick_target`] — only swap TO an account whose session usage is
    /// below it — layering a STRICTER reserve on the always-on session gate
    /// (`session < session_ceiling`, which prevents oscillation on its own). The
    /// EMERGENCY path ([`Self::emergency_swap`]) passes `None` instead: when the active
    /// credential is DEAD, liveness beats the reserve. Supersedes #10's opt-in `None`
    /// default — the config `target_max_session_usage` is now always set.
    target_max_session_usage: f64,
    /// The #452 bounded-blindness preemptive-swap gate `T` (ADR-0017), in seconds:
    /// [`Self::blind_swap`] fires only after the active account has been blind LONGER than
    /// this (config `session_blind_swap_secs`, strict `>`). Set to the config ceiling it
    /// disables the path — the kill-switch. DELIBERATELY DISTINCT from the interim
    /// [`BLIND_GATE_SECS`] const that keys the always-on gate-premise SLI (#482) and the
    /// [`blind_active_view`] status projection (#479): those keep measuring at the interim
    /// band for #484 ratification even when this operator kill-switch is set, so disabling
    /// the SWAP never blinds the ratification SLIs. Equal to the const by default (both 300),
    /// so the measurement band and the action threshold align out of the box.
    session_blind_swap_secs: u64,
    /// The #452 preemptive-swap `risk_band` (ADR-0017) as a session fraction (config
    /// `session_blind_risk_band / 100`): [`Self::blind_swap`] fires only when the retained
    /// pre-blind anchor (`last_good`, #450) — plausibility-corrected to its window high-water
    /// mark (#619) — sat at/over this. Sibling of `session_blind_swap_secs`; likewise distinct from
    /// the SLI/status [`BLIND_GATE_RISK_BAND`].
    session_blind_risk_band: f64,
    /// The widened re-poll cadence CEILING, in seconds, for an out-of-rotation (weekly- or
    /// session-exhausted) NON-active peer (issue #537, config `exhausted_poll_secs`): the most
    /// an exhausted peer's poll is deferred. A known `resets_at` sooner than this pulls the next
    /// poll earlier; the FLOOR of the window is `poll_secs` (a slow-polled peer never re-polls
    /// faster than the normal cadence), read from [`poll_strategy`](Self::poll_strategy)`.base`.
    /// The ACTIVE account is exempt. See [`exhausted_poll_window`] and
    /// [`note_exhausted_poll`](Self::note_exhausted_poll).
    exhausted_poll_secs: u64,
    /// The TIGHTENED poll sub-interval CAP, in seconds, for the ACTIVE account while its reading (or
    /// the #539 projection) is in the near-limit band (issue #540, config `near_limit_poll_secs`):
    /// the near-limit-scoped MIRROR of [`exhausted_poll_secs`](Self::exhausted_poll_secs) — that
    /// WIDENS an exhausted peer, this TIGHTENS the active account on its final climb. Applied by
    /// [`next_subinterval`](Self::next_subinterval) as `min(poll_secs / N, near_limit_poll_secs)`
    /// ONLY while [`near_limit_fast_poll`](DecisionState::near_limit_fast_poll) is set, so with the
    /// #366 active-interleave the active is re-observed within ~`2 ×` this near-limit; below the
    /// band the sub-interval is the unchanged `poll_secs / N`. `0` disables the path — the
    /// kill-switch, like [`session_velocity_horizon_secs`](Self::session_velocity_horizon_secs).
    near_limit_poll_secs: u64,
    /// The #539 velocity-projection horizon `H` (ADR-0017), in seconds (config
    /// `session_velocity_horizon_secs`): [`Self::velocity_swap`] projects
    /// `observed + accounts[active].session_velocity.rate × H` and fires when it crosses the trigger. `0`
    /// disables the path (the projection reduces to `observed`, which the reactive path already
    /// held below the trigger, so it never crosses) — the kill-switch. Kept as `u64` seconds (not a
    /// pre-divided `f64`) so the projection reads `rate` (fraction/sec) × `H` in the SAME units the
    /// EMA stores.
    session_velocity_horizon_secs: u64,
    /// The #539 velocity-projection guard (ADR-0017) as a session FRACTION (config
    /// `session_velocity_min_project_above / 100`): [`Self::velocity_swap`] projects only when the
    /// observed reading is at/over this. The #538 spike's free guard (the projection cannot reach
    /// below it), biased below `session_ceiling` like [`Self::session_blind_risk_band`].
    session_velocity_min_project_above: f64,
    /// The #539 velocity-projection EMA weight α (ADR-0017) as a FRACTION (config
    /// `session_velocity_ema_alpha_pct / 100`): [`Self::note_session_velocity`] blends
    /// `ema = α·instant + (1-α)·prev` to damp a single-interval spike. `1.0` = no smoothing (raw
    /// last-interval velocity).
    session_velocity_ema_alpha: f64,
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
    /// `weekly_ceiling_base` is the stable base distinct from `weekly_ceiling_strategy`.
    cooldown_base: Duration,
    /// Per-cycle poll-interval strategy (issue #38): drawn + clamped to
    /// `5..=3600` s each loop iteration by
    /// [`next_poll_interval`](Self::next_poll_interval).
    poll_strategy: Strategy,
    /// Jitter RNG seam — process entropy in production, a fixed seed in tests
    /// (`with_seed`) so per-cycle draws are deterministic.
    rng: SplitMix64,
    /// The per-daemon TARGET-SELECTION seed (issue #612), threaded to
    /// [`pick_target_ranked`] / [`pick_target_with_reason_ranked`] inside a [`SelectionTiebreak`]
    /// (built by [`selection_tiebreak`](Self::selection_tiebreak)). `Some` (a
    /// once-drawn process-entropy value, set at production boot via
    /// [`with_tiebreak_seed`](Self::with_tiebreak_seed)) activates the enhanced selection: a
    /// velocity-aware preference then a per-daemon-stable jitter that disperses the cross-machine
    /// co-selection herd. `None` — the [`new`](Self::new) default, so every hermetic test keeps the
    /// deterministic pre-#612 soonest-reset + roster-index order — leaves selection velocity-blind
    /// and un-jittered. A single value held for the daemon's lifetime (NOT re-drawn per tick), so a
    /// daemon's tie-break order is STABLE across ticks (no selection flapping). Orthogonal to the
    /// per-cycle [`rng`](Self::rng) jitter and to the downward-only swap-ceiling jitter (issue #609).
    tiebreak_seed: Option<u64>,
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
    /// parked sweep uses for its own near-expiry horizon (no second *cadence* knob; the #468
    /// [`proactive_keep_warm`](Self::proactive_keep_warm) opt-in is a separate on/off gate, not a
    /// cadence). A per-account stagger offset in `[0, cadence)` is added on top for de-correlation.
    /// Only read when [`keep_warm`](Self::keep_warm) is wired; the `new` default is an inert
    /// placeholder.
    keep_warm_cadence: Duration,
    /// Whether the PROACTIVE keep-warm path ([`keep_active_warm`](Self::keep_active_warm)) fires
    /// (issue #468, finding #476 predicate C). **`false` by default** — the pre-emptive near-expiry
    /// mint that rotates the LIVE shared canonical every cadence is the ~44 % of daemon canonical
    /// churn #476 measured, and #467's autonomous adopt-target re-based the scrub it guards against
    /// to `continue`-recoverable, so the default leans on the REACTIVE backstop
    /// ([`should_keep_warm_retry`](Self::should_keep_warm_retry)) + #467 instead. Set by
    /// [`with_proactive_keep_warm`](Self::with_proactive_keep_warm) from `[refresh].proactive_keep_warm`;
    /// gates ONLY the proactive path — the reactive backstop keys off [`keep_warm`](Self::keep_warm)
    /// alone and is UNAFFECTED. Independent of, and nested within, the `[refresh].enabled` seam wiring
    /// (an unwired [`keep_warm`](Self::keep_warm) makes this moot).
    proactive_keep_warm: bool,
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
        // Per-account carried runtime state (issue #668), ONE slot per account — health (#42), the
        // last-known reading (#80) and its timestamp (#449), the session-velocity EMA (#539), the
        // armed landing watch (#613), the session high-water mark (#614), the pre-blind anchor
        // (#583), and the warm-up flag (#80). `AccountRuntime::default()` is the unpolled, no-reading,
        // healthy start: no projection, landing watch, plausibility baseline, or blind episode can
        // read off a signal the daemon has not observed yet. One sizing site for every signal, so a
        // ninth cannot be added and forgotten here.
        let accounts = vec![AccountRuntime::default(); roster.len()];
        Self {
            roster,
            poller,
            store,
            stash,
            clock,
            claude_json,
            session_ceiling_strategy: tunables.session_ceiling_strategy,
            weekly_ceiling_strategy: tunables.weekly_ceiling_strategy,
            // The un-jittered RAW weekly ceiling (the operator's not-cross line). The
            // deterministic `status` `weekly_exhausted` verdict + `use` gate key off the
            // ROTATION line derived from it — `weekly_rotation_line()` = this −
            // `swap::WEEKLY_TAIL_MARGIN` (issue #607/#72) — NOT this raw value directly, and
            // NOT the per-cycle jittered swap-decision draw.
            weekly_ceiling_base: f64::from(tunables.weekly_ceiling) / 100.0,
            session_ceiling_base: f64::from(tunables.session_ceiling) / 100.0,
            target_max_session_usage: f64::from(tunables.target_max_session_usage) / 100.0,
            session_blind_swap_secs: tunables.session_blind_swap_secs,
            session_blind_risk_band: f64::from(tunables.session_blind_risk_band) / 100.0,
            // The widened exhausted-peer cadence ceiling (issue #537); the floor is `poll_secs`,
            // read from `poll_strategy.base` at use so no second raw copy of it is stored.
            exhausted_poll_secs: tunables.exhausted_poll_secs,
            // The tightened near-limit active-poll sub-interval cap (issue #540), in seconds; `0`
            // disables the path. Read as-is (a plain Duration cap in `next_subinterval`), not a
            // fraction, so no conversion here.
            near_limit_poll_secs: tunables.near_limit_poll_secs,
            // The velocity-projection horizon / guard / EMA weight (issue #539); the guard + weight
            // are stored as fractions (like `session_blind_risk_band` / the triggers), the horizon
            // as seconds so the projection multiplies the fraction/sec EMA rate by it directly.
            session_velocity_horizon_secs: tunables.session_velocity_horizon_secs,
            session_velocity_min_project_above: f64::from(
                tunables.session_velocity_min_project_above,
            ) / 100.0,
            session_velocity_ema_alpha: f64::from(tunables.session_velocity_ema_alpha_pct) / 100.0,
            cooldown_strategy: tunables.cooldown_strategy,
            // The un-jittered cooldown window the socket `swap` command gates a manual
            // swap on (issue #167) — the same base `config.tunables.cooldown_secs` the
            // standalone `use` path uses, so routing through the daemon does not shift
            // the cooldown behavior.
            cooldown_base: Duration::from_secs(tunables.cooldown_secs),
            poll_strategy: tunables.poll_strategy,
            rng: SplitMix64::from_entropy(),
            // Enhanced target selection (issue #612) is OFF by default so every hermetic-test daemon
            // keeps the deterministic pre-#612 soonest-reset + roster-index order; production opts in
            // via `with_tiebreak_seed(SplitMix64::from_entropy().next_u64())`.
            tiebreak_seed: None,
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
            // Issue #468: proactive keep-warm of the active account is OFF by default (predicate C);
            // production opts in via `with_proactive_keep_warm` from `[refresh].proactive_keep_warm`.
            // The reactive backstop does not read this, so it is unaffected.
            proactive_keep_warm: false,
            // The #378 systemic-failure threshold defaults to the config default (opt-in wiring
            // via `with_systemic_failure_n`); hermetic tests that exercise the detector pass the
            // threshold directly to `SystemicRefreshHealth::note`, so this placeholder only sets
            // the value for a `note_systemic_refresh` call an integration test drives at defaults.
            systemic_failure_n: DEFAULT_REFRESH_SYSTEMIC_FAILURE_N,
            state: DecisionState {
                accounts,
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

    /// Opt the PROACTIVE keep-warm path in (issue #468, finding #476 predicate C). Off by default
    /// (see [`proactive_keep_warm`](Self::proactive_keep_warm)); production passes
    /// `[refresh].proactive_keep_warm` here, chained after [`with_keep_warm_engine`](Self::with_keep_warm_engine)
    /// inside the `[refresh].enabled` block. Separate from the engine wiring because the flag gates
    /// ONLY the proactive path while the REACTIVE backstop keys off the shared
    /// [`keep_warm`](Self::keep_warm) seam alone — one seam, two independently-gated consumers.
    /// Builder-style to mirror [`with_keep_warm_engine`](Self::with_keep_warm_engine).
    pub(crate) fn with_proactive_keep_warm(mut self, enabled: bool) -> Self {
        self.proactive_keep_warm = enabled;
        self
    }

    /// Arm the per-daemon TARGET-SELECTION seed (issue #612): enable the enhanced selection —
    /// velocity-aware preference + per-daemon jitter — with `seed` as its stable per-daemon key.
    /// Production draws a once-per-process entropy value
    /// (`with_tiebreak_seed(SplitMix64::from_entropy().next_u64())`) so independent daemons over the
    /// same roster disperse instead of co-selecting one target; left unset (`new`'s `None` default),
    /// a hermetic-test daemon keeps the deterministic pre-#612 order. Builder-style to mirror
    /// `with_swap_lock` / `with_config_path` and keep `new`'s args stable. See
    /// [`tiebreak_seed`](Self::tiebreak_seed).
    pub(crate) fn with_tiebreak_seed(mut self, seed: u64) -> Self {
        self.tiebreak_seed = Some(seed);
        self
    }

    /// The issue-#612 tie-break inputs, as every live selection site passes them: the carried
    /// per-account runtime state (whose retained velocity EMAs the tie-break reads) plus this
    /// daemon's seed. One accessor rather than the literal repeated at each call site, so a site
    /// cannot thread the runtime state while forgetting the seed (which compiles, and silently drops
    /// that path back to the pre-#612 order). Inert until production arms the seed — see
    /// [`tiebreak_seed`](Self::tiebreak_seed).
    fn selection_tiebreak(&self) -> SelectionTiebreak<'_> {
        SelectionTiebreak {
            accounts: &self.state.accounts,
            seed: self.tiebreak_seed,
        }
    }

    /// Replace the jitter RNG with a deterministically-seeded one — the test seam
    /// for reproducible per-cycle draws (issue #38 AC).
    #[cfg(test)]
    pub(crate) fn with_seed(mut self, seed: u64) -> Self {
        self.rng = SplitMix64::new(seed);
        self
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
        // Issue #464: distinguish a CONFIRMED gone canonical (`CredentialNotFound`, exit 44 — the
        // scrubbed/empty item) from a transient read failure, so `note_canonical_liveness` below
        // edge-triggers the scrub only on the former and HOLDS the signal on the latter (a flaky
        // read must never fabricate a scrub or a recovery).
        let mut canonical_absent = false;
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
            Err(Error::CredentialNotFound) => {
                // The canonical item is GONE (exit 44) — a confirmed scrub/empty, not a lock:
                // clear the back-off and fall through to poll (the loop never swaps on missing
                // data, so an unknown active simply holds). `note_canonical_liveness` reads
                // `canonical_absent` to edge-trigger the scrub event.
                self.state.lock_backoff = None;
                self.state.signaled_keychain_locked = false;
                canonical_absent = true;
                None
            }
            Err(_) => {
                // Unreadable for a non-lock, non-not-found reason (transient): no
                // change-detection is possible and it is not a confirmed scrub — clear the
                // back-off and fall through to poll, holding the canonical signal.
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

        // Issue #464: record the canonical item's OWN per-poll liveness (present / scrubbed /
        // unknown + a redaction-safe token fingerprint, handle, and expiry) and edge-trigger the
        // durable `canonical_scrubbed` / `canonical_restored` events — so the shared-credential
        // "Not logged in" scrub is diagnosable from `sessiometer.log` alone, even when no
        // `credential_dead` fires (umbrella #463). Reuses the canonical already read above (no
        // extra keychain read) and the just-resolved `active` for the handle.
        let canonical_liveness = self.note_canonical_liveness(
            canonical.as_ref(),
            canonical_absent,
            active,
            &mut events,
            &mut diagnostics,
        );

        // Poll ONE account this tick — the next entry in the staggered schedule
        // (issue #80): the active account interleaved before each enabled,
        // non-quarantined peer (#366), one account per sub-interval, so each poll lands
        // in its own rate-limit window instead of a single back-to-back burst (most of
        // which the source-scoped usage endpoint `429`s at the CDN edge). The polled
        // account's reading replaces its slot in the carried readings; every
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
        // request, no `Diagnostic::Poll`, readings carried untouched); the schedule
        // cursor already advanced in `next_poll_index`, so the slot is consumed and the
        // account is re-attempted once the window elapses. Transient (`5xx` / network) is
        // scoped the same way — see `note_account_backoff`.
        //
        // The SECOND skip predicate is the out-of-rotation slow-poll (issue #537): a NON-active
        // peer read weekly- or session-exhausted is skipped until its `exhausted_poll_until`
        // window elapses — its usage cannot change until its server-side window resets, so
        // re-polling it every `poll_secs` wastes a request. Same skip mechanics as the back-off
        // (slot consumed, readings carried, no usage request); the ACTIVE account is
        // exempt inside `exhausted_slow_polling` so its swap-away trigger stays observable.
        if let Some(i) = poll_idx
            .filter(|&i| !self.account_backing_off(i) && !self.exhausted_slow_polling(i, active))
        {
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
            // outcome clears both. The ACTIVE account self-caps its back-off tighter and
            // hard-floors `Retry-After` (issue #453), so pass whether `i` is the active
            // account. The returned widened wait rides the diagnostic tick line; the durable
            // back-off ENTER / EXIT events (issue #399) ride `events`.
            this_tick_backoff =
                self.note_account_backoff(i, active == Some(i), &result, &mut events);
            // A single monotonic read for this poll, shared by the velocity interval (#449), the
            // blind-window close (#449), and the `last_good` anchor refresh (#450) so all three
            // reason against the SAME instant.
            let now = self.clock.now();
            // Durable per-account usage VELOCITY (issue #399), normalized to %/min (issue #449): the
            // signed percent delta between this reading and the account's previous one, carried with
            // the `elapsed_secs` interval so the durable log expresses a TIME rate (the gated #451
            // spike / #368 measurement). Both readings AND the prior timestamp must be present — the
            // account's FIRST reading, or a reading after a throttle / failure (which clears the slot
            // + its timestamp below), has nothing to diff — the interval must be positive, and the
            // account must have measurably MOVED (a non-zero rounded delta in either dimension),
            // mirroring `usage_rollup`'s no-op silence so a flat idle account stays quiet.
            if let (Some(prev), Ok(next), Some(prev_at)) = (
                self.state.accounts[i].last_reading.as_ref(),
                result.as_ref(),
                self.state.accounts[i].last_reading_at,
            ) {
                let (session_delta_pct, weekly_delta_pct) = usage_velocity(prev, next);
                let elapsed_secs = now.saturating_duration_since(prev_at).as_secs();
                // Copy the readings out (issues #539 / #614) — `Usage` is `Copy`, so this ends the
                // `prev`/`next` borrows and frees `self` for the `&mut self` EMA update below.
                let (prev_usage, next_usage) = (*prev, *next);
                let (prev_session, next_session) = (prev_usage.session, next_usage.session);
                if (session_delta_pct != 0 || weekly_delta_pct != 0) && elapsed_secs > 0 {
                    events.push(Event::UsageVelocity {
                        account: self.roster[i].account_uuid.clone(),
                        session_delta_pct,
                        weekly_delta_pct,
                        elapsed_secs,
                    });
                }
                // Issue #539: fold THIS interval into the account's session-velocity EMA (the
                // projective trigger's signal), from the SAME `(prev, next, elapsed)` the durable
                // velocity event uses. Folded on EVERY interval, including a flat/zero one (which
                // correctly decays the EMA toward "not climbing"), unlike the event above whose
                // non-zero-delta gate keeps the log quiet for an idle account.
                //
                // Issue #614: UNLESS either endpoint is an implausibly LOW reading — one below the
                // high-water mark of the very window it claims to be in, which usage's within-window
                // monotonicity makes impossible (a stale / cache-lagged response). Such an interval
                // is not a measurement of anything, so it is SKIPPED entirely rather than folded:
                // folding the drop would reset the EMA (`next < prev`) and falsely declare the
                // climbing trend stale, while folding the RECOVERY interval off the stale-low `prev`
                // would blend a spuriously steep rate. Skipping leaves the retained EMA untouched —
                // the last real intervals still describe the account's climb. The mark itself is
                // folded further below, AFTER this, so the comparison is against evidence gathered
                // strictly BEFORE this reading (`next` cannot mask its own implausibility).
                let high_water = self.state.accounts[i].session_high_water;
                let stale_low_interval = swap::is_stale_low(high_water, &prev_usage)
                    || swap::is_stale_low(high_water, &next_usage);
                if !stale_low_interval {
                    self.note_session_velocity(i, prev_session, next_session, elapsed_secs);
                }
            }
            // Durable BLIND-WINDOW close on the ACTIVE account (issue #449, umbrella #363 Path B):
            // the active had gone blind (a `429` / `5xx` cleared `accounts[active].last_reading` so
            // `swap::decide` had no reading to act on) and this poll reads it live again. Measured
            // from the retained pre-blind anchor (`last_good`, #450) — read HERE, before the refresh
            // below, so it is still the OLD anchor — so `duration_secs` is the `blind_elapsed` #452
            // keys off (and the "anchor_age" the #482 recovery SLI names), and `near_limit` tags
            // whether the anchor sat at/over the session trigger (the risk band). `session_at_recovery`
            // is this poll's FRESH session pct (issue #482): paired with the stale anchor
            // (`session_pct`) + its age (`duration_secs`) it reconciles a hypothetical stale-anchor
            // preemptive swap as necessary (still climbing) vs wasted (already reset). The
            // `accounts[i].last_reading.is_none()` was-blind check plus `last_good.is_some()` excludes a
            // first-ever poll (a `None` slot that was never blind); `active == Some(i)` scopes it to
            // the active account the anchor belongs to. Edge-triggered: the None→live transition fires
            // exactly once per blind episode.
            if active == Some(i) && result.is_ok() && self.state.accounts[i].last_reading.is_none()
            {
                if let (Some(anchor), Ok(fresh)) = (self.state.last_good, result.as_ref()) {
                    events.push(Event::BlindWindow {
                        account: self.roster[i].account_uuid.clone(),
                        duration_secs: now.saturating_duration_since(anchor.at).as_secs(),
                        session_pct: to_pct(anchor.session),
                        session_at_recovery: to_pct(fresh.session),
                        near_limit: anchor.session >= self.session_ceiling_base,
                        // Issue #634: the retained #539 velocity in force through the window, so the
                        // REPORT-ONLY blind velocity-projection arm
                        // ([`blind_velocity_projected_armed`], #584/#600) — which fires no swap and
                        // so emits no event of its own — is reconstructable offline from this line.
                        // Genuinely the PRE-BLIND rate: the velocity fold above is gated on a
                        // previous live reading (`accounts[i].last_reading`), which is exactly what this
                        // branch requires to be `None`, so the recovery poll cannot have blended
                        // into the EMA before this read. Same SUSTAINED gate the arm itself applies
                        // — an unwarmed EMA could not have armed it, so no ingredient is logged for
                        // a projection that could not have happened.
                        velocity: self.blind_velocity_ingredients(i),
                    });
                }
            }
            // The UNCENSORED blind-episode pair (issue #583): `blind_enter` on this account's
            // live→blind edge, `blind_exit` on its blind→live one, off a PER-ACCOUNT anchor. Both
            // tails the `blind_window` close above cannot reach — an episode that never recovers, and
            // one the daemon swaps away from — are recorded here instead, so #484's promotion bar
            // reads a distribution that is not censored at exactly its tail. Runs AFTER the
            // `blind_window` block (whose recovery-edge semantics stay untouched) and BEFORE the slot
            // assignment below (it anchors off the PRE-poll reading + its timestamp).
            self.note_blind_episode(i, active, &result, now, &mut events);
            // Issue #613: check this poll against any ARMED landing watch on account `i` — a parked
            // account whose live reading crosses the SLO ceiling within its post-swap window is a LOCAL
            // landing overshoot, surfaced through `status`. Runs on the FRESH `result` (before the slot
            // below is overwritten), off the SAME `(i, active, now)` the blind-episode arm uses.
            self.note_landing_overshoot(i, active, &result, now);
            // Issue #614: fold a LIVE reading into this account's per-window session high-water mark
            // — the plausibility baseline the swap arms measure a suspect low reading against. Only a
            // successful poll updates it: a `429` / `5xx` is blindness, not evidence the window
            // rolled, so a failed poll must leave the mark standing. `fold` releases the mark on its
            // own when `session_resets_at` moves to a new window, so it never pins across windows.
            if let Ok(fresh) = result.as_ref() {
                self.state.accounts[i].session_high_water =
                    swap::SessionHighWater::fold(self.state.accounts[i].session_high_water, fresh);
            }
            self.state.accounts[i].last_reading = result.ok();
            // Track WHEN this reading was observed, beside it on the same `AccountRuntime` (issue
            // #449): `now` on a live reading, cleared on a failed poll — so the velocity interval
            // above spans only two real consecutive readings (never a gap).
            self.state.accounts[i].last_reading_at =
                self.state.accounts[i].last_reading.as_ref().map(|_| now);
            // Fold this poll into the account's out-of-rotation slow-poll window (issue #537):
            // arm/refresh it on a NON-active peer read exhausted (skip its poll until its
            // server-side window resets), or clear it when the account reads viable again / is
            // the active account. Reads the reading just stored above; `now` is the shared
            // monotonic instant (the window deadline), `wall_clock_now_secs()` the wall epoch
            // (the `resets_at` delta). Off the swap-decision path — a pure widening of THIS
            // account's own poll cadence, like the per-account back-off (`note_account_backoff`).
            self.note_exhausted_poll(i, active, now, wall_clock_now_secs(), &mut events);
            // Issue #450: retain the ACTIVE account's last-good reading as a pre-blind
            // anchor, SEPARATE from `last_reading` (which the assignment above clears to
            // `None` on a `429` / `5xx`, so the reactive `swap::decide` path is unchanged).
            // Refreshed on every successful active poll and carried untouched across a
            // throttle / failure, so the bounded-blindness preemptive swap (#452) can reason
            // about how near the band the active account was when it went blind. Reset to
            // `None` on swap-away (`record_swap` / `adopt_manual_swap` / the reconcile paths),
            // so the anchor always belongs to the current active account.
            if active == Some(i) {
                if let Some(reading) = self.state.accounts[i].last_reading {
                    self.state.last_good = Some(LastGood {
                        session: reading.session,
                        weekly: reading.weekly,
                        at: now,
                    });
                }
            }
            // Populate the DISPLAY expiry clock (issue #141) from the SAME credential this
            // poll used — kept DISTINCT from the refresh-sourced `access_expires_at` the
            // rollup reads, so `status --json` surfaces the access-token expiry with
            // `[refresh]` off without firing a false-🟠 Stale for an idle lapsed token (the
            // rollup's positive-liveness consumption of this clock lands under #137).
            let poll_expiry = self
                .read_poll_expires_at(&self.roster[i], active == Some(i))
                .await;
            self.state.accounts[i].health.poll_expires_at = poll_expiry;
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
        // Issue #482 (umbrella #363 Path B): record the #452 gate-premise SLI — at the moment the
        // bounded-blindness preemptive swap gate (ADR-0017) turns eligible for the blind active
        // account, whether a viable swap target exists. Evaluated on the SAME carried readings the
        // decision below acts on, BEFORE `decide_action` may mutate state, so it captures the gate's
        // eligibility entering the decision. Edge-triggered once per blind episode; a no-op on a
        // live / dead / not-yet-eligible active. Instruments the premise before #452's swap is built.
        self.note_blind_gate_eligibility(active, &readings, at, &mut events);
        // Issue #467: a SCRUBBED / empty shared canonical (Claude Code's first-`invalid_grant` scrub,
        // ADR-0018) locks out every local `claude` session. If a viable target exists, autonomously
        // adopt its token into the emptied canonical — the narrow ADR-0007 decision-4 carve-out —
        // healing the fleet on the next request with no operator action. No viable target (or the
        // churn bound already hit) falls THROUGH to the normal decision, so the genuinely-all-dead
        // `active_dead_no_target` / surfaced scrub signal stands — never a silent adopt churn.
        let action = if matches!(canonical_liveness, CanonicalLiveness::Scrubbed) {
            match self
                .recover_scrubbed_canonical(active, &readings, at, &mut events)
                .await
            {
                Some(recovered) => recovered,
                None => self.decide_action(at, active, &readings, &mut events).await,
            }
        } else {
            self.decide_action(at, active, &readings, &mut events).await
        };
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
        // The matching LEAVE edge for the active-dead-no-target strand (issue #405), mirroring
        // the `all_exhausted` clear above: any cycle that is NOT the strand clears the guard, and
        // a still-set guard on that cycle means we are LEAVING the strand — push the
        // `active_dead_no_target_cleared` marker BEFORE the reset, so a re-entry signals afresh and
        // a stale strand reading is told from a current one.
        if !matches!(action, TickAction::ActiveDeadNoTarget) {
            if self.state.signaled_active_dead_no_target {
                diagnostics.push(Diagnostic::ActiveDeadNoTargetCleared);
            }
            self.state.signaled_active_dead_no_target = false;
        }
        // Issue #540: refresh the near-limit poll-coverage verdict from the POST-decision state
        // (`self.state.active` is the post-swap active — the same index the snapshot below reads),
        // so `next_subinterval` tightens the wait against the CURRENT active's freshest reading. Its
        // `false → true` transition is the band-entry edge — emit the durable `NearLimitPollCoverage`
        // ONCE here (not every held near-limit tick), mirroring the edge-triggered `ExhaustedSlowPoll`
        // (#537) / `UsageBackoff` (#399) idiom. The `true → false` band-exit clears the latch
        // silently: the band ends either at a swap (which emits its own `Swap`) or at a below-band /
        // blind reading, so no paired CLEARED event is needed to bracket the span.
        let near_limit_engaged = self.near_limit_fast_poll_engaged();
        if near_limit_engaged && !self.state.near_limit_fast_poll {
            if let Some(active_idx) = self.state.active {
                events.push(Event::NearLimitPollCoverage {
                    account: self.roster[active_idx].account_uuid.clone(),
                    sub_interval_secs: self.near_limit_poll_secs,
                });
            }
        }
        self.state.near_limit_fast_poll = near_limit_engaged;
        // The rate-limit / transient back-off is PER-ACCOUNT now (issue #293): it is
        // applied by skipping the throttled account's own poll above (`account_backing_off`
        // / `note_account_backoff`), NOT by widening the WHOLE loop's wait — so the active
        // account and every other account keep polling on their normal cadence. `next_wait`
        // therefore stays `None` in the normal tick path (the near-limit tightening rides
        // `next_subinterval` via the cached `near_limit_fast_poll` above, not a whole-loop wait);
        // only the locked-keychain tick (#13, `locked_tick`) still returns a whole-loop wait.
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
            if self.roster[i].enabled && !self.state.accounts[i].health.quarantined {
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
        self.state.accounts[i].polled_once = true;
        if !self.state.warmed_up
            && self
                .state
                .poll_schedule
                .iter()
                .all(|&j| self.state.accounts[j].polled_once)
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
                    || (self.roster[i].enabled && !self.state.accounts[i].health.quarantined)
                {
                    self.state.accounts[i].last_reading
                } else {
                    None
                }
            })
            .collect()
    }

    /// Check the account just polled against any ARMED landing watch (issue #613) — the runtime
    /// half of the local landing-overshoot signal. When account `i` was parked by a `reason=session`
    /// swap ([`parked_landing`](AccountRuntime::parked_landing) is `Some`), a live reading that reaches
    /// the SLO ceiling ([`landing::is_overshoot`]) within the [`landing::LANDING_WINDOW`] is a LOCAL
    /// landing overshoot: the post-swap committed tail carried the parked account over the ceiling
    /// after the swap redirected only NEW requests — the #595 breach, caught LIVE here rather than
    /// only in a later offline `reliability` run. It retains the breach in
    /// [`last_landing_overshoot`](DecisionState::last_landing_overshoot) (projected onto `status` by
    /// [`recent_landing_overshoot_view`]) and DISARMS the watch, so it fires once per parked episode.
    ///
    /// Disarms with NO overshoot when the account has gone ACTIVE again (the window is moot — it is no
    /// longer a parked tail) or the [`landing::LANDING_WINDOW`] has elapsed (a later climb is a fresh
    /// session cycle, not this swap's tail). A failed poll (`Err`) leaves the watch armed — blindness
    /// is not evidence of a safe landing, so a later live reading can still catch the overshoot inside
    /// the window. MUST be called on the FRESH `result` BEFORE the caller overwrites `accounts[i].last_reading`.
    /// `now` is the tick's monotonic clock, the SAME [`Instant`] the swap arms the watch with.
    fn note_landing_overshoot(
        &mut self,
        i: usize,
        active: Option<usize>,
        result: &Result<Usage>,
        now: Instant,
    ) {
        let Some(parked) = self.state.accounts[i].parked_landing else {
            return;
        };
        // Re-activated (no longer a parked tail) or the landing window has elapsed — disarm, no breach.
        if active == Some(i)
            || now.saturating_duration_since(parked.armed_at) >= landing::LANDING_WINDOW
        {
            self.state.accounts[i].parked_landing = None;
            return;
        }
        // A live reading at/over the SLO ceiling is a landing overshoot: retain it and disarm (once
        // per parked episode). A failed poll leaves the watch armed for a later reading in the window.
        if let Ok(fresh) = result {
            let landing_pct = to_pct(fresh.session);
            if landing::is_overshoot(landing_pct) {
                let from_label = self.roster[i].label.clone();
                self.state.last_landing_overshoot = Some(LandingOvershootRecord {
                    from_label,
                    decision_pct: parked.decision_pct,
                    landing_pct,
                    at: now,
                });
                self.state.accounts[i].parked_landing = None;
            }
        }
    }

    /// Record the UNCENSORED blind-episode edges for the account just polled (issue #583, umbrella
    /// #363 Path B): [`Event::BlindEnter`] on its live→blind transition and [`Event::BlindExit`] on
    /// its blind→live one, measured from a PER-ACCOUNT anchor ([`blind_anchor`](AccountRuntime::blind_anchor)).
    ///
    /// This exists because [`Event::BlindWindow`] (#449) is censored at BOTH tails of the very
    /// distribution #484's promotion bar reads to ratify [`BLIND_GATE_SECS`] / [`BLIND_GATE_RISK_BAND`]:
    ///
    /// - It fires only on the `None → live` RECOVERY edge, so an account that goes dark and never
    ///   comes back emits NOTHING. Recording on ENTRY instead makes the episode durable the moment it
    ///   starts — the worst episodes stop being the silent ones.
    /// - It is guarded by `active == Some(i)` and built from `last_good`, the ACTIVE-only anchor every
    ///   swap-away site drops. Anchoring per-account — in state no swap path touches — records the
    ///   episode whether or not the daemon stays on the account, and tags the swap-away case
    ///   (`swapped_away`) that was previously unobservable. This matters most in combination with the
    ///   swap-away-on-blindness fix (issue #582): swapping away is precisely what made an episode
    ///   invisible, so that fix would otherwise INCREASE the censoring.
    ///
    /// `blind_window` is left byte-for-byte unchanged and keeps its recovery-edge semantics — it is
    /// the retrospective duration histogram for SLO reporting, which is a fine purpose; it was merely
    /// assigned the wrong one (detection).
    ///
    /// The entry edge requires a PRIOR LIVE reading (`accounts[i].last_reading` still `Some` here, before the
    /// caller's assignment clears it): an account whose first-ever poll fails, or one already blind at
    /// startup, has no baseline to difference against, so no anchor is taken and no episode is claimed
    /// — the same never-fabricate-an-anchor reasoning as `blind_window`'s `last_good.is_some()` guard.
    /// Both edges are level-safe: a held blind tick and an ordinary live poll each match no edge and
    /// emit nothing, so the log carries exactly two lines per witnessed episode.
    ///
    /// MUST be called BEFORE the caller writes `accounts[i].last_reading` / `accounts[i].last_reading_at` for this poll
    /// — the entry anchor is read out of the PRE-poll slots. `now` is the tick's monotonic clock, the
    /// SAME [`Instant`] the #450 anchor and the swap cooldown use.
    fn note_blind_episode(
        &mut self,
        i: usize,
        active: Option<usize>,
        result: &Result<Usage>,
        now: Instant,
        events: &mut Vec<Event>,
    ) {
        match (self.state.accounts[i].blind_anchor, result) {
            // ENTRY edge — the account had a live reading and this poll failed. Anchor its last
            // reading (BOTH windows) and open the episode.
            (None, Err(_)) => {
                let (Some(prev), Some(prev_at)) = (
                    self.state.accounts[i].last_reading,
                    self.state.accounts[i].last_reading_at,
                ) else {
                    return; // No prior live reading → no baseline → claim no episode.
                };
                let was_active = active == Some(i);
                // The risk-band tag is a property of the ANCHOR, fixed for the whole episode. The
                // entry line has to carry it regardless (no exit exists yet), so caching it makes
                // both lines of one episode agree BY CONSTRUCTION rather than by two derivations
                // that happen to coincide. Keys off the BASE (un-jittered) trigger, matching
                // `blind_window`'s tag so the two families filter alike.
                let near_limit = prev.session >= self.session_ceiling_base;
                self.state.accounts[i].blind_anchor = Some(BlindAnchor {
                    session: prev.session,
                    weekly: prev.weekly,
                    at: prev_at,
                    was_active,
                    near_limit,
                });
                events.push(Event::BlindEnter {
                    account: self.roster[i].account_uuid.clone(),
                    session_pct: to_pct(prev.session),
                    weekly_pct: to_pct(prev.weekly),
                    was_active,
                    near_limit,
                });
            }
            // EXIT edge — an episode was open and this poll read live. Close it with the anchor, the
            // fresh reading in BOTH windows (the burn the log line derives), and the swap-away tag.
            (Some(anchor), Ok(fresh)) => {
                events.push(Event::BlindExit {
                    account: self.roster[i].account_uuid.clone(),
                    duration_secs: now.saturating_duration_since(anchor.at).as_secs(),
                    session_pct: to_pct(anchor.session),
                    session_at_recovery: to_pct(fresh.session),
                    weekly_pct: to_pct(anchor.weekly),
                    weekly_at_recovery: to_pct(fresh.weekly),
                    was_active: anchor.was_active,
                    // The tail `blind_window` cannot see: active when it went blind, not active now.
                    swapped_away: anchor.was_active && active != Some(i),
                    near_limit: anchor.near_limit,
                });
                self.state.accounts[i].blind_anchor = None;
            }
            // Held blind (episode already open) or an ordinary live poll — no edge, stay silent.
            (Some(_), Err(_)) | (None, Ok(_)) => {}
        }
    }

    /// Record the #452 gate-premise SLI (issue #482): at the moment the bounded-blindness preemptive
    /// swap gate (ADR-0017) turns ELIGIBLE for the active account, emit whether a viable swap target
    /// exists — the no-viable-target-at-gate-fire FALSIFIER for the ADR's cost-asymmetry premise. This
    /// instruments the gate premise BEFORE #452's swap path is built, so #451/#484 can finalise the
    /// interim `T` / `risk_band` against production rather than a replay.
    ///
    /// Eligibility is the gate's first two ADR-0017 conditions on the retained pre-blind anchor
    /// ([`last_good`](DecisionState::last_good), #450): the active account has been blind (its live
    /// reading cleared, `accounts[active].last_reading.is_none()`) past the interim [`BLIND_GATE_SECS`], and
    /// the anchor sat at/over the interim [`BLIND_GATE_RISK_BAND`]. The gate's THIRD condition — a
    /// viable target — is the value the SLI records, selected exactly as #452's gate would via the
    /// shared [`pick_target`] with the BASE (un-jittered) triggers (a standing measurement, not a
    /// per-cycle swap draw). Gated on warm-up (#80): before the first full cycle the carried readings
    /// are partial, so an unpolled peer would read as a FALSE no-viable-target; by `T` = 300 s warm-up
    /// is long done, so this only guards the degenerate early window.
    ///
    /// Edge-triggered exactly ONCE per blind episode (the gate would swap once, ending the episode)
    /// via [`blind_gate_signaled`](DecisionState::blind_gate_signaled). The latch is CLEARED here on
    /// both episode-end edges — the active account regaining a live reading (recovery), and the anchor
    /// being absent (swap-away / active-loss dropped `last_good`, or no active) — so no `last_good`
    /// reset site has to touch it. `at` is the tick's monotonic clock, the SAME [`Instant`] the anchor
    /// and swap cooldown use.
    fn note_blind_gate_eligibility(
        &mut self,
        active: Option<usize>,
        readings: &[Option<Usage>],
        at: Instant,
        events: &mut Vec<Event>,
    ) {
        let Some(active_idx) = active else {
            // No active resolved → no episode; the anchor is already dropped on active-loss. Clear.
            self.state.blind_gate_signaled = false;
            return;
        };
        // A live reading means the active account is NOT blind (recovered, or never blind) → the
        // episode is over: clear the latch so the NEXT episode signals afresh.
        if self.state.accounts[active_idx].last_reading.is_some() {
            self.state.blind_gate_signaled = false;
            return;
        }
        // A QUARANTINED (dead, #42) blind active belongs to the `emergency_swap` path, NOT the #452
        // preemptive gate — ADR-0017 keeps the two separate (bounded blindness is a healthy 429'd
        // active, not a dead one). Exclude it so the premise SLI measures only the #452 path and does
        // not inflate the gate-eligible count with cases the emergency path would handle instead.
        if self.state.accounts[active_idx].health.quarantined {
            return;
        }
        // No anchor → no episode (swap-away / active-loss dropped `last_good`): the latch's other
        // clear edge, so both ends of an episode reset it in this one place.
        let Some(anchor) = self.state.last_good else {
            self.state.blind_gate_signaled = false;
            return;
        };
        // The gate's first two ADR-0017 conditions on the (constant-through-a-blind-episode) anchor,
        // via the shared [`blind_gate_armed`] predicate (the same test #479's `blind_active_view`
        // projects): NOT armed — not yet past the interim T, or the anchor below the risk band → not
        // yet eligible; leave the latch untouched (it is already clear pre-first-emit).
        //
        // Issue #619: on the anchor's PLAUSIBLE session, exactly as `blind_swap` decides — so this
        // ratification SLI measures the premise of the swaps the gate actually fires (a stale-low
        // pre-blind reading no longer hides an episode the corrected swap acts on). The emitted
        // `session_pct` below stays the RAW anchor (the measurement of what was last observed).
        let blind_elapsed = at.saturating_duration_since(anchor.at);
        let gate_session = swap::plausible_anchor_session(
            self.state.accounts[active_idx].session_high_water,
            anchor.session,
        );
        if !blind_gate_armed(blind_elapsed.as_secs(), gate_session) {
            return;
        }
        // Partial-reading guard (#80 warm-up): don't measure viable-target availability off an
        // incomplete first cycle, or an unpolled peer fabricates a no-viable-target falsifier.
        if !self.state.warmed_up {
            return;
        }
        // Edge-trigger: already signalled this episode → the gate would have swapped once; be silent.
        if self.state.blind_gate_signaled {
            return;
        }
        // The gate's THIRD condition IS the SLI: a viable swap target — a peer under
        // `target_max_session_usage` (ADR-0013) — chosen exactly as #452's gate would, via the shared
        // `pick_target` with the BASE (un-jittered) triggers. DELIBERATELY the velocity-blind,
        // un-jittered projection (issue #612): this asks only WHETHER a viable peer exists, and the
        // #612 axes re-order the viable set without changing its MEMBERSHIP (they enter the
        // comparator, never the filters), so they cannot move `.is_some()`. Threading the seed here
        // would add churn to a measurement that is invariant under it.
        let viable_target = pick_target(
            active_idx,
            readings,
            &self.enabled_mask(),
            Some(self.target_max_session_usage),
            self.session_ceiling_base,
            self.weekly_rotation_line(),
        )
        .is_some();
        events.push(Event::BlindGateEligible {
            account: self.roster[active_idx].account_uuid.clone(),
            viable_target,
            blind_secs: blind_elapsed.as_secs(),
            session_pct: to_pct(anchor.session),
        });
        self.state.blind_gate_signaled = true;
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
                    || (self.roster[i].enabled && !self.state.accounts[i].health.quarantined)
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

    /// The per-roster-index enabled (in-rotation, issue #36) mask `pick_target`
    /// consumes — a disabled account is never a viable swap target. Rebuilt per call
    /// (the roster is small); shared by the normal and the #42 emergency swap path.
    fn enabled_mask(&self) -> Vec<bool> {
        self.roster.iter().map(|account| account.enabled).collect()
    }

    /// The deterministic (un-jittered) WEEKLY line the daemon makes ROTATION decisions against
    /// (issue #607): [`Self::weekly_ceiling_base`] less `swap::WEEKLY_TAIL_MARGIN`.
    ///
    /// Every path that decides whether an account may be swapped TO, stays in rotation, or is
    /// previewed as the next swap MUST key off this, not the raw ceiling. The reason is
    /// `pick_target`'s anti-thrash invariant — "the acquire predicate is at least as strict as the
    /// negation of the release predicate on BOTH dimensions". Since #607 the weekly RELEASE point is
    /// `ceiling − WEEKLY_TAIL_MARGIN`, so an acquire gate left at the raw ceiling admits targets in
    /// the `[ceiling − margin, ceiling)` band that re-trip `swap::decide`'s weekly dimension on the
    /// very next tick — a ping-pong bounded only by cooldown. The session dimension closes the same
    /// gap with the `target_max_session_usage` reserve (default 80, far below its ceiling); the
    /// weekly dimension has no such reserve, so it closes it by moving both predicates together.
    ///
    /// The jittered live path derives its own line from the per-cycle draw
    /// (`swap::weekly_effective_ceiling(weekly_ceiling)` in [`Self::decide_action`]); this is its
    /// deterministic counterpart for the display / non-drawing paths, exactly as
    /// [`Self::weekly_ceiling_base`] is the deterministic counterpart of the jittered draw.
    ///
    /// DELIBERATELY NOT used by the two emergency paths ([`Self::emergency_swap`],
    /// [`Self::recover_scrubbed_canonical`]): those already drop the session reserve for liveness,
    /// and a dead or scrubbed credential is worse than a target that must swap again shortly, so
    /// they keep the raw ceiling and admit the widest viable set.
    fn weekly_rotation_line(&self) -> f64 {
        swap::weekly_effective_ceiling(self.weekly_ceiling_base)
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
        if self.state.accounts[active_idx].health.quarantined {
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
        // The active account's own reading is unavailable this cycle (transient / a 401
        // below the dead threshold / a 429|5xx blind window / unreadable). Before skipping,
        // consult the #452 bounded-blindness preemptive gate (ADR-0017): a HEALTHY (non-dead;
        // the dead case took the emergency path above) active that has been blind too long,
        // with a retained pre-blind anchor already near the band and a viable target, swaps
        // away NOW rather than burning to exhaustion behind the blindness. Not eligible →
        // the historical skip; never a swap on missing data (the gate keys off the anchor).
        let Some(active_usage) = readings[active_idx] else {
            return self.blind_swap(at, active_idx, readings, events).await;
        };
        // Issue #614: decide on the active account's PLAUSIBLE session usage, not the raw response.
        // A `/oauth/usage` reading that fell BELOW the high-water mark of its own (unchanged) session
        // window is stale / cache-lagged — usage cannot fall within a window — and taking it at face
        // value would cancel an otherwise-due swap while the account is really higher. Raising it back
        // to the retained mark (a genuine LOWER bound on the truth, never a synthesized number) is the
        // conservative reading under #597's asymmetry: an early swap is the cheap error, an overshoot
        // the expensive one. A plausible reading passes through UNCHANGED, so this is a no-op on every
        // normal tick. Deliberately scoped to this local: `readings` — and so `pick_target`, the
        // all-exhausted relief hint, and the status snapshot — keep the VERBATIM reading, and
        // `last_reading` is never written back over. The projection peer applies the same correction
        // to the same account through the same helper, so both arms decide on one value (ADR-0022's
        // one predicate, two estimators).
        let active_usage = self.plausible_active_usage(active_idx, active_usage);
        // The active is READING again, so any #582 blind episode is over: re-arm the
        // circuit-breaker's edge-trigger guards (the LEAVE edge, mirroring `signaled_all_exhausted`)
        // so a LATER episode reports its holds afresh instead of being silenced by a stale guard.
        // The swap paths re-arm them too, via `record_swap`; this covers recovery WITHOUT a swap.
        self.state.signaled_retry_after_reserve = false;
        self.state.signaled_retry_after_walk = false;
        // Draw this cycle's swap-away CEILINGS — session and weekly (issues #38, #41; #597, #607,
        // #609): each jittered + clamped to 50..=99 percent, then to a fraction. Since #597
        // (session) and #607 (weekly), BOTH are the same KIND of value: a settled CEILING — the line
        // the account must not cross — NOT a fire-at trigger. Each dimension's fire threshold is
        // derived BACKWARD from its own ceiling below, through its own independently-calibrated tail
        // margin (`swap::TAIL_MARGIN` 6 pp / `swap::WEEKLY_TAIL_MARGIN` 1 pp — the weekly tail is a
        // far smaller fraction of the 7-day window than the session tail is of the ~5 h one; see
        // `swap::WEEKLY_TAIL_MARGIN`'s provenance). Because a ceiling is a not-cross line, per-cycle
        // jitter is DOWNWARD-ONLY on BOTH (#609 established this for session; #607 extends it to
        // weekly for the same reason): a configured jitter may only ever pull a ceiling LOWER (more
        // margin), never above the operator-set not-cross value. The session and weekly dimensions
        // stay independent — swap when EITHER reaches its own fire point; below BOTH → hold. Both are
        // drawn every cycle (a fixed strategy consumes no RNG, and `draw_downward` consumes the same
        // per-mode RNG count as `draw`), keeping the per-cycle draw order deterministic.
        let session_ceiling = self.session_ceiling_strategy.draw_downward(
            &mut self.rng,
            SESSION_CEILING_PCT_LO,
            SESSION_CEILING_PCT_HI,
        ) / 100.0;
        let weekly_ceiling = self.weekly_ceiling_strategy.draw_downward(
            &mut self.rng,
            WEEKLY_CEILING_PCT_LO,
            WEEKLY_CEILING_PCT_HI,
        ) / 100.0;
        // #597/#609 ceiling derivation: the reactive arm fires BACKWARD from the effective ceiling
        // (ceiling − tail margin) so the outgoing account lands below the ceiling even after its
        // post-swap committed tail (#595). A `velocity × poll_gap` term pulls the fire earlier so the
        // account is AT the effective ceiling by the time the swap executes — one re-observation gap
        // later, having climbed at the retained EMA rate. The velocity is gated by the same
        // `>= MIN_VELOCITY_SAMPLES` criterion as the projection peer; here an unwarmed EMA reads 0
        // (the peer instead holds outright), so an idle account fires exactly at the effective
        // ceiling — never early. The two arms cover DIFFERENT unseen windows (this one the
        // re-observation gap, the projection peer the velocity horizon `H`); the composed swap fires
        // at `eff − v·max(poll_gap, H)`, the max-window coverage in `swap::reactive_session_threshold`.
        let effective_ceiling = swap::effective_ceiling(session_ceiling);
        let velocity = self.state.accounts[active_idx]
            .sustained_session_velocity()
            .map_or(0.0, |v| v.rate);
        // The reactive re-observation gap: how long the active account climbs UNSEEN between the
        // daemon's successive observations of it. `swap::reactive_poll_gap_secs` looks ahead over the
        // LARGER of the measured p90 gap (`swap::REACTIVE_REOBSERVATION_GAP_SECS`, 313 s) and the
        // cadence-scaled `2 × near_limit_poll_secs` — an unconditional widening of the pre-#609
        // theoretical round-trip, so the account lands under the ceiling even across the gap tail (#609,
        // implementing ADR-0023 § Alternatives 6; the floor / `max` rationale is on
        // `reactive_poll_gap_secs`). When fast-poll is DISABLED (`near_limit_poll_secs == 0`) there is
        // no tight near-limit gap to look ahead over, so `poll_gap` is 0 and the reactive threshold
        // collapses to the bare effective ceiling — the #539 projection is then the sole velocity-aware
        // estimator (#584; the pre-#597 division of labour, now landing-margined).
        let poll_gap = swap::reactive_poll_gap_secs(self.near_limit_poll_secs);
        let session_threshold =
            swap::reactive_session_threshold(effective_ceiling, velocity, poll_gap);
        // #607 weekly ceiling derivation: the weekly fire point is likewise derived BACKWARD from the
        // weekly ceiling — `weekly_ceiling − WEEKLY_TAIL_MARGIN` — so the parked account's weekly
        // usage lands BELOW its ceiling after the same post-swap committed tail (#595/#596 measured
        // the tail on the session axis; it bills the weekly window too, which is why weekly co-moved
        // in 8/13 of the #596 tail episodes). No velocity term: the reactive arm's `velocity ×
        // poll_gap` lookahead is the SESSION arm's gap-staleness correction (#609), and the weekly
        // dimension has no projection peer — `velocity_swap` projects session only — so weekly has a
        // single fire arm and its fire point is the bare effective weekly ceiling. The two dimensions
        // stay independent (#41): each derives from its OWN ceiling through its OWN margin, and
        // neither subsumes the other.
        //
        // ONE derived weekly line is used for BOTH the release (fire) and acquire (target-viability)
        // predicates below. That is load-bearing, not incidental: `pick_target`'s anti-thrash
        // invariant is that "the acquire predicate is at least as strict as the negation of the
        // release predicate on BOTH dimensions". Lowering only the release side would open a
        // `[weekly_threshold, weekly_ceiling)` band in which an account is simultaneously
        // fire-eligible and target-eligible — swap TO it and it re-trips `decide`'s weekly dimension
        // next cycle, the exact ping-pong that exclusion exists to prevent. The session dimension
        // closes the same gap with the `target_max_session_usage` reserve (#398), which has no weekly
        // counterpart, so weekly closes it by moving both predicates together.
        let weekly_threshold = swap::weekly_effective_ceiling(weekly_ceiling);
        if swap::decide(&active_usage, session_threshold, weekly_threshold) == SwapDecision::Hold {
            // Reactive says HOLD — the observed reading is below both fire points (the #597
            // velocity-derived session threshold + the #607 effective weekly ceiling). Before
            // holding, consult the #539 velocity-projection peer (ADR-0017): if the active
            // account's PROJECTED session usage (last + retained velocity × horizon) crosses the
            // effective ceiling within the horizon, swap it away NOW — ahead of the observed
            // reading tripping the reactive
            // threshold — closing the observed reactive overshoot (#363) that #452's blind-window
            // path does not address. Not eligible (kill-switch off, no sustained velocity, below the
            // guard, projection short, or no viable target) → the historical Held. Every branch below
            // this projection call is unchanged; only the reactive threshold + this call now derive
            // from the ceiling (#597).
            return self
                .velocity_swap(
                    at,
                    active_idx,
                    session_ceiling,
                    weekly_threshold,
                    readings,
                    events,
                )
                .await;
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
        // from oscillating (it excludes each as a target of the other). Drawn
        // UNCONDITIONALLY — even when the #611 bypass below skips the wait — so the
        // per-cycle RNG draw order is identical whether or not the spike bypass fires.
        let cooldown = Duration::from_secs_f64(self.cooldown_strategy.draw(
            &mut self.rng,
            COOLDOWN_SECS_LO,
            COOLDOWN_SECS_HI,
        ));
        // Emergency-tier reactive bypass (issue #611): a LIVE (non-quarantined) active whose OBSERVED
        // session reading has ALREADY reached the RAW ceiling (`session_ceiling` — the not-cross line
        // itself, NOT the `effective_ceiling` / `session_threshold` the normal reactive fire point is
        // derived BACKWARD from) is past the ceiling NOW. With cross-machine coordination out of scope,
        // a shared account can be burned from a second machine BETWEEN our observations, so this is the
        // co-consumption spike that velocity-spike detection is the designated mitigation for — and
        // honoring the cooldown here would suppress the reactive arm THROUGH the breach, up to the 1 h
        // cooldown max, against #597's own asymmetry (overshoot is the expensive error). So bypass the
        // WAIT and swap, mirroring the dead-active `emergency_swap` cooldown bypass (until now the ONLY
        // one). STRICTLY above the normal fire point — `session_threshold ≤ effective_ceiling =
        // session_ceiling − TAIL_MARGIN < session_ceiling` (TAIL_MARGIN > 0) — so an ordinary reactive
        // swap fired BELOW the raw ceiling still defers within cooldown: the bypass never defeats
        // cooldown's rate-limiting purpose, nor the #272 sub-floor-jitter guard, for ordinary swaps.
        // Only the WAIT is bypassed — target selection below still HONORS the reserve + session gate
        // (this is a live account with somewhere viable to land, NOT the dead-active
        // liveness-beats-all path), so a spike with no viable target still holds (`NoViableTarget`),
        // never thrashes onto a saturated peer. Interaction (issue #64 manual-hold): a manual `use`
        // that armed the cooldown on an active ALREADY at/over the raw ceiling is likewise reverted —
        // protection at the not-cross line overrides the hold, exactly as it already does for a
        // manually-held DEAD active.
        let within_cooldown = self
            .state
            .last_swap
            .as_ref()
            .is_some_and(|last| at.saturating_duration_since(last.at) < cooldown);
        // `>=` (not `>`) so a reading landing EXACTLY on the raw ceiling still bypasses — the ceiling
        // is the not-cross line, so being AT it already warrants escape.
        let at_raw_ceiling = active_usage.session >= session_ceiling;
        if within_cooldown && !at_raw_ceiling {
            return TickAction::SkippedCooldown;
        }
        // Pick the viable target whose weekly quota resets soonest (issue #37). A
        // disabled (parked) account is not viable (issue #36), and a weekly-exhausted
        // account is not viable (#11) — so when every ENABLED other account is
        // weekly-exhausted this returns `None`. A disabled account, even with weekly
        // headroom, never counts, so it cannot hold the daemon out of the
        // all-exhausted terminal state (#11).
        let Some(target_idx) = pick_target_ranked(
            active_idx,
            readings,
            &self.enabled_mask(),
            Some(self.target_max_session_usage),
            session_ceiling,
            // The #607 effective weekly ceiling, NOT the raw one: the acquire predicate must stay at
            // least as strict as the negation of the release predicate on the weekly dimension, or a
            // target in `[weekly_threshold, weekly_ceiling)` re-trips `decide` next cycle and thrashes.
            weekly_threshold,
            // Enhanced target selection (issue #612): prefer a lower-velocity peer on a reset tie,
            // then break a remaining tie by the per-daemon jitter — dispersing the cross-machine
            // co-selection herd.
            self.selection_tiebreak(),
        ) else {
            // No viable target — every other account is weekly-exhausted, session-
            // saturated (over the always-on session gate), or over the default-on floor.
            // The all-exhausted TERMINAL state (issue #11): HOLD, do NOT swap (swapping
            // among exhausted accounts only thrashes), and emit ONE edge-triggered
            // signal naming the least-bad account and WHY relief is blocked
            // (`cause=session|weekly`), so the operator knows when relief arrives. The
            // hint keys off the SOONEST spare-return across BOTH windows (issue #665) —
            // whichever spare comes back first, named by the dimension gating it: a
            // session reset (issue #398) and a weekly one (#11) are compared on the same
            // footing, since a window's length is not its time remaining. The signal is
            // edge-triggered: emit only on ENTERING the state, so the payload is computed
            // once per episode, not every poll while it holds.
            if !self.state.signaled_all_exhausted {
                let session_block_line = session_ceiling.min(self.target_max_session_usage);
                let (cause, hold_idx, resets_at) = all_exhausted_relief(
                    active_idx,
                    readings,
                    &self.enabled_mask(),
                    session_block_line,
                    // The same #607 effective weekly ceiling the viability filter just used, so the
                    // relief hint EXPLAINS the verdict it accompanies rather than reasoning against a
                    // different weekly line.
                    weekly_threshold,
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
                // session-first when BOTH dimensions are over their (this-cycle) fire
                // points — the session dimension against the #597 velocity-derived
                // `session_threshold` (NOT the raw ceiling), so a velocity-early session
                // swap is attributed to Session, never mis-logged as Weekly. `session_pct`
                // is the #614 plausibility-corrected reading the swap DECIDED on (raised to
                // the retained high-water mark on a stale-low tick), rounded via the same
                // `to_pct` every swap line uses — so on a stale-low tick it is the value the
                // daemon acted on, which differs from the raw reading `status` still shows
                // (that surface stays verbatim by design — see `plausible_active_usage`).
                let reason = if active_usage.session >= session_threshold {
                    SwapReason::Session
                } else {
                    SwapReason::Weekly
                };
                events.push(Event::Swap {
                    from: self.roster[active_idx].label.clone(),
                    to: self.roster[target_idx].label.clone(),
                    reason,
                    session_pct: to_pct(active_usage.session),
                    // Reactive session/weekly swap: it fired on the OBSERVED reading, not a
                    // projection, so `session_pct` fully describes the decision (issue #634).
                    projection: None,
                });
                // Arm the runtime landing watch (issue #613) on the account just PARKED, but only for a
                // `reason=session` swap — the offline #595 landing SLI this mirrors measures
                // reason=session landings only (a weekly swap fires below its session trigger, so its
                // parked account is not near the session ceiling). Its post-swap committed tail keeps
                // billing after this swap redirects only NEW requests, so `note_landing_overshoot`
                // watches its subsequent polls for a climb to the SLO ceiling within the landing window.
                // `record_swap` above already cleared `accounts[target_idx].parked_landing` (the incoming can't be
                // a parked-landing subject); `active_idx != target_idx`, so this arm does not collide.
                if reason == SwapReason::Session {
                    self.state.accounts[active_idx].parked_landing = Some(ParkedLanding {
                        armed_at: at,
                        decision_pct: to_pct(active_usage.session),
                    });
                }
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
        // Issue #613: the account going ACTIVE cannot be a parked-landing subject — disarm any landing
        // watch on it here (the shared swap path), so a prior park's stale window can't fire against an
        // account that is now active again. The complementary arm of the OUTGOING account (reason=session
        // only) is set by the reactive-swap caller right after this returns.
        self.state.accounts[target_idx].parked_landing = None;
        // Issue #450: the swapped-TO account has no pre-blind anchor yet — drop the
        // departing active's `last_good` so a stale foreign anchor cannot outlive the
        // swap. Load-bearing for the bounded-blindness path (#452): its OWN swap lands
        // here, so without this the anchor would still describe the account just left,
        // and the path could re-fire on it once the cooldown lapses.
        self.state.last_good = None;
        self.state.last_swap = Some(LastSwap { at });
        // Issue #479: any swap supersedes an earlier #452 preemptive-swap narration — the active
        // account is changing, so the "swapped off X → Y" notice is now stale. Clear it here (the
        // shared swap path); a blind-preempt swap re-sets its OWN notice right after this call, and a
        // differently-targeted swap self-invalidates the notice at projection time anyway.
        self.state.last_blind_preempt_swap = None;
        // Issue #582: a swap ENDS the blind episode the circuit-breaker's holds were reported
        // against (the active account is changing), so re-arm both edge-trigger guards here — the
        // shared swap path — and a hold taken against the NEW active reports afresh rather than
        // being swallowed as a duplicate of the old one's.
        self.state.signaled_retry_after_reserve = false;
        self.state.signaled_retry_after_walk = false;
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

    /// Run one out-of-band [`swap::adopt_target`] recovery (issue #467), serialized by the
    /// single-writer swap lock (#64) when configured — the adopt counterpart of
    /// [`locked_swap`](Self::locked_swap). UNLIKE a swap it does NOT re-stash an outgoing account
    /// (the canonical is scrubbed / empty — there is nothing to re-stash); it only installs the
    /// incoming account's token via the atomic `-U` write, honouring the no-torn-swap invariant
    /// (ADR-0003). SAFETY is enforced inside the engine: a LOCKED / unreadable keychain aborts with
    /// ZERO writes ("locked ≠ gone"), and the incoming stash is read before any mutation. With no
    /// lock configured (hermetic tests) it runs unlocked — no second in-process writer to serialize.
    async fn locked_adopt(&self, incoming: &str) -> Result<swap::SwapReport> {
        swap::adopt_target_locked(
            self.swap_lock_path
                .as_deref()
                .map(|path| (path, swap::SWAP_LOCK_MAX_WAIT)),
            &self.store,
            &self.stash,
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
        let health = &mut self.state.accounts[prev].health;
        if health.quarantined && health.recovery_successes > 0 {
            health.recovery_successes = 0;
        }
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
        //
        // Issue #607 EXEMPTION, deliberate on both counts. This path keeps the RAW ceiling (no tail
        // margin, unlike `Self::weekly_rotation_line`) and SYMMETRIC `draw` (not `draw_downward`,
        // unlike the live ceiling in `decide_action`), because here the weekly value is a
        // TARGET-viability filter, not a not-cross line for the active account: both choices widen
        // the admissible target set, and a dead active with somewhere — anywhere — live to land
        // beats a target that merely has to swap again shortly. Same reasoning as the reserve this
        // path already drops (`None`). The anti-thrash invariant is knowingly traded for liveness,
        // and only here plus `recover_scrubbed_canonical`.
        let weekly_ceiling = self.weekly_ceiling_strategy.draw(
            &mut self.rng,
            WEEKLY_CEILING_PCT_LO,
            WEEKLY_CEILING_PCT_HI,
        ) / 100.0;
        let Some(target_idx) = pick_target_ranked(
            active_idx,
            readings,
            &self.enabled_mask(),
            // Drop the target-max-session-usage reserve on the emergency path (issue #398): the
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
            weekly_ceiling,
            // Enhanced selection (issue #612): even in escape, disperse two dead-active daemons off
            // one target and prefer a calmer peer.
            self.selection_tiebreak(),
        ) else {
            // No live spare to escape to — the dead active is STRANDED (issue #405). The
            // reserve and the session gate were both bypassed above, so reaching here means
            // every live spare is weekly-exhausted; the daemon holds on the dead active. Emit
            // ONE edge-triggered durable event (the strictly-worse sibling of `all_exhausted`,
            // which until now returned SILENTLY) naming the dead active held and WHEN weekly
            // relief arrives — so the operator sees the real blocker, not just the dead active's
            // `claude /login` cue. Edge-triggered: emit only on ENTERING the strand (mirrors
            // `signaled_all_exhausted`), so the payload is computed once per episode, not per tick.
            if !self.state.signaled_active_dead_no_target {
                // Reuse `all_exhausted_relief` — its one masked pass excludes the active index (and
                // every disabled account) on EVERY dimension since issue #665, so it works
                // unchanged when the active IS the dead one. `session_block_line = INFINITY`
                // because the emergency path bypasses the session gate: relief here is NEVER a
                // session reset (a weekly-viable-but-session-blocked spare would have been picked),
                // so the classification correctly falls to the weekly-wide branch.
                let (cause, _hold, resets_at) = all_exhausted_relief(
                    active_idx,
                    readings,
                    &self.enabled_mask(),
                    f64::INFINITY,
                    weekly_ceiling,
                );
                events.push(Event::ActiveDeadNoTarget {
                    // The DEAD active's label — the account the daemon is stuck on, and the
                    // `claude /login` target (issue #405); NEVER a token/email (#15).
                    hold: self.roster[active_idx].label.clone(),
                    cause,
                    resets_at,
                });
                self.state.signaled_active_dead_no_target = true;
            }
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

    /// The #452 bounded-blindness preemptive swap-away (ADR-0017) — the AVAILABILITY-path peer
    /// of [`emergency_swap`](Self::emergency_swap). Fires for a HEALTHY (non-quarantined) active
    /// that has gone BLIND (its live reading cleared to `None` on a 429/5xx) and stayed blind
    /// too long to keep trusting the last reactive verdict. It keys off the RETAINED pre-blind
    /// anchor (`last_good`, #450), NEVER the missing reading, and swaps only when ALL hold:
    ///
    /// - blind past the config `session_blind_swap_secs` (strict `>`), AND
    /// - EITHER the anchor — plausibility-corrected to its window high-water mark (issue #619), so a
    ///   stale-low pre-blind reading cannot disarm the gate — sat at/over the config
    ///   `session_blind_risk_band` (the ratified #452 arm) OR a nonzero server `Retry-After` is
    ///   still holding the account off its usage poll (the #582 arm, below), AND
    /// - a viable target exists — a peer under `target_max_session_usage` (ADR-0013), chosen via
    ///   [`selection::pick_target_with_reason`] with the BASE (un-jittered) triggers, exactly as
    ///   [`note_blind_gate_eligibility`](Self::note_blind_gate_eligibility)'s premise SLI (#482)
    ///   previews it. For the ANCHOR arm the swap therefore fires where that SLI recorded the gate
    ///   eligible; the #582 server arm fires BELOW the band, where the anchor-keyed
    ///   `note_blind_gate_eligibility` deliberately records nothing (it measures the #451/#484
    ///   anchor-band premise, not this one) — the #582 holds are surfaced by their own durable
    ///   events instead ([`Event::BlindPreemptReserveHold`] / [`Event::RetryAfterWalk`]).
    ///
    /// # The server-`Retry-After` arm (issue #582)
    ///
    /// A `429` carrying a long `Retry-After` on the ACTIVE account arms an UN-CLAMPED back-off
    /// (issue #453 makes the server directive an absolute floor) and the failed poll clears the
    /// reading — so the consumed credential goes blind for the directive's whole duration while
    /// Claude Code keeps burning it. Below the risk band that blindness was unreachable by EVERY
    /// swap path: the reactive `session_ceiling` and the #539 velocity projection both need a
    /// FRESH reading, and the #452 arm above needs `anchor >= session_blind_risk_band` (observed
    /// 2026-07-17: an anchor of `0.29` burned to exhaustion behind a `Retry-After: 3600`). Issue
    /// #619's correction does not close this gap: it raises only a STALE-LOW anchor to its window
    /// high-water mark, and a GENUINELY below-band anchor like the 0.29 one (whose mark agrees)
    /// stays below the band — so the #582 arm remains the only path that reaches it.
    ///
    /// So a nonzero server `Retry-After` on the active account is treated as a swap-away SIGNAL in
    /// its own right, wherever the anchor sat: a known ≥1 h unobservability on the credential
    /// being consumed is per se unacceptable, and a swap needs no usage poll, so it works while
    /// blind. Clamping the directive instead was rejected — it is outcome-identical to doing
    /// nothing (every clamped re-poll 429s and re-clears the reading, so the account stays blind)
    /// and would invert issue #453's ratified absolute-floor AC. This arm changes NO back-off
    /// arithmetic; it reads the retained directive
    /// ([`server_retry_after_holding`]) purely as a decision
    /// input, and reuses `reason=blind_preempt` + the `session_blind_swap_secs` bound (so the
    /// documented kill-switch still disables BOTH arms, and no new tunable is introduced).
    ///
    /// # The circuit-breaker (issue #582)
    ///
    /// The throttle follows the ACTIVE ROLE, not the account: each account observed took its
    /// `RA:3600` within minutes of TAKING the slot, while peers never got one. An unbounded
    /// swap-away therefore WALKS the throttle around the roster, so this arm is bounded twice —
    /// and both bounds apply ONLY when it is firing on its own authority (a server directive with
    /// the anchor BELOW the band). A swap the ratified #452 arm would have fired anyway is left
    /// exactly as it was; the breaker can only ever make the newer, more speculative arm yield:
    ///
    /// - **Never spend the LAST viable target.** This swap acts on a directive, not on observed
    ///   near-limit usage, so it must yield the final target to a CONFIRMED-exhaustion swap rather
    ///   than consume it on a guess. Detected via [`NextSwapReason::OnlyCandidate`] (the existing
    ///   `candidate_count < 2` verdict) and reported as [`Event::BlindPreemptReserveHold`] — the
    ///   AC's "reported, not hidden".
    /// - **Stop the walk.** At/over `RETRY_AFTER_WALK_MAX` server-throttled preemptive swaps
    ///   inside `RETRY_AFTER_WALK_WINDOW`, rotation STOPS and [`Event::RetryAfterWalk`] alarms:
    ///   holding a blind-but-low account beats walking a 3600 s throttle onto the last good one.
    ///
    /// Both holds are edge-triggered (once per blind episode, not per tick — a 3600 s window spans
    /// hundreds of ticks) and both fall through to the historical
    /// [`TickAction::SkippedActiveUnavailable`].
    ///
    /// Unlike [`emergency_swap`](Self::emergency_swap) (dead active, #42) this does NOT drop the
    /// target reserve to `f64::INFINITY` and does NOT bypass cooldown — ADR-0017 keeps the
    /// availability path separate from the liveness one, so it takes no ADR-0013 emergency
    /// exemption. No anchor / not-yet-armed / not warmed up / no viable target all fall THROUGH
    /// to the historical [`TickAction::SkippedActiveUnavailable`] (the SLI already recorded the
    /// no-viable-target episode where relevant); within cooldown yields
    /// [`TickAction::SkippedCooldown`]. On a swap-engine error the state stays coherent (#6
    /// no-half-swap) and the anchor is untouched, so the gate re-arms and retries next cycle.
    async fn blind_swap(
        &mut self,
        at: Instant,
        active_idx: usize,
        readings: &[Option<Usage>],
        events: &mut Vec<Event>,
    ) -> TickAction {
        // Key off the retained pre-blind anchor (#450) — NOT the missing reading. No anchor (a
        // genuinely-unknown active, or one whose anchor a prior swap-away / active-loss dropped)
        // → no episode, historical skip; never a spurious swap on absent data (#369's caution).
        let Some(anchor) = self.state.last_good else {
            return TickAction::SkippedActiveUnavailable;
        };
        // Gate conditions 1+2 (ADR-0017), on the CONFIG thresholds — the operator-tunable,
        // kill-switchable ACTION band, deliberately distinct from the interim const the
        // gate-premise SLI / status projection measure at (so a kill-switch disables the swap
        // without blinding #484's ratification SLIs): blind past `session_blind_swap_secs` AND
        // the anchor at/over `session_blind_risk_band`.
        //
        // Issue #619: the anchor arm decides on the anchor's PLAUSIBLE session, not the raw
        // pre-blind reading. A `/oauth/usage` response that came back stale-low for its own window
        // just before the account went blind would write a below-band anchor and cancel this
        // otherwise-due swap, letting the account burn to exhaustion unobserved for the whole blind
        // window — the same stale-low failure #614 fixed for the LIVE arms (`plausible_active_usage`),
        // reached through the blind path. Raised to the frozen per-window high-water mark (a true
        // lower bound, never a synthesized number); the stored anchor + every measurement/display
        // surface keep the raw value (see `swap::plausible_anchor_session`).
        let blind_elapsed = at.saturating_duration_since(anchor.at).as_secs();
        let anchor_armed = swap::plausible_anchor_session(
            self.state.accounts[active_idx].session_high_water,
            anchor.session,
        ) >= self.session_blind_risk_band;
        // Issue #582: the server-directed arm. A nonzero `Retry-After` STILL holding the active
        // account off its poll arms the gate on its OWN, wherever the anchor sat — below the band
        // that blindness is otherwise unreachable by every swap path (see the doc comment). Shares
        // condition 1 (`session_blind_swap_secs`), so the kill-switch disables both arms.
        let retry_after = server_retry_after_holding(&self.state.accounts[active_idx].health, at);
        if !(blind_elapsed > self.session_blind_swap_secs
            && (anchor_armed || retry_after.is_some()))
        {
            return TickAction::SkippedActiveUnavailable;
        }
        // The #582 circuit-breaker's subject: the server-directed arm firing on its OWN authority —
        // a directive with the anchor BELOW the band. When the anchor IS armed the ratified #452
        // swap fires regardless of the directive, so bounding it there would regress a ratified AC;
        // `None` here leaves that path byte-for-byte unchanged.
        let speculative = retry_after.filter(|_| !anchor_armed);
        // #80 warm-up: until the staggered loop has polled every rotation account once, the
        // carried readings are partial — a viable-target verdict off them could be spurious, so
        // hold (skip) until the first cycle completes. Mirrors the reactive path + the SLI guard.
        if !self.state.warmed_up {
            return TickAction::SkippedActiveUnavailable;
        }
        // Cooldown (#10) is HONORED — this is not the emergency path (ADR-0017). Draw the
        // per-cycle (jittered) cooldown exactly as the reactive path does; within it, defer.
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
        // Prune the walk record UNCONDITIONALLY (issue #582), even on the anchor-armed path that
        // appends to it but is not subject to the breaker below: the append keys on
        // `retry_after.is_some()` (any server-throttled swap) while the breaker consults only
        // `speculative`, so without a prune here an anchor-armed episode would grow the vector
        // without bound (its own doc invariant). Pruning always keeps the count honest and the
        // vector at most a handful of entries whichever arm fired.
        let recent_retry_after_swaps = self.retry_after_walk_count(at);
        // #582 circuit-breaker 1 — STOP THE WALK. Checked before target selection: this is a
        // "rotation itself is counter-productive" verdict, so it holds whatever targets exist.
        // At/over the limit, each further swap would only hand the throttle to the next account
        // (it follows the ACTIVE ROLE, not the credential), so hold position and alarm.
        if let Some(ra) = speculative {
            let swaps = recent_retry_after_swaps;
            if swaps >= RETRY_AFTER_WALK_MAX {
                if !self.state.signaled_retry_after_walk {
                    events.push(Event::RetryAfterWalk {
                        account: self.roster[active_idx].account_uuid.clone(),
                        swaps,
                        window_secs: RETRY_AFTER_WALK_WINDOW.as_secs(),
                        retry_after_secs: ra.as_secs(),
                    });
                    self.state.signaled_retry_after_walk = true;
                }
                return TickAction::SkippedActiveUnavailable;
            }
        }
        // Gate condition 3 (ADR-0017): a viable target — a peer under `target_max_session_usage`
        // (ADR-0013), the reserve HONORED (NOT the emergency `None` / `f64::INFINITY` bypass).
        // BASE (un-jittered) triggers, the same preview `note_blind_gate_eligibility` /
        // `next_swap` use, so the fired swap matches the recorded premise. None → the SLI
        // already logged a no-viable-target episode; fall through to the historical skip (never
        // swap among saturated / exhausted peers).
        //
        // `pick_target_with_reason_ranked` rather than `pick_target_ranked` (issue #582): selection
        // is IDENTICAL (the index-only projection just discards the reason), but the RETAINED reason
        // carries the viable-set cardinality the reserve breaker below needs — read without a second,
        // driftable filter. The enhanced #612 axes (velocity + per-daemon jitter) apply here exactly
        // as they do to `next_swap`'s preview (same daemon seed), so the fired target still matches
        // the surfaced one; the reserve breaker keys off cardinality, which the tie-break never moves.
        let Some((target_idx, target_reason)) = pick_target_with_reason_ranked(
            active_idx,
            readings,
            &self.enabled_mask(),
            Some(self.target_max_session_usage),
            self.session_ceiling_base,
            // Issue #607: this path fires a REAL swap, so it must honour the same weekly rotation
            // line the reactive arm releases on — a raw-ceiling gate here would land the preemptive
            // swap on a band account that bounces straight back.
            self.weekly_rotation_line(),
            self.selection_tiebreak(),
        ) else {
            return TickAction::SkippedActiveUnavailable;
        };
        // #582 circuit-breaker 2 — NEVER SPEND THE LAST VIABLE TARGET. `OnlyCandidate` is
        // `pick_target_with_reason`'s existing `candidate_count < 2` verdict, and we hold a `Some`,
        // so it means EXACTLY one viable target remains. This swap is speculative (a directive, not
        // observed near-limit usage), so it yields that last target to a confirmed-exhaustion swap
        // — and says so, rather than going quietly blind.
        if let Some(ra) = speculative {
            if matches!(target_reason, NextSwapReason::OnlyCandidate) {
                if !self.state.signaled_retry_after_reserve {
                    events.push(Event::BlindPreemptReserveHold {
                        account: self.roster[active_idx].account_uuid.clone(),
                        retry_after_secs: ra.as_secs(),
                        blind_secs: blind_elapsed,
                    });
                    self.state.signaled_retry_after_reserve = true;
                }
                return TickAction::SkippedActiveUnavailable;
            }
        }
        // Run the swap through the SAME single-writer, no-torn-swap primitive (#6 / #64,
        // ADR-0003) the reactive + emergency paths use — an error (incl. a fail-closed
        // contended lock) leaves the canonical + both stashes coherent and the anchor intact
        // (`record_swap` drops it only on success), so the gate re-arms and retries next cycle.
        let outgoing = self.roster[active_idx].stash();
        let incoming = self.roster[target_idx].stash();
        match self.locked_swap(&outgoing, &incoming).await {
            Ok(_report) => {
                let from_label = self.roster[active_idx].label.clone();
                let to_label = self.roster[target_idx].label.clone();
                // The active is BLIND, so the stale pre-blind anchor is the only session signal — the
                // same value the `swap` log line records and the narration surfaces (#479).
                let last_known_session_pct = to_pct(anchor.session);
                self.record_swap(target_idx, &incoming, at).await;
                events.push(Event::Swap {
                    from: from_label.clone(),
                    to: to_label.clone(),
                    reason: SwapReason::BlindPreempt,
                    // Log it (as a percent, like every swap line) so the `swap` line agrees with the
                    // value the gate keyed off. Never a token / email (#15).
                    session_pct: last_known_session_pct,
                    // The blind-preempt arm's projection ingredients ride its own
                    // `blind_window` line (issue #634), not the swap line — this swap fires on the
                    // stale anchor, and the retained velocity is carried where the report-only arm
                    // that consumes it lives. So the swap line itself carries no projection.
                    projection: None,
                });
                // Retain the swap for `status` to NARRATE (issue #479): `record_swap` above cleared any
                // prior notice (superseding a same-active one), so set THIS swap's as the current one.
                // `recent_blind_preempt_swap_view` projects it onto the wire only while still-current +
                // recent; the narration names the `use <from>` undo (derived from `from`, not stored).
                self.state.last_blind_preempt_swap = Some(BlindPreemptSwapRecord {
                    from: from_label,
                    to: to_label,
                    last_known_session_pct,
                    at,
                });
                // Issue #582: record a swap fired away from a SERVER-THROTTLED active — the
                // evidence the walk breaker above counts. Recorded on `retry_after` (any swap the
                // directive was live for), NOT on `speculative`: the throttle moved whichever arm
                // authorised the move, and the walk is a property of the throttle, not the
                // authorisation. `record_swap` above already re-armed the edge-trigger guards.
                if retry_after.is_some() {
                    self.state.retry_after_swaps.push(at);
                }
                TickAction::PreemptivelySwapped {
                    from: active_idx,
                    to: target_idx,
                }
            }
            Err(_) => TickAction::SwapFailed,
        }
    }

    /// How many server-`Retry-After`-driven preemptive swaps fired inside the trailing
    /// [`RETRY_AFTER_WALK_WINDOW`] ending at `at` (issue #582) — the #582 walk breaker's count.
    ///
    /// PRUNES the record of entries that have aged out, so the window slides and the vector cannot
    /// grow without bound across a long-lived daemon (hence `&mut self` for what reads as a query).
    /// Pruning on read rather than on a timer keeps the record's only mutation points the swap that
    /// appends and the count that consumes it.
    fn retry_after_walk_count(&mut self, at: Instant) -> usize {
        self.state.retry_after_swaps.retain(|&swapped_at| {
            at.saturating_duration_since(swapped_at) < RETRY_AFTER_WALK_WINDOW
        });
        self.state.retry_after_swaps.len()
    }

    /// Account `i`'s reading as the swap arms should DECIDE on it (issue #614): the verbatim
    /// response, except that a session fraction sitting below the high-water mark of its own
    /// (unchanged) window is raised back to that mark.
    ///
    /// Usage is monotonic within a session window, so such a reading is a stale / cache-lagged
    /// response rather than real drain, and the retained mark is a genuine LOWER bound on the true
    /// usage — deciding on it keeps a lagging response from cancelling an otherwise-due swap. A
    /// plausible reading (or one carrying no window evidence — see [`swap::SessionHighWater`]) is
    /// returned unchanged, so this is a no-op on every normal tick.
    ///
    /// Both swap arms route through here — the reactive [`decide_action`](Self::decide_action) and
    /// the projective [`velocity_swap`](Self::velocity_swap) — so they cannot disagree about how high
    /// the account is (ADR-0022: one predicate, two estimators).
    ///
    /// # What the correction does and does not reach
    ///
    /// It is applied to the swap DECISION and to the record OF that decision — nothing else. The
    /// carried [`last_reading`](AccountRuntime::last_reading) slot is never written back over, so
    /// `pick_target`, the all-exhausted relief hint, the `status` snapshot, and the usage-sample store
    /// all keep exactly what the API reported.
    ///
    /// The ONE surface that carries the corrected value is the `session_pct` of the
    /// [`Event::Swap`] this decision emits (both arms), and with it the #595 / #539 landing and
    /// swap-out-overshoot SLIs derived from it. That is deliberate, not an oversight: that field's
    /// contract is "the usage the account was at when we swapped away", and on a stale-low tick the
    /// raw response is precisely the number we did NOT act on — logging it would under-report the very
    /// overshoot the SLI exists to measure. The retained mark is a true lower bound on the account's
    /// real usage, so it is the honest estimate of the swap-out point. It is, however, a PRIOR
    /// observation rather than this tick's fresh one, which is the caveat to read those SLIs with.
    fn plausible_active_usage(&self, i: usize, usage: Usage) -> Usage {
        swap::plausible_session(self.state.accounts[i].session_high_water, &usage)
    }

    /// Fold one poll interval into account `i`'s retained session-velocity EMA (issue #539,
    /// ADR-0017) — the signal [`velocity_swap`](Self::velocity_swap) projects from. Called from the
    /// poll fold with the SAME `(prev, next, elapsed_secs)` the durable [`Event::UsageVelocity`]
    /// uses (fraction readings copied out so `self` is free to mutate). Pure state update, no event.
    ///
    /// - A session-usage DROP (`next < prev`) means the 5 h window reset (usage is monotonic within a
    ///   window) or a recovery — the prior climbing trend is stale, so the slot is reset to `None`;
    ///   a zero/degenerate interval likewise resets (nothing to integrate). Either way the projective
    ///   path then needs a fresh pair of readings (≥ [`MIN_VELOCITY_SAMPLES`]) before it can fire.
    /// - Otherwise the instantaneous rate `(next - prev) / elapsed` (session fraction per second,
    ///   `>= 0`) is blended into the EMA at weight `session_velocity_ema_alpha`, seeded with the raw
    ///   rate on the first sample (NOT zero — a zero seed biases the EMA to asymptote BELOW the true
    ///   rate and would miss real overshoots; the single-spike case is instead gated by
    ///   [`MIN_VELOCITY_SAMPLES`], not by seed choice), and the sample count is advanced.
    ///
    /// The DROP branch above reads a drop as a genuine window reset / recovery, so issue #614's
    /// plausibility guard keeps IMPLAUSIBLE drops away from here entirely: the poll fold skips this
    /// call when either endpoint of the interval is a stale-low reading (one below its own window's
    /// high-water mark), leaving the retained EMA untouched rather than resetting it on a reading
    /// that never happened. This function therefore still treats every drop it is handed as real.
    fn note_session_velocity(
        &mut self,
        i: usize,
        prev_session: f64,
        next_session: f64,
        elapsed_secs: u64,
    ) {
        if elapsed_secs == 0 || next_session < prev_session {
            self.state.accounts[i].session_velocity = None;
            return;
        }
        let instant = (next_session - prev_session) / elapsed_secs as f64;
        let alpha = self.session_velocity_ema_alpha;
        self.state.accounts[i].session_velocity =
            Some(match self.state.accounts[i].session_velocity {
                Some(prev) => VelocityEma {
                    rate: alpha * instant + (1.0 - alpha) * prev.rate,
                    samples: prev.samples.saturating_add(1),
                },
                None => VelocityEma {
                    rate: instant,
                    samples: 1,
                },
            });
    }

    /// The #539 velocity ingredients account `i`'s blind window should carry (issue #634), or `None`
    /// when the REPORT-ONLY blind velocity-projection arm ([`blind_velocity_projected_armed`],
    /// issues #584/#600) could not have armed on this account at all.
    ///
    /// That arm decides but emits nothing — it moves `status` to DEGRADED and fires no swap — so its
    /// inputs left no trace and the report was unfalsifiable after the fact. This supplies the one
    /// missing ingredient (the anchor and the window are already on [`Event::BlindWindow`]), letting
    /// an offline reader recompute `anchor + rate × inflation × blind_secs` and check it against the
    /// ceiling. Since issue #632 the live arm projects off the #619 plausibility-CORRECTED anchor,
    /// while `session_pct` on the event is the RAW anchor (a measurement, kept raw) — so this recompute
    /// reproduces the arm exactly absent a stale-low correction and is a conservative LOWER bound under
    /// one; the frozen high-water mark that would close the gap is not yet carried here (issue #670).
    /// The recomputation is deliberate: the arm is report-only and not in every running
    /// binary, so logging the ingredient must NOT require the live arm to be active.
    ///
    /// Gated on the SAME sustained-EMA precondition the arm applies (a retained EMA with
    /// `>= MIN_VELOCITY_SAMPLES` blended intervals), so absent tokens mean "this arm could not have
    /// armed here" rather than "unknown" — no ingredient is published for a projection that could not
    /// have happened.
    ///
    /// Stamps the CONSTANTS IN FORCE beside the ingredient rather than leaving them to be re-derived:
    /// [`BLIND_VELOCITY_RATE_INFLATION`] is explicitly interim and ratification-pending, and the
    /// ceiling is operator-configurable, so an offline reader applying today's values to an old
    /// window would silently mis-report it. The ceiling is the BASE (un-jittered) draw — the line
    /// this arm projects against ([`blind_velocity_projected_armed`]'s `session_ceiling`), NOT the
    /// per-tick jittered effective ceiling [`velocity_swap`](Self::velocity_swap) fires on.
    ///
    /// A pure read of retained state; the fraction→percent conversion is the log's unit boundary
    /// ([`to_pct_exact`]).
    fn blind_velocity_ingredients(&self, i: usize) -> Option<BlindVelocity> {
        let vel = self.state.accounts[i].sustained_session_velocity()?;
        Some(BlindVelocity {
            rate_pct_per_sec: to_pct_exact(vel.rate),
            inflation: BLIND_VELOCITY_RATE_INFLATION,
            ceiling_pct: to_pct_exact(self.session_ceiling_base),
        })
    }

    /// The #539 velocity-projection preemptive swap (ADR-0017) — the OBSERVED-overshoot peer of the
    /// reactive session trigger, called from [`decide_action`](Self::decide_action) exactly where the
    /// reactive path would HOLD (observed below the trigger). It swaps the active account away when
    /// its PROJECTED session usage crosses the trigger within the horizon, closing the residual
    /// reactive overshoot (#363) that fires because `usage_velocity` (#399) peaks between the ~cadence
    /// observations. Unlike [`blind_swap`](Self::blind_swap) it keys off a FRESH reading + its
    /// retained velocity, never a stale anchor; like it, it is NOT emergency — HONORS cooldown and the
    /// swap-target reserve ([`target_max_session_usage`](Self::target_max_session_usage), ADR-0013),
    /// and routes through the no-torn-swap primitive ([`locked_swap`](Self::locked_swap), ADR-0003).
    ///
    /// Fires only when ALL hold (else `Held` — the reading is present and below the fire point, so the
    /// non-fire outcome is a genuine hold, not the "unavailable" skip `blind_swap` returns):
    /// - the horizon is non-zero (`0` is the config kill-switch — the projection would reduce to the
    ///   observed reading, which already held below the effective ceiling), **and**
    /// - a retained velocity EMA exists with `>= MIN_VELOCITY_SAMPLES` samples (SUSTAINED — never a
    ///   single-interval spike, and never a missing/`None` signal), **and**
    /// - the observed reading is at/over `session_velocity_min_project_above` (the free guard), **and**
    /// - `observed + rate × horizon >= effective_ceiling` (the projection crosses the ceiling minus
    ///   the tail margin, issue #597), **and**
    /// - the roster is warmed up, the cooldown has elapsed (else `SkippedCooldown`), and a viable
    ///   target exists (a peer under the reserve).
    ///
    /// `session_ceiling` is the session CEILING draw the reactive path used this tick; this arm
    /// derives the SAME effective ceiling (ceiling − tail margin) the reactive threshold does, and
    /// `pick_target` sees the same reserve — so the two are coupled (one ceiling, one reserve), not
    /// differently-calibrated triggers. This arm covers the velocity horizon (it fires at
    /// `effective_ceiling − velocity × H`), a DIFFERENT unseen window from the reactive arm's
    /// re-observation gap (issue #609); the composed swap fires at whichever is earlier
    /// (`effective_ceiling − velocity × max(poll_gap, H)`), covering the larger window.
    ///
    /// `weekly_threshold` is the ALREADY-DERIVED effective weekly ceiling (issue #607 —
    /// `weekly_ceiling − WEEKLY_TAIL_MARGIN`), not the raw weekly ceiling. This arm projects SESSION
    /// only, so it never fires on the weekly dimension; it forwards this value solely to
    /// `pick_target`, where it must match the line the reactive arm releases on so the acquire
    /// predicate stays at least as strict as the negation of the release predicate (the anti-thrash
    /// invariant — see the caller and `pick_target`).
    async fn velocity_swap(
        &mut self,
        at: Instant,
        active_idx: usize,
        session_ceiling: f64,
        weekly_threshold: f64,
        readings: &[Option<Usage>],
        events: &mut Vec<Event>,
    ) -> TickAction {
        // Kill-switch: horizon 0 → the projection is just the observed reading, which the reactive
        // path already held below the effective ceiling, so it can never cross. Cheap early exit.
        let horizon = self.session_velocity_horizon_secs;
        if horizon == 0 {
            return TickAction::Held;
        }
        // The active's own FRESH reading (issue #539) — re-read from `readings[active_idx]` rather
        // than threaded in as a separate arg (the `Copy` reading is already carried here, and the
        // caller only reaches this path with it present). A missing slot cannot occur on the reactive
        // Hold path that calls this, but hold defensively rather than project on absent data.
        // Issue #614: corrected to the window's high-water mark when the response came back
        // implausibly LOW, through the SAME helper the reactive arm used this tick — so a stale
        // reading cannot silence this arm either, and the two arms project from one value.
        let Some(active_usage) =
            readings[active_idx].map(|u| self.plausible_active_usage(active_idx, u))
        else {
            return TickAction::Held;
        };
        // The retained velocity signal must exist AND be SUSTAINED (≥ MIN_VELOCITY_SAMPLES blended
        // intervals). Absent (a first/failed poll, or reset by a window drop — the poll-gap case #540
        // owns) or single-sample → never project (the "no-fire on a missing/unwarmed velocity"
        // invariant): hold on the fresh reading rather than swap on a guess.
        let Some(vel) = self.state.accounts[active_idx].sustained_session_velocity() else {
            return TickAction::Held;
        };
        // The free guard (#538): only project from a reading already at/over the band. The
        // projection cannot reach below it (max reach ≤ ~14 pp at H ≤ 150 s), so a lower reading can
        // never cross anyway — the guard just excludes spurious low-usage projections cheaply.
        if active_usage.session < self.session_velocity_min_project_above {
            return TickAction::Held;
        }
        // #597: project `last + velocity × H` and require it to reach the EFFECTIVE CEILING (the
        // ceiling minus the tail margin — the same landing target the reactive arm derives its
        // threshold from this tick). Below → the velocity is not steep enough to reach the effective
        // ceiling within the horizon → hold and let the reactive path catch it if it climbs. Firing
        // at the effective ceiling (not the raw ceiling) keeps the projected landing under the ceiling
        // after the post-swap tail. This arm covers the velocity horizon `H`; the reactive arm covers
        // the re-observation gap (#609); the composed swap fires at `eff − v·max(poll_gap, H)` (see
        // `swap::reactive_session_threshold`), so whichever window is larger sets the fire point.
        let effective_ceiling = swap::effective_ceiling(session_ceiling);
        let projected = active_usage.session + vel.rate * horizon as f64;
        if projected < effective_ceiling {
            return TickAction::Held;
        }
        // #80 warm-up: the carried readings are partial until the staggered loop has polled every
        // rotation account once — a viable-target verdict off them could be spurious, so hold. Mirrors
        // the reactive + blind paths.
        if !self.state.warmed_up {
            return TickAction::Held;
        }
        // Cooldown (#10) is HONORED — this is not the emergency path (ADR-0017). Draw the per-cycle
        // (jittered) cooldown exactly as the reactive + blind paths do; within it, defer. Reported as
        // SkippedCooldown (the projection WANTED to fire) rather than a silent Held.
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
        // A viable target — a peer under `target_max_session_usage` (ADR-0013), the reserve HONORED
        // (NOT the emergency `None` bypass). The SAME jittered triggers the reactive path used — and
        // the SAME #607 effective weekly ceiling — so the projective peer selects exactly as the
        // reactive swap it front-runs would. None → hold (never swap among saturated / exhausted
        // peers; the reactive path owns the all-exhausted signal).
        let Some(target_idx) = pick_target_ranked(
            active_idx,
            readings,
            &self.enabled_mask(),
            Some(self.target_max_session_usage),
            session_ceiling,
            weekly_threshold,
            // Enhanced target selection (issue #612), same as the reactive path this front-runs, so
            // the projective peer selects exactly as the swap it precedes: velocity-preferred + herd-
            // dispersed.
            self.selection_tiebreak(),
        ) else {
            return TickAction::Held;
        };
        // Run the swap through the SAME single-writer, no-torn-swap primitive (#6 / #64, ADR-0003) the
        // reactive + blind + emergency paths use — an error (incl. a fail-closed contended lock) leaves
        // the canonical + both stashes coherent, so we retry next cycle.
        let outgoing = self.roster[active_idx].stash();
        let incoming = self.roster[target_idx].stash();
        match self.locked_swap(&outgoing, &incoming).await {
            Ok(_report) => {
                self.record_swap(target_idx, &incoming, at).await;
                events.push(Event::Swap {
                    from: self.roster[active_idx].label.clone(),
                    to: self.roster[target_idx].label.clone(),
                    reason: SwapReason::VelocityPreempt,
                    // The reading the projection fired off (issue #539) — a LIVE reading, so
                    // `session_pct` is the real swap-out point (the projected swap-out overshoot SLI's
                    // #363-acceptance sample), never a stale BLIND anchor. Since #614 it is the
                    // plausibility-corrected live reading: the retained high-water mark (a true lower
                    // bound on the account's usage, and the value the swap decided on) when the
                    // response came back stale-low for its own window — see `plausible_active_usage`.
                    // A percent, like every swap line; never a token / email (#15).
                    session_pct: to_pct(active_usage.session),
                    // The PROJECTION this arm actually decided on (issue #634), so the line explains
                    // its own fire instead of reading like a bug at a below-trigger `session_pct`:
                    // `projected >= ceiling` is the very predicate checked above, now checkable from
                    // the log alone. The ingredient (`rate`) is PERSISTED at full precision rather
                    // than left to be re-derived — the durable `usage_velocity` carries only a
                    // rounded `i16` delta, which cannot reproduce this decision — and the CONSTANTS
                    // in force are stamped with it, so a later change to the horizon tunable or to
                    // the ceiling semantics cannot make this record read wrong.
                    projection: Some(SwapProjection {
                        projected_pct: to_pct_exact(projected),
                        // The internal EMA is a fraction per second; the log speaks percent, like
                        // the `session_pct` / `ceiling` it sits beside.
                        rate_pct_per_sec: to_pct_exact(vel.rate),
                        horizon_secs: horizon,
                        // The EFFECTIVE ceiling (the tick's draw less the #597 tail margin) — the
                        // comparand itself, not the raw configured ceiling, so no offline reader has
                        // to know which margin applied.
                        ceiling_pct: to_pct_exact(effective_ceiling),
                    }),
                });
                TickAction::VelocityPreemptivelySwapped {
                    from: active_idx,
                    to: target_idx,
                }
            }
            Err(_) => TickAction::SwapFailed,
        }
    }

    /// The forward-looking next-swap candidate for the `status` display (issue #88):
    /// who the daemon's live selection would choose right now, or why there is no
    /// candidate. THE candidate is computed daemon-side — the CLI never re-derives the
    /// selection rule (it cannot: the wire carries only rounded percents, not the raw
    /// `Usage` / `target_max_session_usage` / triggers the selection consumes). Runs the
    /// SAME enhanced selection as the live swap paths (issue #612: velocity-preferred,
    /// then per-daemon jittered, off this daemon's seed) so the surfaced candidate matches
    /// what the daemon would actually promote. Uses the BASE (un-jittered) session and
    /// weekly triggers ([`Self::session_ceiling_base`], [`Self::weekly_ceiling_base`]) —
    /// the same thresholds the snapshot's per-account exhaustion flags key off — so the
    /// candidate and the displayed exhaustion state can never disagree, and the candidate
    /// does not flicker with the per-cycle swap-decision jitter. The #612 seed is likewise
    /// fixed per daemon and adds no flicker; its velocity axis, however, DOES track the
    /// retained EMAs — see the call site below for what that legitimately moves.
    ///
    /// `None` only when there is no active account to swap FROM (no anchor). Otherwise
    /// the three cases mirror the selection's verdict: a viable [`NextSwap::Target`]; a
    /// [`NextSwap::NoViableTarget`] when readings are in hand but none qualifies (or no
    /// other enabled, non-quarantined account exists at all); and
    /// [`NextSwap::AwaitingData`] for the post-restart moment when such an account exists
    /// but none has a reading yet — the distinction #88 exists to draw.
    fn next_swap(&self, active: Option<usize>, readings: &[Option<Usage>]) -> Option<NextSwap> {
        let active_idx = active?;
        let enabled = self.enabled_mask();
        if let Some((target, reason)) = pick_target_with_reason_ranked(
            active_idx,
            readings,
            &enabled,
            Some(self.target_max_session_usage),
            self.session_ceiling_base,
            // Issue #607: the rotation line, so this PREVIEW names the account the daemon would
            // actually pick. Against the raw ceiling the preview could surface a band account the
            // live path would refuse — a status/menubar display that contradicts the daemon.
            self.weekly_rotation_line(),
            // The same enhanced #612 selection (velocity + per-daemon jitter) the live swap paths
            // use, so the surfaced candidate matches what the daemon would pick from these readings.
            // The per-daemon SEED is fixed, so — like the base triggers above — it contributes no
            // flicker; the velocity axis does TRACK the EMAs, so a reset-tied pair whose velocities
            // cross can legitimately move the surfaced candidate between ticks (a real change in the
            // better landing, not jitter noise).
            self.selection_tiebreak(),
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
            if i != active_idx && enabled[i] && !self.state.accounts[i].health.quarantined {
                any_other_enabled = true;
                all_unpolled &= reading.is_none();
            }
        }
        Some(if any_other_enabled && all_unpolled {
            NextSwap::AwaitingData
        } else {
            // Carry the fleet-capacity RELIEF hint (issue #405) so the status footer can say WHY the
            // fleet is blocked and WHEN capacity returns, instead of a content-free "no viable
            // target". Uses the SAME `all_exhausted_relief` classification the durable events do,
            // with the PROACTIVE session ceiling (`min(session_ceiling_base, target_max_session_usage)`) so
            // it agrees with the base-trigger `pick_target_with_reason` verdict just above (the
            // snapshot keys off the BASE, un-jittered triggers — #88 — so the footer never flickers
            // with the per-cycle swap-decision jitter). Covers BOTH the active-alive-and-over-trigger
            // and the active-DEAD-and-stranded cases: a dead active leaves every live spare
            // weekly-exhausted, so relief classifies `Weekly` here while the dead active's 🔴 health
            // rides its own account row (the composite an operator needs — issue #405).
            let session_block_line = self.session_ceiling_base.min(self.target_max_session_usage);
            let (cause, _hold, resets_at) = all_exhausted_relief(
                active_idx,
                readings,
                &enabled,
                session_block_line,
                // Issue #607: the same rotation line the viability filter above used, so the relief
                // hint explains the verdict it accompanies rather than a different weekly line.
                self.weekly_rotation_line(),
            );
            let cause = match cause {
                SwapReason::Session => Some(NoTargetCause::Session),
                SwapReason::Weekly => Some(NoTargetCause::Weekly),
                // `all_exhausted_relief` only ever classifies Session|Weekly; the operator-swap
                // reasons (Manual / Forced) and the preemptive reasons (#452 BlindPreempt / #539
                // VelocityPreempt) cannot arise from a no-target verdict.
                SwapReason::Manual
                | SwapReason::Forced
                | SwapReason::BlindPreempt
                | SwapReason::VelocityPreempt => None,
            };
            NextSwap::NoViableTarget { cause, resets_at }
        })
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
        let base = interval / len;
        // Issue #540: while the active account is near its limit, cap the sub-interval to
        // `near_limit_poll_secs` so the active — re-observed every ~2 sub-intervals by the #366
        // interleave — is re-polled within ~2× that near-limit, closing the poll gap. A `min`, never
        // a replacement: a roster whose base sub-interval is already tighter than the cap keeps its
        // faster cadence, so the tightening only ever SHORTENS the wait. Below the band
        // `near_limit_fast_poll` is clear and the base is returned unchanged — the steady-state
        // cadence the source-scoped 429 footprint depends on stays flat. The cached bool can only be
        // `true` when `near_limit_poll_secs != 0` (its sole writer,
        // `near_limit_fast_poll_engaged`, gates on the kill-switch), so the cap is never `0`.
        //
        // This caps the SHARED per-tick spacing, so peer ticks in the #366 interleave tighten
        // transiently WITH the active while it is in-band — a deliberate, accepted consequence, not
        // a leak of the active-only TRIGGER (which keys off the active reading alone, never a peer's):
        // capping only the active-position ticks would leave the active re-poll gap at `base + cap`
        // (the wait after the interposed peer stays uncapped) and fail to close it, while a full
        // active-priority takeover would deadlock #537's exhausted-peer observation. The peer
        // transient is in-band-only (the steady-state above the band stays flat) and within the
        // #538-validated envelope (active re-poll ≈ 2× cap, inside the ratified 60–150 s band).
        if self.state.near_limit_fast_poll {
            base.min(Duration::from_secs(self.near_limit_poll_secs))
        } else {
            base
        }
    }

    /// Whether the ACTIVE account `active_idx` is in — or the #539 velocity projection places it
    /// into — the near-limit band (issue #540): the trigger for the near-limit poll-coverage
    /// fast-poll. Keyed off the SAME signals #539's [`velocity_swap`](Self::velocity_swap) uses (the
    /// active's fresh reading plus its retained session-velocity EMA), so #540 keeps the very
    /// reading + velocity #539 projects from warm through the final climb — the poll-gap
    /// `velocity_swap` explicitly holds on (a velocity reset by a window drop). Requires a PRESENT
    /// reading: a blind active (its slot cleared by a 429/5xx) carries no OBSERVED near-limit signal
    /// and belongs to the #452 bounded-blindness path, not this one. Fires when EITHER the observed
    /// reading is at/over the band floor
    /// ([`session_velocity_min_project_above`](Self::session_velocity_min_project_above) — reused so
    /// #540 and #539 share ONE band, no drift) OR, for a still-below-band reading, the sustained
    /// projection `last + rate × H` reaches that floor within the horizon (the "approaching the
    /// band" arm, so the fast-poll engages BEFORE a fast burst is even observed in-band). A pure
    /// read of `self.state`; `active_idx` is a resolved roster index, so the slot always exists.
    fn active_near_limit(&self, active_idx: usize) -> bool {
        let Some(active_usage) = self.state.accounts[active_idx].last_reading else {
            return false;
        };
        // Issue #614: through the SAME plausibility correction the two swap arms apply, because this
        // band's whole contract (above) is that #540 and #539 key off ONE band with no drift. Without
        // it a stale-low reading would DISENGAGE the near-limit fast-poll exactly when the account is
        // really near the ceiling — widening the very re-observation gap the reactive arm looks ahead
        // over, and so compounding the overshoot this guard exists to narrow.
        let active_usage = self.plausible_active_usage(active_idx, active_usage);
        let floor = self.session_velocity_min_project_above;
        // In-band by the observed reading.
        if active_usage.session >= floor {
            return true;
        }
        // Approaching the band: the #539 projection reaches the floor within the horizon. Requires a
        // SUSTAINED velocity EMA (≥ `MIN_VELOCITY_SAMPLES`), exactly as `velocity_swap` does — never
        // a single-interval spike; a `0` horizon (the #539 kill-switch) collapses the projection to
        // the observed reading, which is below the floor here, so it cannot reach.
        let horizon = self.session_velocity_horizon_secs;
        if horizon == 0 {
            return false;
        }
        let Some(vel) = self.state.accounts[active_idx].sustained_session_velocity() else {
            return false;
        };
        active_usage.session + vel.rate * horizon as f64 >= floor
    }

    /// Whether the near-limit poll-coverage fast-poll (issue #540) is engaged THIS tick: the path is
    /// enabled (`near_limit_poll_secs != 0`, the kill-switch), the roster is warmed up (#80 — before
    /// the first full cycle the carried readings are partial, and tightening the shared tick before
    /// warm-up could starve peers of their first poll and stall the warm-up latch), an active
    /// account is resolved, and it is [`active_near_limit`](Self::active_near_limit). The cached
    /// verdict [`near_limit_fast_poll`](DecisionState::near_limit_fast_poll) is refreshed from this
    /// every tick, so the wait path and the edge-triggered event read ONE consistent decision.
    fn near_limit_fast_poll_engaged(&self) -> bool {
        self.near_limit_poll_secs != 0
            && self.state.warmed_up
            && self
                .state
                .active
                .is_some_and(|active_idx| self.active_near_limit(active_idx))
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
        self.state.accounts[i]
            .health
            .poll_backoff_until
            .is_some_and(|until| self.clock.now() < until)
    }

    /// Whether NON-active account `i` is being SLOW-POLLED because it is out of rotation —
    /// weekly- or session-exhausted (issue #537): it has an armed `exhausted_poll_until`
    /// window that has not yet elapsed on the monotonic [`Clock`]. While `true`,
    /// [`tick`](Self::tick) SKIPS that account's poll this cycle — an exhausted peer's usage
    /// number cannot change until its server-side window resets, so re-polling every
    /// `poll_secs` wastes a request. The ACTIVE account is EXEMPT (`active != Some(i)`): its
    /// swap-away trigger must stay observable at full cadence (the #453 active-vs-peer
    /// asymmetry), so even a stale armed window on an account just promoted to active (via
    /// `use`) never skips it — the belt-and-suspenders partner of
    /// [`note_exhausted_poll`](Self::note_exhausted_poll) never arming the active account.
    /// `false` once the window elapses, when the account reads viable again, or when it is the
    /// active account. The quota-exhaustion sibling of
    /// [`account_backing_off`](Self::account_backing_off)'s rate-limit skip (ADR-0019).
    fn exhausted_slow_polling(&self, i: usize, active: Option<usize>) -> bool {
        active != Some(i)
            && self.state.accounts[i]
                .health
                .exhausted_poll_until
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
    /// decorrelation — times `2^min(streak, POLL_BACKOFF_MAX_SHIFT)`. The first throttled
    /// poll already earns ~2× the interval, so the account's re-poll spacing is WIDER than
    /// the fixed cadence — #76's core ask, now per-account. The exponential arm is clamped
    /// to a ceiling that DIFFERS by role (issue #453, `is_active`): a PEER settles at
    /// [`POLL_BACKOFF_CAP`] (one poll/hour), while the ACTIVE account — whose throttle blinds
    /// the very account being consumed — settles at the much tighter
    /// [`ACTIVE_POLL_BACKOFF_CAP`], recovering observability fast instead of climbing toward
    /// an hour.
    ///
    /// A server-advised `Retry-After` is honoured as a MINIMUM (the wait is never shorter
    /// than it), but the MAXIMUM differs by role: for a PEER it is itself clamped to
    /// [`POLL_BACKOFF_CAP`], so a pathological value cannot dark the account past the ceiling
    /// (issue #294); for the ACTIVE account it is an ABSOLUTE, un-clamped floor, so the daemon
    /// never re-polls before the server said it may (issue #453 AC) — even past
    /// [`POLL_BACKOFF_CAP`]. Un-clamping the active floor re-introduces the unbounded-`wait`
    /// overflow #294 had retired, so the armed instant is computed with `checked_add` below.
    ///
    /// Scoping BOTH the `429` and the transient per-account is deliberate (issue #293): the
    /// `429` is per-Anthropic-org (independent buckets), and under a genuine endpoint outage
    /// every account fails its OWN poll and arms its OWN window anyway — so one per-account
    /// path is the simplest correct design and needs no separate global case.
    fn note_account_backoff(
        &mut self,
        i: usize,
        is_active: bool,
        result: &Result<Usage>,
        events: &mut Vec<Event>,
    ) -> Option<TickBackoff> {
        // The account UUID is the durable identity for the #399 events (never the free-form,
        // PII-capable `label`, #15). Cloned up front so the borrow does not tangle with the
        // `&mut self.state.accounts[i].health` below.
        let account_uuid = self.roster[i].account_uuid.clone();
        let Some(signal) = backoff_signal(result) else {
            let health = &mut self.state.accounts[i].health;
            // Edge-triggered EXIT (issue #399): a non-throttling poll (success / 401 / 403) that
            // CLEARED an actually-armed window emits a durable `usage_backoff_cleared`, bracketing
            // the episode's span. A plain clean poll with no armed window stays silent (mirroring
            // `usage_rollup`'s no-op silence), so the exit is a true edge, not a per-clean-poll line.
            let was_backing_off = health.poll_backoff_until.is_some();
            health.poll_backoff_streak = 0;
            health.poll_backoff_until = None;
            // Issue #582: the retained server directive dies with the window it described — a
            // non-throttling poll means the account is READABLE again, so a stale `Retry-After`
            // must never outlive it and re-arm the swap-away path on a recovered account.
            health.poll_backoff_retry_after = None;
            if was_backing_off {
                events.push(Event::UsageBackoffCleared {
                    account: account_uuid,
                });
            }
            return None;
        };
        let streak = self.state.accounts[i]
            .health
            .poll_backoff_streak
            .saturating_add(1);
        self.state.accounts[i].health.poll_backoff_streak = streak;
        let shift = streak.min(POLL_BACKOFF_MAX_SHIFT);
        // The exponential self-backoff ceiling is role-dependent (issue #453): the ACTIVE
        // account clamps tighter (`ACTIVE_POLL_BACKOFF_CAP`) so a throttle on the consumed
        // account does not go dark for long; a PEER keeps the #294 `POLL_BACKOFF_CAP` ceiling.
        let exp_cap = if is_active {
            ACTIVE_POLL_BACKOFF_CAP
        } else {
            POLL_BACKOFF_CAP
        };
        let widened = self
            .next_poll_interval()
            .checked_mul(1u32 << shift)
            .unwrap_or(exp_cap)
            .min(exp_cap);
        let wait = match signal.retry_after {
            // ACTIVE (issue #453): a server `Retry-After` is an ABSOLUTE floor — NOT clamped,
            // so the daemon never re-polls before the server said it may, even past
            // `POLL_BACKOFF_CAP`. (The exponential arm is already ≤ `ACTIVE_POLL_BACKOFF_CAP`.)
            Some(ra) if is_active => widened.max(ra),
            // PEER (issue #294): the `Retry-After` arm is clamped to `POLL_BACKOFF_CAP` as a
            // MAXIMUM, bounding a pathological / buggy value (e.g. `86400`). `widened` is
            // already ≤ the cap, so this bites only the `Retry-After` arm.
            Some(ra) => widened.max(ra).min(POLL_BACKOFF_CAP),
            None => widened,
        };
        // Arm the window on the monotonic clock. A PEER's `wait` is bounded to `POLL_BACKOFF_CAP`,
        // so `now + wait` cannot overflow. The ACTIVE account's `Retry-After` floor is un-clamped
        // (issue #453), which re-opens the unbounded-`Retry-After` overflow #294 had retired — so
        // arm with `checked_add`. A value large enough to overflow the monotonic instant (~hundreds
        // of billions of years) is garbage, not a bona-fide server directive, so it falls back to
        // the peer ceiling rather than panic. `now` is read ONCE, and the REPORTED window (`armed`)
        // is derived from the armed `until`, so the durable event / tick line always AGREE with
        // `poll_backoff_until` — including in that fallback, where they then render like the peer
        // clamp (a bounded `backoff_secs` beside the raw pathological `retry_after_secs`).
        let now = self.clock.now();
        let until = now
            .checked_add(wait)
            .unwrap_or_else(|| now + POLL_BACKOFF_CAP);
        self.state.accounts[i].health.poll_backoff_until = Some(until);
        // Issue #582: retain the RAW (pre-cap #295) server directive alongside the window it just
        // armed, so the bounded-blindness swap-away path can read it on a LATER tick — the active
        // account is skipped while backing off, so the deciding tick never re-observes this `429`.
        // Written here, in lockstep with `poll_backoff_until` above, and cleared with it on any
        // non-throttling poll.
        //
        // A ZERO `Retry-After` is normalized to `None` — the issue's `ra > 0` classification, and
        // load-bearing, not cosmetic. `widened.max(0) == widened`, so a zero directive contributes
        // NOTHING: the window is the daemon's OWN self-capped exponential, which is exactly the
        // bounded blindness the ratified #452 anchor gate already governs (ADR-0017's S1 spike:
        // ALL 181 observed 429s carried `retry_after_secs=0`, so this is the DOMINANT real case,
        // not a corner). Retaining it would fire the #582 below-band arm on ordinary self-backoff
        // blindness the anchor gate deliberately leaves alone, and let the walk alarm claim a
        // "server throttle" that does not exist. The raw zero still rides the `UsageBackoff` event
        // + tick diagnostic below (#295) — this normalization is for the DECISION, not the report.
        self.state.accounts[i].health.poll_backoff_retry_after =
            signal.retry_after.filter(|ra| !ra.is_zero());
        let armed = until.saturating_duration_since(now);
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
            backoff_secs: armed.as_secs(),
        });
        // Carry the RAW server `Retry-After` (pre-cap) alongside the effective `armed` window so
        // the diagnostic tick line can LABEL the wait's source (issue #295): a `Some` marks a
        // server-advised floor, a `None` marks the self-capped exponential. Pre-cap keeps a
        // pathological value the #294 PEER clamp bit visible (`armed` ≪ `retry_after`).
        Some(TickBackoff {
            wait: armed,
            retry_after: signal.retry_after,
        })
    }

    /// Fold account `i`'s poll into its out-of-rotation slow-poll window (issue #537), the
    /// quota-exhaustion sibling of [`note_account_backoff`](Self::note_account_backoff)'s
    /// rate-limit back-off. Reads the account's freshly-stored reading
    /// (`accounts[i].last_reading`):
    ///
    /// - **NON-active peer, reading out of rotation** (`weekly >= weekly_rotation_line()` —
    ///   i.e. `weekly_ceiling_base − WEEKLY_TAIL_MARGIN`, issue #607 — `|| session >=
    ///   session_ceiling_base`): arm `exhausted_poll_until = now + <window>` (the
    ///   reset-aware [`exhausted_poll_window`]) so the peer's poll is skipped until the window
    ///   elapses. Edge-triggered ENTER — a durable [`Event::ExhaustedSlowPoll`] fires ONLY on
    ///   the normal→slow transition; a re-arm while the peer stays exhausted is not a new entry
    ///   (it never left the widened cadence), mirroring `note_account_backoff`'s
    ///   `was_backing_off` idiom.
    /// - **Viable again, OR the ACTIVE account** (exempt — full cadence): clear the window.
    ///   Edge-triggered EXIT — a durable [`Event::ExhaustedSlowPollCleared`] fires ONLY when a
    ///   window was actually armed, bracketing the episode; a plain viable poll stays silent.
    ///
    /// A FAILED poll (`accounts[i].last_reading` is `None`) carries NO exhaustion signal, so the window
    /// is left untouched — the peer keeps whatever window it had (exactly as a throttle carries
    /// the prior reading). `now` is the tick's monotonic [`Clock`] instant (the armed window's
    /// deadline); `now_secs` is the tick's wall-clock epoch (the `resets_at` delta) — both read
    /// once per tick and passed in, keeping the arithmetic on the pure [`exhausted_poll_window`]
    /// (the same read-at-tick-pass-into-a-pure-fn idiom as `keep_active_warm`'s `now_ms`).
    fn note_exhausted_poll(
        &mut self,
        i: usize,
        active: Option<usize>,
        now: Instant,
        now_secs: i64,
        events: &mut Vec<Event>,
    ) {
        // A failed poll carries no exhaustion signal — leave any window as-is.
        let Some(reading) = self.state.accounts[i].last_reading else {
            return;
        };
        // The ACTIVE account is EXEMPT (peers only): polled at full cadence, so by definition
        // NOT in the widened cadence. Treat it as "viable" so any armed window (e.g. a peer just
        // promoted to active via `use`) is cleared with the EXIT edge, and it is never armed.
        // Otherwise, out of rotation = at/above EITHER deterministic line (the same thresholds the
        // snapshot's `weekly_exhausted` verdict and the swap gate key off — weekly through the
        // #607 rotation line, so an account the daemon will not rotate onto is also the account it
        // slow-polls, with no band that is excluded from selection yet still polled at full rate).
        let out_of_rotation = active != Some(i)
            && (reading.weekly >= self.weekly_rotation_line()
                || reading.session >= self.session_ceiling_base);
        let account_uuid = self.roster[i].account_uuid.clone();
        if out_of_rotation {
            let window = exhausted_poll_window(
                &reading,
                // Issue #607: the same rotation line the `out_of_rotation` verdict above used, so
                // the widened re-poll window is computed against the line that armed it.
                self.weekly_rotation_line(),
                self.session_ceiling_base,
                now_secs,
                self.exhausted_poll_secs,
                // The floor is `poll_secs` — the un-jittered base of the poll strategy (the same
                // value `config.tunables.poll_secs` seeds it with); a slow-polled peer must never
                // re-poll faster than the normal cadence.
                self.poll_strategy.base as u64,
            );
            let health = &mut self.state.accounts[i].health;
            let was_slow_polling = health.exhausted_poll_until.is_some();
            // `window <= exhausted_poll_secs <= 86400 s`, so this bounded add cannot overflow the
            // monotonic instant (unlike `note_account_backoff`'s un-clamped active `Retry-After`).
            health.exhausted_poll_until = Some(now + window);
            if !was_slow_polling {
                events.push(Event::ExhaustedSlowPoll {
                    account: account_uuid,
                    window_secs: window.as_secs(),
                });
            }
        } else {
            let health = &mut self.state.accounts[i].health;
            let was_slow_polling = health.exhausted_poll_until.is_some();
            health.exhausted_poll_until = None;
            if was_slow_polling {
                events.push(Event::ExhaustedSlowPollCleared {
                    account: account_uuid,
                });
            }
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

/// The widened slow-poll window for an out-of-rotation reading (issue #537): how long a
/// weekly- or session-exhausted NON-active peer's poll is deferred.
///
/// `min(exhausted_poll_secs, max(soonest_applicable_resets_at - now, floor))`, where the
/// "applicable" reset is the SOONEST `resets_at` among the dimensions that are actually
/// exhausted (weekly's when weekly-exhausted, session's when session-exhausted; the sooner
/// of the two when both are), and `floor` = `poll_secs`. When no applicable `resets_at` is
/// known (absent / unparseable), it falls back to the full `exhausted_poll_secs` hourly
/// ceiling. Rationale (issue #537): the hourly ceiling bounds worst-case blindness for the
/// RARE server-side early reset; a known `resets_at` (which the daemon already retains) pulls
/// the next poll EARLIER so a window that elapses sooner than an hour is caught promptly. The
/// floor guards the degenerate `resets_at <= now` case (a server that is late resetting) from
/// a busy re-poll every tick, and keeps a slow-polled peer from ever re-polling FASTER than a
/// normal account's `poll_secs` cadence.
///
/// Pure — a function of the reading, the two base triggers, an explicit wall-clock `now_secs`
/// (for the reset delta), and the two config bounds — so the arithmetic is unit-tested without
/// a daemon or a real clock; the caller arms `exhausted_poll_until = monotonic_now + <this>`.
/// With the validated `exhausted_poll_secs >= poll_secs` the result lands in
/// `poll_secs..=exhausted_poll_secs` (both positive), so the caller's `Instant + Duration`
/// cannot underflow.
fn exhausted_poll_window(
    reading: &Usage,
    weekly_ceiling: f64,
    session_ceiling: f64,
    now_secs: i64,
    exhausted_poll_secs: u64,
    poll_secs: u64,
) -> Duration {
    // The soonest reset among the EXHAUSTED dimensions only — the window is keyed off a reset
    // that actually gates this peer's return to rotation. A dimension below its trigger does
    // not contribute its reset (it is not why the peer is out of rotation).
    let mut soonest: Option<i64> = None;
    let mut consider = |resets_at: Option<i64>| {
        if let Some(at) = resets_at {
            soonest = Some(soonest.map_or(at, |cur: i64| cur.min(at)));
        }
    };
    if reading.weekly >= weekly_ceiling {
        consider(reading.weekly_resets_at);
    }
    if reading.session >= session_ceiling {
        consider(reading.session_resets_at);
    }
    let ceiling = exhausted_poll_secs as i64;
    let floor = poll_secs as i64;
    let secs = match soonest {
        // Reset-aware: poll again by the known reset, but never sooner than the floor and
        // never later than the hourly ceiling. A reset at/behind `now` collapses to the floor.
        Some(resets_at) => (resets_at - now_secs).max(floor).min(ceiling),
        // No applicable reset known → the plain hourly ceiling (issue #537 fallback).
        None => ceiling,
    };
    // `secs` lands in `floor..=ceiling` (with the validated `poll_secs <= exhausted_poll_secs`);
    // the `max(0)` is a belt-and-suspenders non-negativity guard for the cast.
    Duration::from_secs(secs.max(0) as u64)
}

/// WHEN a blocked spare returns to viability, and the dimension gating that return.
///
/// A spare needs EVERY dimension currently blocking it to clear, so it returns at the
/// LATEST reset among those dimensions — and the gating cause is that latest one. The
/// caller establishes `session_blocked` / `weekly_blocked` against its own block lines;
/// this answers only WHEN.
///
/// `None` when a blocking dimension reports no parseable reset: the return moment is then
/// UNKNOWABLE (the unknown window may clear last), so the caller holds with the timestamp
/// omitted rather than quoting an optimistic ETA off whichever dimension happens to be
/// known. Also `None` for a spare blocked on neither dimension — it is not blocked at all.
fn spare_relief(
    usage: &Usage,
    session_blocked: bool,
    weekly_blocked: bool,
) -> Option<(i64, SwapReason)> {
    let session = if session_blocked {
        Some((usage.session_resets_at?, SwapReason::Session))
    } else {
        None
    };
    let weekly = if weekly_blocked {
        Some((usage.weekly_resets_at?, SwapReason::Weekly))
    } else {
        None
    };
    match (session, weekly) {
        // Blocked on BOTH: it returns only once the LATER window clears, named by that
        // dimension. An exact tie names WEEKLY — the scarcer window, and the #11 default
        // that #398's session refinement was layered onto.
        (Some(s), Some(w)) => Some(if w.0 >= s.0 { w } else { s }),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// Classify why [`pick_target`] found no viable target, for the `all_exhausted`
/// relief hint (issue #398): `(cause, hold_idx, resets_at)`.
///
/// Fleet relief is the SOONEST moment any spare returns to capacity:
///
/// ```text
/// min over blocked spares of ( max over that spare's blocked dimensions of its reset )
/// ```
///
/// The inner `max` is [`spare_relief`]; the outer `min` is this function. `cause` names the
/// dimension gating the WINNING spare, and `hold_idx` names that spare — the account relief
/// actually arrives on.
///
/// Both dimensions are compared on the SAME footing, because a window's LENGTH is not its
/// time REMAINING: a spare six days into its weekly window returns sooner than one that just
/// opened a session. Issue #665 corrects the superseded rule, which early-returned
/// [`SwapReason::Session`] the moment ANY weekly-viable-but-session-blocked spare existed and
/// so never compared the weekly-exhausted spares' resets — overstating the ETA on a live
/// 6-account fleet by 2h and mislabelling `hold`. It rested on a false universal (such a
/// spare "returns at its SESSION reset, sooner than any weekly reset") that conflated the
/// two. The `max` is sound because BOTH windows HARD-reset: weekly is a fixed 7-day window
/// (verified against 22,261 usage samples, #665) and session is monotonic-within-window
/// (#614 `SessionHighWater`).
///
/// ONE masked pass serves every dimension, so the active and disabled accounts are excluded
/// identically throughout — the superseded weekly fallback iterated ALL readings unmasked and
/// could key relief off the active's or a disabled account's weekly reset (#665).
///
/// `resets_at` is `None` when no blocked spare reports a usable reset (the
/// forward-compatible "hold, timestamp omitted" case); with no blocked spare at all it falls
/// back to holding the active account (the #11 default).
fn all_exhausted_relief(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    session_block_line: f64,
    weekly_block_line: f64,
) -> (SwapReason, usize, Option<i64>) {
    // Soonest spare-return across the fleet, plus a naming fallback (the first blocked
    // spare) for when none of them reports a usable reset.
    let mut relief: Option<(usize, i64, SwapReason)> = None;
    let mut first_blocked: Option<(usize, SwapReason)> = None;
    for (i, reading) in readings.iter().enumerate() {
        if i == active || !enabled[i] {
            continue;
        }
        let Some(usage) = reading else { continue };
        let session_blocked = usage.session >= session_block_line;
        let weekly_blocked = usage.weekly >= weekly_block_line;
        if !session_blocked && !weekly_blocked {
            continue;
        }
        first_blocked.get_or_insert((
            i,
            if weekly_blocked {
                SwapReason::Weekly
            } else {
                SwapReason::Session
            },
        ));
        if let Some((at, cause)) = spare_relief(usage, session_blocked, weekly_blocked) {
            // Strict `<` keeps the earliest roster index on an exact tie.
            if relief.is_none_or(|(_, best, _)| at < best) {
                relief = Some((i, at, cause));
            }
        }
    }
    match (relief, first_blocked) {
        (Some((idx, at, cause)), _) => (cause, idx, Some(at)),
        // Blocked spares exist, but none reports a usable reset — hold, timestamp omitted.
        (None, Some((idx, cause))) => (cause, idx, None),
        // No blocked spare at all (every other reading unpolled, or masked away): the #11
        // default of holding the active account, with nothing to promise.
        (None, None) => (SwapReason::Weekly, active, None),
    }
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
    pub(super) struct FakeClock {
        now: Cell<Instant>,
        step: Duration,
    }

    impl FakeClock {
        pub(super) fn new(step: Duration) -> Self {
            Self {
                now: Cell::new(Instant::now()),
                step,
            }
        }
        pub(super) fn frozen() -> Self {
            Self::new(Duration::ZERO)
        }
        pub(super) fn advance(&self, by: Duration) {
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
    pub(super) enum Scripted {
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
    pub(super) struct FakeRosterPoller {
        readings: HashMap<String, Scripted>,
    }

    impl FakeRosterPoller {
        pub(super) fn new() -> Self {
            Self {
                readings: HashMap::new(),
            }
        }
        pub(super) fn ok(mut self, uuid: &str, session: f64, weekly: f64) -> Self {
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
        pub(super) fn ok_resets(
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
        /// Like [`ok`](Self::ok) but with a known SESSION `resets_at` (epoch seconds) —
        /// the out-of-rotation slow-poll tests (issue #537) script a session-exhausted
        /// peer's session-window reset through this.
        pub(super) fn ok_resets_session(
            mut self,
            uuid: &str,
            session: f64,
            weekly: f64,
            session_resets_at: i64,
        ) -> Self {
            self.readings.insert(
                uuid.to_owned(),
                Scripted::Ok(Usage {
                    session,
                    weekly,
                    weekly_resets_at: None,
                    session_resets_at: Some(session_resets_at),
                }),
            );
            self
        }
        pub(super) fn failing(mut self, uuid: &str) -> Self {
            self.readings.insert(uuid.to_owned(), Scripted::Transient);
            self
        }
        /// Script a `429` rate-limit, optionally carrying a `Retry-After` (issue
        /// #76) — exercises the poll back-off path.
        pub(super) fn rate_limited(mut self, uuid: &str, retry_after: Option<Duration>) -> Self {
            self.readings
                .insert(uuid.to_owned(), Scripted::RateLimited(retry_after));
            self
        }
        pub(super) fn unauthorized(mut self, uuid: &str) -> Self {
            self.readings
                .insert(uuid.to_owned(), Scripted::Unauthorized);
            self
        }
        pub(super) fn keychain_locked(mut self, uuid: &str) -> Self {
            self.readings.insert(uuid.to_owned(), Scripted::Locked);
            self
        }
        pub(super) fn scope_missing(mut self, uuid: &str) -> Self {
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
    pub(super) struct FakeShutdown {
        calls: Cell<u32>,
        stop_at: u32,
    }

    impl FakeShutdown {
        pub(super) fn after(stop_at: u32) -> Self {
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
    pub(super) struct NoControl;

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
    pub(super) struct RecordingControl {
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
    pub(super) struct NoopRefreshTicker;

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
    pub(super) struct NoopExternalLoginWatch;

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
    pub(super) struct OnceExternalLogin {
        fired: Cell<bool>,
        store: Rc<FakeCredentialStore>,
        fresh: Vec<u8>,
    }

    impl OnceExternalLogin {
        pub(super) fn new(store: Rc<FakeCredentialStore>, fresh: &[u8]) -> Self {
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
    pub(super) struct ScriptedExternalLogin {
        pub(super) fired: Cell<bool>,
        pub(super) result: Option<Credential>,
        pub(super) probed: Cell<bool>,
    }

    impl ScriptedExternalLogin {
        pub(super) fn returning(result: Option<Credential>) -> Self {
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
    pub(super) struct OnceRefreshTicker {
        pub(super) fired: Cell<bool>,
        pub(super) swept: RefCell<Vec<Vec<String>>>,
        pub(super) swept_quarantined: RefCell<Vec<Vec<String>>>,
        pub(super) outcome: RefCell<SweepOutcome>,
        /// The `has_recovery_work` flag each `until_due` call was handed (issue #280), in call
        /// order — so a run-loop test can prove the daemon threads the "≥1 quarantined-parked"
        /// signal into the tick's DUE computation (not only into `sweep`).
        pub(super) due_recovery: RefCell<Vec<bool>>,
    }

    impl OnceRefreshTicker {
        pub(super) fn new() -> Self {
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
        pub(super) fn returning(outcome: SweepOutcome) -> Self {
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
    pub(super) struct HangingRefreshTicker {
        fired: Cell<bool>,
    }

    impl HangingRefreshTicker {
        pub(super) fn new() -> Self {
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
    pub(super) struct OnceManualSwap {
        fired: Cell<bool>,
    }

    impl OnceManualSwap {
        pub(super) fn new() -> Self {
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
    pub(super) struct OnceRosterReload {
        fired: Cell<bool>,
    }

    impl OnceRosterReload {
        pub(super) fn new() -> Self {
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
    pub(super) struct OnceRestored {
        uuid: String,
        fired: Cell<bool>,
    }

    impl OnceRestored {
        pub(super) fn new(uuid: &str) -> Self {
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
    pub(super) struct OnceShutdown {
        fired: Cell<bool>,
    }

    impl OnceShutdown {
        pub(super) fn new() -> Self {
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
    pub(super) struct OnceSwap {
        pub(super) command: SwapCommand,
        pub(super) stream: RefCell<Option<tokio::net::UnixStream>>,
        pub(super) fired: Cell<bool>,
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
    pub(super) struct OnceCapture {
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

    pub(super) fn account(uuid: &str, label: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            label: label.to_owned(),
            enabled: true,
        }
    }

    /// A roster account that starts parked (issue #36) — for the disable paths.
    pub(super) fn disabled_account(uuid: &str, label: &str) -> Account {
        Account {
            enabled: false,
            ..account(uuid, label)
        }
    }

    pub(super) fn tunables(session_ceiling: u8, floor: u8, cooldown: u64) -> Tunables {
        // Weekly trigger fixed high (98) so the existing tests' weekly readings
        // (all well below it) never trip the new weekly path (issue #41): these
        // tests pin the SESSION trigger. A fixed strategy draws no RNG, so the
        // per-cycle draw sequence — and every seeded-jitter test — is unchanged.
        const WEEKLY_CEILING: u8 = 98;
        Tunables {
            poll_secs: 60,
            // The out-of-rotation slow-poll cadence (issue #537), default 3600. Baseline daemon
            // tests never let a peer stay exhausted across ticks, so this is inert for them; the
            // slow-poll tests drive it explicitly.
            exhausted_poll_secs: 3600,
            // The near-limit fast-poll (issue #540) is OFF by default here (the `0` kill-switch), so
            // baseline daemon tests keep their exact `poll_secs/N` cadence and emit no
            // `NearLimitPollCoverage` event; the #540 timing-seam tests set it explicitly to arm it.
            near_limit_poll_secs: 0,
            cooldown_secs: cooldown,
            // Most daemon tests set an explicit floor; `tunables_floor_off` sets it
            // inert (== session_ceiling) for the tests that pin the always-on gate instead.
            target_max_session_usage: floor,
            session_ceiling,
            weekly_ceiling: WEEKLY_CEILING,
            // Issue #452 bounded-blindness preemptive swap: INERT by default (T parked at
            // the kill-switch ceiling) so baseline daemon tests are unperturbed by the new
            // gate; the blind-swap tests override `session_blind_swap_secs` to arm it.
            session_blind_swap_secs: 86_400,
            session_blind_risk_band: 60,
            // Issue #539 velocity-projection preemptive trigger: INERT by default (horizon parked
            // at 0, the kill-switch) so baseline daemon tests are unperturbed by the new projective
            // gate; the velocity-swap tests override `session_velocity_horizon_secs` to arm it.
            session_velocity_horizon_secs: 0,
            session_velocity_min_project_above: 85,
            session_velocity_ema_alpha_pct: 50,
            monitor_401_n: 3,
            monitor_recovery_m: 2,
            // Existing daemon tests exercise the fixed (no-jitter) path: each
            // strategy draws its base verbatim, identical to the pre-#38 scalars.
            poll_strategy: Strategy::fixed(60.0),
            session_ceiling_strategy: Strategy::fixed(f64::from(session_ceiling)),
            weekly_ceiling_strategy: Strategy::fixed(f64::from(WEEKLY_CEILING)),
            cooldown_strategy: Strategy::fixed(cooldown as f64),
        }
    }

    /// Tunables with the target-max-session-usage reserve INERT — set to `session_ceiling`, so
    /// `pick_target`'s floor filter never tightens beyond the always-on session gate
    /// (config allows `target_max_session_usage == session_ceiling`). Post-#398 the floor is
    /// always-valued, so "no extra tightening" is expressed this way rather than the
    /// removed opt-out; behaviorally identical to the old `None` for target selection.
    /// The tests that use it pin the always-on gate / weekly behavior, not the reserve.
    pub(super) fn tunables_floor_off(session_ceiling: u8, cooldown: u64) -> Tunables {
        tunables(session_ceiling, session_ceiling, cooldown)
    }

    pub(super) fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    pub(super) fn oauth(uuid: &str) -> OauthAccount {
        OauthAccount::from_object_bytes(
            format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{uuid}@example.com"}}"#).as_bytes(),
        )
        .unwrap()
    }

    pub(super) fn stashed(token: &[u8], uuid: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(token),
            oauth_account: oauth(uuid),
        }
    }

    /// A temp `~/.claude.json` displaying `uuid`. Returns the tempdir guard + path.
    pub(super) fn claude_json(uuid: &str) -> (tempfile::TempDir, PathBuf) {
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

    pub(super) fn displayed_uuid(path: &Path) -> Option<String> {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        value["oauthAccount"]["accountUuid"]
            .as_str()
            .map(str::to_owned)
    }

    pub(super) async fn store_holding(blob: &[u8]) -> FakeCredentialStore {
        let store = FakeCredentialStore::empty();
        store.write(&cred(blob)).await.unwrap();
        store
    }

    pub(super) async fn stash_with(entries: &[(&str, &[u8], &str)]) -> FakeAccountStash {
        let stash = FakeAccountStash::empty();
        for (service, token, uuid) in entries {
            stash.write(service, &stashed(token, uuid)).await.unwrap();
        }
        stash
    }

    pub(super) type FakeDaemon =
        Daemon<FakeRosterPoller, FakeCredentialStore, FakeAccountStash, FakeClock>;

    /// Write a minimal valid `config.toml` at `path` carrying `accounts` as `(uuid,
    /// label)` pairs — the on-disk fixture the runtime roster-reload (#139) re-reads.
    /// Tunables are omitted (they default via `#[serde(default)]`), so this exercises
    /// the exact `Config::load_path` path `adopt_roster_reload` takes. Written in one
    /// `std::fs::write`, so a reader sees a complete file (production's atomic rename
    /// gives the same all-or-nothing guarantee).
    pub(super) fn write_roster_config(path: &Path, accounts: &[(&str, &str)]) {
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
    pub(super) async fn warmed_tick(daemon: &mut FakeDaemon) -> TickOutcome {
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

    /// The per-poll `diag=canonical` line every tick now emits (issue #464). These hermetic
    /// tests seed the canonical store with an OPAQUE token (`b"A-token"`, etc.) rather than a
    /// `claudeAiOauth`-shaped blob, so `refresh::refresh_token` cannot parse it — the item is
    /// present but its liveness is UNKNOWN (never a false scrub), with the handle the resolved
    /// active account's label. The expected leading diagnostic for any tick of such a daemon.
    pub(super) fn canonical_unknown_diag(active_label: &str) -> Diagnostic {
        Diagnostic::Canonical {
            state: CanonicalLiveness::Unknown,
            fingerprint: None,
            account: Some(active_label.to_owned()),
            expires_at: None,
            rotated_from: None,
        }
    }

    // Fixtures shared across the issue-659 cluster split: each is reached
    // from a child module's `mod tests` through `use crate::daemon::tests::*`,
    // so they live HERE rather than in whichever cluster first needed them.

    /// A three-account daemon (`work` active, `spare`, `backup`) with the canonical
    /// holding `work`'s token — the fixture for the scheduling tests below. The
    /// caller supplies the poller so each test scripts its own per-account readings.
    pub(super) async fn three_account_daemon(poller: FakeRosterPoller) -> FakeDaemon {
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

    /// A minimal daemon for the pure [`reconcile_roster`] tests (#139): reconcile
    /// touches no seam (no poll / store / clock / json read), so inert fixtures and a
    /// throwaway `claude_json` path suffice. State is seeded directly on `daemon.state`.
    pub(super) fn reconcile_daemon(roster: Vec<Account>) -> FakeDaemon {
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

    /// A carried usage reading for seeding an account's `last_reading` in the reconcile tests.
    pub(super) fn reading(session: f64, weekly: f64) -> Usage {
        Usage {
            session,
            weekly,
            weekly_resets_at: None,
            session_resets_at: None,
        }
    }

    pub(super) fn roster_uuids(daemon: &FakeDaemon) -> Vec<String> {
        daemon
            .roster
            .iter()
            .map(|a| a.account_uuid.clone())
            .collect()
    }

    /// A two-account daemon (`work` active, `spare` spare) with both tokens stashed
    /// and the canonical holding `work`'s — the common fixture for the lifecycle
    /// tests below. `monitor_401_n` = 3, `monitor_recovery_m` = 2 (the test defaults).
    pub(super) async fn lifecycle_daemon() -> FakeDaemon {
        lifecycle_daemon_with(FakeRosterPoller::new(), tunables(95, 80, 0)).await
    }

    /// Like [`lifecycle_daemon`] but with a caller-chosen poller + tunables, for the
    /// tick-driven tests that script per-account poll outcomes.
    pub(super) async fn lifecycle_daemon_with(
        poller: FakeRosterPoller,
        tun: Tunables,
    ) -> FakeDaemon {
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

    pub(super) fn live(session: f64, weekly: f64) -> Result<Usage> {
        Ok(Usage {
            session,
            weekly,
            weekly_resets_at: None,
            session_resets_at: None,
        })
    }

    /// Shared, mutable per-account poll outcome the #162 seam tests drive: [`SeamPoller`]
    /// reads the CURRENT outcome and [`SeamRefresh`] may FLIP it (revive an expired token)
    /// on refresh — modelling the poll↔refresh seam the fix composes. Keyed by
    /// `account_uuid`, mirroring [`FakeRosterPoller`].
    pub(super) type SeamOutcomes = Rc<RefCell<HashMap<String, Scripted>>>;

    /// A [`RosterPoller`] reading its per-account outcome from a shared [`SeamOutcomes`]
    /// cell, so a refresh that revives a token is observed on the very next poll.
    pub(super) struct SeamPoller {
        pub(super) outcomes: SeamOutcomes,
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

    /// A far-future access-token expiry (epoch ms, ~year 2100): well beyond any keep-warm
    /// horizon, so a canonical carrying it never trips the PROACTIVE near-expiry gate — used to
    /// ISOLATE the reactive path in the `tick`-driven tests (only the scripted 401 fires it).
    pub(super) const FAR_FUTURE_MS: i64 = 4_102_444_800_000;

    /// A realistic active canonical blob: the non-secret `expiresAt` (epoch ms) beside a
    /// `refreshToken` — an EMPTY `refresh_token` is the DEAD signal (`has_live_refresh_token`
    /// returns false), the invariant-4 case the keep-warm must NOT try to revive.
    pub(super) fn warm_canonical(expires_at_ms: i64, refresh_token: &str) -> Credential {
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
    pub(super) struct SeamKeepWarm {
        pub(super) outcomes: SeamOutcomes,
        pub(super) outcome: RefreshOutcome,
        pub(super) revive_to: Option<Usage>,
        pub(super) fresh: Option<Credential>,
        pub(super) calls: Rc<Cell<u32>>,
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
    pub(super) async fn keep_warm_daemon(
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
        )
        // Issue #468: opt the proactive path IN for the seam tests (default is off) so the
        // existing proactive assertions still exercise it; the new gate test overrides to `false`.
        .with_proactive_keep_warm(true);
        (daemon, outcomes, calls)
    }

    // --- all_exhausted_relief (pure, #11 / #398 / #665) -------------------

    /// A weekly-exhausted (`weekly = 0.99`), session-viable spare with the given weekly reset —
    /// the shape the #11 weekly-wide branch is built on.
    fn weekly_exhausted(weekly_resets_at: Option<i64>) -> Option<Usage> {
        Some(Usage {
            session: 0.0,
            weekly: 0.99,
            weekly_resets_at,
            session_resets_at: None,
        })
    }

    #[test]
    fn all_exhausted_relief_picks_the_earliest_known_reset_and_skips_unknowns() {
        // Ported from the retired `soonest_weekly_reset` unit tests (issue #665 folded that
        // helper into the one masked pass): among weekly-exhausted spares the hint keys off
        // the EARLIEST known reset, spares without a parseable reset are skipped rather than
        // treated as soonest, and an exact tie keeps the earliest roster index. The masked-out
        // active (idx 0) holds the earliest reset of all and must NOT win.
        let readings = vec![
            weekly_exhausted(Some(10)), // idx 0: the active — masked, would win unmasked.
            weekly_exhausted(None),     // idx 1: unknown reset — skipped, not "soonest".
            weekly_exhausted(Some(300)), // idx 2: first of the tie → the winner.
            weekly_exhausted(Some(300)), // idx 3: same instant, later index → loses.
            weekly_exhausted(Some(700)), // idx 4: later.
        ];
        let enabled = vec![true; 5];
        let (cause, hold_idx, resets_at) = all_exhausted_relief(0, &readings, &enabled, 0.80, 0.95);
        assert_eq!(cause, SwapReason::Weekly);
        assert_eq!(hold_idx, 2);
        assert_eq!(resets_at, Some(300));
    }

    #[test]
    fn all_exhausted_relief_holds_without_a_timestamp_when_no_reset_is_known() {
        // The forward-compatible "hold, timestamp omitted" case: blocked spares exist but none
        // reports a parseable reset, so the hint names the first blocked spare with NO ETA
        // rather than inventing one.
        let readings = vec![
            weekly_exhausted(None),
            weekly_exhausted(None),
            weekly_exhausted(None),
        ];
        let enabled = vec![true; 3];
        assert_eq!(
            all_exhausted_relief(0, &readings, &enabled, 0.80, 0.95),
            (SwapReason::Weekly, 1, None),
        );
        // With no blocked spare AT ALL (every other reading unpolled) it falls back to holding
        // the active account — the #11 default, still with nothing to promise.
        let unpolled = vec![weekly_exhausted(None), None, None];
        assert_eq!(
            all_exhausted_relief(0, &unpolled, &enabled, 0.80, 0.95),
            (SwapReason::Weekly, 0, None),
        );
    }

    #[test]
    fn all_exhausted_relief_names_the_soonest_session_reset_among_blocked_spares() {
        // #417 secondary: among session-blocked spares the relief hint keys off the SOONEST
        // reset — the outer `min`'s `relief.is_none_or(|(_, best, _)| at < best)` (ADR-0013
        // Decision 4, as corrected to cross-dimension semantics by #665; every spare here is
        // session-only-blocked, so the winner's cause is still Session). The all-`None`
        // coverage above exercises only the fallback-naming arm, so an inverted comparison
        // (`at > best`) or a wrong index would ship green. Here TWO spares qualify with
        // DISTINCT session resets and the later-indexed one resets sooner, so a correct
        // comparison must override the first-seen fallback.
        let session_block_line = 0.80_f64;
        let weekly_ceiling = 0.95_f64;
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
            all_exhausted_relief(0, &readings, &enabled, session_block_line, weekly_ceiling);
        assert_eq!(cause, SwapReason::Session);
        // idx 2 (soonest, 150) wins over idx 1 (first-seen fallback, 300).
        assert_eq!(hold_idx, 2);
        assert_eq!(resets_at, Some(150));
    }

    #[test]
    fn all_exhausted_relief_keys_off_a_weekly_spare_returning_before_every_session_spare() {
        // Issue #665, the core regression. The superseded rule EARLY-RETURNED `Session` the
        // moment ANY weekly-viable-but-session-blocked spare existed, so it never compared the
        // weekly-exhausted spares' resets — pinning the ETA to the session subset even when a
        // weekly-exhausted spare returned to capacity FIRST. Modelled on the live 6-account
        // fleet from the issue (block lines: session ≥ 0.80, weekly ≥ 0.97), where the hint
        // said "over its session limit; resets in 3h4m" while a weekly-exhausted spare was
        // ~1h out — a 2h overstatement, on the wrong account, with the wrong cause.
        let readings = vec![
            // idx 0: the exhausted active — masked (active == 0).
            Some(Usage {
                session: 0.99,
                weekly: 0.99,
                weekly_resets_at: Some(9_000),
                session_resets_at: Some(9_000),
            }),
            // idx 1: weekly-EXHAUSTED, session-viable → returns at its weekly reset, SOONEST.
            Some(Usage {
                session: 0.0,
                weekly: 0.98,
                weekly_resets_at: Some(3_600),
                session_resets_at: None,
            }),
            // idx 2: weekly-viable, session-BLOCKED → returns at its session reset, LATER.
            Some(Usage {
                session: 0.96,
                weekly: 0.53,
                weekly_resets_at: None,
                session_resets_at: Some(10_800),
            }),
        ];
        let enabled = vec![true, true, true];
        // Pre-fix this returned `(Session, 2, Some(10_800))` — the overstated ETA, naming the
        // session-blocked spare instead of the one relief actually arrives on.
        let (cause, hold_idx, resets_at) = all_exhausted_relief(0, &readings, &enabled, 0.80, 0.97);
        assert_eq!(cause, SwapReason::Weekly);
        assert_eq!(hold_idx, 1);
        assert_eq!(resets_at, Some(3_600));
    }

    #[test]
    fn all_exhausted_relief_takes_the_later_reset_of_a_double_blocked_spare() {
        // The inner `max` (issue #665): a spare blocked on BOTH dimensions returns only once
        // the LATER window clears, so fleet relief is min-over-spares of MAX-over-that-spare's-
        // blocked-dimensions — never a flat min across every reset in the fleet. Here the
        // double-blocked idx 1 has the earliest reset of all (its session, 1_000), but its
        // weekly does not clear until 8_000, so the single-blocked idx 2 (5_000) wins.
        let readings = vec![
            // idx 0: the exhausted active — masked.
            Some(Usage {
                session: 0.99,
                weekly: 0.99,
                weekly_resets_at: Some(9_000),
                session_resets_at: Some(9_000),
            }),
            // idx 1: session-blocked AND weekly-exhausted → returns at max(1_000, 8_000).
            Some(Usage {
                session: 0.90,
                weekly: 0.99,
                weekly_resets_at: Some(8_000),
                session_resets_at: Some(1_000),
            }),
            // idx 2: session-blocked only → returns at 5_000, the true fleet soonest.
            Some(Usage {
                session: 0.85,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: Some(5_000),
            }),
        ];
        let enabled = vec![true, true, true];
        // A flat `min` over all resets would report idx 1 at 1_000 — an account still weekly-
        // exhausted for another ~2h at that moment.
        let (cause, hold_idx, resets_at) = all_exhausted_relief(0, &readings, &enabled, 0.80, 0.97);
        assert_eq!(cause, SwapReason::Session);
        assert_eq!(hold_idx, 2);
        assert_eq!(resets_at, Some(5_000));
        // A double-blocked spare whose blocking weekly reset is UNKNOWN has an unknowable
        // return, so it is skipped rather than quoted at its known session reset.
        let unknowable = vec![
            readings[0],
            Some(Usage {
                weekly_resets_at: None,
                ..readings[1].expect("idx 1 present")
            }),
            readings[2],
        ];
        assert_eq!(
            all_exhausted_relief(0, &unknowable, &enabled, 0.80, 0.97),
            (SwapReason::Session, 2, Some(5_000)),
        );
        // Intra-spare tie: when a double-blocked spare's two resets land on the SAME instant,
        // the gating cause is WEEKLY (the `>=` in `spare_relief`) — the scarcer window,
        // deterministic rather than iteration-order-dependent.
        let tie = vec![
            readings[0], // idx 0: the active — masked.
            Some(Usage {
                session: 0.90,
                weekly: 0.99,
                weekly_resets_at: Some(4_000),
                session_resets_at: Some(4_000),
            }),
        ];
        assert_eq!(
            all_exhausted_relief(0, &tie, &[true, true], 0.80, 0.97),
            (SwapReason::Weekly, 1, Some(4_000)),
        );
    }

    #[test]
    fn all_exhausted_relief_masks_the_active_and_disabled_accounts_on_every_dimension() {
        // Issue #665: the superseded weekly fallback called `soonest_weekly_reset(readings)`,
        // which iterated ALL readings with no active/enabled mask — so it could key relief off
        // the ACTIVE account's or a DISABLED account's weekly reset, neither of which is a
        // spare the daemon can swap to. The session branch always masked both; one masked pass
        // now makes the asymmetry structurally impossible.
        let readings = vec![
            weekly_exhausted(Some(100)), // idx 0: the active — earliest, must be ignored.
            weekly_exhausted(Some(200)), // idx 1: DISABLED — next earliest, must be ignored.
            weekly_exhausted(Some(700)), // idx 2: the only real spare → the answer.
        ];
        let enabled = vec![true, false, true];
        // Pre-fix this returned `(Weekly, 0, Some(100))` — the active account's own reset.
        assert_eq!(
            all_exhausted_relief(0, &readings, &enabled, 0.80, 0.95),
            (SwapReason::Weekly, 2, Some(700)),
        );
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
                canonical_unknown_diag("work"),
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
                canonical_unknown_diag("work"),
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
    async fn tick_swaps_one_weekly_tail_margin_below_the_weekly_ceiling() {
        // Issue #607 (the headline AC): the weekly arm fires BACKWARD from the weekly ceiling, at
        // `ceiling − WEEKLY_TAIL_MARGIN`, so the parked account lands BELOW the ceiling after its
        // post-swap committed tail. Weekly 0.97 with the helper's 98 ceiling is EXACTLY the
        // effective weekly ceiling: pre-#607 this HELD (0.97 < the raw 0.98 fire-at line) and the
        // in-flight tail then carried the parked account past 98 unreachably; now it swaps.
        // Session is held at 0.50 throughout so the weekly axis is isolated.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.97)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0); // session ceiling 95, weekly ceiling 98 → weekly fires at 97

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

        assert_eq!(
            outcome.action,
            TickAction::Swapped { from: 0, to: 1 },
            "the weekly arm fires at the effective weekly ceiling, one tail margin early",
        );
        assert_eq!(daemon.state.active, Some(1));
        // Attributed to the WEEKLY dimension — session never crossed its own (derived) threshold.
        assert!(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, Event::Swap { reason, .. } if *reason == SwapReason::Weekly)),
            "an early weekly fire is attributed to Weekly, not Session: {:?}",
            outcome.events,
        );
    }

    #[tokio::test]
    async fn tick_holds_just_below_the_effective_weekly_ceiling() {
        // Issue #607's other edge: the fire point MOVED, it did not disappear. A weekly reading
        // just under the effective ceiling (0.965 < 0.97) still HOLDS — the arm is strictly-early
        // by exactly one tail margin, not unboundedly early. Together with the test above this
        // pins the new boundary from both sides.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.965)
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
        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(outcome.action, TickAction::Held);
        assert_eq!(daemon.state.active, Some(0));
    }

    #[tokio::test]
    async fn a_weekly_saturated_active_does_not_swap_onto_a_band_peer() {
        // Issue #607, the reactive anti-thrash core (`decide_action`'s live `pick_target_ranked`).
        // The active is weekly-saturated (0.99 ≥ the 0.97 effective ceiling → `decide` fires
        // Weekly), and the ONLY peer sits in the tail-margin band (0.975 ∈ `[0.97, 0.98)`). The
        // acquire predicate must be at least as strict as the release predicate: the band peer is
        // NOT a viable target, so the daemon must report `NoViableTarget` and HOLD — never swap onto
        // it, which would re-trip `decide`'s weekly dimension next cycle and ping-pong. Against the
        // RAW ceiling `pick_target_ranked` would accept the 0.975 peer and swap, so reverting the
        // `decide_action` call site to the raw ceiling fails here. The emitted relief also names the
        // WEEKLY cause (computed from the same effective line), pinning the relief call site too.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.99) // active: weekly-saturated → wants to swap away
            .ok("u-B", 0.10, 0.975); // sole peer: inside the tail-margin band → NOT a viable target
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

        assert_eq!(
            outcome.action,
            TickAction::NoViableTarget,
            "a band peer is not viable, so a weekly-saturated active holds rather than thrashing",
        );
        assert_eq!(daemon.state.active, Some(0), "no swap landed");
        // The relief hint reads the same effective weekly line: the block is WEEKLY-wide (the band
        // peer is weekly-exhausted), not session. (An `ExhaustedSlowPoll` for the band peer rides
        // along — it is out of rotation, so the scheduler correctly widens its cadence too.)
        assert!(
            outcome.events.iter().any(|e| matches!(
                e,
                Event::AllExhausted {
                    cause: SwapReason::Weekly,
                    ..
                }
            )),
            "the all-exhausted relief must blame WEEKLY, computed from the effective line: {:?}",
            outcome.events,
        );
    }

    #[tokio::test]
    async fn the_all_exhausted_relief_cause_reads_the_effective_weekly_line() {
        // Issue #607: the all-exhausted relief HINT (`cause=session|weekly`) must reason against the
        // same effective weekly ceiling the viability filter used, or it explains the wrong block.
        // The sole peer is BOTH session-blocked (0.85 ≥ the 0.80 reserve) AND in the weekly band
        // (0.975 ∈ `[0.97, 0.98)`). Under the effective line the peer is weekly-exhausted, so the
        // block is WEEKLY-wide; under the RAW ceiling it would look weekly-VIABLE-but-session-blocked
        // and the hint would flip to `Session`. This flip is what pins the relief call sites — both
        // `decide_action`'s emitted `AllExhausted` event AND `next_swap`'s preview cause.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.99) // active weekly-saturated → wants to swap
            .ok("u-B", 0.85, 0.975); // peer session-blocked AND weekly-band → the flip case
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
        assert!(
            outcome.events.iter().any(|e| matches!(
                e,
                Event::AllExhausted {
                    cause: SwapReason::Weekly,
                    ..
                }
            )),
            "decide_action's relief must blame WEEKLY (peer is weekly-exhausted at the effective \
             line), not Session: {:?}",
            outcome.events,
        );
        // The `next_swap` preview reads the same effective line → the same WEEKLY cause.
        let readings = [
            Some(Usage {
                session: 0.10,
                weekly: 0.99,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.85,
                weekly: 0.975,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        assert_eq!(
            daemon.next_swap(Some(0), &readings),
            Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: None,
            }),
            "the preview's relief cause reads the same effective weekly line",
        );
    }

    #[tokio::test]
    async fn every_weekly_verdict_reads_the_rotation_line_not_the_raw_ceiling() {
        // Issue #607: moving the RELEASE point (`decide`) without moving every ACQUIRE-side and
        // operator-facing weekly verdict would open a band — `weekly ∈ [ceiling − margin, ceiling)`
        // = `[0.97, 0.98)` under the default 98 ceiling — in which the daemon refuses to rotate ONTO
        // an account while the UI, the `use` gate, the swap preview and the poll scheduler all still
        // call it viable. This pins the four DISPLAY/SCHEDULING verdicts against a peer parked
        // squarely in that band (0.975): each must read the ROTATION line, so reverting any one of
        // them to `weekly_ceiling_base` fails here. (The release side — `decide` — is pinned by the
        // two `tick_*` tests above; `pick_target`'s own filter by
        // `pick_target_excludes_the_weekly_tail_margin_band_so_the_arms_cannot_thrash`.)
        let daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let usage = |session: f64, weekly: f64| {
            Some(Usage {
                session,
                weekly,
                weekly_resets_at: None,
                session_resets_at: None,
            })
        };
        // work is the active near its session trigger; BOTH peers sit in the weekly band. Under the
        // raw 0.98 ceiling every one of these accounts reads "weekly fine".
        let readings = [usage(0.97, 0.40), usage(0.10, 0.975), usage(0.10, 0.975)];

        // 1. The swap PREVIEW (#88): no candidate the daemon would actually accept, and the block is
        //    weekly-wide — not `Target { to: "spare" }`, which would name an account the live swap
        //    path refuses. Also pins the relief hint riding along with the verdict.
        assert_eq!(
            daemon.next_swap(Some(0), &readings),
            Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: None,
            }),
            "the preview must name what the daemon would pick — a band peer is not pickable",
        );

        // 2. The wire snapshot's per-account `weekly_exhausted` verdict (#72): "exhausted" means
        //    exactly "the daemon will not rotate onto this", so a band peer reads EXHAUSTED. The
        //    active account is judged by the same line (it is above it too).
        let snap = daemon.snapshot(Some(0), &readings, 0);
        assert!(
            snap.accounts[1].weekly_exhausted && snap.accounts[2].weekly_exhausted,
            "a peer in the band is un-rotatable, so the UI must not report it as usable",
        );

        // 3. The refresh EXCLUSION set (#105/#106): it shadows the swap paths, so with no pickable
        //    target it names the ACTIVE account only — never a band peer as the "imminent target".
        let mut daemon = daemon;
        daemon.state.active = Some(0);
        daemon.state.seed_readings(readings);
        assert_eq!(
            daemon.refresh_exclusions(),
            vec!["u-A".to_owned()],
            "no viable target → the imminent-target exclusion is empty, not a band peer",
        );

        // 4. Control: pulling the peers one notch BELOW the rotation line (0.965 < 0.97) restores
        //    every verdict. Without this the three assertions above would also pass against a
        //    hypothetical always-exhausted implementation — it is the discriminating half.
        let viable = [usage(0.97, 0.40), usage(0.10, 0.965), usage(0.10, 0.965)];
        assert!(
            matches!(
                daemon.next_swap(Some(0), &viable),
                Some(NextSwap::Target { .. })
            ),
            "below the rotation line the same peers are pickable again",
        );
        assert!(!daemon.snapshot(Some(0), &viable, 0).accounts[1].weekly_exhausted);
    }

    #[tokio::test]
    async fn a_peer_inside_the_weekly_tail_margin_band_is_slow_polled() {
        // Issue #607, scheduling half of the band sweep above: an account the daemon will not
        // rotate onto must also be the account it stops polling at full cadence — otherwise the
        // band is excluded from selection yet still burns the poll budget. The peer reads 0.975,
        // inside `[0.97, 0.98)`, with a weekly reset ~10 min out. Two verdicts both key off the
        // effective line here: whether to slow-poll at all (`out_of_rotation`), and — because the
        // window keys off a reset that ACTUALLY gates the peer's return — that the window is pulled
        // to the weekly reset rather than the hourly ceiling. Against the raw 0.98 ceiling the peer
        // is neither out of rotation (polled every cycle, no ENTER edge) nor recognised as
        // weekly-exhausted by the window keying (which would fall back to the 3600 ceiling) — so
        // reverting EITHER the condition or the window arg to the raw ceiling fails here.
        let now = wall_clock_now_secs();
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok_resets("u-B", 0.10, 0.975, now + 600),
        )
        .await;
        let armed = next_peer_tick(&mut daemon).await;
        assert!(
            armed.events.iter().any(|e| matches!(
                e, Event::ExhaustedSlowPoll { account, window_secs }
                    // pulled to the ~600 s weekly reset (± wall drift), NOT the 3600 ceiling — proof
                    // the window keyed off weekly exhaustion via the effective line.
                    if account == "u-B" && (595..=600).contains(window_secs)
            )),
            "a band peer arms a reset-aware sub-ceiling window off its weekly reset: {:?}",
            armed.events,
        );
        for _ in 0..4 {
            let tick = daemon.tick().await;
            assert!(
                !peer_polled(&tick, "spare"),
                "a band peer must be skipped inside its slow-poll window",
            );
        }
    }

    #[tokio::test]
    async fn perform_socket_swap_refuses_a_target_inside_the_weekly_tail_margin_band() {
        // Issue #607, `use` half of the band sweep: the daemon-attached `use` gate re-validates
        // against the ROTATION line, so it refuses a band target (0.975) rather than accepting it
        // and letting the very next tick's `decide` swap straight back off it — an operator `use`
        // that silently undoes itself moments later. Against the raw 0.98 ceiling this swap is
        // ACCEPTED, so this test fails if the re-validation reverts.
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
            .ok("u-B", 0.10, 0.975);
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
            },
            "a band target is not rotatable, so `use` must refuse it up front",
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
    }

    #[tokio::test]
    async fn tick_holds_when_weekly_is_below_its_own_trigger_even_above_the_session_ceiling() {
        // Issue #41: weekly is gated by its OWN (higher) trigger, not the session
        // one. Weekly 0.96 sits ABOVE the 0.95 session trigger yet BELOW the 0.98
        // weekly trigger, and session itself (0.50) is below its trigger — so the
        // cycle HOLDS. (Under a single-threshold rule keyed on session_ceiling this
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
    async fn last_good_retains_the_pre_blind_reading_across_an_active_429() {
        // Issue #450 AC1: a `429` on the active account's poll clears `accounts[active].last_reading`
        // to `None` (the reactive `decide()` path is unchanged — it never swaps on missing
        // data), but the SEPARATE `last_good` anchor keeps the pre-blind reading + its
        // timestamp, so #452 can still reason about how near the band the active account was
        // when it went blind.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.68, 0.40)
                .ok("u-B", 0.05, 0.05)
                .ok("u-C", 0.05, 0.05),
        )
        .await;

        // Tick 1 polls the active `work` (u-A) — a clean reading seeds both the reactive slot
        // and the anchor.
        daemon.tick().await;
        assert_eq!(
            daemon.state.accounts[0]
                .last_reading
                .map(|r| (r.session, r.weekly)),
            Some((0.68, 0.40)),
        );
        let anchor = daemon
            .state
            .last_good
            .expect("a clean active poll sets last_good");
        assert_eq!((anchor.session, anchor.weekly), (0.68, 0.40));

        // The active now `429`s. Drive to its next scheduled poll — the #366 interleave
        // re-observes the active on tick 3 (`[active, peer, active, peer]`).
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.05, 0.05)
            .ok("u-C", 0.05, 0.05);
        daemon.tick().await; // tick 2 — peer `spare`
        daemon.tick().await; // tick 3 — active `work` 429s

        // Reactive path byte-for-byte unchanged: the slot is cleared to `None`.
        assert_eq!(daemon.state.accounts[0].last_reading, None);
        // …but the pre-blind anchor is retained intact.
        assert_eq!(
            daemon.state.last_good.map(|g| (g.session, g.weekly)),
            Some((0.68, 0.40)),
            "a 429 must not disturb the retained last-good anchor",
        );
    }

    #[tokio::test]
    async fn last_good_refreshes_on_each_successful_active_poll() {
        // Issue #450 AC2: every successful active-account poll overwrites the anchor with the
        // fresh reading AND a fresh observation time.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.20, 0.10)
                .ok("u-B", 0.05, 0.05)
                .ok("u-C", 0.05, 0.05),
        )
        .await;

        daemon.tick().await; // tick 1 — active `work` at 0.20
        let first = daemon
            .state
            .last_good
            .expect("the first active poll sets the anchor");
        assert_eq!((first.session, first.weekly), (0.20, 0.10));

        // A later active poll reads higher, at a later time.
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.30)
            .ok("u-B", 0.05, 0.05)
            .ok("u-C", 0.05, 0.05);
        daemon.clock.advance(Duration::from_secs(120));
        daemon.tick().await; // tick 2 — peer `spare`
        daemon.tick().await; // tick 3 — active `work` re-observed (#366)

        let second = daemon.state.last_good.expect("the anchor is still set");
        assert_eq!(
            (second.session, second.weekly),
            (0.50, 0.30),
            "the anchor tracks the latest active reading",
        );
        assert!(
            second.at > first.at,
            "the anchor's timestamp advances with each refresh",
        );
    }

    #[tokio::test]
    async fn a_swap_away_clears_last_good_so_it_never_describes_the_departed_active() {
        // Issue #450: the anchor is "for the ACTIVE account", so a swap-away must clear it —
        // otherwise the bounded-blindness path (#452), whose OWN swap lands in `record_swap`,
        // would keep reading the near-band anchor of the account it just left and could
        // spuriously re-swap once the cooldown lapses. (The reset also fires at
        // `adopt_manual_swap` and the reconcile paths; this exercises the load-bearing
        // daemon-swap path.)
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Active `work` sits over the trigger; `spare` is wide open — a viable target.
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

        // The warming run polls the active at 0.97 (which sets the anchor), then swaps away
        // from it — you cannot swap an over-trigger active without first observing it, so the
        // anchor is provably populated before the swap.
        let outcome = warmed_tick(&mut daemon).await;
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });

        assert_eq!(daemon.state.active, Some(1));
        assert_eq!(
            daemon.state.last_good, None,
            "swapping away from the active account must clear its pre-blind anchor",
        );
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
        // first throttled poll arms a 2×-interval (120 s) window. A is the ACTIVE account, so
        // that window is clamped at ACTIVE_POLL_BACKOFF_CAP (120 s, #453) and stays flat on
        // re-poll rather than doubling — the widened-vs-fixed-interval spacing is what this pins.
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
                canonical_unknown_diag("work"),
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

        // Advancing past the window lets A be re-polled. A is the ACTIVE account, so its
        // self-backoff is CLAMPED at ACTIVE_POLL_BACKOFF_CAP (120 s, issue #453) — it stays
        // flat on each throttled re-poll rather than doubling toward the peer ceiling.
        daemon.clock.advance(Duration::from_secs(120));
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(120));
        daemon.clock.advance(Duration::from_secs(120));
        assert_eq!(tick_backoff_secs(&daemon.tick().await), Some(120));
    }

    #[tokio::test]
    async fn the_active_accounts_back_off_is_capped_not_climbing() {
        // AC (issue #453): the ACTIVE account's self-backoff is clamped to
        // ACTIVE_POLL_BACKOFF_CAP (120 s) and does NOT climb toward the peer POLL_BACKOFF_CAP
        // (3600 s) — a 429 blinds the very account being consumed, so recovering observability
        // fast (a 120 s ceiling) beats the peer's gentle-on-the-endpoint hour. With the 60 s
        // base the first throttle already sits at the 120 s cap, so every subsequent throttle
        // stays FLAT at 120 s rather than doubling (240, 480, …) — the peer path (which DOES
        // climb) is pinned separately in `consecutive_rate_limits_widen_a_peers_durable_backoff_window`.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        let mut waits = Vec::new();
        for _ in 0..7 {
            let secs = tick_backoff_secs(&daemon.tick().await).unwrap();
            waits.push(secs);
            // Step past the just-armed window so the next tick re-polls (not skips) A.
            daemon.clock.advance(Duration::from_secs(secs));
        }
        // Flat at the active cap across a climbing streak — never 240/480/… (the PEER shape).
        assert_eq!(waits, vec![120, 120, 120, 120, 120, 120, 120]);
        // The durable event's streak still CLIMBS (the episode stays diagnosable) while the
        // armed window stays pinned at the active cap: streak 8 on the next throttle, window 120.
        let last = daemon.tick().await;
        assert!(
            last.events.iter().any(|e| matches!(
                e,
                Event::UsageBackoff { account, consecutive, backoff_secs, .. }
                    if account == "u-A" && *consecutive == 8 && *backoff_secs == 120
            )),
            "active durable event: streak climbs to 8, window pinned at 120: {:?}",
            last.events,
        );
    }

    #[tokio::test]
    async fn retry_after_is_honoured_as_a_minimum_wait() {
        // AC: Retry-After is honoured as a MINIMUM for the account's back-off window. When
        // it exceeds the exponential it wins; when it is smaller, the larger exponential
        // governs but the window is never below Retry-After. `u-A` is the ACTIVE account, so
        // its exponential arm is the ACTIVE_POLL_BACKOFF_CAP (120 s) — which here coincides
        // with the 60 s base's first-cycle 2× (issue #453 does not change RA-as-minimum).
        // Larger than the 120 s cap → Retry-After (600 s) wins.
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
    async fn the_active_accounts_retry_after_is_an_absolute_floor() {
        // AC (issue #453): for the ACTIVE account a server `Retry-After` is an ABSOLUTE floor —
        // the daemon NEVER re-polls before it — so it is NOT clamped to POLL_BACKOFF_CAP the way
        // a PEER's is (issue #294). A pathological full-day `Retry-After` therefore governs the
        // active window IN FULL (it does not collapse to the 1 h peer ceiling); the peer clamp
        // is pinned separately in `a_large_retry_after_is_clamped_to_the_cap_for_a_peer`.
        let one_day = Duration::from_secs(86_400);
        assert!(one_day > POLL_BACKOFF_CAP);
        let (_d1, mut pathological) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", Some(one_day))).await;
        assert_eq!(
            tick_backoff_secs(&pathological.tick().await),
            Some(86_400),
            "active Retry-After is an un-clamped floor — no re-poll before it (AC #453)",
        );

        // A value above the active cap but below the peer cap is likewise honoured verbatim
        // (the active exponential arm is 120 s, so the 3599 s server floor governs).
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

        // (The flagship #295 clamp case — a pathological `Retry-After` clamped to the cap with
        // the raw value still surfaced on the label — is now PEER-only, since the active `u-A`
        // hard-floors `Retry-After` un-clamped (#453). It is pinned in
        // `a_large_retry_after_is_clamped_to_the_cap_for_a_peer`.)
    }

    #[tokio::test]
    async fn a_clean_cycle_resets_a_peer_rate_limit_back_off() {
        // Once an account polls clean again the back-off clears (streak + window reset), so a
        // LATER 429 restarts at the base 2× — not where the prior episode left off. The reset
        // path is role-agnostic (it runs before the active/peer split), but the "restart at
        // base, not at 480" evidence needs the exponential CLIMB, which is PEER-only since #453
        // — so this pins it on the throttled non-active `spare` (u-B); `work` (u-A) stays
        // active and clean.
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .rate_limited("u-B", None),
        )
        .await;
        // Climb the peer's window: 120 → 240.
        assert_eq!(
            tick_backoff_secs(&next_peer_tick(&mut daemon).await),
            Some(120)
        );
        daemon.clock.advance(Duration::from_secs(120));
        assert_eq!(
            tick_backoff_secs(&next_peer_tick(&mut daemon).await),
            Some(240)
        );

        // The peer polls clean → its back-off clears. Advance past the 240 s window so the peer
        // is actually re-polled (not skipped) on its next turn.
        daemon.clock.advance(Duration::from_secs(240));
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.20, 0.10);
        let cleared = next_peer_tick(&mut daemon).await;
        assert_eq!(
            tick_backoff_secs(&cleared),
            None,
            "the peer re-polled clean → no back-off"
        );
        assert!(
            cleared.events.iter().any(|e| matches!(
                e, Event::UsageBackoffCleared { account } if account == "u-B"
            )),
            "the clean re-poll emits a durable CLEARED, bracketing the episode: {:?}",
            cleared.events,
        );

        // A later 429 restarts the climb at the base multiplier (120), NOT at 480 — the cleared
        // window means the peer is re-polled straightaway, no advance needed.
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .rate_limited("u-B", None);
        assert_eq!(
            tick_backoff_secs(&next_peer_tick(&mut daemon).await),
            Some(120),
            "streak reset → later 429 restarts at base, not where it left off",
        );
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
    async fn a_large_retry_after_is_clamped_to_the_cap_for_a_peer() {
        // AC (issue #294, UNCHANGED for peers by #453): a PEER's server `Retry-After` is honoured
        // as a MINIMUM but clamped to POLL_BACKOFF_CAP as a MAXIMUM, so a pathological value cannot
        // dark the peer past the 1 h ceiling. Both the tick line and the durable event surface the
        // RAW pre-cap value beside the clamped window (`backoff_secs=3600 retry_after_secs=86400`),
        // resolving the #295 ambiguity. Contrast the active account, whose `Retry-After` is an
        // un-clamped floor (`the_active_accounts_retry_after_is_an_absolute_floor`). `spare` (u-B)
        // is the throttled peer; `work` (u-A) stays active and clean.
        let one_day = Duration::from_secs(86_400);
        assert!(one_day > POLL_BACKOFF_CAP);
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .rate_limited("u-B", Some(one_day)),
        )
        .await;
        let outcome = next_peer_tick(&mut daemon).await;
        // The pathological full-day floor clamps to the 1 h peer cap...
        assert_eq!(
            tick_backoff_secs(&outcome),
            Some(POLL_BACKOFF_CAP.as_secs())
        );
        // ...while the RAW server value stays visible on the tick label (pre-cap #295)...
        assert_eq!(tick_retry_after_secs(&outcome), Some(86_400));
        // ...and on the durable event (clamped window beside the raw retry_after).
        assert!(
            outcome.events.contains(&Event::UsageBackoff {
                account: "u-B".to_owned(),
                class: BackoffClass::RateLimited,
                consecutive: 1,
                retry_after_secs: Some(86_400),
                backoff_secs: POLL_BACKOFF_CAP.as_secs(),
            }),
            "peer durable event: clamped window + raw retry_after: {:?}",
            outcome.events,
        );
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
    async fn consecutive_rate_limits_widen_a_peers_durable_backoff_window() {
        // AC (issue #453): a PEER's back-off is UNCHANGED from #293/#294 — the exponential still
        // doubles from the interval toward POLL_BACKOFF_CAP (1 h), NOT the tight active cap. Each
        // throttled re-poll widens the window (120 → 240 → 480) and emits a fresh durable ENTER
        // with the climbing streak — the residual-late-swap signal (#363's ~1674 s active-account
        // gap) a single first-throttle line would hide. `spare` (u-B) is the throttled non-active
        // peer; `work` (u-A) stays active and clean. Contrast the active account, capped flat
        // (`the_active_accounts_back_off_is_capped_not_climbing`).
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .rate_limited("u-B", None),
        )
        .await;
        let mut seen = Vec::new();
        let mut prior_window = 0u64;
        for _ in 0..3 {
            if prior_window != 0 {
                // Advance past the peer's prior window so its next turn re-polls (not skips).
                daemon.clock.advance(Duration::from_secs(prior_window));
            }
            let outcome = next_peer_tick(&mut daemon).await;
            let window = tick_backoff_secs(&outcome).expect("the peer's turn armed a back-off");
            for e in &outcome.events {
                if let Event::UsageBackoff {
                    account,
                    consecutive,
                    backoff_secs,
                    ..
                } = e
                {
                    if account == "u-B" {
                        seen.push((*consecutive, *backoff_secs));
                    }
                }
            }
            prior_window = window;
        }
        // Peer doubling toward the 1 h ceiling — the exact #293/#294 shape, unchanged by #453.
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
        // AC (issue #399, normalized by #449): velocity is queryable from the durable log. The FIRST
        // reading has no prior to diff (silent); the SECOND emits a `usage_velocity` with the signed
        // percent delta AND the interval it spanned — here session climbed 10% → 17% (=+7) while
        // weekly held at 20% (=0) over 60 s (so the rendered rate is 7.00 %/min).
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
        // The velocity is a %/min rate now (#449), so an interval must elapse between the two
        // readings — advance the monotonic clock 60 s before the second poll.
        daemon.clock.advance(Duration::from_secs(60));
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.17, 0.20);
        let second = daemon.tick().await;
        assert!(
            second.events.contains(&Event::UsageVelocity {
                account: "u-A".to_owned(),
                session_delta_pct: 7,
                weekly_delta_pct: 0,
                elapsed_secs: 60,
            }),
            "the second reading must emit its velocity + interval: {:?}",
            second.events,
        );
    }

    #[tokio::test]
    async fn usage_velocity_is_silent_when_the_reading_is_flat() {
        // A flat reading (no measurable change) emits no `usage_velocity` — the no-op silence that
        // keeps an idle account quiet on the always-on event log (mirroring `usage_rollup`). Time
        // DOES elapse (60 s) between the two reads, so the suppression is proven to come from the
        // zero delta, not a zero interval (#449).
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.10, 0.20)).await;
        daemon.tick().await; // seed the prior reading
        daemon.clock.advance(Duration::from_secs(60));
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
        // durable log distinguishes a reset from a climb by sign, the signal #368 consumes. The
        // reset spans a 60 s interval (#449), so the rendered rate is -85.00 %/min.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.90, 0.30)).await;
        daemon.tick().await; // seed a prior reading at 90% session
        daemon.clock.advance(Duration::from_secs(60));
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.05, 0.30);
        let reset = daemon.tick().await;
        assert!(reset.events.contains(&Event::UsageVelocity {
            account: "u-A".to_owned(),
            session_delta_pct: -85,
            weekly_delta_pct: 0,
            elapsed_secs: 60,
        }));
    }

    #[tokio::test]
    async fn usage_velocity_does_not_span_a_throttle_gap() {
        // A velocity always spans two CONSECUTIVE real readings. A throttle clears the prior reading
        // (its `Err` result sets `last_reading` to `None`), so the recovering clean poll has
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
    async fn blind_window_close_is_emitted_on_active_recovery_with_duration_and_near_limit() {
        // Issue #449 (umbrella #363 Path B): the active account was near the limit (96%) when a 429
        // blinded it — its `last_reading` slot cleared, so `swap::decide` had no reading. Five
        // minutes later it reads live again; the daemon emits ONE durable `blind_window` close with
        // the blind DURATION (300 s), the pre-blind anchor's session pct (96), and `near_limit=true`
        // (the anchor sat at/over the 95% trigger). The two SLIs the #451 spike reads.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.96, 0.30)).await;
        daemon.tick().await; // seed the pre-blind anchor at 96% session (in the risk band)

        // A 429 blinds the active account: its reading is cleared, the #450 anchor is retained. No
        // blind-window closes while still blind.
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        let blind = daemon.tick().await;
        assert!(
            !blind
                .events
                .iter()
                .any(|e| matches!(e, Event::BlindWindow { .. })),
            "no blind-window closes while still blind: {:?}",
            blind.events,
        );
        daemon.clock.advance(Duration::from_secs(300));

        // The recovering clean poll closes the blind window — exactly one `blind_window` event,
        // measured from the retained anchor.
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.97, 0.30);
        let recovered = daemon.tick().await;
        assert!(
            recovered.events.contains(&Event::BlindWindow {
                account: "u-A".to_owned(),
                duration_secs: 300,
                session_pct: 96,
                // Recovered at 97% — above the 96% anchor, so a stale-anchor preemptive swap would
                // have been necessary (still climbing). The #482 recovery pct (raw, un-classified).
                session_at_recovery: 97,
                near_limit: true,
                // No sustained velocity was seeded (a single pre-blind reading), so the arm could
                // not have armed and no ingredient is published (issue #634).
                velocity: None,
            }),
            "the recovery must close the blind window with its duration + recovery pct + near-limit tag: {:?}",
            recovered.events,
        );
        assert_eq!(
            recovered
                .events
                .iter()
                .filter(|e| matches!(e, Event::BlindWindow { .. }))
                .count(),
            1,
            "exactly one blind_window per episode: {:?}",
            recovered.events,
        );
    }

    #[tokio::test]
    async fn blind_window_close_tags_a_below_band_anchor_not_near_limit() {
        // A blind window whose pre-blind anchor was well below the trigger (40%) is STILL recorded
        // (the spike wants the full distribution) but tagged `near_limit=false` — it does not count
        // toward time-blind-near-limit (#449).
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.40, 0.30)).await;
        daemon.tick().await; // anchor at 40% session — below the 95% risk band
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        daemon.tick().await; // blind
        daemon.clock.advance(Duration::from_secs(120));
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.42, 0.30);
        let recovered = daemon.tick().await;
        assert!(
            recovered.events.contains(&Event::BlindWindow {
                account: "u-A".to_owned(),
                duration_secs: 120,
                session_pct: 40,
                session_at_recovery: 42,
                near_limit: false,
                velocity: None,
            }),
            "a below-band blind window is recorded but not near-limit: {:?}",
            recovered.events,
        );
    }

    #[tokio::test]
    async fn blind_window_carries_the_retained_velocity_when_the_account_climbed_before_going_blind(
    ) {
        // Issue #634: the load-bearing finding — the #539 EMA retained at the blind-window emit is
        // the PRE-BLIND one, so the report-only blind velocity-projection arm (#584/#600) is
        // reconstructable offline. Drive two climbing intervals to SUSTAIN the EMA (samples ≥ 2),
        // then blind the account and recover it: the velocity fold is gated on a live previous
        // reading — exactly what a blind window clears — so the recovery poll cannot overwrite the
        // retained rate before `blind_window` is emitted, and the ingredient rides the line.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.90, 0.30)).await;
        daemon.tick().await; // reading 1 at 90 % — first reading, nothing to diff yet

        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.93, 0.30);
        daemon.clock.advance(Duration::from_secs(60));
        daemon.tick().await; // reading 2 → seeds the EMA (samples = 1)

        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.95, 0.30);
        daemon.clock.advance(Duration::from_secs(60));
        daemon.tick().await; // reading 3 → blends (samples = 2, SUSTAINED)
        assert_eq!(
            daemon.state.accounts[0].session_velocity.map(|v| v.samples),
            Some(2),
            "two climbing intervals sustain the EMA",
        );

        // A 429 blinds the account; the retained EMA is untouched (the fold is skipped on a failed
        // poll).
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        daemon.tick().await;
        daemon.clock.advance(Duration::from_secs(300));

        // Recovery closes the blind window. The recovery poll's velocity fold is skipped (the prior
        // reading was cleared by the 429), so the emitted line carries the PRE-BLIND rate.
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.97, 0.30);
        let recovered = daemon.tick().await;
        let ceiling_base = daemon.session_ceiling_base;

        let velocity = recovered
            .events
            .iter()
            .find_map(|e| match e {
                Event::BlindWindow { velocity, .. } => *velocity,
                _ => None,
            })
            .expect("the blind_window carries the retained velocity ingredient (#634)");
        assert!(
            velocity.rate_pct_per_sec > 0.0,
            "the retained climbing rate survives to the emit, got {}",
            velocity.rate_pct_per_sec,
        );
        assert!(
            (velocity.inflation - BLIND_VELOCITY_RATE_INFLATION).abs() < 1e-9,
            "the inflation constant in force is stamped, got {}",
            velocity.inflation,
        );
        assert!(
            (velocity.ceiling_pct - to_pct_exact(ceiling_base)).abs() < 1e-9,
            "the BASE ceiling in force is stamped, got {}",
            velocity.ceiling_pct,
        );
    }

    #[tokio::test]
    async fn blind_velocity_ingredients_gate_on_a_sustained_ema() {
        // Issue #634: the ingredient is published only when the report-only arm could actually have
        // armed — a retained EMA that is SUSTAINED (>= MIN_VELOCITY_SAMPLES). No EMA and a
        // single-interval spike both yield `None`, so absent tokens on a `blind_window` line mean
        // "the arm could not have armed here", never "unknown". When present the fraction-domain rate
        // is converted to percent-per-second and the constants in force are stamped.
        let mut daemon = three_account_daemon(FakeRosterPoller::new().ok("u-A", 0.50, 0.20)).await;

        daemon.state.accounts[0].session_velocity = None;
        assert!(
            daemon.blind_velocity_ingredients(0).is_none(),
            "no retained EMA → no ingredient",
        );

        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.001,
            samples: MIN_VELOCITY_SAMPLES - 1,
        });
        assert!(
            daemon.blind_velocity_ingredients(0).is_none(),
            "an unsustained single-interval EMA → no ingredient (the arm could not arm)",
        );

        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.001,
            samples: MIN_VELOCITY_SAMPLES,
        });
        let ingredient = daemon
            .blind_velocity_ingredients(0)
            .expect("a sustained EMA publishes its ingredient");
        assert!(
            (ingredient.rate_pct_per_sec - 0.1).abs() < 1e-9,
            "0.001 fraction/s renders as 0.1 %/s, got {}",
            ingredient.rate_pct_per_sec,
        );
        assert!(
            (ingredient.inflation - BLIND_VELOCITY_RATE_INFLATION).abs() < 1e-9,
            "the inflation constant in force is stamped",
        );
        assert!(
            (ingredient.ceiling_pct - to_pct_exact(daemon.session_ceiling_base)).abs() < 1e-9,
            "the BASE (un-jittered) ceiling in force is stamped",
        );
    }

    #[tokio::test]
    async fn a_clean_reactive_re_poll_emits_no_blind_window() {
        // The negative: two consecutive LIVE active readings (no intervening blindness) close no
        // blind window — the None→live edge never occurs, so `blind_window` is silent (#449).
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.50, 0.30)).await;
        daemon.tick().await; // first live reading (sets the anchor)
        daemon.clock.advance(Duration::from_secs(60));
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.55, 0.30);
        let second = daemon.tick().await; // a second live reading — never blind
        assert!(
            !second
                .events
                .iter()
                .any(|e| matches!(e, Event::BlindWindow { .. })),
            "a clean re-poll closes no blind window: {:?}",
            second.events,
        );
    }

    #[tokio::test]
    async fn a_blind_episode_that_never_recovers_is_still_recorded_once() {
        // Issue #583 AC #1 — the FIRST censoring tail. `blind_window` fires only on the None→live
        // RECOVERY edge, so an account that goes dark and STAYS dark emits nothing at all: the
        // distribution #484's promotion bar reads is censored at exactly its tail. The episode is
        // recorded on ENTRY instead, so it is durable the moment it starts — no recovery required.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.96, 0.30)).await;
        daemon.tick().await; // the live reading that becomes the anchor: 96% session, 30% weekly

        // The 429 that blinds it — and it never reads live again.
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        let entered = daemon.tick().await;
        assert!(
            entered.events.contains(&Event::BlindEnter {
                account: "u-A".to_owned(),
                session_pct: 96,
                weekly_pct: 30,
                was_active: true,
                // The anchor sat at/over the 95% trigger — the risk band, tagged at entry.
                near_limit: true,
            }),
            "the entry edge must record the episode the moment it starts: {:?}",
            entered.events,
        );

        // Stay dark for an hour of ticks. The episode is already recorded; `blind_window` — correctly,
        // per its retained recovery-edge semantics — never closes, which is precisely why it could
        // not see this episode at all before the entry edge existed.
        let mut later = Vec::new();
        for _ in 0..12 {
            daemon.clock.advance(Duration::from_secs(300));
            later.extend(daemon.tick().await.events);
        }
        assert!(
            !later
                .iter()
                .any(|e| matches!(e, Event::BlindWindow { .. } | Event::BlindExit { .. })),
            "an episode that never recovers closes nothing — that is the tail #583 fixes: {:?}",
            later,
        );
        assert!(
            !later.iter().any(|e| matches!(e, Event::BlindEnter { .. })),
            "a held blind tick must not re-open the episode — exactly one entry per episode: {:?}",
            later,
        );
    }

    #[tokio::test]
    async fn a_blind_episode_the_daemon_swaps_away_from_is_still_recorded() {
        // Issue #583 AC #2 — the SECOND censoring tail, and the one that interacts adversely with
        // issue #582. `blind_window` is guarded by `active == Some(i)` AND built from `last_good`, the
        // ACTIVE-only anchor `record_swap` DROPS — so once the #452 gate swaps off a blind account,
        // its later recovery is unrecoverable. Swapping away on blindness is precisely what makes an
        // episode invisible, so the swap-away fix would otherwise INCREASE the censoring. The
        // per-account anchor no swap path touches records it anyway, and tags the case.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.68, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        // Arm the #452 preemptive swap at the interim T (the `tunables()` helper parks it at the
        // kill-switch ceiling so baseline tests stay inert), as `blind_swap_fires_past_t_*` does.
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert_eq!(daemon.state.active, Some(0), "u-A active before the swap");

        // Blind the active. The clock is frozen through this loop, so the gate never arms yet.
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        let mut entry = Vec::new();
        while daemon.state.accounts[0].last_reading.is_some() {
            entry.extend(daemon.tick().await.events);
        }
        assert!(
            entry.contains(&Event::BlindEnter {
                account: "u-A".to_owned(),
                session_pct: 68,
                weekly_pct: 20,
                was_active: true,
                // 68% is inside the interim 60% risk band but below the 95% reactive trigger, so the
                // `near_limit` tag (which keys off the trigger, matching `blind_window`) is false.
                near_limit: false,
            }),
            "the blind entry is recorded while u-A is still the active account: {:?}",
            entry,
        );

        // Cross the interim T: the bounded-blindness gate swaps the blind u-A away.
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let swapped = daemon.tick().await;
        assert!(
            matches!(
                swapped.action,
                TickAction::PreemptivelySwapped { from: 0, .. }
            ),
            "the gate must swap the blind active away for this test to exercise the tail: {:?}",
            swapped.action,
        );
        assert_ne!(daemon.state.active, Some(0), "the active moved off u-A");

        // u-A — now an out-of-rotation PEER — reads live again 10 minutes later.
        daemon.clock.advance(Duration::from_secs(600));
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.71, 0.22)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        let mut exit = Vec::new();
        while daemon.state.accounts[0].last_reading.is_none() {
            exit.extend(daemon.tick().await.events);
        }
        assert!(
            exit.contains(&Event::BlindExit {
                account: "u-A".to_owned(),
                // The anchor was stamped on the last live reading (frozen clock), so the episode
                // spans the two explicit advances: the gate crossing plus the 10 min out of rotation.
                duration_secs: BLIND_GATE_SECS + 1 + 600,
                session_pct: 68,
                session_at_recovery: 71,
                weekly_pct: 20,
                weekly_at_recovery: 22,
                was_active: true,
                swapped_away: true,
                near_limit: false,
            }),
            "the swapped-away account's recovery must still close its episode, off the per-account \
             anchor and with the swap-away tagged: {exit:?}",
        );
        // The censoring this fixes, pinned: `blind_window` stays silent for this whole episode —
        // `active == Some(0)` is false at recovery and `record_swap` dropped its `last_good`. It is
        // not broken; it is retained for SLO reporting (AC #4) and simply cannot see this case.
        assert!(
            !exit
                .iter()
                .any(|e| matches!(e, Event::BlindWindow { .. })),
            "blind_window structurally cannot close a swapped-away episode — that is the tail: {exit:?}",
        );
    }

    #[tokio::test]
    async fn a_blind_exit_exposes_a_weekly_burn_that_a_session_reset_masks() {
        // Issue #583 AC #3 — "did it burn?", answerable from the log alone. This replays the exact
        // production episode from the issue: on 2026-07-17 u-A was last seen at session 0.29 / weekly
        // 0.050, went blind for ~2 h, and read back at session 0.00 / weekly 0.170. The SESSION window
        // had reset behind the blindness, so a session-only record (all `blind_window` carries) reads
        // 29 → 0 — indistinguishable from "never burned" — while the WEEKLY window had in fact burned
        // +12 pp. Carrying BOTH windows is what makes the burn question answerable at all; the answer
        // had to be dug out of `usage-samples.jsonl` by hand.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.29, 0.050)).await;
        daemon.tick().await; // 08:26:01Z — the last live reading before the blindness
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        daemon.tick().await; // the 429 that blinds it
        daemon.clock.advance(Duration::from_secs(7512)); // ~2h05m dark

        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.00, 0.170);
        let mut recovered = Vec::new();
        while daemon.state.accounts[0].last_reading.is_none() {
            recovered.extend(daemon.tick().await.events);
        }
        let exit = recovered
            .iter()
            .find(|e| matches!(e, Event::BlindExit { .. }))
            .expect("the recovery must close the episode");
        assert_eq!(
            *exit,
            Event::BlindExit {
                account: "u-A".to_owned(),
                duration_secs: 7512,
                // The session dimension alone tells the WRONG story: 29 → 0 reads as a quiet reset.
                session_pct: 29,
                session_at_recovery: 0,
                // The weekly dimension carries the truth: 5 → 17 is the +12 pp that actually burned.
                weekly_pct: 5,
                weekly_at_recovery: 17,
                was_active: true,
                swapped_away: false,
                near_limit: false,
            },
            "the episode must carry BOTH windows or the burn stays invisible: {recovered:?}",
        );
        // And the rendered line answers it directly, without a query-time join.
        assert!(
            exit.to_log_line(std::time::UNIX_EPOCH)
                .contains("weekly_burn_pct=12"),
            "the line must state the burn: {}",
            exit.to_log_line(std::time::UNIX_EPOCH),
        );
    }

    #[tokio::test]
    async fn blind_window_keeps_its_recovery_edge_semantics_alongside_the_episode_pair() {
        // Issue #583 AC #4: the episode pair is ADDITIVE. On the one case `blind_window` can see — an
        // active account that recovers while still active — it still emits exactly one event with
        // exactly its old fields, unchanged. It is the retrospective duration histogram for SLO
        // reporting; it was only ever assigned the wrong PURPOSE (detection), so it is not touched.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.96, 0.30)).await;
        daemon.tick().await;
        daemon.poller = FakeRosterPoller::new().rate_limited("u-A", None);
        daemon.tick().await;
        daemon.clock.advance(Duration::from_secs(300));
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.97, 0.31);
        let recovered = daemon.tick().await;

        assert!(
            recovered.events.contains(&Event::BlindWindow {
                account: "u-A".to_owned(),
                duration_secs: 300,
                session_pct: 96,
                session_at_recovery: 97,
                near_limit: true,
                velocity: None,
            }),
            "blind_window keeps its #449/#482 recovery-edge semantics verbatim: {:?}",
            recovered.events,
        );
        assert_eq!(
            recovered
                .events
                .iter()
                .filter(|e| matches!(e, Event::BlindWindow { .. }))
                .count(),
            1,
            "still exactly one blind_window per episode: {:?}",
            recovered.events,
        );
        // Both instruments describe the SAME episode from the same anchor, so their shared fields
        // must agree — a divergence here would mean the two anchors had drifted apart.
        assert!(
            recovered.events.contains(&Event::BlindExit {
                account: "u-A".to_owned(),
                duration_secs: 300,
                session_pct: 96,
                session_at_recovery: 97,
                weekly_pct: 30,
                weekly_at_recovery: 31,
                was_active: true,
                swapped_away: false,
                near_limit: true,
            }),
            "the episode pair closes the same window with the weekly dimension added: {:?}",
            recovered.events,
        );
    }

    #[tokio::test]
    async fn a_first_ever_failed_poll_claims_no_blind_episode() {
        // The never-fabricate-an-anchor negative: an account whose FIRST poll fails was never seen
        // live, so there is no baseline to difference a burn against — no episode is claimed, and no
        // later "recovery" invents one. Mirrors `blind_window`'s `last_good.is_some()` guard.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().rate_limited("u-A", None)).await;
        let first = daemon.tick().await;
        assert!(
            !first
                .events
                .iter()
                .any(|e| matches!(e, Event::BlindEnter { .. })),
            "a first-ever failed poll has no anchor, so it opens no episode: {:?}",
            first.events,
        );
        assert!(daemon.state.accounts[0].blind_anchor.is_none());

        daemon.clock.advance(Duration::from_secs(600));
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.40, 0.10);
        let mut live = Vec::new();
        while daemon.state.accounts[0].last_reading.is_none() {
            live.extend(daemon.tick().await.events);
        }
        assert!(
            !live.iter().any(|e| matches!(e, Event::BlindExit { .. })),
            "the first live reading closes no episode that was never opened: {live:?}",
        );
    }

    #[tokio::test]
    async fn a_clean_re_poll_emits_no_blind_episode_edges() {
        // The level-safety negative: two consecutive LIVE readings cross neither edge, so the pair is
        // silent. Blindness is rare; the log must not gain a line per ordinary poll.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.50, 0.30)).await;
        daemon.tick().await;
        daemon.clock.advance(Duration::from_secs(60));
        daemon.poller = FakeRosterPoller::new().ok("u-A", 0.55, 0.30);
        let second = daemon.tick().await;
        assert!(
            !second
                .events
                .iter()
                .any(|e| matches!(e, Event::BlindEnter { .. } | Event::BlindExit { .. })),
            "a clean re-poll crosses no blind edge: {:?}",
            second.events,
        );
        assert!(daemon.state.accounts[0].blind_anchor.is_none());
    }

    #[tokio::test]
    async fn blind_gate_eligible_signals_a_viable_target_once_past_the_interim_t() {
        // Issue #482 SLI #1 (umbrella #363 Path B): the #452 preemptive-swap gate's premise falsifier.
        // The active account's retained anchor sits at 70 % — inside the interim risk band (≥ 60 %,
        // #484), below the 95 % reactive trigger — and a peer (u-B) has session reserve under the 80 % target_max.
        // Once the active has been blind past the interim T, the gate is ELIGIBLE and a viable target
        // exists, so the ADR-0017 cost-asymmetry premise holds this episode: exactly one
        // `blind_gate_eligible` with `viable_target=true`, emitted whether or not a swap follows.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.70, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        // Warm up the full interleaved cycle so the viable-target read is off a COMPLETE last-known
        // set (the SLI's #80 warm-up guard), not a partial one that would fabricate a false verdict.
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);

        // Blind the active: a 429 clears u-A's reading (the #450 anchor at 70 % is retained). No gate
        // signal yet — blind_elapsed is still 0 under the frozen clock, below the interim T.
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            let out = daemon.tick().await;
            assert!(
                !out.events
                    .iter()
                    .any(|e| matches!(e, Event::BlindGateEligible { .. })),
                "no gate signal before the interim T elapses: {:?}",
                out.events,
            );
        }

        // Cross the interim T — now the gate's first two conditions hold and a viable target exists.
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let eligible = daemon.tick().await;
        assert!(
            eligible.events.iter().any(|e| matches!(
                e,
                Event::BlindGateEligible {
                    viable_target: true,
                    session_pct: 70,
                    ..
                }
            )),
            "the gate turns eligible with a viable target present: {:?}",
            eligible.events,
        );
    }

    #[tokio::test]
    async fn blind_gate_eligible_reports_no_viable_target_the_premise_falsifier() {
        // Issue #482 SLI #1: the FALSIFIER. Same eligible active (anchor 70 %, blind past T) but every
        // peer is over the 80 % target_max reserve (u-B 85 %, u-C 90 %) — session-viable readings, just
        // no reserve to catch a swap. `pick_target` finds none, so the gate is eligible with NO viable
        // target: `viable_target=false` is the ADR-0017 cost-asymmetry counter-evidence #482 exists to
        // surface (if non-trivial, #452's predicate must be revisited).
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.70, 0.20)
                .ok("u-B", 0.85, 0.10)
                .ok("u-C", 0.90, 0.10),
        )
        .await;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.85, 0.10)
            .ok("u-C", 0.90, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let eligible = daemon.tick().await;
        assert!(
            eligible.events.iter().any(|e| matches!(
                e,
                Event::BlindGateEligible {
                    viable_target: false,
                    ..
                }
            )),
            "eligible but no peer has reserve → the no-viable-target falsifier fires: {:?}",
            eligible.events,
        );
    }

    #[tokio::test]
    async fn blind_gate_eligible_reports_no_viable_target_when_every_peer_is_in_the_weekly_band() {
        // Issue #607: the blind-gate SLI's third condition IS a `pick_target` viability check, so it
        // reads the effective weekly ceiling like every other target-selection site. Both peers are
        // session-viable (0.10, under the 80 % reserve) but sit in the tail-margin band (weekly
        // 0.975 ∈ `[0.97, 0.98)`), so no peer the daemon would rotate onto exists →
        // `viable_target=false`. Against the RAW ceiling these peers read weekly-viable and the SLI
        // would over-report `viable_target=true` — miscounting the #482/#595 landing evidence — so
        // reverting this call site to the raw ceiling flips the flag and fails here.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.70, 0.20)
                .ok("u-B", 0.10, 0.975)
                .ok("u-C", 0.10, 0.975),
        )
        .await;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.975)
            .ok("u-C", 0.10, 0.975);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let eligible = daemon.tick().await;
        assert!(
            eligible.events.iter().any(|e| matches!(
                e,
                Event::BlindGateEligible {
                    viable_target: false,
                    ..
                }
            )),
            "band peers are not rotatable, so the SLI must report no viable target: {:?}",
            eligible.events,
        );
    }

    #[tokio::test]
    async fn blind_swap_fires_past_t_with_a_viable_target() {
        // Issue #452 (ADR-0017): the S1 replay. u-A `429`'d with its retained pre-blind anchor at
        // 68 % — inside the risk band (≥ 60 %), below the 95 % reactive trigger — and a peer (u-B /
        // u-C at 10 %) has reserve under the 80 % target_max. Under the reactive path alone u-A would
        // burn to exhaustion behind the blindness (S1: to 98 %); the bounded-blindness gate swaps it
        // AWAY once blind past T, keying off the stale anchor, before it self-exhausts.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.68, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        // Arm the #452 SWAP at the interim T. The `tunables()` helper parks `session_blind_swap_secs`
        // at the kill-switch ceiling so baseline tests stay inert; here we exercise the gate. The
        // risk band is already 0.60 (the config default the helper carries).
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A active before the blind swap"
        );

        // Blind the active — its reading clears, the 68 % anchor (#450) is retained. The clock is
        // frozen through this loop, so blind_elapsed stays 0 and the gate never arms yet.
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            let out = daemon.tick().await;
            assert!(
                !matches!(out.action, TickAction::PreemptivelySwapped { .. }),
                "no preemptive swap before the interim T elapses: {:?}",
                out.action,
            );
        }

        // Cross the interim T — the gate arms (blind_elapsed > T, anchor ≥ risk band) and a viable
        // target exists, so the preemptive swap fires.
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let swapped = daemon.tick().await;
        assert!(
            matches!(
                swapped.action,
                TickAction::PreemptivelySwapped { from: 0, .. }
            ),
            "the bounded-blindness gate swaps the blind active away before it self-exhausts: {:?}",
            swapped.action,
        );
        assert_ne!(
            daemon.state.active,
            Some(0),
            "the active moved off the blind u-A"
        );
        // The swap logged a `blind_preempt` reason carrying the STALE anchor pct (68) as session_pct
        // — the only session signal available while blind — and record_swap dropped the anchor so it
        // cannot re-fire.
        assert!(
            swapped.events.iter().any(|e| matches!(
                e,
                Event::Swap {
                    reason: SwapReason::BlindPreempt,
                    session_pct: 68,
                    ..
                }
            )),
            "a blind_preempt swap event carrying the stale anchor pct: {:?}",
            swapped.events,
        );
        assert!(
            daemon.state.last_good.is_none(),
            "the swap dropped the departed active's anchor (no re-fire)",
        );

        // Issue #479 (surface 2): the swap is RETAINED for `status` to NARRATE — source (the departed
        // active's LABEL "work", the operator-facing identifier the `use <label>` undo keys off, NOT
        // the internal account id u-A), the stale pct it fired on (68), and the target (the new
        // active) — the undo derivable as `use work`. Captured at swap-time, since `record_swap` has
        // already dropped the anchor above.
        let new_active = daemon.state.active.expect("a new active after the swap");
        let record = daemon
            .state
            .last_blind_preempt_swap
            .as_ref()
            .expect("the preemptive swap is retained for narration (#479)");
        assert_eq!(
            record.from, "work",
            "narrates the LABEL of the account swapped away from (the `use <label>` undo target)",
        );
        assert_eq!(
            record.to, daemon.roster[new_active].label,
            "narrates the target the swap landed on",
        );
        assert_eq!(
            record.last_known_session_pct, 68,
            "carries the stale anchor pct"
        );

        // And the wire snapshot PROJECTS it (still-current: the target is still active; recent: the
        // clock has not advanced past the notice window since the swap), so `status` can render it.
        let readings = daemon.decision_readings(daemon.state.active);
        let snap = daemon.snapshot(daemon.state.active, &readings, 0);
        let narrated = snap
            .recent_blind_preempt_swap
            .expect("the wire narrates the recent preemptive swap (#479)");
        assert_eq!(narrated.from_label, "work");
        assert_eq!(narrated.to_label, daemon.roster[new_active].label);
        assert_eq!(narrated.last_known_session_pct, 68);
    }

    #[tokio::test]
    async fn blind_swap_does_not_fire_without_an_anchor() {
        // Issue #452 / #369: the gate keys off the retained `last_good` anchor (#450), NEVER the
        // missing reading. An active that never got a good reading has no anchor, so a genuinely-
        // unknown account produces NO spurious swap however long it is blind.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .rate_limited("u-A", None)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(
            daemon.state.last_good.is_none(),
            "u-A never read good → no pre-blind anchor",
        );
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let out = daemon.tick().await;
        assert!(
            !matches!(out.action, TickAction::PreemptivelySwapped { .. }),
            "no anchor → no preemptive swap, only the historical skip: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A unchanged — nothing to key off"
        );
    }

    #[tokio::test]
    async fn blind_swap_does_not_fire_without_a_viable_target() {
        // Issue #452 / ADR-0013: eligible active (anchor 70 %, blind past T) but every peer is over
        // the 80 % target_max reserve (u-B 85 %, u-C 90 %) — session-viable readings, no reserve to
        // catch a swap. The reserve is HONORED (not the emergency bypass), so `pick_target` finds
        // none and the gate falls through to the historical skip — never a swap onto a saturated peer.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.70, 0.20)
                .ok("u-B", 0.85, 0.10)
                .ok("u-C", 0.90, 0.10),
        )
        .await;
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.85, 0.10)
            .ok("u-C", 0.90, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::SkippedActiveUnavailable),
            "eligible but no viable target → the historical skip, no swap: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A stays active — no target to swap to"
        );
    }

    #[tokio::test]
    async fn blind_swap_does_not_fire_onto_a_weekly_tail_margin_band_peer() {
        // Issue #607: the blind-preempt path fires a REAL swap, so its target selection must honour
        // the same effective weekly ceiling the reactive arm releases on — otherwise it lands the
        // preemptive swap on a band account that the very next reactive tick bounces straight back.
        // Both peers are session-viable (0.10, under the 80 % reserve) but sit in the tail-margin
        // band (weekly 0.975 ∈ `[0.97, 0.98)`), so the gate must fall through to the historical skip.
        // Against the RAW ceiling the band peers ARE viable and the preemptive swap FIRES — so
        // reverting this call site to the raw ceiling flips this to `PreemptivelySwapped` and fails.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.70, 0.20)
                .ok("u-B", 0.10, 0.975)
                .ok("u-C", 0.10, 0.975),
        )
        .await;
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.975)
            .ok("u-C", 0.10, 0.975);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::SkippedActiveUnavailable),
            "a band peer is not a viable target → the historical skip, no thrashing swap: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A stays active — the band peers are not rotatable targets"
        );
    }

    #[tokio::test]
    async fn blind_swap_kill_switch_disables_the_path_but_the_sli_keeps_measuring() {
        // Issue #452 (ADR-0017): the config kill-switch. `session_blind_swap_secs` parked high (the
        // `tunables()` helper's 86400 ceiling) disables the SWAP even long past the interim T — yet
        // the gate-premise SLI (#482), which keys off the interim const, keeps firing, so #484's
        // ratification data keeps flowing while the action is disabled (the deliberate const/config split).
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.70, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        assert!(
            daemon.session_blind_swap_secs >= 86_400,
            "the helper parks the gate at the kill-switch ceiling (swap disabled by default)",
        );
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        // Advance FAR past the interim T (but nowhere near the 24 h kill-switch ceiling).
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS * 10));
        let out = daemon.tick().await;
        assert!(
            !matches!(out.action, TickAction::PreemptivelySwapped { .. }),
            "the config kill-switch disables the swap even long past the interim T: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "no swap fired — active unchanged"
        );
        assert!(
            out.events
                .iter()
                .any(|e| matches!(e, Event::BlindGateEligible { .. })),
            "the gate-premise SLI keeps measuring at the interim const even with the swap disabled: {:?}",
            out.events,
        );
    }

    #[tokio::test]
    async fn blind_swap_below_the_risk_band_does_not_fire() {
        // Issue #452 / #484: an anchor BELOW the risk band (50 % < the 60 % band) never arms the
        // gate, however long the active is blind — the preemptive swap acts only near the band, where
        // self-exhaustion is a real risk. Below it, the historical skip stands.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.50, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::SkippedActiveUnavailable),
            "an anchor below the risk band never arms the gate, however long blind: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A unchanged — below the band"
        );
    }

    /// A warmed three-account daemon with u-A (slot 0) blind, its pre-blind anchor seeded to
    /// `anchor_session` and this window's retained high-water mark to `mark_session`, and the clock
    /// advanced one second past the interim gate bound — the tick on which `blind_swap` decides (issue
    /// #619). The tick-based `.ok(...)` poller does not stamp `session_resets_at`, so the mark is
    /// seeded through the real [`swap::SessionHighWater::fold`] seam ([`seed_high_water`]) and the
    /// anchor set directly; a blind episode has no successful active poll to refresh either, so both
    /// stay frozen through the deciding tick exactly as they would in production.
    async fn blind_daemon_with_seeded_anchor(anchor_session: f64, mark_session: f64) -> FakeDaemon {
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.50, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A active before it goes blind"
        );
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        // Seed the episode directly: the anchor's own pre-blind reading (`anchor_session`) and the
        // window's high-water mark (`mark_session`) — an earlier plausible reading `fold`'s max kept.
        let anchor_at = daemon.clock.now();
        daemon.state.last_good = Some(LastGood {
            session: anchor_session,
            weekly: 0.20,
            at: anchor_at,
        });
        seed_high_water(&mut daemon, mark_session);
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        daemon
    }

    #[tokio::test]
    async fn blind_swap_fires_on_a_stale_low_pre_blind_anchor() {
        // Issue #619 AC 1: a stale / cache-lagged LOW `/oauth/usage` reading arriving JUST before the
        // active account goes blind writes a below-band anchor (`last_good`, #450). Keyed off the RAW
        // anchor the #452 gate sees it under the risk band and DECLINES the otherwise-due preemptive
        // swap, letting the account burn to exhaustion unobserved for the whole blind window — the
        // exact overshoot #614 narrows, reached one path over through the blind gate. The gate now
        // decides on the anchor's PLAUSIBLE session (raised to the frozen 0.90 high-water mark), so
        // the swap that was actually due fires despite the stale 0.10 the API last echoed back.
        let mut daemon = blind_daemon_with_seeded_anchor(0.10, 0.90).await;
        let swapped = daemon.tick().await;
        assert!(
            matches!(
                swapped.action,
                TickAction::PreemptivelySwapped { from: 0, .. }
            ),
            "the corrected anchor (0.90 ≥ band) fires the due swap despite the stale-low 0.10 \
             reading: {:?}",
            swapped.action,
        );
        assert_ne!(
            daemon.state.active,
            Some(0),
            "the active moved off the blind u-A"
        );
        // The #482 eligibility SLI ALSO records this episode (it shares the corrected arming test), so
        // the ratification data tracks the swaps the gate actually fires — pinning the
        // `note_blind_gate_eligibility` half of the correction (reverting it leaves this unfired).
        assert!(
            swapped
                .events
                .iter()
                .any(|e| matches!(e, Event::BlindGateEligible { .. })),
            "the corrected gate turns eligible, so the #482 SLI records the episode: {:?}",
            swapped.events,
        );
        // #614 invariant: the DECISION used the corrected value, but the `blind_preempt` swap line
        // logs the RAW anchor pct (10) — `session_pct` is the last-known measurement (documented as
        // the stale pre-blind anchor), never the synthesized correction.
        assert!(
            swapped.events.iter().any(|e| matches!(
                e,
                Event::Swap {
                    reason: SwapReason::BlindPreempt,
                    session_pct: 10,
                    ..
                }
            )),
            "the swap line carries the raw stale-low anchor pct, not the correction: {:?}",
            swapped.events,
        );
    }

    #[tokio::test]
    async fn blind_swap_does_not_fire_on_a_genuinely_low_pre_blind_anchor() {
        // Issue #619 AC 2: the correction must not OVER-fire. Here the anchor is GENUINELY at 0.10 —
        // the window's high-water mark AGREES (0.10), so the reading was not stale-low — and a
        // below-band anchor never arms the gate, exactly as before #619. Distinct from
        // `blind_swap_below_the_risk_band_does_not_fire` (which seeds NO mark at all): this pins that
        // the correction is inert when the mark confirms the low reading, rather than a blanket raise.
        let mut daemon = blind_daemon_with_seeded_anchor(0.10, 0.10).await;
        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::SkippedActiveUnavailable),
            "a genuinely-low anchor (mark agrees) stays disarmed however long blind: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A unchanged — genuinely below the band"
        );
    }

    // --- #582 server-`Retry-After` swap-away + circuit-breaker (ADR-0017) ---------------------

    /// A warmed three-account daemon replaying the 2026-07-17 incident: u-A active with a
    /// BELOW-band anchor (`anchor_session`, 29 % observed) that then `429`s carrying a long server
    /// `Retry-After`, with u-B / u-C at `peer_session` as candidate targets. Returns it blind, with
    /// the clock advanced just past the gate bound, on the tick the #582 arm decides.
    ///
    /// The peers' session usage is the knob the reserve tests turn: at 10 % BOTH are viable (two
    /// candidates, the reserve satisfied); pushing one over the 80 % `target_max` leaves exactly one
    /// and arms [`NextSwapReason::OnlyCandidate`].
    async fn blind_retry_after_daemon(
        anchor_session: f64,
        peer_c_session: f64,
        retry_after: Duration,
    ) -> FakeDaemon {
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", anchor_session, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", peer_c_session, 0.10),
        )
        .await;
        // Arm the SWAP bound at the interim T (`tunables()` parks it at the kill-switch ceiling so
        // baseline tests stay inert). The risk band stays at its 0.60 default — every test here
        // keeps the anchor BELOW it, so ONLY the #582 server-directed arm can fire.
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A active before the throttle"
        );

        // Throttle the active with a LONG server directive: its reading clears (the #582 blindness)
        // while the below-band anchor is retained, and the un-clamped floor (#453) arms a window far
        // longer than the gate bound.
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", Some(retry_after))
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", peer_c_session, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        // Cross the gate bound. The directive's window (3600 s) still has hours to run, so the
        // server is STILL holding u-A off — the signal the #582 arm keys on.
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        daemon
    }

    #[tokio::test]
    async fn a_server_retry_after_swaps_the_blind_active_away_below_the_risk_band() {
        // AC 1 (issue #582): the 2026-07-17 replay. u-A `429`'d carrying `Retry-After: 3600` at a
        // 29 % anchor and went blind for the whole directive while Claude Code burned it to
        // exhaustion (+12pp weekly behind the blindness). At 29 % it fell through EVERY swap path:
        // the reactive trigger and the #539 projection both need a fresh reading, and #452's arm
        // needs anchor >= 60 %. The server directive is now itself the swap-away signal — a swap
        // needs no usage poll, so it works while blind.
        let mut daemon = blind_retry_after_daemon(0.29, 0.10, Duration::from_secs(3600)).await;

        let swapped = daemon.tick().await;
        assert!(
            matches!(
                swapped.action,
                TickAction::PreemptivelySwapped { from: 0, .. }
            ),
            "a long server Retry-After swaps the blind active away, not a blind wait: {:?}",
            swapped.action,
        );
        assert_ne!(
            daemon.state.active,
            Some(0),
            "the active moved off the throttled u-A"
        );
        // REUSES `reason=blind_preempt` (issue #582): no new reason token, so `reliability.rs`'s
        // `parse_swap_events` folds it into the existing #452 counter and the `_ => continue`
        // catch-all is never reached. `session_pct` is the stale below-band anchor (29) — the only
        // session signal available while blind.
        assert!(
            swapped.events.iter().any(|e| matches!(
                e,
                Event::Swap {
                    reason: SwapReason::BlindPreempt,
                    session_pct: 29,
                    ..
                }
            )),
            "the swap reuses reason=blind_preempt carrying the stale anchor pct: {:?}",
            swapped.events,
        );
        // The swap is recorded as walk evidence — the counter the circuit-breaker reads.
        assert_eq!(
            daemon.state.retry_after_swaps.len(),
            1,
            "the server-throttled swap is counted toward the walk breaker",
        );
    }

    #[tokio::test]
    async fn the_retry_after_swap_is_disabled_by_the_existing_kill_switch() {
        // AC 5 (issue #582): NO new tunable. The arm shares condition 1 with #452, so parking
        // `session_blind_swap_secs` at the documented kill-switch ceiling (ADR-0017) disables the
        // server-directed arm exactly as it disables the anchor one — the operator keeps ONE dial.
        let mut daemon = blind_retry_after_daemon(0.29, 0.10, Duration::from_secs(3600)).await;
        daemon.session_blind_swap_secs = u64::MAX;

        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::SkippedActiveUnavailable),
            "the kill-switch disables the server-directed arm too: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A unchanged — path disabled"
        );
    }

    #[tokio::test]
    async fn a_lapsed_retry_after_does_not_arm_the_swap_away() {
        // Issue #582: the arm keys on a directive the server is STILL enforcing, not on one merely
        // once seen. A window that has LAPSED means the daemon may re-poll now — the account may be
        // about to recover on its own — so a swap-away would be premature. (The anchor stays below
        // the band, so #452 cannot fire either and the historical skip stands.)
        let mut daemon = blind_retry_after_daemon(0.29, 0.10, Duration::from_secs(60)).await;

        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::SkippedActiveUnavailable),
            "a lapsed Retry-After window does not arm the swap-away: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A unchanged — directive spent"
        );
    }

    #[tokio::test]
    async fn a_zero_retry_after_does_not_arm_the_below_band_swap_away() {
        // Issue #582 classifies `ra > 0` — NOT merely "a header was present". A zero directive
        // contributes nothing to the wait (`widened.max(0) == widened`), so the window is the
        // daemon's OWN self-capped exponential: precisely the bounded blindness the ratified #452
        // anchor gate already governs, and which below the band it deliberately leaves alone.
        //
        // This is the DOMINANT real case, not a corner — ADR-0017's S1 spike records that ALL 181
        // observed 429s carried `retry_after_secs=0` (the blind window there was the daemon's own
        // back-off, #453-capped, not a server directive). Gating on mere presence would swap
        // below-band accounts away on ordinary self-backoff and let the walk alarm claim a "server
        // throttle" that never existed.
        // Deliberately NOT `blind_retry_after_daemon`: that helper takes ONE 429 then jumps the
        // clock, so the self-backoff window has already LAPSED by the decision and the swap-away
        // would be declined by the lapse check — passing without ever exercising the zero-filter.
        // This drives S1's real shape instead: SUSTAINED zero-directive 429s, each re-poll
        // re-arming the account's own 120 s cap (#453), so the window stays OPEN while blindness
        // accumulates far past the 300 s gate. That is precisely the state that WOULD swap if a
        // zero were mistaken for a server directive — so this test can actually fail.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.29, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        daemon.session_blind_swap_secs = BLIND_GATE_SECS;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", Some(Duration::ZERO))
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);

        // Advance in sub-window steps so u-A is re-polled (and re-429'd) throughout, holding its
        // self-backoff window open the whole way — never a single lapse the gate could decline on.
        let mut fired = None;
        for _ in 0..40 {
            daemon.clock.advance(Duration::from_secs(60));
            let out = daemon.tick().await;
            if matches!(out.action, TickAction::PreemptivelySwapped { .. }) {
                fired = Some(out.action);
                break;
            }
        }
        assert!(
            fired.is_none(),
            "a zero Retry-After never arms the below-band swap-away, however long blind: {fired:?}",
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A unchanged — no server directive"
        );

        // The scenario is NOT degenerate: it really did reach (far past) the gate while blind, so
        // the non-fire above is the zero-filter's doing, not a gate that never armed.
        let anchor = daemon
            .state
            .last_good
            .expect("the pre-blind anchor is retained");
        let blind_secs = daemon
            .clock
            .now()
            .saturating_duration_since(anchor.at)
            .as_secs();
        assert!(
            blind_secs > BLIND_GATE_SECS,
            "the episode must cross the gate for this to test anything: blind_secs={blind_secs}",
        );
        assert!(
            daemon.state.accounts[0].last_reading.is_none(),
            "u-A is genuinely blind throughout",
        );
        // The mechanism, pinned at the field: a zero is normalized away, so no directive is ever
        // "holding" u-A — even though its self-backoff window IS open (the lapse check is NOT what
        // declined the swap here).
        assert_eq!(
            daemon.state.accounts[0].health.poll_backoff_retry_after, None,
            "a zero Retry-After is normalized to None — it is not a server directive",
        );
        assert!(
            daemon.state.accounts[0]
                .health
                .poll_backoff_until
                .is_some_and(|until| until > daemon.clock.now()),
            "the self-backoff window is OPEN — so only the zero-filter can be declining the swap",
        );
        assert!(
            daemon.state.retry_after_swaps.is_empty(),
            "a self-backoff blind window is not walk evidence",
        );
        // ...and `status` does not over-claim either: below the band with no server directive, the
        // anchor arm is the only authority and it says OK (the #479 projection is unchanged here).
        let snapshot = daemon.tick().await.snapshot;
        assert_eq!(
            snapshot.accounts[0]
                .blind_active
                .as_ref()
                .map(|b| b.auto_protection_degraded),
            Some(false),
            "no server directive → the below-band projection is unchanged from #479",
        );
    }

    #[tokio::test]
    async fn a_recovered_poll_clears_the_retained_retry_after() {
        // Issue #582: the retained directive dies with the window it described. A non-throttling
        // poll means the account is READABLE again, so a stale `Retry-After` must never outlive it
        // and re-arm the swap-away on a recovered account.
        let mut daemon = blind_retry_after_daemon(0.29, 0.10, Duration::from_secs(3600)).await;
        assert_eq!(
            daemon.state.accounts[0].health.poll_backoff_retry_after,
            Some(Duration::from_secs(3600)),
            "the directive is retained while its window holds u-A off",
        );

        // u-A answers again. The clock is past the armed window's start by T+1 only, so move it
        // past the directive itself, then let the recovered poll land.
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.29, 0.20)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        daemon.clock.advance(Duration::from_secs(3600));
        while daemon.state.accounts[0].last_reading.is_none() {
            daemon.tick().await;
        }
        assert_eq!(
            daemon.state.accounts[0].health.poll_backoff_retry_after, None,
            "a clean poll clears the retained directive with its window",
        );
    }

    #[tokio::test]
    async fn the_retry_after_swap_never_spends_the_last_viable_target() {
        // AC 2 (issue #582): the swap-away is SPECULATIVE — it acts on a server directive, not on
        // observed near-limit usage — so it must yield the LAST viable target to a confirmed-
        // exhaustion swap rather than consume it on a guess. u-C is pushed over the 80 % target_max
        // reserve, leaving u-B as the sole candidate: the path holds, and SAYS SO.
        let mut daemon = blind_retry_after_daemon(0.29, 0.90, Duration::from_secs(3600)).await;

        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::SkippedActiveUnavailable),
            "no swap when it would spend the last viable target: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A held — the target is reserved"
        );
        // "Reported, not hidden": the hold is a deliberate choice, and an operator watching an
        // account go dark deserves to see it made.
        assert!(
            out.events.iter().any(|e| matches!(
                e,
                Event::BlindPreemptReserveHold {
                    retry_after_secs: 3600,
                    ..
                }
            )),
            "the reserve hold is reported: {:?}",
            out.events,
        );
        // Issue #582's NAMED regression: `status` must NOT report "auto-protection OK" while the
        // daemon sits on a below-band account blind behind a server directive. The wire projection
        // shows `auto_protection_degraded` even at the 29 % anchor the anchor arm alone would call
        // OK — the surface the issue said "reported auto-protection OK for the whole episode".
        assert_eq!(
            out.snapshot.accounts[0].blind_active,
            Some(BlindActive {
                blind_secs: BLIND_GATE_SECS + 1,
                last_known_session_pct: 29,
                auto_protection_degraded: true,
            }),
            "status reports auto-protection DEGRADED during a #582 reserve hold, not OK",
        );
        assert!(
            daemon.state.retry_after_swaps.is_empty(),
            "a held swap is not walk evidence",
        );
        // Edge-triggered: a 3600 s window spans hundreds of ticks, so the line fires once per
        // episode — not once per held tick.
        let again = daemon.tick().await;
        assert!(
            !again
                .events
                .iter()
                .any(|e| matches!(e, Event::BlindPreemptReserveHold { .. })),
            "the reserve hold is edge-triggered, not re-emitted per held tick: {:?}",
            again.events,
        );
    }

    #[tokio::test]
    async fn the_reserve_does_not_bound_the_ratified_anchor_band_swap() {
        // Issue #582 (regression guard): the circuit-breaker binds ONLY the new server-directed arm
        // firing on its own authority. With the anchor INSIDE the band (68 %) the ratified #452 swap
        // fires on a sole viable target exactly as it always has — bounding it there would regress a
        // ratified AC, so `speculative` is `None` whenever the anchor is armed.
        let mut daemon = blind_retry_after_daemon(0.68, 0.90, Duration::from_secs(3600)).await;

        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::PreemptivelySwapped { from: 0, .. }),
            "the #452 anchor-band swap still fires on its last viable target: {:?}",
            out.action,
        );
        assert!(
            !out.events
                .iter()
                .any(|e| matches!(e, Event::BlindPreemptReserveHold { .. })),
            "the reserve never reports against a ratified anchor-band swap: {:?}",
            out.events,
        );
    }

    #[tokio::test]
    async fn repeated_retry_after_swaps_stop_the_rotation_and_alarm() {
        // AC 3 (issue #582): the throttle follows the ACTIVE ROLE — each account observed took its
        // `RA:3600` within minutes of TAKING the slot, while peers never got one — so an unbounded
        // swap-away WALKS the throttle around the roster. At the limit, rotation stops: holding a
        // blind-but-low account beats walking a 3600 s throttle onto the last good one.
        let mut daemon = blind_retry_after_daemon(0.29, 0.10, Duration::from_secs(3600)).await;
        // The state two prior server-throttled swaps leave behind (the previous test proves a fired
        // swap appends exactly this), both inside the trailing window.
        let now = daemon.clock.now();
        daemon.state.retry_after_swaps = vec![now, now];

        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::SkippedActiveUnavailable),
            "the walk breaker stops the rotation: {:?}",
            out.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "position held — the walk stopped"
        );
        assert!(
            out.events.iter().any(|e| matches!(
                e,
                Event::RetryAfterWalk {
                    swaps: 2,
                    window_secs: 3600,
                    retry_after_secs: 3600,
                    ..
                }
            )),
            "the walk raises an alarm naming the count and window: {:?}",
            out.events,
        );
        // Same #582 regression guard as the reserve hold: a stopped walk leaves the account blind
        // and burning, so `status` must report DEGRADED, never "auto-protection OK".
        assert_eq!(
            out.snapshot.accounts[0]
                .blind_active
                .as_ref()
                .map(|b| b.auto_protection_degraded),
            Some(true),
            "status reports auto-protection DEGRADED while the walk breaker holds a blind account",
        );
        // Edge-triggered, like the reserve hold.
        let again = daemon.tick().await;
        assert!(
            !again
                .events
                .iter()
                .any(|e| matches!(e, Event::RetryAfterWalk { .. })),
            "the walk alarm is edge-triggered, not re-emitted per held tick: {:?}",
            again.events,
        );
    }

    #[tokio::test]
    async fn retry_after_swaps_outside_the_window_do_not_stop_the_rotation() {
        // AC 3 (issue #582), the "within a window" half: the breaker counts a trailing window, not
        // a daemon lifetime. Two swaps that have AGED OUT are not a walk — a throttle that recurred
        // hours later is a fresh episode, and refusing to protect the active on that history would
        // strand it blind. The aged entries are pruned on read, so the record cannot grow unbounded.
        let mut daemon = blind_retry_after_daemon(0.29, 0.10, Duration::from_secs(3600)).await;
        let stale = daemon.clock.now() - (RETRY_AFTER_WALK_WINDOW + Duration::from_secs(1));
        daemon.state.retry_after_swaps = vec![stale, stale];

        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::PreemptivelySwapped { from: 0, .. }),
            "swaps outside the window are not a walk — the swap-away still fires: {:?}",
            out.action,
        );
        assert!(
            !out.events
                .iter()
                .any(|e| matches!(e, Event::RetryAfterWalk { .. })),
            "no alarm on aged-out history: {:?}",
            out.events,
        );
        assert_eq!(
            daemon.state.retry_after_swaps.len(),
            1,
            "the aged entries were pruned; only THIS swap remains",
        );
    }

    #[tokio::test]
    async fn the_anchor_armed_path_still_prunes_the_walk_record() {
        // Issue #582 (regression guard): the walk record is appended by ANY server-throttled swap
        // (`retry_after.is_some()`), including a ratified #452 anchor-band one — but the breaker
        // consults it only on the speculative arm. If the prune rode the breaker, an anchor-armed
        // episode (anchor 68 %, in-band) would grow the vector without bound. It must prune anyway.
        let mut daemon = blind_retry_after_daemon(0.68, 0.10, Duration::from_secs(3600)).await;
        // Seed aged-out entries an anchor-armed episode would otherwise never clear.
        let stale = daemon.clock.now() - (RETRY_AFTER_WALK_WINDOW + Duration::from_secs(1));
        daemon.state.retry_after_swaps = vec![stale, stale, stale];

        let out = daemon.tick().await;
        assert!(
            matches!(out.action, TickAction::PreemptivelySwapped { from: 0, .. }),
            "the in-band anchor arm fires (the breaker never bounds it): {:?}",
            out.action,
        );
        // The prune ran despite the swap being anchor-armed (not speculative): the three aged
        // entries are gone and only THIS swap's instant remains — the vector stays bounded.
        assert_eq!(
            daemon.state.retry_after_swaps.len(),
            1,
            "the anchor-armed path prunes the walk record; it does not grow unbounded",
        );
    }

    // --- #539 velocity-projection preemptive swap (ADR-0017) ---------------------------------

    /// A warmed three-account daemon with u-A active at `active_session` (kept below the 0.93
    /// effective ceiling — the 99 % ceiling minus the 6 pp tail margin, issue #597 — so the reactive
    /// path HOLDS and the projective peer is the only thing that can fire) and u-B / u-C viable targets
    /// at 10 % (under the 80 % reserve). The velocity gate is left INERT
    /// (`session_velocity_horizon_secs == 0`, the `tunables()` default) and the EMA slot is `None` —
    /// each direct-call test arms the horizon and seeds `state.accounts[0].session_velocity` to the exact signal
    /// it exercises, then calls [`Daemon::velocity_swap`] with a cloned reading set (so `self` is free
    /// for the `&mut` swap path). Peer readings default to viable targets.
    ///
    /// The ceiling is raised to 99 (from the `three_account_daemon` default of 95) so the effective
    /// ceiling is 0.93: with `near_limit_poll_secs == 0` the reactive `poll_gap` is 0, so the reactive
    /// threshold is exactly 0.93 and in-band readings in `[0.85, 0.93)` hold reactively while the
    /// projection can still cross 0.93.
    async fn warmed_velocity_daemon(active_session: f64) -> FakeDaemon {
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", active_session, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        // #597: the reactive arm now fires at the effective ceiling (ceiling − tail margin), so at the
        // 95 default that is 0.89 — too tight a band above the 0.85 projection floor. Raise the ceiling
        // to 99 (effective ceiling 0.93) so the in-band climbing readings hold reactively.
        daemon.session_ceiling_strategy = Strategy::fixed(99.0);
        daemon.session_ceiling_base = 0.99;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A active before the projective swap"
        );
        // Frozen clock through the warm-up → every re-poll of the active saw a zero interval, so the
        // EMA reset to `None`: the seed each test installs is the ONLY velocity signal in play.
        assert!(
            daemon.state.accounts[0].session_velocity.is_none(),
            "the warm-up leaves the EMA unseeded (zero-interval resets)",
        );
        daemon
    }

    #[tokio::test]
    async fn velocity_swap_fires_past_the_projected_trigger() {
        // Issue #539 (ADR-0017): the headline acceptance, end-to-end through the real `decide_action`
        // hook. u-A climbs inside the guard band (86 % → 90 % → 92 %, all below the 0.93 effective
        // ceiling — the 99 % ceiling minus the 6 pp tail margin, #597 — so the reactive path HOLDS
        // every tick; `near_limit_poll_secs == 0` makes the reactive poll_gap 0, so the reactive
        // threshold is exactly 0.93) at a SUSTAINED rate. Once two intervals have been blended
        // (samples ≥ MIN_VELOCITY_SAMPLES), the projection `last + rate × H` crosses the effective
        // ceiling within the horizon and the active swaps AWAY — ahead of the observed reading
        // tripping the reactive threshold — closing the observed reactive overshoot (#363).
        // Warmed on a frozen clock (u-A at 86 %, schedule A, B, A, C): the helper asserts warmed-up,
        // u-A active, and — since no interval elapsed — an unseeded (`None`) EMA. Same fixture the
        // five direct-call velocity_swap tests route through.
        let mut daemon = warmed_velocity_daemon(0.86).await;
        // Arm the projective gate at 150 s — the TOP of the #538-validated safe band (H ≤ 150 s).
        daemon.session_velocity_horizon_secs = 150;

        // First climbing interval (86 % → 90 % over 60 s): the next active poll is tick 5 (A). This
        // SEEDS the EMA (samples = 1) — a single interval, still below MIN_VELOCITY_SAMPLES.
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.90, 0.20)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        daemon.clock.advance(Duration::from_secs(60));
        let seeded = daemon.tick().await;
        assert!(
            matches!(seeded.action, TickAction::Held),
            "one interval (samples = 1) is not yet SUSTAINED — the projection holds: {:?}",
            seeded.action,
        );
        assert_eq!(
            daemon.state.accounts[0].session_velocity.map(|v| v.samples),
            Some(1),
            "the first climbing interval seeds the EMA at one sample",
        );

        // Second climbing interval (90 % → 92 % over 60 s): tick 6 polls the peer u-B (no active
        // update), tick 7 re-polls u-A and blends the second interval (samples = 2 → SUSTAINED). The
        // projection 0.92 + rate × 150 now clears the 0.93 effective ceiling, so the projective swap
        // fires.
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.92, 0.20)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        daemon.clock.advance(Duration::from_secs(60));
        let peer = daemon.tick().await; // tick 6 = u-B, no active poll
        assert!(
            matches!(peer.action, TickAction::Held),
            "the peer poll does not advance the active's velocity: {:?}",
            peer.action,
        );
        let swapped = daemon.tick().await; // tick 7 = u-A → samples = 2, projection crosses
        assert!(
            matches!(
                swapped.action,
                TickAction::VelocityPreemptivelySwapped { from: 0, .. }
            ),
            "the projected usage crosses the effective ceiling within the horizon → preemptive swap: {:?}",
            swapped.action,
        );
        assert_ne!(
            daemon.state.active,
            Some(0),
            "the active moved off the climbing u-A"
        );
        // The swap logged a `velocity_preempt` reason carrying the FRESH observed reading (92 %, the
        // live swap-out point — the projected-swap-out-overshoot SLI sample), never a stale anchor.
        // The projective peer runs ONLY where the reactive path HELD (observed strictly below the
        // effective ceiling in fraction space), so a swap-out is always below the effective ceiling —
        // here 92 < the 0.93 effective ceiling. With the effective ceiling sitting a 6 pp tail margin
        // below the 99 % ceiling, the ROUNDED swap-out pct stays well under the P100 ≤ 98
        // projected-overshoot acceptance (the #538 spike's empirically-measured SLO) — the very margin
        // the #597 ceiling redesign buys, which the reliability readout then surfaces honestly.
        assert!(
            swapped.events.iter().any(|e| matches!(
                e,
                Event::Swap {
                    reason: SwapReason::VelocityPreempt,
                    session_pct: 92,
                    ..
                }
            )),
            "a velocity_preempt swap event carrying the fresh observed pct: {:?}",
            swapped.events,
        );
        // Issue #634: that same swap line carries the PROJECTION it fired on, end-to-end from the
        // live decision — so the below-the-ceiling swap-out (92 < the 93 effective ceiling) explains
        // itself. Assert the ingredients are internally consistent with the decision rather than
        // pinning brittle EMA floats: the stamped constants are the ones in force (horizon 150, the
        // #597 effective ceiling 93 = (0.99 − 0.06) × 100), the rate is a real climbing rate, and the
        // projection reproduces `observed + rate × horizon` — the very predicate `velocity_swap`
        // checked — and clears the stamped ceiling that fired it.
        let projection = swapped
            .events
            .iter()
            .find_map(|e| match e {
                Event::Swap {
                    reason: SwapReason::VelocityPreempt,
                    projection,
                    ..
                } => *projection,
                _ => None,
            })
            .expect("the velocity_preempt swap carries its projection ingredients (#634)");
        assert_eq!(
            projection.horizon_secs, 150,
            "the horizon in force is stamped"
        );
        assert!(
            (projection.ceiling_pct - 93.0).abs() < 1e-9,
            "the EFFECTIVE ceiling (99 − 6 pp tail) is the stamped comparand, got {}",
            projection.ceiling_pct,
        );
        assert!(
            projection.rate_pct_per_sec > 0.0,
            "a climbing rate is logged, got {}",
            projection.rate_pct_per_sec,
        );
        // The self-consistency identity: projected == observed (92.0) + rate × horizon, so an offline
        // reader recomputes the decision from the line's own tokens. The observed reading is exactly
        // 0.92 → 92.0 here.
        let reproduced = 92.0 + projection.rate_pct_per_sec * 150.0;
        assert!(
            (projection.projected_pct - reproduced).abs() < 1e-6,
            "projected={} must equal observed + rate × horizon = {}",
            projection.projected_pct,
            reproduced,
        );
        assert!(
            projection.projected_pct >= projection.ceiling_pct,
            "the projection that fired the swap clears its stamped ceiling: {} >= {}",
            projection.projected_pct,
            projection.ceiling_pct,
        );
    }

    #[tokio::test]
    async fn velocity_swap_does_not_fire_onto_a_weekly_tail_margin_band_peer() {
        // Issue #607: the velocity-projection path fires a REAL swap through the live `decide_action`
        // hook, so the weekly line it hands to target selection must be the effective ceiling — a
        // raw-ceiling gate here would land the preemptive swap on a band peer the reactive arm
        // bounces straight back. Same climbing fixture as
        // `velocity_swap_fires_past_the_projected_trigger`, but BOTH peers sit in the tail-margin
        // band (weekly 0.975 ∈ `[0.97, 0.98)`) from the start (warm-up HOLDS regardless, so the band
        // peers never mislead it early), so at the tick the projection crosses there is NO viable
        // target and the daemon HOLDS. Against the raw ceiling these peers are viable and it FIRES,
        // so reverting the `decide_action` → `velocity_swap` weekly arg to the raw ceiling flips this
        // to `VelocityPreemptivelySwapped` and fails.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.86, 0.20)
                .ok("u-B", 0.10, 0.975)
                .ok("u-C", 0.10, 0.975),
        )
        .await;
        // #597: raise the ceiling to 99 (effective 0.93) so the climbing readings hold reactively
        // and only the projection can cross — matching `warmed_velocity_daemon`.
        daemon.session_ceiling_strategy = Strategy::fixed(99.0);
        daemon.session_ceiling_base = 0.99;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        assert_eq!(
            daemon.state.active,
            Some(0),
            "u-A active before the projection"
        );
        daemon.session_velocity_horizon_secs = 150;

        // First climbing interval (0.86 → 0.90): seeds the EMA at one sample (not yet SUSTAINED).
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.90, 0.20)
            .ok("u-B", 0.10, 0.975)
            .ok("u-C", 0.10, 0.975);
        daemon.clock.advance(Duration::from_secs(60));
        daemon.tick().await; // tick 5 (A): seeds
                             // Second interval (0.90 → 0.92): samples = 2 → SUSTAINED; the projection crosses the 0.93
                             // effective ceiling within the horizon — the exact tick the fires test swaps. Every peer is
                             // in the band, so there is no viable target.
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.92, 0.20)
            .ok("u-B", 0.10, 0.975)
            .ok("u-C", 0.10, 0.975);
        daemon.clock.advance(Duration::from_secs(60));
        daemon.tick().await; // tick 6 (B): peer poll, no active velocity update
        let held = daemon.tick().await; // tick 7 (A): projection crosses, but no viable target
        assert!(
            matches!(held.action, TickAction::Held),
            "the projection crosses but every peer is in the band → hold, not a thrashing swap: {:?}",
            held.action,
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "no preemptive swap landed on a band peer",
        );
    }

    #[tokio::test]
    async fn velocity_swap_holds_below_the_project_above_guard() {
        // Issue #539 / #538: the free guard. A reading BELOW `session_velocity_min_project_above`
        // (80 % < 85 %) never projects, even with a steep, well-sustained velocity — the guard
        // short-circuits BEFORE the projection, excluding spurious low-usage projections cheaply.
        let mut daemon = warmed_velocity_daemon(0.80).await;
        daemon.session_velocity_horizon_secs = 150;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.01,
            samples: 5,
        });
        let at = daemon.clock.now();
        let readings = daemon.state.readings();
        let mut events = Vec::new();
        let action = daemon
            .velocity_swap(at, 0, 0.95, 0.98, &readings, &mut events)
            .await;
        assert!(
            matches!(action, TickAction::Held),
            "a reading below the guard band holds regardless of velocity: {action:?}",
        );
        assert_eq!(daemon.state.active, Some(0), "no swap — below the guard");
        assert!(events.is_empty(), "no swap event emitted");
    }

    #[tokio::test]
    async fn velocity_swap_holds_without_sustained_velocity() {
        // Issue #539: SUSTAINED means ≥ MIN_VELOCITY_SAMPLES blended intervals. A single-interval
        // spike (samples = 1) and a MISSING signal (`None`, the poll-gap case #540 owns) both HOLD on
        // the fresh reading — the projection never fires on a one-off spike or an unwarmed velocity.
        let mut daemon = warmed_velocity_daemon(0.90).await;
        daemon.session_velocity_horizon_secs = 150;
        let at = daemon.clock.now();
        let readings = daemon.state.readings();

        // A single-interval spike steep enough to cross IF it counted — but samples = 1 holds.
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.01,
            samples: 1,
        });
        let mut events = Vec::new();
        let spike = daemon
            .velocity_swap(at, 0, 0.95, 0.98, &readings, &mut events)
            .await;
        assert!(
            matches!(spike, TickAction::Held),
            "a single-interval spike is not SUSTAINED → hold: {spike:?}",
        );
        assert!(events.is_empty());

        // No retained signal at all → hold (never project on a missing velocity).
        daemon.state.accounts[0].session_velocity = None;
        let mut events = Vec::new();
        let missing = daemon
            .velocity_swap(at, 0, 0.95, 0.98, &readings, &mut events)
            .await;
        assert!(
            matches!(missing, TickAction::Held),
            "a missing velocity signal → hold: {missing:?}",
        );
        assert_eq!(daemon.state.active, Some(0), "no swap either way");
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn velocity_swap_holds_when_the_projection_falls_short() {
        // Issue #539: an in-band, SUSTAINED, but SHALLOW velocity whose `rate × H` reach stays under
        // the gap to the effective ceiling does NOT fire — the projection is short, so hold and let the
        // reactive path catch it if it keeps climbing. (0.90 + 0.0001 × 150 = 0.915 < 0.93, the
        // effective ceiling for the 0.99 ceiling passed below.)
        let mut daemon = warmed_velocity_daemon(0.90).await;
        daemon.session_velocity_horizon_secs = 150;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.0001,
            samples: 3,
        });
        let at = daemon.clock.now();
        let readings = daemon.state.readings();
        let mut events = Vec::new();
        let action = daemon
            .velocity_swap(at, 0, 0.99, 0.98, &readings, &mut events)
            .await;
        assert!(
            matches!(action, TickAction::Held),
            "a shallow projection that stays under the effective ceiling holds: {action:?}",
        );
        assert_eq!(daemon.state.active, Some(0));
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn velocity_swap_kill_switch_horizon_zero_never_fires() {
        // Issue #539 (ADR-0005): the config kill-switch. `session_velocity_horizon_secs == 0` reduces
        // the projection to the observed reading — which the reactive path already held below the
        // effective ceiling — so even a steep, sustained, in-band velocity (90 %, under the 0.93
        // effective ceiling) can never cross. The disabled projective path is a plain `Held`.
        let mut daemon = warmed_velocity_daemon(0.90).await;
        daemon.session_velocity_horizon_secs = 0;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.01,
            samples: 5,
        });
        let at = daemon.clock.now();
        let readings = daemon.state.readings();
        let mut events = Vec::new();
        let action = daemon
            .velocity_swap(at, 0, 0.95, 0.98, &readings, &mut events)
            .await;
        assert!(
            matches!(action, TickAction::Held),
            "the horizon-0 kill-switch disables the projective swap: {action:?}",
        );
        assert_eq!(daemon.state.active, Some(0), "no swap — kill-switch");
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn velocity_swap_holds_without_a_viable_target() {
        // Issue #539 / ADR-0013: an otherwise-eligible projection (in-band, sustained, crossing) but
        // every peer is over the 80 % `target_max_session_usage` reserve (u-B 85 %, u-C 90 %). The
        // reserve is HONORED — NOT the emergency `None` bypass — so `pick_target` finds none and the
        // projective peer HOLDS rather than swap onto a saturated peer.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.90, 0.20)
                .ok("u-B", 0.85, 0.10)
                .ok("u-C", 0.90, 0.10),
        )
        .await;
        daemon.session_velocity_horizon_secs = 150;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.001,
            samples: 3,
        });
        let at = daemon.clock.now();
        let readings = daemon.state.readings();
        let mut events = Vec::new();
        let action = daemon
            .velocity_swap(at, 0, 0.95, 0.98, &readings, &mut events)
            .await;
        assert!(
            matches!(action, TickAction::Held),
            "no peer under the reserve → the projective swap holds: {action:?}",
        );
        assert_eq!(daemon.state.active, Some(0), "u-A stays active — no target");
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn velocity_swap_honors_cooldown_not_emergency() {
        // Issue #539 (ADR-0017): the projective peer is NOT the emergency path — it HONORS the swap
        // cooldown (#10). With an otherwise-eligible projection but a swap inside the (jittered)
        // cooldown window, it defers as `SkippedCooldown` (the projection WANTED to fire) rather than
        // a silent hold — distinguishing it from the reserve-bypassing emergency swap.
        let mut daemon = warmed_velocity_daemon(0.90).await;
        daemon.session_velocity_horizon_secs = 150;
        // A 600 s cooldown with a swap that JUST happened (frozen clock → zero elapsed since).
        daemon.cooldown_strategy = Strategy::fixed(600.0);
        daemon.state.last_swap = Some(LastSwap {
            at: daemon.clock.now(),
        });
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.001,
            samples: 3,
        });
        let at = daemon.clock.now();
        let readings = daemon.state.readings();
        let mut events = Vec::new();
        let action = daemon
            .velocity_swap(at, 0, 0.95, 0.98, &readings, &mut events)
            .await;
        assert!(
            matches!(action, TickAction::SkippedCooldown),
            "an eligible projection inside the cooldown defers, not swaps: {action:?}",
        );
        assert_eq!(daemon.state.active, Some(0), "no swap — cooldown honored");
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn note_session_velocity_seeds_blends_and_resets() {
        // Issue #539: the retained-EMA update seam. First interval SEEDS at the raw instant rate (NOT
        // zero — a zero seed biases the EMA to asymptote BELOW the true rate and would miss real
        // overshoots); the second BLENDS at α = 0.5; a session DROP (window reset / recovery) and a
        // ZERO interval each RESET the slot to `None` (the climbing trend is stale / nothing to
        // integrate), forcing a fresh pair of samples before the projection can fire again.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;

        // First sample: seed at the raw instant rate (0.06 / 60 s = 0.001 /s), samples = 1.
        daemon.note_session_velocity(0, 0.80, 0.86, 60);
        let seeded = daemon.state.accounts[0]
            .session_velocity
            .expect("seeded on the first interval");
        assert!(
            (seeded.rate - 0.001).abs() < 1e-9,
            "seeded at the raw instant rate, not zero: {}",
            seeded.rate,
        );
        assert_eq!(seeded.samples, 1);

        // Second sample: blend at α (0.12 / 60 s = 0.002 /s), rate = 0.5·0.002 + 0.5·0.001, samples = 2.
        daemon.note_session_velocity(0, 0.86, 0.98, 60);
        let blended = daemon.state.accounts[0]
            .session_velocity
            .expect("still present after the blend");
        assert!(
            (blended.rate - (0.5 * 0.002 + 0.5 * 0.001)).abs() < 1e-9,
            "EMA blend at α = 0.5: {}",
            blended.rate,
        );
        assert_eq!(blended.samples, 2);

        // A session-usage DROP (next < prev — a 5 h window reset) resets the slot.
        daemon.note_session_velocity(0, 0.98, 0.10, 60);
        assert!(
            daemon.state.accounts[0].session_velocity.is_none(),
            "a usage drop resets the EMA (the climbing trend is stale)",
        );

        // Re-seed, then a ZERO interval (degenerate — nothing to integrate) resets it too.
        daemon.note_session_velocity(0, 0.10, 0.20, 60);
        assert!(
            daemon.state.accounts[0].session_velocity.is_some(),
            "re-seeded"
        );
        daemon.note_session_velocity(0, 0.20, 0.30, 0);
        assert!(
            daemon.state.accounts[0].session_velocity.is_none(),
            "a zero interval resets the EMA",
        );
    }

    /// A [`warmed_velocity_daemon`] (ceiling 99 → effective ceiling 0.93) with the reactive
    /// re-observation-gap lookahead ARMED at the production-default `near_limit_poll_secs = 60` and the
    /// projection peer OFF (`session_velocity_horizon_secs = 0`), so a `decide_action` call exercises
    /// the REACTIVE-velocity arm in isolation. The `tunables()` fixture ships `near_limit_poll_secs = 0`
    /// (poll_gap 0 → the reactive velocity term inert), so this is the wiring the production path uses
    /// but no other daemon test composes (issue #610). Peers (u-B / u-C at 0.10) are viable swap
    /// targets, so a fire executes end-to-end to `TickAction::Swapped`.
    async fn armed_reactive_daemon() -> FakeDaemon {
        let mut daemon = warmed_velocity_daemon(0.80).await;
        daemon.near_limit_poll_secs = 60; // DEFAULT_NEAR_LIMIT_POLL_SECS → reactive poll_gap armed (313 s)
        daemon.session_velocity_horizon_secs = 0; // projection kill-switch OFF → isolate the reactive arm
        daemon
    }

    /// A decision reading set with the active account (u-A, slot 0) at `session`, peers carried at their
    /// warmed viable-target readings.
    fn active_session_reading(daemon: &FakeDaemon, session: f64) -> Vec<Option<Usage>> {
        let mut readings = daemon.state.readings();
        readings[0] = Some(Usage {
            session,
            weekly: 0.20,
            weekly_resets_at: None,
            session_resets_at: None,
        });
        readings
    }

    #[tokio::test]
    async fn reactive_arm_fires_early_on_the_production_default_poll_gap() {
        // Issue #610: the production reactive-velocity path is the DEFAULT firing path
        // (`near_limit_poll_secs = 60` → `poll_gap = max(2×60, 313) = 313 s`, reactive checked first in
        // `decide_action`), yet no integration test composed it — every daemon test builds tunables via
        // `tunables()`, which hard-defaults `near_limit_poll_secs = 0` (poll_gap 0, reactive velocity
        // term inert). This drives the REAL path: a sustained EMA + an in-window reading fires the
        // reactive swap EARLIER than the bare effective ceiling, while a control reading below the
        // velocity-derived threshold holds. Fails if the poll_gap / velocity wiring regresses (e.g. the
        // poll_gap collapses to 0 → threshold = bare effective ceiling → the in-window reading holds).
        let eff = swap::effective_ceiling(0.99); // ceiling 99 → 0.93
        let poll_gap = swap::reactive_poll_gap_secs(60);
        assert_eq!(
            poll_gap,
            swap::REACTIVE_REOBSERVATION_GAP_SECS, // 313 s: at the default cadence the p90 floor dominates 2×60
            "the production-default near-limit cadence looks ahead over the p90 re-observation gap",
        );
        let rate = 0.0004; // frac/s (~2.4 %/min) — a sustained velocity inside the observed spread
        let threshold = swap::reactive_session_threshold(eff, rate, poll_gap); // 0.93 − 0.0004×313
        let in_window = threshold + 0.02; // at/above the velocity threshold, below the effective ceiling
        let below = threshold - 0.02; // below the velocity threshold
        assert!(
            in_window < eff,
            "the in-window reading {in_window} is an EARLY fire — strictly below the effective ceiling {eff}",
        );

        // Control: a reading below the velocity-derived threshold HOLDS — the term is bounded (not an
        // always-swap), and it is the velocity that moves the fire point, not a constant.
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        let at = daemon.clock.now();
        let readings = active_session_reading(&daemon, below);
        let mut events = Vec::new();
        let held = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(held, TickAction::Held),
            "a reading below the velocity threshold holds: {held:?}",
        );
        assert!(events.is_empty(), "no swap event on a hold");

        // The in-window reading fires the reactive arm EARLY (below the effective ceiling), attributed
        // to Session. A fresh daemon so the swap executes from the same warmed baseline.
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        let at = daemon.clock.now();
        let readings = active_session_reading(&daemon, in_window);
        let mut events = Vec::new();
        let fired = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(fired, TickAction::Swapped { from: 0, .. }),
            "the reactive velocity lookahead fires early at {in_window} (< effective ceiling {eff}): {fired:?}",
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Swap {
                    reason: SwapReason::Session,
                    ..
                }
            )),
            "an early reactive fire is attributed to Session, not Weekly: {events:?}",
        );
    }

    #[tokio::test]
    async fn reactive_arm_is_bounded_at_the_effective_ceiling_during_the_cold_ema_blind_window() {
        // Issue #610 problem #1: below MIN_VELOCITY_SAMPLES the reactive velocity term reads 0 (an
        // unwarmed EMA, or one just nulled by a 5 h window reset), so the FIRST interval of a burst is
        // velocity-BLIND. This pins that the blind window is BOUNDED — the reactive arm still fires at
        // the bare effective ceiling, never silently riding the burst past it — and that
        // ≥ MIN_VELOCITY_SAMPLES samples re-engages the early velocity fire.
        let eff = swap::effective_ceiling(0.99); // 0.93
        let poll_gap = swap::reactive_poll_gap_secs(60);
        let rate = 0.0004;
        let warm_threshold = swap::reactive_session_threshold(eff, rate, poll_gap); // ~0.805
        let in_window = warm_threshold + 0.02; // would fire early IF the velocity counted
        assert!(
            in_window < eff,
            "the in-window reading is below the effective ceiling (would be an early velocity fire)",
        );

        // (1) Cold EMA (samples = 1 < MIN_VELOCITY_SAMPLES): velocity gated to 0 → the reactive
        // threshold is the BARE effective ceiling, so the in-window reading HOLDS (the burst is ridden
        // velocity-blind for this interval).
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 1 });
        let at = daemon.clock.now();
        let readings = active_session_reading(&daemon, in_window);
        let mut events = Vec::new();
        let cold_hold = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(cold_hold, TickAction::Held),
            "an unsustained EMA (samples < MIN_VELOCITY_SAMPLES) ignores velocity → in-window holds: {cold_hold:?}",
        );

        // (2) The blind window is BOUNDED at the effective ceiling: a reading AT the effective ceiling
        // fires the bare-ceiling swap even while velocity-blind — the burst is never ridden past it.
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 1 });
        let at = daemon.clock.now();
        let readings = active_session_reading(&daemon, eff);
        let mut events = Vec::new();
        let cold_fire = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(cold_fire, TickAction::Swapped { from: 0, .. }),
            "the blind window is bounded — a reading at the effective ceiling still swaps: {cold_fire:?}",
        );

        // (3) A 5 h window reset nulls the EMA (`None`); that post-reset blind window behaves exactly
        // like the cold one — velocity absent → bare effective ceiling → the in-window reading holds.
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = None;
        let at = daemon.clock.now();
        let readings = active_session_reading(&daemon, in_window);
        let mut events = Vec::new();
        let reset_hold = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(reset_hold, TickAction::Held),
            "a post-window-reset (None) EMA is velocity-blind too → in-window holds: {reset_hold:?}",
        );

        // (4) Once the EMA is SUSTAINED (samples ≥ MIN_VELOCITY_SAMPLES) the early velocity fire
        // re-engages: the SAME in-window reading now swaps early, below the effective ceiling.
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        let at = daemon.clock.now();
        let readings = active_session_reading(&daemon, in_window);
        let mut events = Vec::new();
        let warm_fire = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(warm_fire, TickAction::Swapped { from: 0, .. }),
            "≥ MIN_VELOCITY_SAMPLES re-engages the early velocity fire at {in_window}: {warm_fire:?}",
        );
    }

    // --- #614 stale-reading plausibility guard (end to end through both swap arms) ---

    /// An epoch-second session-window stamp for the #614 tests; `WINDOW_ROLLED` is the one the 5 h
    /// window moves to on a legitimate reset.
    const WINDOW: i64 = 1_800_000_000;
    const WINDOW_ROLLED: i64 = WINDOW + 18_000;

    /// [`active_session_reading`] with a SESSION window stamp on the active account (issue #614) —
    /// the window identity the plausibility guard needs to judge a low reading.
    fn stamped_active_reading(
        daemon: &FakeDaemon,
        session: f64,
        window: Option<i64>,
    ) -> Vec<Option<Usage>> {
        let mut readings = daemon.state.readings();
        readings[0] = Some(Usage {
            session,
            weekly: 0.20,
            weekly_resets_at: None,
            session_resets_at: window,
        });
        readings
    }

    /// Install a session high-water mark of `session` in window [`WINDOW`] on the active account
    /// (slot 0), through the real [`swap::SessionHighWater::fold`] seam rather than by hand.
    fn seed_high_water(daemon: &mut FakeDaemon, session: f64) {
        daemon.state.accounts[0].session_high_water = swap::SessionHighWater::fold(
            None,
            &Usage {
                session,
                weekly: 0.20,
                weekly_resets_at: None,
                session_resets_at: Some(WINDOW),
            },
        );
    }

    #[tokio::test]
    async fn a_stale_low_reading_in_an_unchanged_window_does_not_cancel_a_due_swap() {
        // Issue #614 AC 1, end to end through `decide_action`: both swap arms trusted
        // `active_usage.session` at face value, so a cache-lagged LOW `/oauth/usage` response sat below
        // the fire threshold and CANCELLED an otherwise-due swap while the account was really higher —
        // and velocity-spike detection (the designated multi-machine mitigation) is defeated with it.
        // The account is really at 0.88 in this window; the guard decides on that retained mark rather
        // than the 0.40 the API just echoed back.
        let eff = swap::effective_ceiling(0.99); // 0.93
        let rate = 0.0004;
        let threshold =
            swap::reactive_session_threshold(eff, rate, swap::reactive_poll_gap_secs(60)); // ~0.805
        let real = 0.88; // above the threshold → a swap is DUE
        let stale = 0.40; // the cache-lagged response
        assert!(
            real > threshold && stale < threshold,
            "the fixture straddles the fire point"
        );

        // Control — WITHOUT a high-water mark the stale reading is taken at face value and holds. This
        // is the pre-#614 behaviour, and it pins that the guard (not the fixture) is what changes it.
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        let at = daemon.clock.now();
        let readings = stamped_active_reading(&daemon, stale, Some(WINDOW));
        let mut events = Vec::new();
        let unguarded = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(unguarded, TickAction::Held),
            "with no mark to compare against, the stale reading cancels the due swap: {unguarded:?}",
        );

        // Guarded: the same stale reading, now measured against the window's retained high-water mark,
        // fires the swap that was actually due — attributed to Session (the velocity-derived
        // threshold), not mis-logged as Weekly.
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        seed_high_water(&mut daemon, real);
        let at = daemon.clock.now();
        let readings = stamped_active_reading(&daemon, stale, Some(WINDOW));
        let mut events = Vec::new();
        let guarded = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(guarded, TickAction::Swapped { from: 0, .. }),
            "the stale-low reading no longer cancels the due swap: {guarded:?}",
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Swap {
                    reason: SwapReason::Session,
                    ..
                }
            )),
            "attributed to the session dimension: {events:?}",
        );
        // The carried reading is left VERBATIM — the correction is applied to the DECISION only, never
        // written back over what the API reported (so `status` / telemetry stay honest).
        assert_eq!(
            readings[0].expect("active reading").session,
            stale,
            "the decision-path correction does not mutate the carried reading",
        );
    }

    #[tokio::test]
    async fn a_stale_low_reading_does_not_silence_the_projection_arm_either() {
        // Issue #614 AC 1 for the SECOND arm. The issue names both — `swap::decide` AND `velocity_swap`
        // — and the reactive test above cannot reach this one (it fires reactively before the
        // projection is consulted), so without this the projection-arm correction is untested: reverting
        // it leaves the whole suite green.
        //
        // Wiring: poll_gap 0 (fast-poll off) → the reactive threshold is the bare effective ceiling
        // 0.93, so a real 0.88 HOLDS reactively and `decide_action` consults the projection peer. The
        // stale 0.40 would be stopped dead by the projection's own `session_velocity_min_project_above`
        // free guard (0.85) — a distinct gate from the reactive threshold, so this exercises a
        // genuinely different code path.
        let rate = 0.0004; // 0.88 + 0.0004×150 = 0.94 ≥ the 0.93 effective ceiling → the projection crosses
        let real = 0.88;
        let stale = 0.40;

        // Control: no mark → the stale reading is below the 0.85 project-above guard → the projection
        // holds, and the account rides past the ceiling unswapped.
        let mut daemon = warmed_velocity_daemon(0.86).await;
        daemon.session_velocity_horizon_secs = 150;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        let at = daemon.clock.now();
        let readings = stamped_active_reading(&daemon, stale, Some(WINDOW));
        let mut events = Vec::new();
        let unguarded = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(unguarded, TickAction::Held),
            "unguarded, the stale reading falls under the project-above guard and holds: {unguarded:?}",
        );

        // Guarded: measured against the window's mark the projection sees 0.88, clears the guard, and
        // the preemptive swap fires.
        let mut daemon = warmed_velocity_daemon(0.86).await;
        daemon.session_velocity_horizon_secs = 150;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        seed_high_water(&mut daemon, real);
        let at = daemon.clock.now();
        let readings = stamped_active_reading(&daemon, stale, Some(WINDOW));
        let mut events = Vec::new();
        let guarded = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(
                guarded,
                TickAction::VelocityPreemptivelySwapped { from: 0, .. }
            ),
            "the projection arm projects from the plausible reading and fires: {guarded:?}",
        );
        // The swap-out pct logged for the #539/#595 SLIs is the PLAUSIBLE value (88), not the stale 40
        // the API echoed — the swap fired because the account was believed to be there, and reporting
        // the number we did NOT act on would under-report the overshoot the SLI exists to measure.
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Swap {
                    reason: SwapReason::VelocityPreempt,
                    session_pct: 88,
                    ..
                }
            )),
            "the swap event carries the decided-on pct, not the stale one: {events:?}",
        );
    }

    #[tokio::test]
    async fn a_stale_low_usage_reading_is_not_caught_and_overshoots() {
        // Issue #614: the RESIDUAL trust boundary, stated as a test so it is a known, deliberate gap
        // rather than an assumed-covered one. The guard fires only on POSITIVE evidence that the window
        // is unchanged — a `session_resets_at` the API did not report leaves no way to tell a stale
        // reading from a legitimate reset, so the reading is still trusted at face value and the
        // account keeps climbing UNSEEN past the fire point (the overshoot #614 narrows but does not
        // eliminate). Closing this tail would need a different signal than the reading itself carries.
        let rate = 0.0004;
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        seed_high_water(&mut daemon, 0.88); // a mark exists — but the reading claims no window
        let at = daemon.clock.now();
        let readings = stamped_active_reading(&daemon, 0.40, None);
        let mut events = Vec::new();
        let held = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(held, TickAction::Held),
            "an UNSTAMPED stale-low reading is still trusted — the documented residual gap: {held:?}",
        );
        assert!(events.is_empty(), "no swap event on the uncaught overshoot");
    }

    #[tokio::test]
    async fn the_plausibility_guard_does_not_misfire_on_a_legitimate_window_reset() {
        // Issue #614 AC 2, end to end: the SAME drop that the guard catches inside one window must pass
        // through untouched once `session_resets_at` has moved on — a post-reset account really IS at
        // 0.40, and flooring it at the previous window's mark would swap away a freshly-reset account
        // for no reason (and pin it there for the rest of the new window).
        let rate = 0.0004;
        let mut daemon = armed_reactive_daemon().await;
        daemon.state.accounts[0].session_velocity = Some(VelocityEma { rate, samples: 2 });
        seed_high_water(&mut daemon, 0.88); // the PREVIOUS window's mark
        let at = daemon.clock.now();
        let readings = stamped_active_reading(&daemon, 0.40, Some(WINDOW_ROLLED));
        let mut events = Vec::new();
        let held = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;
        assert!(
            matches!(held, TickAction::Held),
            "a drop across a ROLLED window is expected — the guard must not fire: {held:?}",
        );
        assert!(
            events.is_empty(),
            "no swap event on a legitimate window reset"
        );
    }

    #[tokio::test]
    async fn a_stale_low_reading_leaves_the_retained_velocity_ema_intact() {
        // Issue #614 AC 1, the velocity half — through the REAL poll fold, not the `note_session_velocity`
        // seam. `note_session_velocity` resets the EMA on any `next < prev` (reading a drop as a window
        // reset), so an implausible drop used to null a perfectly good climbing trend and leave BOTH
        // arms velocity-blind (the projection holds outright; the reactive term collapses to 0). The
        // fold now skips such an interval entirely, so the retained EMA survives untouched.
        let mut daemon = window_stamped_climbing_daemon().await;
        let warm = daemon.state.accounts[0]
            .session_velocity
            .expect("a sustained EMA from the real climb");
        assert!(
            warm.samples >= MIN_VELOCITY_SAMPLES,
            "the fixture leaves a SUSTAINED EMA ({} samples)",
            warm.samples,
        );
        assert_eq!(
            daemon.state.accounts[0].session_high_water,
            swap::SessionHighWater::fold(
                None,
                &Usage {
                    session: 0.94,
                    weekly: 0.20,
                    weekly_resets_at: None,
                    session_resets_at: Some(WINDOW)
                }
            ),
            "the fold accrued the window's high-water mark from the real polls",
        );

        // A stale-low response in the SAME window. The reading itself is carried verbatim, but the
        // interval is not folded — the EMA is byte-for-byte what the real climb left.
        poll_active_at(&mut daemon, 0.40, Some(WINDOW)).await;
        assert_eq!(
            daemon.state.accounts[0].session_velocity,
            Some(warm),
            "the implausible interval left the retained EMA untouched",
        );

        // Control: the SAME drop, but with the window rolled, IS a real reset — the EMA nulls exactly
        // as it always did, so the guard has not disabled the reset path it narrows.
        let mut daemon = window_stamped_climbing_daemon().await;
        poll_active_at(&mut daemon, 0.40, Some(WINDOW_ROLLED)).await;
        assert!(
            daemon.state.accounts[0].session_velocity.is_none(),
            "a legitimate window reset still resets the EMA (the climbing trend really is stale)",
        );
    }

    #[tokio::test]
    async fn a_suspend_resume_gap_neither_spurious_swaps_nor_misses_one() {
        // Issue #615. The session-velocity interval is measured on `Clock::now` — a
        // `std::time::Instant` in production — as `now.saturating_duration_since(prev_at)`, and
        // `note_session_velocity` divides the usage delta by it. A laptop suspend/resume is the one
        // ordinary event that can put an arbitrarily large WALL-CLOCK gap between two consecutive
        // readings, so it is the interval this arithmetic is least obviously safe across.
        //
        // `Clock::now`'s own docs carry the suspend/resume requirement this rests on — the clock
        // must keep counting while the system sleeps, because session usage accrues in wall-clock
        // time — and why a sleep-FROZEN clock would fire both velocity-aware swap arms early. Not
        // restated here: the contract belongs on the trait, this test pins the FOLD's half of it.
        //
        // The three legs below pin the behavior that must hold whatever the platform does:
        // dilution (a correctly-measured long gap) must not fabricate a swap, and must not mask a
        // genuinely-due one; a degenerate zero-length interval must reset rather than fabricate.
        let eff = swap::effective_ceiling(0.99); // ceiling 99 → 0.93
        let mut daemon = pre_suspend_climbing_daemon().await;
        let pre_suspend = daemon.state.accounts[0]
            .session_velocity
            .expect("a sustained pre-suspend EMA");

        // --- Leg 1: the resume interval is divided by the WALL-CLOCK gap ---------------------
        // Arm the projective peer, then resume after a 30 min suspend across which usage rose two
        // points. The projection is `observed + rate × horizon`, so the rate this one interval
        // yields decides directly whether the resume tick swaps — which is what makes the clock
        // choice observable in the swap OUTCOME and not merely in the arithmetic:
        //
        //   honest (2 pp over the full 1800 s) → rate 8.89e-5 → 0.89 + 0.027 = 0.917 → HOLDS
        //   sleep-frozen (2 pp over ~60 s)     → rate 2.50e-4 → 0.89 + 0.075 = 0.965 → SWAPS
        //
        // The resume reading sits deliberately so that only the frozen-clock projection crosses the
        // 0.93 effective ceiling: a clock that stopped through the suspend would swap the account
        // out on a climb that never happened. Measured honestly the gap DILUTES the retained climb
        // (2 pp per 30 min across the suspend vs 1 pp per minute while awake), so the arm relaxes.
        //
        // The horizon is armed only NOW, after the climb, and the pre-suspend state is kept clear of
        // the ceiling on purpose (0.87 + 1.67e-4 × 300 = 0.92 < 0.93) so the peer ticks that run
        // before the active is re-polled cannot themselves fire and mask what this leg measures.
        const SUSPEND: Duration = Duration::from_secs(30 * 60);
        const HORIZON: u64 = 300;
        daemon.session_velocity_horizon_secs = HORIZON;
        assert!(
            0.87 + pre_suspend.rate * (HORIZON as f64) < eff,
            "the pre-suspend state must itself stay short of the ceiling, so a peer tick cannot \
             fire before the resume poll lands",
        );
        poll_active_after(&mut daemon, SUSPEND, 0.89, None).await;
        let resumed = daemon.state.accounts[0]
            .session_velocity
            .expect("the resume interval blends, not resets");
        let alpha = daemon.session_velocity_ema_alpha;
        let wall_clock_rate = (0.89 - 0.87) / SUSPEND.as_secs() as f64;
        let expected = alpha * wall_clock_rate + (1.0 - alpha) * pre_suspend.rate;
        assert!(
            (resumed.rate - expected).abs() < 1e-12,
            "the resume interval must be divided by the WALL-CLOCK gap: expected {expected}, \
             got {} (a sleep-frozen clock would blend a much steeper rate here)",
            resumed.rate,
        );
        assert!(
            resumed.rate < pre_suspend.rate,
            "an honestly-measured resume gap DILUTES the retained climb ({} → {})",
            pre_suspend.rate,
            resumed.rate,
        );
        // The reading straddles the two projections — the property that makes the swap assertion
        // below discriminate the clock semantics rather than hold for unrelated reasons.
        let frozen_rate = alpha * ((0.89 - 0.87) / 60.0) + (1.0 - alpha) * pre_suspend.rate;
        assert!(
            0.89 + resumed.rate * (HORIZON as f64) < eff,
            "the honest rate must leave the projection BELOW the {eff} effective ceiling",
        );
        assert!(
            0.89 + frozen_rate * (HORIZON as f64) >= eff,
            "a sleep-frozen clock would project ACROSS the {eff} effective ceiling",
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "no spurious swap: an honestly-measured resume gap leaves the projection short of the \
             effective ceiling",
        );

        // --- Leg 2: the diluted rate cannot MASK a genuinely-due swap -------------------------
        // Velocity only ever SUBTRACTS from the reactive fire point, so however far a resume gap
        // dilutes the EMA the threshold can never rise above the bare effective ceiling — a reading
        // at or over it still swaps. Checked at the production re-observation gap (issue #609),
        // where that term is largest, then driven end to end.
        assert!(
            swap::reactive_session_threshold(eff, resumed.rate, swap::reactive_poll_gap_secs(60))
                <= eff,
            "a diluted velocity can only lower the fire point, never raise it above {eff}",
        );
        poll_active_after(&mut daemon, Duration::from_secs(60), 0.94, None).await;
        let after = daemon.state.active;
        assert!(
            after.is_some() && after != Some(0),
            "no missed swap: a reading at/over the effective ceiling still swaps out (to a peer, \
             not to nothing) despite the resume-diluted velocity — got {after:?}",
        );

        // --- Leg 3: a zero-length resume interval resets rather than fabricates ---------------
        // The mirror hazard: if the platform's monotonic clock did NOT advance across the sleep and
        // the resume poll lands in the same second as the pre-suspend one, the interval is 0. The
        // rate `delta / 0` is undefined, and `note_session_velocity` must NOT invent one — it drops
        // the EMA to `None`, which un-sustains the projective arm (samples < MIN_VELOCITY_SAMPLES)
        // and collapses the reactive velocity term to 0, i.e. back to the bare effective ceiling.
        let mut frozen_clock = pre_suspend_climbing_daemon().await;
        frozen_clock.session_velocity_horizon_secs = HORIZON;
        poll_active_after(&mut frozen_clock, Duration::ZERO, 0.89, None).await;
        assert!(
            frozen_clock.state.accounts[0].session_velocity.is_none(),
            "a zero-length interval resets the EMA instead of fabricating a rate",
        );
        assert_eq!(
            frozen_clock.state.active,
            Some(0),
            "no spurious swap off an unmeasurable interval",
        );
    }

    /// A [`warmed_velocity_daemon`] whose ACTIVE account (u-A, slot 0) has then really climbed
    /// 85 % → 86 % → 87 % over two ordinary 60 s polls, leaving the SUSTAINED pre-suspend velocity
    /// EMA the issue-#615 suspend/resume test measures a resume interval against. Every reading
    /// stays below the 0.93 effective ceiling, so the reactive arm holds throughout and the climb is
    /// the only thing the EMA records. Shared by that test's legs so they cannot drift apart —
    /// leg 3's zero-length interval must land on the SAME pre-suspend state leg 1 diluted.
    async fn pre_suspend_climbing_daemon() -> FakeDaemon {
        let mut daemon = warmed_velocity_daemon(0.85).await;
        poll_active_at(&mut daemon, 0.86, None).await;
        poll_active_at(&mut daemon, 0.87, None).await;
        assert!(
            daemon.state.accounts[0]
                .session_velocity
                .is_some_and(|v| v.samples >= MIN_VELOCITY_SAMPLES),
            "two real climbing intervals leave a SUSTAINED EMA",
        );
        daemon
    }

    /// A warmed three-account daemon whose ACTIVE account (u-A, slot 0) has really climbed
    /// 0.86 → 0.90 → 0.94 inside session window [`WINDOW`], so the poll fold has accrued both a
    /// SUSTAINED velocity EMA and a high-water mark of 0.94 from real polls (issue #614).
    async fn window_stamped_climbing_daemon() -> FakeDaemon {
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok_resets_session("u-A", 0.86, 0.20, WINDOW)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        // Ceiling 99 (effective 0.93) so the climbing readings stay reactively held, exactly as
        // `warmed_velocity_daemon` does; the projection peer is off so no swap interrupts the climb.
        daemon.session_ceiling_strategy = Strategy::fixed(99.0);
        daemon.session_ceiling_base = 0.99;
        daemon.session_velocity_horizon_secs = 0;
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert!(daemon.state.warmed_up);
        assert_eq!(daemon.state.active, Some(0), "u-A active through the climb");
        poll_active_at(&mut daemon, 0.90, Some(WINDOW)).await; // seeds the EMA (samples = 1)
        poll_active_at(&mut daemon, 0.94, Some(WINDOW)).await; // blends it (samples = 2 → SUSTAINED)
        daemon
    }

    /// Advance one poll interval and tick until the staggered schedule has re-polled the ACTIVE
    /// account (slot 0) at `session` in `window`, so the reading lands through the real fold.
    async fn poll_active_at(daemon: &mut FakeDaemon, session: f64, window: Option<i64>) {
        poll_active_after(daemon, Duration::from_secs(60), session, window).await;
    }

    /// [`poll_active_at`] with a caller-chosen clock `gap` before the poll, so a test can drive an
    /// interval that is not the ordinary one-poll cadence — the seam the issue-#615 suspend/resume
    /// tests use to land a sleep-sized (or zero-length) interval through the REAL poll fold.
    async fn poll_active_after(
        daemon: &mut FakeDaemon,
        gap: Duration,
        session: f64,
        window: Option<i64>,
    ) {
        daemon.poller = match window {
            Some(w) => FakeRosterPoller::new().ok_resets_session("u-A", session, 0.20, w),
            None => FakeRosterPoller::new().ok("u-A", session, 0.20),
        }
        .ok("u-B", 0.10, 0.10)
        .ok("u-C", 0.10, 0.10);
        daemon.clock.advance(gap);
        for _ in 0..4 {
            daemon.tick().await;
            if daemon.state.accounts[0]
                .last_reading
                .is_some_and(|u| u.session == session)
            {
                return;
            }
        }
        panic!("the active account was not re-polled within one schedule cycle");
    }

    #[test]
    fn reconcile_roster_preserves_session_high_water_in_lockstep() {
        // Issue #614: the high-water mark is a per-account `AccountRuntime` field (bundled with the
        // reading and the #539 EMA, #668), so a roster reconcile MUST re-key it by uuid — preserved
        // for a persisting account (merely re-indexed), `None` for a newly-onboarded one — or the
        // plausibility guard would judge one account's reading against ANOTHER account's mark, or
        // panic out of bounds. Here u-A is REMOVED (index shift) and u-C is ONBOARDED. (The exhaustive
        // cross-signal sweep is `reconcile_roster_rekeys_every_per_account_signal_by_uuid_not_by_index`;
        // this focused case narrates the high-water stake specifically.)
        let stamped = |session: f64| Usage {
            session,
            weekly: 0.20,
            weekly_resets_at: None,
            session_resets_at: Some(WINDOW),
        };
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.accounts[0].session_high_water =
            swap::SessionHighWater::fold(None, &stamped(0.95));
        daemon.state.accounts[1].session_high_water =
            swap::SessionHighWater::fold(None, &stamped(0.42));

        daemon.reconcile_roster(vec![account("u-B", "spare"), account("u-C", "third")]);

        // The vec stays length- and index-aligned with the new roster.
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-C"]);
        assert_eq!(daemon.state.accounts.len(), 2);
        // u-B's mark is preserved, re-indexed from slot 1 to slot 0 — its window's plausibility
        // baseline is still valid (a reconcile of another account is not a window roll).
        assert_eq!(
            daemon.state.accounts[0].session_high_water,
            swap::SessionHighWater::fold(None, &stamped(0.42)),
            "u-B's mark survives the reconcile, re-indexed",
        );
        // u-C onboards with a fresh (`None`) mark — no stale floor leaks in from the removed u-A.
        assert!(
            daemon.state.accounts[1].session_high_water.is_none(),
            "the onboarded account starts with no high-water mark",
        );
    }

    #[test]
    fn reconcile_roster_preserves_session_velocity_in_lockstep() {
        // Issue #539: the per-account EMA is an `AccountRuntime` field (bundled with the reading,
        // #668), so a roster reconcile MUST re-key it by uuid — preserved for a persisting account
        // (merely re-indexed), `None` for a newly-onboarded one — or a projective read would index
        // the wrong account or panic out of bounds. Here u-A is REMOVED (index shift) and u-C is
        // ONBOARDED.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.003,
            samples: 4,
        });
        daemon.state.accounts[1].session_velocity = Some(VelocityEma {
            rate: 0.007,
            samples: 2,
        });

        daemon.reconcile_roster(vec![account("u-B", "spare"), account("u-C", "third")]);

        // The vec stays length- and index-aligned with the new roster.
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-C"]);
        assert_eq!(daemon.state.accounts.len(), 2);
        // u-B's EMA is preserved, re-indexed from slot 1 to slot 0.
        let preserved = daemon.state.accounts[0]
            .session_velocity
            .expect("u-B's EMA survives the reconcile");
        assert_eq!(preserved.samples, 2);
        assert!((preserved.rate - 0.007).abs() < 1e-9);
        // u-C onboards with a fresh (`None`) EMA — no stale velocity leaks in from the removed u-A.
        assert!(
            daemon.state.accounts[1].session_velocity.is_none(),
            "the onboarded account starts with no velocity",
        );
    }

    #[test]
    fn reconcile_roster_preserves_an_in_flight_blind_anchor_in_lockstep() {
        // Issue #583: the per-account pre-blind anchor is an `AccountRuntime` field (#668), so a
        // reconcile MUST re-key it by uuid for the same reason the #539 EMA above must — an episode
        // edge would otherwise anchor off the wrong account or panic out of bounds. It also carries a
        // liveness stake the
        // EMA does not: an in-flight blind episode must SURVIVE a reconcile triggered by an unrelated
        // account, or a `remove` elsewhere silently truncates it and the episode is censored again by
        // a third route. Here u-A is REMOVED (index shift) and u-C is ONBOARDED while u-B is blind.
        let base = Instant::now();
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.accounts[0].blind_anchor = Some(BlindAnchor {
            session: 0.96,
            weekly: 0.40,
            at: base,
            was_active: true,
            near_limit: true,
        });
        daemon.state.accounts[1].blind_anchor = Some(BlindAnchor {
            session: 0.30,
            weekly: 0.10,
            at: base,
            was_active: false,
            near_limit: false,
        });

        daemon.reconcile_roster(vec![account("u-B", "spare"), account("u-C", "third")]);

        // The vec stays length- and index-aligned with the new roster.
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-C"]);
        assert_eq!(daemon.state.accounts.len(), 2);
        // u-B's in-flight episode survives whole, re-indexed from slot 1 to slot 0 — its eventual
        // recovery still closes against the anchor it went blind on.
        assert_eq!(
            daemon.state.accounts[0].blind_anchor,
            Some(BlindAnchor {
                session: 0.30,
                weekly: 0.10,
                at: base,
                was_active: false,
                near_limit: false,
            }),
            "an unrelated roster change must not truncate an in-flight blind episode",
        );
        // u-C onboards with no anchor — the removed u-A's 96% anchor must not leak into its slot.
        assert!(
            daemon.state.accounts[1].blind_anchor.is_none(),
            "the onboarded account starts inside no episode",
        );
    }

    #[test]
    fn recent_blind_preempt_swap_view_projects_only_while_current_and_recent() {
        // Issue #479 (surface 2): the narration projects a retained #452 preemptive swap ONLY while
        // the swap's target is STILL the active account (still-current) AND within the
        // `BLIND_PREEMPT_NOTICE_SECS` window (recent). A pure function of the record, the current
        // active label, and the monotonic clock.
        let base = Instant::now();
        let record = BlindPreemptSwapRecord {
            from: "spare".to_owned(),
            to: "work".to_owned(),
            last_known_session_pct: 68,
            at: base,
        };

        // Still-current (active is still the target `work`) + recent (mid-window) → narrated, carrying
        // source + stale pct + target; the undo `use spare` is derived by the renderer, not stored.
        let shown = recent_blind_preempt_swap_view(
            Some(&record),
            Some("work"),
            base + Duration::from_secs(BLIND_PREEMPT_NOTICE_SECS - 1),
        )
        .expect("a current, recent preemptive swap is narrated");
        assert_eq!(shown.from_label, "spare");
        assert_eq!(shown.to_label, "work");
        assert_eq!(shown.last_known_session_pct, 68);

        // Superseded — the active account is NO LONGER the swap target (a later swap / manual `use` /
        // external login moved it away): the narration is stale → dropped, even well within the window.
        assert!(
            recent_blind_preempt_swap_view(Some(&record), Some("personal"), base).is_none(),
            "a swap whose target is no longer active self-invalidates",
        );
        // The undo itself — a manual `use spare` moves active back to the swapped-AWAY account, which
        // is neither `to` → also dropped (the hint vanishes once acted on).
        assert!(recent_blind_preempt_swap_view(Some(&record), Some("spare"), base).is_none());
        // No active account resolved → nothing to narrate a swap TO.
        assert!(recent_blind_preempt_swap_view(Some(&record), None, base).is_none());

        // Aged out — at/after the window boundary the notice expires even while its target is active.
        assert!(
            recent_blind_preempt_swap_view(
                Some(&record),
                Some("work"),
                base + Duration::from_secs(BLIND_PREEMPT_NOTICE_SECS),
            )
            .is_none(),
            "at the window boundary the notice has aged out",
        );

        // No record at all → nothing to narrate.
        assert!(recent_blind_preempt_swap_view(None, Some("work"), base).is_none());
    }

    #[test]
    fn recent_landing_overshoot_view_projects_only_while_recent() {
        // Issue #613: the runtime landing-overshoot notice surfaces a retained overshoot ONLY within
        // the `LANDING_OVERSHOOT_NOTICE_SECS` window. Unlike the #479 preempt-swap view there is NO
        // still-current gate — a landing overshoot is a point event about a PAST parked account, not a
        // property of the current active one — so only recency bounds it. A pure function of the record
        // and the monotonic clock.
        let base = Instant::now();
        let record = LandingOvershootRecord {
            from_label: "spare".to_owned(),
            decision_pct: 95,
            landing_pct: 99,
            at: base,
        };

        // Recent (mid-window) → surfaced, carrying the parked handle + the fired-vs-landed spread.
        let shown = recent_landing_overshoot_view(
            Some(&record),
            base + Duration::from_secs(landing::LANDING_OVERSHOOT_NOTICE_SECS - 1),
        )
        .expect("a recent landing overshoot is surfaced");
        assert_eq!(shown.from_label, "spare");
        assert_eq!(shown.decision_pct, 95);
        assert_eq!(shown.landing_pct, 99);

        // Aged out — at/after the window boundary the notice expires.
        assert!(
            recent_landing_overshoot_view(
                Some(&record),
                base + Duration::from_secs(landing::LANDING_OVERSHOOT_NOTICE_SECS),
            )
            .is_none(),
            "at the window boundary the notice has aged out",
        );

        // No record at all → nothing to surface.
        assert!(recent_landing_overshoot_view(None, base).is_none());
    }

    #[tokio::test]
    async fn snapshot_projects_blind_active_degraded_for_the_blind_active_account() {
        // Issue #479: the daemon projects the bounded-blindness state onto the wire snapshot for the
        // ACTIVE account only. Drive u-A (active, index 0) blind past the interim T with its anchor
        // in-band, then assert the snapshot carries a DEGRADED `blind_active` on u-A and `None` on
        // the peers — the same episode the #482 gate-eligibility SLI keys off, now SURFACED.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.70, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        for _ in 0..4 {
            daemon.tick().await;
        }
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));

        let readings = daemon.decision_readings(daemon.state.active);
        let snap = daemon.snapshot(daemon.state.active, &readings, 0);
        let active = snap
            .accounts
            .iter()
            .find(|account| account.active)
            .expect("an active account");
        let blind = active
            .blind_active
            .expect("the blind active account carries a bounded-blindness projection");
        assert!(
            blind.auto_protection_degraded,
            "blind past T with an in-band anchor is DEGRADED: {blind:?}",
        );
        assert_eq!(
            blind.last_known_session_pct, 70,
            "the projection carries the retained anchor's last-known session pct",
        );
        assert!(
            blind.blind_secs > BLIND_GATE_SECS,
            "blind_elapsed exceeds the interim T: {}",
            blind.blind_secs,
        );
        assert!(
            snap.accounts
                .iter()
                .filter(|account| !account.active)
                .all(|account| account.blind_active.is_none()),
            "only the active account carries a bounded-blindness projection",
        );
    }

    #[test]
    fn blind_gate_risk_band_holds_in_the_conservative_interim_band() {
        // Issue #484: the interim `risk_band` is biased CONSERVATIVE to the 0.60 low end and MUST
        // sit in the [0.60, 0.65] band until the documented production ratification bar (≥ 5
        // gated-eligible episodes, zero session walls, majority `swap_necessary`) promotes it. The
        // #451 replay bounds only the ≤ 0.68 CEILING, so the floor is a deliberate conservative choice,
        // not data — this locks it against silently drifting back up toward that ceiling absent
        // production evidence.
        assert!(
            (0.60..=0.65).contains(&BLIND_GATE_RISK_BAND),
            "risk_band {BLIND_GATE_RISK_BAND} left the conservative interim [0.60, 0.65] band (#484)"
        );
    }

    #[tokio::test]
    async fn blind_gate_below_the_interim_risk_band_never_signals() {
        // Issue #482 SLI #1: an anchor comfortably below the interim risk band (50 % < 60 %, #484) never makes
        // the gate eligible, however long the active stays blind — the gate acts only on a near-band
        // anchor, so a low-usage blind active is not a premise data point.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.50, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        for _ in 0..4 {
            daemon.tick().await;
        }
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        // Well past the interim T — still no signal: the anchor is below the risk band.
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS * 3));
        let out = daemon.tick().await;
        assert!(
            !out.events
                .iter()
                .any(|e| matches!(e, Event::BlindGateEligible { .. })),
            "a below-band anchor never makes the gate eligible: {:?}",
            out.events,
        );
    }

    #[tokio::test]
    async fn blind_gate_signals_once_per_episode_and_rearms_after_recovery() {
        // Issue #482 SLI #1: edge-triggered exactly ONCE per blind episode (the gate would swap once,
        // ending it), then re-arms once the active recovers and blinds afresh — so a long blind window
        // is ONE data point and a later episode is a distinct one.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.70, 0.20)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;
        for _ in 0..4 {
            daemon.tick().await;
        }

        // Episode 1: blind, cross T, signal — then stay blind several more ticks: still exactly one.
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let mut episode1 = 0;
        for _ in 0..5 {
            episode1 += daemon
                .tick()
                .await
                .events
                .iter()
                .filter(|e| matches!(e, Event::BlindGateEligible { .. }))
                .count();
            daemon.clock.advance(Duration::from_secs(60));
        }
        assert_eq!(episode1, 1, "exactly one gate signal per blind episode");

        // Recover the active: a live reading closes the episode and re-arms the latch.
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.70, 0.20)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_none() {
            daemon.tick().await;
        }

        // Episode 2: blind afresh, cross T → the gate signals again (a distinct episode).
        daemon.poller = FakeRosterPoller::new()
            .rate_limited("u-A", None)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
        while daemon.state.accounts[0].last_reading.is_some() {
            daemon.tick().await;
        }
        daemon
            .clock
            .advance(Duration::from_secs(BLIND_GATE_SECS + 1));
        let episode2 = daemon.tick().await;
        assert!(
            episode2
                .events
                .iter()
                .any(|e| matches!(e, Event::BlindGateEligible { .. })),
            "a fresh blind episode re-arms the gate signal: {:?}",
            episode2.events,
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

    /// Tick a [`two_account_rate_limit_daemon`] until the NON-active peer `spare` (u-B) is
    /// actually POLLED this tick — throttled or clean — returning that tick's outcome. The #366
    /// schedule interleaves `[active, peer, …]`, so the active `work` ticks (and any skipped peer
    /// ticks still inside a back-off window) are consumed until the peer's turn lands. The caller
    /// MUST `advance` the clock past the peer's PRIOR window before each call after the first,
    /// else the peer is skipped (still backing off) and never re-polls. Bounded so a misuse fails
    /// loudly instead of hanging. The peer-path counterpart of driving the single-account
    /// [`rate_limit_daemon`] every tick — needed because #453's active/peer split means the
    /// peer's UNCHANGED back-off (climb-to-`POLL_BACKOFF_CAP`, #294 clamp) can only be observed on
    /// a genuinely non-active account.
    async fn next_peer_tick(daemon: &mut FakeDaemon) -> TickOutcome {
        for _ in 0..6 {
            let outcome = daemon.tick().await;
            if outcome.diagnostics.iter().any(
                |d| matches!(d, Diagnostic::Poll { account, .. } if account.as_str() == "spare"),
            ) {
                return outcome;
            }
        }
        panic!(
            "the peer `spare` was never polled — advance past its back-off window before calling"
        );
    }

    /// Whether the tick POLLED the account with operator `label` (a `Diagnostic::Poll` for it)
    /// — the observable that a slow-poll / back-off window did NOT suppress its poll this tick.
    fn peer_polled(outcome: &TickOutcome, label: &str) -> bool {
        outcome
            .diagnostics
            .iter()
            .any(|d| matches!(d, Diagnostic::Poll { account, .. } if account == label))
    }

    // --- Out-of-rotation slow-poll cadence (issue #537) ---

    #[test]
    fn exhausted_poll_window_caps_at_the_hourly_ceiling_when_the_reset_is_far() {
        // A weekly-exhausted reading whose weekly window resets 2 h out (> the 1 h ceiling):
        // the window is the ceiling — the hourly cap bounds worst-case blindness for the rare
        // early reset. (Base triggers: weekly 0.98, session 0.95.)
        let now = 1_000_000;
        let reading = Usage {
            session: 0.10,
            weekly: 0.99,
            weekly_resets_at: Some(now + 7_200),
            session_resets_at: None,
        };
        assert_eq!(
            exhausted_poll_window(&reading, 0.98, 0.95, now, 3600, 60),
            Duration::from_secs(3600),
        );
    }

    #[test]
    fn exhausted_poll_window_pulls_earlier_to_a_known_soon_reset() {
        // The known reset lands in 30 min (< the 1 h ceiling) → poll again AT the reset, earlier
        // than the ceiling: a window that elapses sooner than an hour is caught promptly.
        let now = 1_000_000;
        let reading = Usage {
            session: 0.10,
            weekly: 0.99,
            weekly_resets_at: Some(now + 1_800),
            session_resets_at: None,
        };
        assert_eq!(
            exhausted_poll_window(&reading, 0.98, 0.95, now, 3600, 60),
            Duration::from_secs(1_800),
        );
    }

    #[test]
    fn exhausted_poll_window_floors_at_poll_secs_when_the_reset_is_imminent_or_past() {
        let now = 1_000_000;
        // Reset 10 s away (< the 60 s floor) → floored to poll_secs, not a sub-cadence re-poll.
        let imminent = Usage {
            session: 0.10,
            weekly: 0.99,
            weekly_resets_at: Some(now + 10),
            session_resets_at: None,
        };
        assert_eq!(
            exhausted_poll_window(&imminent, 0.98, 0.95, now, 3600, 60),
            Duration::from_secs(60),
        );
        // Reset already PAST but the account still reads exhausted (rare server lateness) →
        // floored to poll_secs, so a persistently-late reset never busy-polls every tick.
        let past = Usage {
            session: 0.10,
            weekly: 0.99,
            weekly_resets_at: Some(now - 500),
            session_resets_at: None,
        };
        assert_eq!(
            exhausted_poll_window(&past, 0.98, 0.95, now, 3600, 60),
            Duration::from_secs(60),
        );
    }

    #[test]
    fn exhausted_poll_window_falls_back_to_the_ceiling_when_the_reset_is_unknown() {
        // No parseable reset for the exhausted dimension → the plain hourly ceiling (#537).
        let now = 1_000_000;
        let reading = Usage {
            session: 0.10,
            weekly: 0.99,
            weekly_resets_at: None,
            session_resets_at: None,
        };
        assert_eq!(
            exhausted_poll_window(&reading, 0.98, 0.95, now, 3600, 60),
            Duration::from_secs(3600),
        );
    }

    #[test]
    fn exhausted_poll_window_keys_the_session_reset_when_only_session_is_exhausted() {
        // session-exhausted (0.96 >= 0.95), weekly VIABLE (0.10 < 0.98): only session_resets_at
        // is applicable — a weekly reset would NOT be why this peer is out of rotation, so a
        // (here absent, but even if present) weekly reset does not key the window.
        let now = 1_000_000;
        let reading = Usage {
            session: 0.96,
            weekly: 0.10,
            weekly_resets_at: Some(now + 100), // must be IGNORED — weekly is not exhausted
            session_resets_at: Some(now + 1_200),
        };
        assert_eq!(
            exhausted_poll_window(&reading, 0.98, 0.95, now, 3600, 60),
            Duration::from_secs(1_200),
        );
    }

    #[test]
    fn exhausted_poll_window_uses_the_sooner_reset_when_both_dimensions_are_exhausted() {
        // Both exhausted; the weekly window resets sooner (15 min) than the session (40 min) →
        // the SOONER applicable reset governs, catching relief at the earliest opportunity.
        let now = 1_000_000;
        let reading = Usage {
            session: 0.96,
            weekly: 0.99,
            weekly_resets_at: Some(now + 900),
            session_resets_at: Some(now + 2_400),
        };
        assert_eq!(
            exhausted_poll_window(&reading, 0.98, 0.95, now, 3600, 60),
            Duration::from_secs(900),
        );
    }

    #[tokio::test]
    async fn a_weekly_exhausted_peer_is_slow_polled_until_its_window_elapses() {
        // AC: a NON-active peer polled weekly-exhausted is SKIPPED on subsequent ticks until the
        // widened window elapses, rather than re-polled every poll_secs. The reset is far out, so
        // the window is the hourly ceiling (3600).
        let now = wall_clock_now_secs();
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10) // active: viable → holds
                .ok_resets("u-B", 0.10, 0.99, now + 999_999), // peer: weekly-exhausted, reset far
        )
        .await;
        // The peer's first poll reads it exhausted → arms the window + emits the ENTER edge.
        let armed = next_peer_tick(&mut daemon).await;
        assert!(
            armed.events.iter().any(|e| matches!(
                e, Event::ExhaustedSlowPoll { account, window_secs }
                    if account == "u-B" && *window_secs == 3600
            )),
            "the first exhausted poll arms the hourly window + emits ENTER: {:?}",
            armed.events,
        );
        // Now the peer is SKIPPED on its subsequent turns (no clock advance): a full two cycles'
        // worth of ticks come and go without a `spare` poll.
        for _ in 0..4 {
            let tick = daemon.tick().await;
            assert!(
                !peer_polled(&tick, "spare"),
                "an exhausted peer must be skipped inside its slow-poll window",
            );
        }
        // Advancing past the window re-polls it on its next turn (next_peer_tick would panic if
        // it stayed skipped).
        daemon.clock.advance(Duration::from_secs(3600));
        let repoll = next_peer_tick(&mut daemon).await;
        assert!(
            peer_polled(&repoll, "spare"),
            "the window elapsed → re-polled"
        );
        // The re-poll still reads u-B weekly-exhausted, so it RE-ARMS the window — but the
        // edge-triggered ENTER is NOT re-emitted: the account never left the widened cadence (the
        // window field stayed `Some` across the elapse, so `was_slow_polling` holds). Only a
        // clear→re-arm re-emits ENTER. Binds AC: the enter/exit events are edge-triggered (once
        // per transition), mirroring `note_account_backoff`'s was-backing-off idiom.
        assert!(
            !repoll
                .events
                .iter()
                .any(|e| matches!(e, Event::ExhaustedSlowPoll { .. })),
            "a re-arm while still exhausted must NOT re-emit the ENTER edge: {:?}",
            repoll.events,
        );
    }

    #[tokio::test]
    async fn a_session_exhausted_peer_is_slow_polled_keyed_off_the_session_reset() {
        // AC: a session-exhausted (session >= session_ceiling) non-active peer is slow-polled the
        // same way, keyed off its SESSION reset. Here the session resets ~10 min out, so the
        // window is pulled EARLIER than the hourly ceiling.
        let now = wall_clock_now_secs();
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok_resets_session("u-B", 0.96, 0.10, now + 600), // session-exhausted, resets soon
        )
        .await;
        let armed = next_peer_tick(&mut daemon).await;
        assert!(
            armed.events.iter().any(|e| matches!(
                e, Event::ExhaustedSlowPoll { account, window_secs }
                    // pulled earlier than the 3600 ceiling, ≈ the 600 s reset delta (± wall drift)
                    if account == "u-B" && (595..=600).contains(window_secs)
            )),
            "session-exhausted peer arms a reset-aware sub-ceiling window: {:?}",
            armed.events,
        );
        // Skipped within the window.
        for _ in 0..4 {
            assert!(!peer_polled(&daemon.tick().await, "spare"));
        }
    }

    #[tokio::test]
    async fn an_exhausted_peer_with_no_known_reset_falls_back_to_the_hourly_ceiling() {
        // AC: an exhausted peer whose reset is unknown/unparseable falls back to the hourly
        // default (exhausted_poll_secs) — the window is EXACTLY 3600, independent of the wall
        // clock (the deterministic fallback, distinct from the reset-aware sub-ceiling above).
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok("u-B", 0.10, 0.99), // weekly-exhausted, NO resets_at
        )
        .await;
        let armed = next_peer_tick(&mut daemon).await;
        assert!(
            armed.events.iter().any(|e| matches!(
                e, Event::ExhaustedSlowPoll { account, window_secs }
                    if account == "u-B" && *window_secs == 3600
            )),
            "unknown reset → the plain hourly fallback window: {:?}",
            armed.events,
        );
    }

    #[tokio::test]
    async fn a_slow_polled_peer_that_reads_viable_again_clears_and_returns_to_full_cadence() {
        // AC: a slow-polled peer that next polls viable clears its window (emitting the EXIT
        // edge) and returns to the full cadence.
        let now = wall_clock_now_secs();
        let (_dir, mut daemon) =
            two_account_rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.10, 0.10).ok_resets(
                "u-B",
                0.10,
                0.99,
                now + 999_999,
            ))
            .await;
        // Arm the peer's slow-poll window.
        next_peer_tick(&mut daemon).await;
        // The peer's window resets (it is now viable). Advance past the window so it re-polls.
        daemon.poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        daemon.clock.advance(Duration::from_secs(3600));
        let cleared = next_peer_tick(&mut daemon).await;
        assert!(
            cleared.events.iter().any(|e| matches!(
                e, Event::ExhaustedSlowPollCleared { account } if account == "u-B"
            )),
            "the viable re-poll emits the EXIT edge, bracketing the episode: {:?}",
            cleared.events,
        );
        // Back to full cadence: the peer polls again on its very next turn, no advance needed.
        let full = next_peer_tick(&mut daemon).await;
        assert!(
            peer_polled(&full, "spare"),
            "a cleared peer is back on the normal cadence",
        );
    }

    #[tokio::test]
    async fn the_active_account_is_never_slow_polled_even_when_exhausted() {
        // AC: the ACTIVE account is NEVER slow-polled, even weekly-exhausted — its swap-away
        // trigger must stay observable at full cadence (the #453 active-vs-peer asymmetry).
        // Single-account daemon: the active `work` reads weekly-exhausted with nowhere to swap,
        // so it stays active and must keep polling every tick.
        let (_dir, mut daemon) =
            rate_limit_daemon(FakeRosterPoller::new().ok("u-A", 0.10, 0.99)).await;
        for _ in 0..5 {
            let tick = daemon.tick().await;
            assert!(
                tick_polled(&tick),
                "the active account must be polled every tick, even exhausted",
            );
            assert!(
                !tick
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::ExhaustedSlowPoll { .. })),
                "the active account is exempt — it never arms a slow-poll window: {:?}",
                tick.events,
            );
        }
    }

    #[tokio::test]
    async fn a_peer_promoted_to_active_while_slow_polled_is_polled_at_full_cadence() {
        // AC (issue #537): a peer armed for slow-polling while NON-active can then be promoted to
        // active via `use`. Nothing on the promotion path clears its `exhausted_poll_until` (only a
        // poll does), and the arm-site "never arm the active account" guarantee does not apply to a
        // window armed legitimately while it WAS a peer — so `exhausted_slow_polling`'s
        // `active != Some(i)` consult guard is what keeps the now-active account polling at full
        // cadence despite the stale window (its swap-away trigger must stay observable, the #453
        // asymmetry). Without the guard it would be SKIPPED until the window elapsed — active AND
        // never re-polled. The sibling `the_active_account_is_never_slow_polled_even_when_exhausted`
        // binds the ARM site; this binds the CONSULT site.
        //
        // Both accounts read weekly-exhausted so the promoted (still-exhausted) active has no viable
        // target and simply HOLDS — isolating the exemption from a swap-away that would otherwise
        // repoint active on the same tick (the all-exhausted-holds setup of the relief test).
        let now = wall_clock_now_secs();
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.99) // active: weekly-exhausted → holds (no viable target)
                .ok_resets("u-B", 0.10, 0.99, now + 999_999), // peer: weekly-exhausted, reset far
        )
        .await;
        // The peer's first poll (still NON-active) reads it exhausted → arms its slow-poll window.
        next_peer_tick(&mut daemon).await;
        assert!(
            daemon.state.accounts[1]
                .health
                .exhausted_poll_until
                .is_some(),
            "the peer's slow-poll window is armed before promotion",
        );
        // Promote u-B to active — what `use u-B` effects (repoint active) — WITHOUT advancing the
        // clock, so the window is still armed when the consult site runs on u-B's next slot.
        daemon.state.active = Some(1);
        // The now-active u-B polls at full cadence on its next slot: the consult-site exemption, not
        // the still-armed window, decides. (next_peer_tick panics if it stays skipped.)
        let promoted = next_peer_tick(&mut daemon).await;
        assert!(
            peer_polled(&promoted, "spare"),
            "a peer promoted to active must poll at full cadence despite a stale armed window",
        );
        // That active poll cleared the stale window (active is treated viable) — no dangling
        // deadline once it is back in rotation as the consumed account.
        assert!(
            daemon.state.accounts[1]
                .health
                .exhausted_poll_until
                .is_none(),
            "the active poll cleared the stale slow-poll window",
        );
    }

    #[tokio::test]
    async fn all_exhausted_relief_still_computes_from_a_slow_polled_peers_retained_reset() {
        // AC: all-exhausted relief still computes correctly under slow-polling — it reads the
        // peer's RETAINED reading (with its resets_at), which the slow-poll skip carries
        // untouched (the same carry-readings mechanism as the back-off skip).
        let now = wall_clock_now_secs();
        let (_dir, mut daemon) = two_account_rate_limit_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.99) // active: weekly-exhausted (stays active — no viable target)
                .ok_resets("u-B", 0.10, 0.99, now + 999_999), // peer: weekly-exhausted, known reset
        )
        .await;
        // Warm up: poll both (A then B). B reads exhausted → armed for slow-poll.
        next_peer_tick(&mut daemon).await;
        // Tick to a B-SKIP tick and assert the all-exhausted decision still stands.
        let mut saw_skipped_all_exhausted = false;
        for _ in 0..4 {
            let tick = daemon.tick().await;
            if !peer_polled(&tick, "spare") {
                assert_eq!(
                    tick.action,
                    TickAction::NoViableTarget,
                    "all-exhausted relief must still compute while the peer is slow-polled",
                );
                saw_skipped_all_exhausted = true;
            }
        }
        assert!(
            saw_skipped_all_exhausted,
            "the peer was skipped at least once"
        );
        // The peer's reading — with its weekly reset — is RETAINED across the skips, so the
        // relief math (all_exhausted_relief) still has the reset to key off.
        let retained = daemon.state.accounts[1]
            .last_reading
            .expect("peer reading retained across skips");
        assert_eq!(retained.weekly_resets_at, Some(now + 999_999));
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
                .accounts
                .iter()
                .map(|a| a.last_reading.as_ref().map(|r| r.session))
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
        daemon.state.accounts[2].health.quarantined = true;
        assert_eq!(daemon.build_poll_schedule(Some(0)), vec![0, 1]);

        // Quarantine the last remaining peer too: an active with NO peers still polls
        // itself (its swap-away trigger / dead-active re-probe), never an empty schedule.
        daemon.state.accounts[1].health.quarantined = true;
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
    async fn near_limit_fast_poll_engages_only_on_an_in_band_active_reading_or_projection() {
        // Issue #540: the engage predicate for the near-limit poll-coverage fast-poll. It fires
        // ONLY when the ACTIVE account's OBSERVED reading is in the #539 band (≥ the shared
        // `session_velocity_min_project_above` floor, 0.85) OR the #539 velocity projection
        // `last + rate × H` reaches that floor from a still-below reading — reusing the very signals
        // #539 projects from, so #540 keeps them warm through the final climb (the poll-gap
        // `velocity_swap` explicitly holds on). Every other arm holds it OFF, so the steady-state
        // (below-band) cadence is never tightened (AC 3).
        let mut daemon = warmed_velocity_daemon(0.90).await;
        daemon.near_limit_poll_secs = 60; // enable the path (the `tunables` fixture ships it at 0)
        daemon.session_velocity_horizon_secs = 150; // arm the #539 projection horizon
        let usage = |session: f64| Usage {
            session,
            weekly: 0.20,
            weekly_resets_at: None,
            session_resets_at: None,
        };

        // In-band by the OBSERVED reading (0.90 ≥ 0.85): engaged, no projection needed.
        assert!(
            daemon.near_limit_fast_poll_engaged(),
            "an in-band observed reading engages the fast-poll",
        );

        // Below the band with NO velocity signal (the poll-gap case #540 owns): NOT engaged.
        daemon.state.accounts[0].last_reading = Some(usage(0.80));
        daemon.state.accounts[0].session_velocity = None;
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "below the band with no velocity → not engaged",
        );

        // Below the band, but a SUSTAINED projection reaches the floor (0.80 + 0.01 × 150 = 2.3 ≥
        // 0.85): the "approaching the band" arm engages BEFORE the fast burst is observed in-band.
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.01,
            samples: 3,
        });
        assert!(
            daemon.near_limit_fast_poll_engaged(),
            "a sustained projection into the band engages the fast-poll",
        );

        // A SHORT projection (0.80 + 0.0001 × 150 = 0.815 < 0.85) stays below the floor → NOT
        // engaged; the reactive path catches it if it keeps climbing.
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.0001,
            samples: 3,
        });
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "a projection that falls short of the floor → not engaged",
        );

        // A single-interval spike (samples < MIN_VELOCITY_SAMPLES) is not SUSTAINED → NOT engaged,
        // exactly as #539's projection guards — never fire on a one-off spike.
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.01,
            samples: 1,
        });
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "a single-sample spike is not sustained → not engaged",
        );

        // The #539 horizon kill-switch (0) collapses the projection to the observed (below-band)
        // reading → NOT engaged, even with a steep, well-sustained velocity.
        daemon.state.accounts[0].session_velocity = Some(VelocityEma {
            rate: 0.01,
            samples: 5,
        });
        daemon.session_velocity_horizon_secs = 0;
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "horizon 0 disables the projection arm → not engaged below-band",
        );
        daemon.session_velocity_horizon_secs = 150; // restore for the arms below

        // A BLIND active (its slot cleared by a 429/5xx) carries no OBSERVED near-limit signal —
        // that is the #452 bounded-blindness path, not #540's — so a None reading holds it OFF.
        daemon.state.accounts[0].last_reading = None;
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "a blind active (None reading) → not engaged (#452's domain, not #540's)",
        );

        // Re-arm an in-band reading, then verify the three GATE guards each independently hold off.
        daemon.state.accounts[0].last_reading = Some(usage(0.90));
        assert!(daemon.near_limit_fast_poll_engaged(), "re-armed in-band");

        // Kill-switch: near_limit_poll_secs == 0 disables the whole path.
        daemon.near_limit_poll_secs = 0;
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "the near_limit_poll_secs kill-switch (0) disables the path",
        );
        daemon.near_limit_poll_secs = 60;

        // Not warmed up: tightening the shared tick before the first full cycle could starve peers
        // of their first poll and stall the warm-up latch (#80).
        daemon.state.warmed_up = false;
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "an un-warmed roster never engages (peers must complete their first poll)",
        );
        daemon.state.warmed_up = true;

        // No active resolved: there is nothing to keep warm.
        daemon.state.active = None;
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "no active account → not engaged",
        );
    }

    #[tokio::test]
    async fn the_near_limit_fast_poll_engages_on_a_plausibility_corrected_reading() {
        // Issue #614: `active_near_limit` shares ONE band with #539's `velocity_swap` (its doc
        // promise), so it must see the SAME plausibility correction — otherwise a stale-low response
        // would DISENGAGE the near-limit fast-poll exactly when the account is really near the ceiling,
        // widening the re-observation gap the reactive arm looks ahead over and compounding the
        // overshoot the guard exists to narrow. The account is really at 0.90 (this window's mark),
        // above the 0.85 band floor, but the response echoed a cache-lagged 0.40.
        let mut daemon = warmed_velocity_daemon(0.90).await;
        daemon.near_limit_poll_secs = 60; // enable the path
        daemon.session_velocity_horizon_secs = 0; // projection OFF → isolate the OBSERVED-band arm
        daemon.state.accounts[0].session_velocity = None; // no velocity → only the reading can carry the band
        let stale = Usage {
            session: 0.40,
            weekly: 0.20,
            weekly_resets_at: None,
            session_resets_at: Some(WINDOW),
        };
        daemon.state.accounts[0].last_reading = Some(stale);

        // Control: with no high-water mark the stale 0.40 is below the band → NOT engaged (pre-#614).
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "unguarded, the stale-low reading disengages the fast-poll near the ceiling",
        );

        // Guarded: the window's retained mark of 0.90 makes the plausible reading in-band → engaged.
        seed_high_water(&mut daemon, 0.90);
        assert!(
            daemon.near_limit_fast_poll_engaged(),
            "the plausibility-corrected reading keeps the near-limit fast-poll engaged",
        );

        // And the correction is scoped to the window: once it rolls, the same 0.40 really IS below the
        // band, so the fast-poll correctly disengages (the mark does not pin the fast-poll on).
        daemon.state.accounts[0].last_reading = Some(Usage {
            session_resets_at: Some(WINDOW_ROLLED),
            ..stale
        });
        assert!(
            !daemon.near_limit_fast_poll_engaged(),
            "across a rolled window the low reading is real → not engaged",
        );
    }

    #[tokio::test]
    async fn near_limit_fast_poll_caps_the_active_sub_interval_in_band_only() {
        // Issue #540 (AC 2 + AC 3): the near-limit poll-coverage TIMING seam. While the active's
        // reading is in the band, the per-tick sub-interval is capped to `near_limit_poll_secs`
        // (60 s) — so the active, re-observed every ~2 sub-intervals by the #366 interleave, never
        // opens a near-limit poll gap. BELOW the band the cap is inert and the sub-interval is the
        // un-capped base, so the steady-state poll footprint the source-scoped 429 budget depends on
        // stays flat.
        //
        // A `fixed(600 s)` interval over the 3-account rotation gives a base sub-interval of
        // 600 / 3 = 200 s — deliberately ABOVE the 60 s cap so the cap actually BINDS (the small-N /
        // high-poll_secs regime #540 targets, the ≥ 400–900 s poll-gap residual; where the base is
        // already tighter than the cap the `min` leaves it untouched). The FakeClock is frozen, so
        // the seam is exercised via a direct `next_subinterval()` call — no real clock, no network.

        // --- BELOW the band: the cap is inert, the base cadence is unchanged, no edge event ---
        let mut below = warmed_velocity_daemon(0.80).await; // active reading below the 0.85 floor
        below.poll_strategy = Strategy::fixed(600.0);
        below.near_limit_poll_secs = 60;
        let out = below.tick().await;
        assert!(
            !below.state.near_limit_fast_poll,
            "0.80 < 0.85 floor → the fast-poll stays disengaged",
        );
        assert!(
            !out.events
                .iter()
                .any(|e| matches!(e, Event::NearLimitPollCoverage { .. })),
            "no band-entry event below the band: {:?}",
            out.events,
        );
        assert_eq!(
            below.next_subinterval(),
            Duration::from_secs(200),
            "below the band the sub-interval is the un-capped base (600 / 3) — footprint flat",
        );

        // --- IN the band: the cap binds, the active re-polls within it, the edge event emits once
        let mut inband = warmed_velocity_daemon(0.90).await; // active reading at/over the floor
        inband.poll_strategy = Strategy::fixed(600.0);
        inband.near_limit_poll_secs = 60;
        let out = inband.tick().await;
        assert!(
            inband.state.near_limit_fast_poll,
            "0.90 ≥ 0.85 floor → the fast-poll engages",
        );
        assert!(
            out.events.iter().any(|e| matches!(
                e,
                Event::NearLimitPollCoverage {
                    sub_interval_secs: 60,
                    ..
                }
            )),
            "band entry emits the durable near_limit_poll_coverage event carrying the cap: {:?}",
            out.events,
        );
        assert_eq!(
            inband.next_subinterval(),
            Duration::from_secs(60),
            "in the band the sub-interval is capped to near_limit_poll_secs — no poll gap ≥ the cap",
        );
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
        // Floor-driven exhaustion: B is weekly-viable but over the target-max-session-usage, so
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
        // All three weekly-exhausted (weekly 0.99 ≥ weekly_ceiling 0.98). B resets
        // soonest, so it is the least-bad hold target even though A is active.
        const A_RESET: i64 = 1_782_777_600; // 2026-06-30T00:00:00Z
        const B_RESET: i64 = 1_782_496_800; // 2026-06-26T18:00:00Z (soonest)
        const C_RESET: i64 = 1_782_864_000; // 2026-07-01T00:00:00Z
        let poller = FakeRosterPoller::new()
            .ok_resets("u-A", 0.50, 0.99, A_RESET)
            .ok_resets("u-B", 0.50, 0.99, B_RESET)
            .ok_resets("u-C", 0.50, 0.99, C_RESET);
        // Floor inert (== trigger via tunables_floor_off); weekly_ceiling 98, so the
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
        // That last warm-up tick also polls a weekly-exhausted PEER, which arms its
        // out-of-rotation slow-poll window and emits an ExhaustedSlowPoll (issue #537) — a
        // concern orthogonal to this all-exhausted HOLD test, and whose window value is
        // wall-clock-dependent (the fixture resets are fixed historical dates). Filter the
        // slow-poll events out (they have their own tests) and pin the DECISION event exactly.
        let decision_events: Vec<_> = first
            .events
            .iter()
            .filter(|e| {
                !matches!(
                    e,
                    Event::ExhaustedSlowPoll { .. } | Event::ExhaustedSlowPollCleared { .. }
                )
            })
            .cloned()
            .collect();
        assert_eq!(
            decision_events,
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
                canonical_unknown_diag("work"),
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
            // Over the reactive fire point but BELOW the raw ceiling (0.95): an ORDINARY
            // swap, so the cooldown is honored — the #611 spike bypass fires only AT/above the
            // raw ceiling (test
            // `reactive_spike_at_raw_ceiling_bypasses_cooldown_but_a_normal_swap_still_honors_it`).
            .ok("u-A", 0.92, 0.40)
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
        // Same below-raw-ceiling reading (0.92) as the within-cooldown sibling above, so the pair
        // differs ONLY in elapsed time: here the cooldown has passed, so the ordinary swap fires.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.92, 0.40)
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
    async fn reactive_spike_at_raw_ceiling_bypasses_cooldown_but_a_normal_swap_still_honors_it() {
        // Issue #611: the emergency-tier reactive bypass. A LIVE (non-quarantined) active whose
        // OBSERVED session reading has reached the RAW ceiling (the not-cross line — `session_ceiling`
        // = 0.95 here, NOT the lower `effective_ceiling` / `session_threshold` the normal fire point
        // is derived backward from) is the co-consumption spike velocity-detection is meant to catch,
        // and it MUST escape even inside the swap cooldown. Before the fix the SAME setup returned
        // `SkippedCooldown` and burned on THROUGH the breach for up to the 1 h cooldown max — the gap
        // this closes. The bypass is STRICTLY above the normal fire point, so an ORDINARY
        // over-fire-point swap BELOW the raw ceiling still defers within the cooldown: cooldown's
        // rate-limiting purpose is untouched for ordinary swaps.
        //
        // ONE fixture, two readings inside the SAME freshly-armed 100 s cooldown (a swap 10 s ago),
        // so the ONLY difference is whether the reading crosses the raw ceiling: 0.97 (≥ ceiling →
        // bypass → swap) vs 0.92 (over the 0.89 fire point but below the ceiling → cooldown honored).
        async fn action_for(active_session: f64) -> (TickAction, bool) {
            let roster = vec![account("u-A", "work"), account("u-B", "spare")];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
            ])
            .await;
            let (_dir, json) = claude_json("u-A");
            // B is wide open — a genuinely viable target, so the ONLY thing that can hold the swap
            // is the cooldown (which the bypass must defeat at the raw ceiling).
            let poller = FakeRosterPoller::new()
                .ok("u-A", active_session, 0.40)
                .ok("u-B", 0.05, 0.05);
            let tun = tunables(95, 80, 100); // raw ceiling 0.95, cooldown 100 s
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
            // A swap 10 s ago: the 100 s cooldown is firmly armed and un-elapsed.
            daemon.state.last_swap = Some(LastSwap {
                at: daemon.clock.now(),
            });
            daemon.clock.advance(Duration::from_secs(10));
            let action = warmed_tick(&mut daemon).await.action;
            let canonical_a = daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token"));
            (action, canonical_a)
        }

        // At the raw ceiling: the bypass fires — a swap despite the un-elapsed cooldown, and the
        // canonical is rerouted off A onto the wide-open B.
        let (spike_action, spike_canonical_a) = action_for(0.97).await;
        assert_eq!(
            spike_action,
            TickAction::Swapped { from: 0, to: 1 },
            "a live active at the raw ceiling must swap despite the active cooldown (#611 bypass)",
        );
        assert!(
            !spike_canonical_a,
            "the bypass swap rerouted the canonical off A"
        );

        // The exact-equality boundary: AC1 is "observed ≥ raw ceiling", so a reading landing EXACTLY
        // on the raw ceiling (0.95) must also bypass — pinning the `≥` (not `>`) semantics of the
        // strict-`<` guard.
        let (boundary_action, boundary_canonical_a) = action_for(0.95).await;
        assert_eq!(
            boundary_action,
            TickAction::Swapped { from: 0, to: 1 },
            "a reading exactly AT the raw ceiling must also bypass the cooldown (AC1 is ≥, not >)",
        );
        assert!(
            !boundary_canonical_a,
            "the boundary bypass swap rerouted the canonical off A"
        );

        // Below the raw ceiling: an ordinary over-fire-point swap still honors the cooldown, leaving
        // the canonical on A.
        let (normal_action, normal_canonical_a) = action_for(0.92).await;
        assert_eq!(
            normal_action,
            TickAction::SkippedCooldown,
            "a normal (below-raw-ceiling) swap must still defer within the cooldown",
        );
        assert!(
            normal_canonical_a,
            "the honored cooldown left the canonical on A"
        );
    }

    #[tokio::test]
    async fn reactive_spike_bypass_still_honors_the_session_gate_so_it_holds_without_a_target() {
        // Issue #611: the bypass skips only the cooldown WAIT — NOT the reserve or the always-on
        // session gate (unlike the dead-active `emergency_swap`, which bypasses everything because
        // liveness beats headroom). So a raw-ceiling spike with NO viable target still HOLDS
        // (`NoViableTarget`); it never thrashes onto a session-saturated peer. This is what keeps the
        // bypass "strictly above the normal fire point" from becoming a liveness-beats-all escape.
        //
        // The discriminator vs. the pre-fix behavior: within the cooldown the OLD code returned
        // `SkippedCooldown` (never reaching `pick_target`); the bypass now reaches `pick_target`,
        // where the always-on session gate excludes the saturated peer → `NoViableTarget`. Asserting
        // `NoViableTarget` (not `SkippedCooldown`) proves BOTH that the bypass engaged AND that the
        // gate still held.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A spikes to the raw ceiling; B is ALSO session-saturated (over the 95 trigger), so the
        // always-on session gate in `pick_target` excludes it — there is nowhere viable to land.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.20)
            .ok("u-B", 0.96, 0.20);
        // Floor OFF so the ONLY exclusion is the always-on session gate (not the reserve).
        let tun = tunables_floor_off(95, 100); // cooldown 100 s
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
        // A swap 10 s ago: firmly within the 100 s cooldown, so the OLD path would have skipped here.
        daemon.state.last_swap = Some(LastSwap {
            at: daemon.clock.now(),
        });
        daemon.clock.advance(Duration::from_secs(10));

        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(
            outcome.action,
            TickAction::NoViableTarget,
            "a raw-ceiling spike bypasses the cooldown but still HOLDS on a saturated peer: {:?}",
            outcome.action,
        );
        // The canonical is untouched — no thrash onto the saturated B.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
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
            // Below the raw ceiling (0.95): an ORDINARY over-fire-point swap, so the floor
            // still binds it (the #611 spike bypass fires only AT/above the raw ceiling).
            .ok("u-A", 0.92, 0.40)
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
    async fn a_jittered_session_ceiling_is_deterministic_and_varies_the_swap_decision() {
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
            tun.session_ceiling_strategy = Strategy {
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
    async fn a_jittered_weekly_ceiling_is_deterministic_and_varies_the_swap_decision() {
        // The WEEKLY-axis mirror of the jittered-trigger test (issue #41): session
        // is held LOW (never trips its trigger), weekly sits at a fixed 60%, and a
        // wide uniform weekly-ceiling jitter spans downward from the 95 base to the
        // 50 clamp — so some cycles draw a weekly ceiling whose fire point is ≤ 60
        // (→ swap on the weekly dimension) and others above it (→ hold).
        // Deterministic per seed, varying across seeds: proof the weekly ceiling is
        // drawn anew each cycle from its own strategy.
        //
        // Since issue #607 the draw is DOWNWARD-ONLY (`draw_downward`, as the session
        // ceiling has been since #609): a ceiling is a not-cross line, so jitter may
        // only ever buy MORE margin, never push the effective ceiling above the
        // operator-set value. The straddle at 60% is unaffected — the base 95 with a
        // 100-wide spread still reaches well below it — so this test keeps asserting
        // per-cycle redraw, not jitter direction (`timing::draw_downward_never_draws_
        // above_base` owns that).
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
            tun.weekly_ceiling_strategy = Strategy {
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
                // Below the raw ceiling (0.95): an ORDINARY over-fire-point swap, so whether it
                // skips depends purely on the jittered cooldown draw — the property under test.
                // (At/above the raw ceiling the #611 bypass would swap every seed, so `skipped`
                // would be 0 and the "produces both" assertion could never hold.)
                .ok("u-A", 0.92, 0.40)
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

    /// The #613 daemon-side wiring the pure `landing` / `recent_landing_overshoot_view` unit tests
    /// can't reach: `note_landing_overshoot` watches a PARKED account's post-swap polls, records a
    /// LOCAL landing overshoot when the parked account crosses the SLO ceiling within the landing
    /// window, and the retained record surfaces in `snapshot`'s `recent_landing_overshoot` projection —
    /// the `status`-visible signal that turns the silent post-swap-tail breach into an operator-visible
    /// one. Covers the fire path plus every disarm: below-ceiling, re-activation, window-expiry, and a
    /// failed poll leaving the watch armed.
    #[tokio::test]
    async fn note_landing_overshoot_fires_on_a_parked_climb_and_projects_into_snapshot() {
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        daemon.state.active = Some(0); // "work" active; "spare" (idx 1) is the parked account watched
        const NOW: i64 = 1_782_777_600;
        let no_readings: [Option<Usage>; 3] = [None, None, None];
        let base = daemon.clock.now(); // frozen clock, so `snapshot`'s `blind_at` equals this
        let notice = |d: &FakeDaemon| {
            d.snapshot(Some(0), &no_readings, NOW)
                .recent_landing_overshoot
        };

        // Arm the landing watch on the parked "spare" as a reason=session swap would (fired at 95%).
        let arm = |d: &mut FakeDaemon| {
            d.state.accounts[1].parked_landing = Some(ParkedLanding {
                armed_at: base,
                decision_pct: 95,
            });
        };

        // A parked live reading AT the SLO ceiling (99) within the window → a landing overshoot: the
        // record is retained (parked handle + fired-vs-landed spread), the watch disarms (fire once),
        // and the snapshot surfaces it.
        arm(&mut daemon);
        daemon.note_landing_overshoot(1, Some(0), &Ok(reading(0.99, 0.50)), base);
        assert_eq!(
            daemon.state.last_landing_overshoot,
            Some(LandingOvershootRecord {
                from_label: "spare".to_owned(),
                decision_pct: 95,
                landing_pct: 99,
                at: base,
            })
        );
        assert!(
            daemon.state.accounts[1].parked_landing.is_none(),
            "fires once, disarms"
        );
        assert_eq!(
            notice(&daemon),
            Some(LandingOvershoot {
                from_label: "spare".to_owned(),
                decision_pct: 95,
                landing_pct: 99,
            }),
            "the overshoot is status-visible",
        );

        // Reset the retained record; below the ceiling is NOT an overshoot — the watch stays armed for a
        // later reading, and nothing is recorded.
        daemon.state.last_landing_overshoot = None;
        arm(&mut daemon);
        daemon.note_landing_overshoot(1, Some(0), &Ok(reading(0.98, 0.50)), base);
        assert!(
            daemon.state.last_landing_overshoot.is_none(),
            "98 < 99: no breach"
        );
        assert!(
            daemon.state.accounts[1].parked_landing.is_some(),
            "stays armed below the ceiling"
        );

        // The account going ACTIVE again disarms with no breach — it is no longer a parked tail (even at
        // an over-ceiling reading).
        arm(&mut daemon);
        daemon.note_landing_overshoot(1, Some(1), &Ok(reading(0.99, 0.50)), base);
        assert!(
            daemon.state.last_landing_overshoot.is_none(),
            "re-activated: no breach"
        );
        assert!(
            daemon.state.accounts[1].parked_landing.is_none(),
            "re-activation disarms"
        );

        // Past the landing window the watch disarms with no breach — a later climb is a fresh session
        // cycle, not this swap's committed tail.
        arm(&mut daemon);
        daemon.note_landing_overshoot(
            1,
            Some(0),
            &Ok(reading(0.99, 0.50)),
            base + landing::LANDING_WINDOW,
        );
        assert!(
            daemon.state.last_landing_overshoot.is_none(),
            "window elapsed: no breach"
        );
        assert!(
            daemon.state.accounts[1].parked_landing.is_none(),
            "window-expiry disarms"
        );

        // A FAILED poll (blindness, not a safe landing) leaves the watch armed for a later live reading.
        arm(&mut daemon);
        daemon.note_landing_overshoot(1, Some(0), &Err(Error::UsageUnauthorized), base);
        assert!(
            daemon.state.last_landing_overshoot.is_none(),
            "a failed poll is not a breach"
        );
        assert!(
            daemon.state.accounts[1].parked_landing.is_some(),
            "a failed poll leaves the watch armed"
        );
    }

    #[tokio::test]
    async fn the_runtime_landing_boundary_rounds_like_the_offline_slo() {
        // Issue #615, the RUNTIME half of the rounding boundary the offline reader pins in
        // `reliability::landing_boundary_fraction_rounds_consistently_with_the_slo`. The live
        // detector reaches the same `>= 99` comparison by a different route — `to_pct` here vs the
        // reader's own fraction→percent conversion — so the boundary is asserted on BOTH sides: the
        // two must classify an identical landing fraction identically, or the runtime `status`
        // signal and the offline SLI would disagree about what breached.
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        daemon.state.active = Some(0);
        let base = daemon.clock.now();
        // Arm the landing watch on the parked "spare" as a reason=session swap would (fired at 95%),
        // feed it one live reading, and report the `landing_pct` it recorded — `None` when the
        // reading held below the ceiling and no breach was retained.
        let breach_pct_at = |d: &mut FakeDaemon, session: f64| {
            d.state.last_landing_overshoot = None;
            d.state.accounts[1].parked_landing = Some(ParkedLanding {
                armed_at: base,
                decision_pct: 95,
            });
            d.note_landing_overshoot(1, Some(0), &Ok(reading(session, 0.50)), base);
            d.state
                .last_landing_overshoot
                .as_ref()
                .map(|r| r.landing_pct)
        };

        // The rounding IS the boundary: 0.9849 → 98.49 → 98 holds; 0.985 → 98.5 → 99 breaches. Half a
        // percentage point below the nominal 0.99 ceiling, exactly as the offline reader classes it.
        assert_eq!(
            to_pct(0.9849),
            98,
            "just under the boundary rounds DOWN, below the ceiling"
        );
        assert_eq!(
            to_pct(0.985),
            99,
            "the boundary rounds UP, onto the ceiling"
        );
        assert_eq!(breach_pct_at(&mut daemon, 0.9849), None, "0.9849 holds");
        assert_eq!(
            breach_pct_at(&mut daemon, 0.985),
            Some(99),
            "0.985 is already a runtime landing overshoot",
        );

        // The REST of the sub-ceiling band — fractions BELOW the nominal 0.99 that rounding alone
        // pulls onto it. These are the ones truncation would silently release to a compliant 98, so
        // they carry the discrimination; 0.99 and up round to 99 under either mode.
        for session in [0.9875, 0.9899] {
            assert_eq!(
                breach_pct_at(&mut daemon, session),
                Some(99),
                "{session} is below the nominal ceiling but rounds onto it at runtime",
            );
        }
    }

    #[tokio::test]
    async fn a_reason_session_swap_arms_the_landing_watch_but_a_weekly_swap_does_not() {
        // Issue #613: the arm is wired into the REAL reactive-swap path (`decide_action`), which the
        // `note_landing_overshoot` unit test above bypasses (it sets `parked_landing` by hand). Drive
        // two genuine swaps end-to-end and pin the three load-bearing arming invariants that manual test
        // cannot reach: (a) a reason=session swap arms the OUTGOING account with its swap-DECISION
        // percent, (b) the INCOMING account is left disarmed, (c) a reason=weekly swap does NOT arm (the
        // offline #595 landing SLI this mirrors measures reason=session landings only).

        // (a) + (b): `work` (idx 0) over the 95 session trigger, weekly well below → a reason=session
        // swap to the only viable target `spare` (idx 1).
        {
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
            assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
            // The OUTGOING account (0) is armed with the swap-DECISION percent (to_pct(0.97) = 97)…
            assert_eq!(
                daemon.state.accounts[0].parked_landing.map(|p| p.decision_pct),
                Some(97),
                "a reason=session swap arms the parked (outgoing) account with its decision percent",
            );
            // …and the INCOMING account (1) is left disarmed (`record_swap` cleared it).
            assert!(
                daemon.state.accounts[1].parked_landing.is_none(),
                "the account going active is never a parked-landing subject",
            );
        }

        // (c): `work` session 0.50 (below the 95 session trigger) but weekly 0.98 (at the helper's 98
        // weekly trigger) → a reason=weekly swap. The parked account must NOT be armed.
        {
            let roster = vec![account("u-A", "work"), account("u-B", "spare")];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
            ])
            .await;
            let (_dir, json) = claude_json("u-A");
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
                json,
                &tun,
            );

            let outcome = warmed_tick(&mut daemon).await;
            assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
            assert!(
                daemon.state.accounts[0].parked_landing.is_none(),
                "a reason=weekly swap does not arm the landing watch",
            );
        }
    }

    #[tokio::test]
    async fn snapshot_projects_the_canonical_scrub_rollup_gated_on_the_scrubbed_signal() {
        // Issue #516: `snapshot` projects the two edge-latched scrub signals into the wire rollup,
        // GATED on `signaled_canonical_scrubbed` FIRST and only THEN refined by exhaustion. The gate
        // is load-bearing: the restore path clears `signaled_canonical_scrubbed` (a confirmed live
        // re-read) but NOT `signaled_scrub_adopt_exhausted` (cleared only on a churn-window age-out,
        // inside the scrubbed-gated `recover_scrubbed_canonical`), so `(scrubbed=false, exhausted=true)`
        // is REACHABLE after a `claude /login` recovery — and MUST read healthy, never a stale
        // un-recoverable. This projection is the daemon-side wiring the pure serialize test can't reach.
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        const NOW: i64 = 1_782_777_600;
        let no_readings: [Option<Usage>; 3] = [None, None, None];
        let rollup = |d: &FakeDaemon| d.snapshot(None, &no_readings, NOW).canonical_scrub;

        // Healthy: neither signal set → the rollup is absent.
        assert_eq!(
            rollup(&daemon),
            None,
            "a healthy canonical projects no rollup"
        );

        // Scrubbed, adopt still in progress (#464) → Recovering, the lower-severity self-may-heal state.
        daemon.state.signaled_canonical_scrubbed = true;
        assert_eq!(
            rollup(&daemon),
            Some(CanonicalScrub::Recovering),
            "a scrubbed-but-recovering canonical projects Recovering"
        );

        // Scrubbed AND recovery exhausted (#467) → Exhausted, the residual un-recoverable state #469
        // renders with the `claude /login` remedy. Exhaustion OUTRANKS recovering (most-severe wins).
        daemon.state.signaled_scrub_adopt_exhausted = true;
        assert_eq!(
            rollup(&daemon),
            Some(CanonicalScrub::Exhausted),
            "recovery-exhausted outranks merely-recovering"
        );

        // THE GATE (the correctness subtlety): the canonical RECOVERS — a live re-read clears
        // `signaled_canonical_scrubbed` — while the exhausted latch still lingers (not yet aged out).
        // The rollup MUST read healthy, never a stale `Exhausted` over a healed canonical.
        daemon.state.signaled_canonical_scrubbed = false;
        assert_eq!(
            rollup(&daemon),
            None,
            "a healed canonical reads healthy even while the exhausted latch lingers"
        );
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
        assert!(crate::redaction::meter::unauthored_emails(&json, &[]).is_empty());
        assert!(!json.to_lowercase().contains("token"));

        // The two no-candidate verdicts project without a label, so the client can tell
        // `no viable target` from `awaiting usage data`. `no_viable_target` now carries the #405
        // fleet-capacity relief hint (`cause` + `resets_at`), projected straight to the wire.
        let no_target = StatusSnapshot {
            next_swap: Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: Some(1_893_800_500),
            }),
            ..Default::default()
        };
        assert!(serde_json::to_string(&status_response(&no_target))
            .unwrap()
            .contains(r#""next_swap":{"state":"no_viable_target","cause":"weekly","resets_at":1893800500}"#));
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

    #[test]
    fn no_viable_target_relief_round_trips_and_its_absence_still_parses() {
        // #405 / schema 1.2 → 1.3: the fleet-capacity relief payload on `no_viable_target`
        // round-trips, and a PRE-#405 wire (the fields absent) still decodes to a bare no-target —
        // the additive minor-bump forward-compat both media rest on.
        let with_relief = NextSwap::NoViableTarget {
            cause: Some(NoTargetCause::Weekly),
            resets_at: Some(1_893_800_500),
        };
        let json = serde_json::to_string(&with_relief).unwrap();
        assert_eq!(
            json,
            r#"{"state":"no_viable_target","cause":"weekly","resets_at":1893800500}"#
        );
        assert_eq!(
            serde_json::from_str::<NextSwap>(&json).unwrap(),
            with_relief
        );

        // A current daemon that found no parseable reset: `cause` present, `resets_at` null.
        let no_reset = NextSwap::NoViableTarget {
            cause: Some(NoTargetCause::Session),
            resets_at: None,
        };
        assert_eq!(
            serde_json::to_string(&no_reset).unwrap(),
            r#"{"state":"no_viable_target","cause":"session","resets_at":null}"#
        );

        // ABSENCE still parses: a pre-#405 daemon omits BOTH fields → both `None`. An OLD client
        // tolerates the NEW daemon by ignoring the added keys; a NEW client tolerates the OLD daemon
        // by defaulting the absent ones (`#[serde(default)]`, mirrored by Swift `decodeIfPresent`).
        let bare: NextSwap = serde_json::from_str(r#"{"state":"no_viable_target"}"#).unwrap();
        assert_eq!(
            bare,
            NextSwap::NoViableTarget {
                cause: None,
                resets_at: None
            }
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
            crate::redaction::meter::unauthored_emails(&reply, &[]).is_empty(),
            "the ack wire leaks no non-authored email (#15/#444): {reply:?}"
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
            crate::redaction::meter::unauthored_emails(&reply, &[]).is_empty(),
            "the ack wire leaks no non-authored email (#15/#444): {reply:?}"
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
            Error::ConfigInvalid("session_ceiling must be in 50..=99, got 120".to_owned()),
            Error::ConfigTargetMaxSessionAboveTrigger {
                target_max_session_usage: 95,
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
            daemon.state.accounts[0].health.quarantined = true; // dead, now being re-probed
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
                target_max_session_usage: 70,
                session_ceiling: 90,
                weekly_ceiling: 98,
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
            // Issue #405: on the dead-active tick the escape target is not yet polled, so the
            // emergency path strands and emits the durable `active_dead_no_target` event through
            // the real daemon path — proving the #15 secret-scan below covers it non-vacuously.
            corpus.contains("event=active_dead_no_target hold=work cause=weekly"),
            "log channel: active_dead_no_target event missing"
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
        assert_clean(&corpus, &secrets, &[]);
    }

    #[tokio::test]
    async fn redaction_meter_permits_an_operator_authored_email_label_across_channels() {
        use crate::redaction::meter::{assert_clean, scan, Finding, Secrets};
        // #444: the operator may label an account with their OWN email. That authored
        // label legitimately reaches EVERY operator-facing channel (the event log, the
        // `status` render — colored and not —, the UDS wire, the swap echo). The METER
        // must PERMIT it there — while STILL failing on the credential-read email, which
        // is never a label. Proves the relaxation is uniform (no panel/CLI divergence)
        // AND provenance-scoped: the allow-set is the capture-input label, never the
        // credential-read email.
        let secrets = Secrets::meter_fixture();
        const EMAIL_LABEL: &str = "me@my-own.example"; // the operator's own choice (capture-input)
        const A: (&str, &str) = ("11111111-1111-1111-1111-111111111111", EMAIL_LABEL);
        const B: (&str, &str) = ("22222222-2222-2222-2222-222222222222", "spare");

        let poller = FakeRosterPoller::new()
            .ok(A.0, 0.97, 0.40) // active, over the session trigger
            .ok(B.0, 0.10, 0.20); // the viable target
        let tun = tunables(95, 80, 0);
        let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B], poller, &tun).await;
        let outcome = warmed_tick(&mut daemon).await;
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });

        let mut corpus = String::new();
        harvest_channels(&outcome, &mut corpus);

        // The authored email label DID reach a channel (else the test is vacuous).
        assert!(
            corpus.contains(EMAIL_LABEL),
            "the authored email label reached an operator-facing channel: {corpus}"
        );
        // The credential-read email is seeded into the stashes + `~/.claude.json` only —
        // it must NEVER surface on a harvested channel (#444 AC2).
        assert!(
            !corpus.contains(secrets.email()),
            "the credential-read email must not leak onto any channel"
        );

        // With the label in the capture-input allow-set, the METER PASSES — the
        // relaxation applies uniformly to EVERY channel the corpus spans.
        assert_clean(&corpus, &secrets, &[EMAIL_LABEL]);
        // Provenance seam: the label classifies as a permitted KnownEmail, never leak.
        let findings = scan(&corpus, &secrets, &[EMAIL_LABEL]);
        assert!(
            findings.iter().any(|f| matches!(f, Finding::KnownEmail)),
            "the authored label classifies as a permitted KnownEmail: {findings:#?}"
        );
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::EmailShape { .. })),
            "no unauthored email shape on any channel: {findings:#?}"
        );
        // Divergence guard: WITHOUT authoring it, the SAME corpus fails — proving it is
        // the allow-set (not an accident of the fixture) that permits the label.
        assert!(
            scan(&corpus, &secrets, &[])
                .iter()
                .any(|f| matches!(f, Finding::EmailShape { matched } if matched == EMAIL_LABEL)),
            "an un-authored email label is a leak on the strict scan"
        );
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
        assert_clean(&corpus, &secrets, &[]);
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

        // Redaction: a handle fixture carries no email at all — with an empty
        // allow-set that is the strict bar (any `@`-shape would be UNAUTHORED and
        // fail), now in the provenance vocabulary rather than a blanket no-`@`
        // (issue #15, relaxed provenance-scoped by #444/#447).
        let raw = std::fs::read_to_string(&samples_path).unwrap();
        assert!(
            crate::redaction::meter::unauthored_emails(&raw, &[]).is_empty(),
            "no unauthored email may reach the store: {raw}"
        );
        assert!(
            !raw.contains("sk-ant"),
            "no token may reach the store: {raw}"
        );
    }

    /// #447: an operator-authored email label flows through the collector into the
    /// store verbatim (`append_sample_for_poll` copies the roster label into
    /// `Sample.acct`) and is PERMITTED under the provenance-scoped waiver — while a
    /// stray unauthored email would still fail. Companion to the handle-fixture case
    /// above; guards that the store bar tracks the label's provenance, not the mere
    /// presence of an `@`.
    #[test]
    fn collector_carries_an_operator_authored_email_label() {
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
        let authored = "alice@example.com";

        append_sample_for_poll(&samples_path, authored, &reading, 1_700_000_000);

        let samples = crate::usage_store::read_samples(&samples_path).unwrap();
        assert_eq!(samples[0].acct, authored, "the authored label, verbatim");

        let raw = std::fs::read_to_string(&samples_path).unwrap();
        // Permitted WHEN authored…
        assert!(
            crate::redaction::meter::unauthored_emails(&raw, &[authored]).is_empty(),
            "an operator-authored email label is permitted: {raw}"
        );
        // …but the very same bytes surface as a leak WITHOUT the provenance allow-set
        // (the assertion is not vacuous).
        assert_eq!(
            crate::redaction::meter::unauthored_emails(&raw, &[]),
            vec![authored.to_owned()],
            "without provenance the label reads as an unauthored email: {raw}"
        );
        assert!(!raw.contains("sk-ant"), "no token: {raw}");
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
        assert!(
            crate::redaction::meter::unauthored_emails(&line, &[]).is_empty(),
            "no non-authored email (#15/#444): {line}"
        );
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
