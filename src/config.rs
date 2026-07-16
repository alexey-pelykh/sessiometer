// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Runtime configuration: the roster and tunables persisted in `config.toml`.
//!
//! `config.toml` (at [`paths::config_file`]) is the daemon's source of truth —
//! the captured account roster plus the poll/swap tunables. It is loaded at
//! daemon start and written by `capture` (issue #4).
//!
//! Loading is two-stage so invalid states are unrepresentable: TOML is first
//! deserialized into a permissive [`RawConfig`] (wide integer types, every key
//! optional with a documented default), then [`validate`](Config::validate)d
//! into the typed [`Config`]. A malformed file is a hard error — there are no
//! silent fallbacks to defaults for a file that exists but does not parse or
//! does not satisfy the bounds.
//!
//! Nothing here carries secret material: the roster keys accounts by
//! `account_uuid` / `label`, never by token or email (issues #9, #15, #17). The
//! keychain stash name is derived from `account_uuid`, not stored (issue #70).
//! Error messages quote only those non-secret fields.

use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use toml_writer::{ToTomlValue, TomlStringBuilder};

use crate::error::{Error, Result};
use crate::paths;
use crate::timing::{Jitter, Strategy};
use crate::usage_store::RetentionPolicy;

/// Default seconds between re-polling a given account — the per-account cadence.
/// Issue #38 lengthened this from the original fixed 60 s to a longer base that the
/// normal poll jitter then decorrelates across accounts/cycles; issue #76 confirmed
/// 5 min as a comfortable steady-state cadence (the rate-limit / transient back-off
/// widens it further under sustained `429` / `5xx`, so the base need not be
/// conservative). Issue #80 spreads the roster WITHIN this cadence — one account per
/// `poll_secs / N` sub-interval — rather than bursting all N at once.
const DEFAULT_POLL_SECS: u64 = 300;
/// Default standard deviation (seconds) of the poll interval's normal jitter —
/// ~20% of [`DEFAULT_POLL_SECS`]. Poll is the one tunable that jitters by default
/// (issue #38). Issue #76 WIDENED this from 30 s to 60 s so concurrent accounts
/// (and successive cycles) decorrelate over a ~1–3 min spread rather than clustering
/// within ~±30 s of the 5 min mark.
const DEFAULT_POLL_JITTER_STDDEV: f64 = 60.0;
/// Default `exhausted_poll_secs` (issue #537): the WIDENED poll cadence applied to an
/// out-of-rotation (weekly- or session-exhausted) NON-active peer. Such a peer's usage
/// number can only change when its server-side window resets (a time the daemon already
/// knows — `weekly_resets_at` / `session_resets_at`) or on a RARE out-of-band server reset,
/// so re-polling it every `poll_secs` is a wasted request each cycle. One hour is the
/// worst-case blindness ceiling for the rare early reset; the daemon pulls the next poll
/// EARLIER when a known `resets_at` lands sooner (see [`crate::daemon`]'s exhaustion window).
/// Deliberately the same 3600 s as [`DEFAULT_REFRESH_CADENCE_SECS`] — an hour is the crate's
/// standard "slow background" cadence.
const DEFAULT_EXHAUSTED_POLL_SECS: u64 = 3600;
/// Default `near_limit_poll_secs` (issue #540): the TIGHTENED poll sub-interval (the per-tick
/// spacing) applied to the ACTIVE account while its reading — or the #539 projection — is in the
/// near-limit band. The OPPOSITE direction of [`DEFAULT_EXHAUSTED_POLL_SECS`] (which WIDENS an
/// exhausted peer): near-limit, the active is the one account that can climb to its limit WHILE
/// active, so its poll cadence is tightened so no long gap opens on the final stretch (the poll-gap
/// residual #363/#538 named). Applied as `min(poll_secs / N, near_limit_poll_secs)`, so via the
/// #366 active-interleave (which re-observes the active every ~2 sub-intervals) the active is
/// re-polled within ~`2 × near_limit_poll_secs` = ~120 s near-limit — inside the ratified 60-150 s
/// active band. Bounded BELOW the band: it only engages when the active is in/approaching the band,
/// so the steady-state (below-band) cadence — and the source-scoped 429 footprint the fixed
/// `poll_secs` protects — is unchanged. 60 s keeps the tightened tick at/above the ~50 s
/// observed-safe spacing (the D-LAT-2 finding); `0` disables the path (the kill-switch, like
/// `session_velocity_horizon_secs`).
const DEFAULT_NEAR_LIMIT_POLL_SECS: u64 = 60;
/// Default seconds to wait after a swap before another is allowed.
const DEFAULT_COOLDOWN_SECS: u64 = 60;
/// The NON-ZERO floor for `cooldown_secs` (issue #272): the smallest interval, in
/// seconds, the daemon will pace account swaps by. The cooldown is an operator
/// tunable, but it cannot be configured — or jittered — below this floor, so swap
/// pacing can never be disabled to zero. Rapid-fire flapping between accounts is a
/// safety hazard (each swap rewrites the live credential out-of-band), so the floor
/// is a hard minimum, not a default.
///
/// Anchored to the poll-interval floor (`daemon::POLL_SECS_LO` = 5 s): a
/// quota-driven swap can only fire on a poll tick, so a cooldown below the minimum
/// poll cadence would be unobservable — 5 s is the smallest meaningful non-zero
/// floor. It is enforced at BOTH edges: [`Config::validate`] rejects a smaller
/// `cooldown_secs` base at config load, and the daemon's per-cycle draw clamps to it
/// (`daemon::COOLDOWN_SECS_LO`, derived from this constant) so a configured jitter
/// spread cannot dip a single cycle beneath it either. Emergency swaps away from a
/// dead active bypass the cooldown entirely, so the floor never delays account-death
/// recovery — it only paces non-urgent quota-driven swaps.
pub(crate) const COOLDOWN_SECS_FLOOR: u64 = 5;
// Compile-time guard (issue #272): the floor must stay strictly positive so swap
// pacing can never be disabled to zero. Zeroing it is a BUILD failure here, not a
// silent runtime gap — and because `daemon::COOLDOWN_SECS_LO` derives from this
// constant, the guard covers the per-cycle jitter clamp too.
const _: () = assert!(COOLDOWN_SECS_FLOOR >= 1);
/// Default `session_trigger` percent.
const DEFAULT_SESSION_TRIGGER: u8 = 95;
/// Default `target_max_session_usage` percent (issue #398): the default-on swap-target
/// reserve — only swap TO an account whose session usage is below this. Sits
/// below `session_trigger` so a swapped-to target keeps runway before the next
/// poll; supersedes #10's opt-in (an absent key now means this, not "off").
const DEFAULT_TARGET_MAX_SESSION_USAGE: u8 = 80;
/// Default `weekly_trigger` percent — separate from and higher than
/// `session_trigger` (issue #41): the weekly window is the longer, harder limit,
/// so the active account is allowed closer to full on it before a swap-away.
const DEFAULT_WEEKLY_TRIGGER: u8 = 98;
/// Default consecutive-401 count before an account is treated as rejected.
const DEFAULT_MONITOR_401_N: u8 = 3;
/// Default consecutive recovery-probe successes before a quarantined (dead)
/// account is restored to the rotation (issue #42).
const DEFAULT_MONITOR_RECOVERY_M: u8 = 2;
/// Default `session_blind_swap_secs` (issue #452, ADR-0017): the interim `T` for the
/// bounded-blindness preemptive-swap gate — the active account's retained pre-blind
/// anchor must be stale beyond this before the gate turns eligible. 300 s is roughly
/// three consecutive active 429s under the #453 active-poll backoff cap (120 s), i.e.
/// genuinely stuck (confirmed by the #451 replay). Setting the tunable to its 86400 s
/// ceiling (24 h, far beyond any real blind window) disables the path — the kill-switch.
const DEFAULT_SESSION_BLIND_SWAP_SECS: u64 = 300;
/// Default `session_blind_risk_band` percent (issue #452, ADR-0017): the retained
/// pre-blind anchor (`last_good`, #450) must be at/over this for the preemptive gate to
/// arm. Deliberately LOWER than `session_trigger` — the gate acts preemptively on a
/// stale anchor, before the account would trip the reactive trigger. 60 is the low end
/// of the conservative 60-to-65 band #484 ratified: the #451 replay is flat from 68 %
/// down to 50 %, so the data bounds only the ceiling and the fire-early-is-cheaper
/// asymmetry (a late swap hits the wall on an unattended run; an early one only spends a
/// recoverable-target swap) picks the low end.
const DEFAULT_SESSION_BLIND_RISK_BAND: u8 = 60;
/// Default `session_velocity_horizon_secs` (issue #539, ADR-0017): the projection horizon `H`
/// (seconds) for the velocity-projection preemptive trigger — the active account swaps away when
/// its PROJECTED session usage (`last + velocity × H`) crosses the trigger, before the observed
/// reading does. 120 s ≈ one active poll interval (the post-#366 interleave cadence, NOT the
/// `poll_secs=300` peer cadence), the horizon the #538 spike validated on 22,022 real samples
/// (P50=94 / P100=98 covered-swap, 0 over-fire at H ≤ 150 s). Setting it to `0` disables the path
/// — the projection reduces to `last`, which never crosses (the reactive path already held) — the
/// kill-switch.
const DEFAULT_SESSION_VELOCITY_HORIZON_SECS: u64 = 120;
/// Default `session_velocity_min_project_above` percent (issue #539, ADR-0017): the projective
/// trigger only projects when the observed session reading is at/over this. The #538 spike's FREE
/// guard — projection can't reach below it anyway (max reach ≤ 14 pp at H ≤ 150 s) — so it costs no
/// benefit while excluding spurious low-usage projections. Conventionally BELOW `session_trigger`
/// (the projection fires in the band beneath the reactive trigger).
const DEFAULT_SESSION_VELOCITY_MIN_PROJECT_ABOVE: u8 = 85;
/// Default `session_velocity_ema_alpha_pct` (issue #539, ADR-0017): the EMA smoothing weight α
/// (percent) applied to the per-account session-velocity signal (#399) — `ema = α·instant +
/// (1-α)·prev` — to damp a single-interval velocity spike so the projection keys off SUSTAINED
/// motion. α ≈ 0.5 (the #538-validated value); 100 means no smoothing (raw last-interval velocity).
const DEFAULT_SESSION_VELOCITY_EMA_ALPHA_PCT: u8 = 50;

/// Default seconds between periodic isolated-refresh ticks (issue #105). A conservative one-hour
/// cadence: #101's TTL question is resolved (the stored access-token expiry slides forward on each
/// refresh — a sliding window, not a fixed cap), but the cadence is deliberately NOT pinned to a
/// specific `TTL/3` — one hour keeps a parked token fresh well within its access-token TTL without
/// churning refresh-token rotations, and doubles as the near-expiry SELECTION horizon (an account
/// is due when its stored token would expire before the next tick — see [`RefreshConfig::cadence`]).
/// The #104 `poke` all-accounts horizon uses the same one hour for the same reason.
const DEFAULT_REFRESH_CADENCE_SECS: u64 = 3600;
/// Default seconds the daemon must idle before a refresh sweep fires (issue #105). One minute lets
/// the idle floor — anchored absolutely since #260, so it accumulates rather than resetting on each
/// idle re-arm — elapse soon after start-up without waiting out a whole poll interval.
const DEFAULT_REFRESH_IDLE_AFTER_SECS: u64 = 60;
/// Default seconds bounding ONE account's whole isolated-refresh cycle (issue #105). The
/// engine's internal `claude -p` spawn budget is ~40 s (#102); ninety seconds leaves
/// comfortable headroom for the seed, read-back and CAS re-stash around it, so a healthy
/// cycle is never truncated while a wedged one (a stuck keychain) still cannot stall the
/// daemon's return to polling. Cancelling mid-cycle is safe — the engine's RAII teardown
/// always runs (#102) and a forfeited token is bounded/recoverable (the engine Caller contract).
const DEFAULT_REFRESH_TIMEOUT_SECS: u64 = 90;
/// Default consecutive all-eligible-account refresh-error sweeps before the daemon surfaces a
/// SYSTEMIC refresh-mechanism failure (issue #378). Three sweeps balances early warning against a
/// transient blip: a stale-`claude`-path (#375) style outage errors every sweep and trips it fast,
/// while a single flaky sweep does not. A conservative default, tunable per the ADR-0005 hand-emit
/// pattern; the near-expiry cadence governs how quickly three sweeps accrue in wall-clock.
/// `pub(crate)` so the daemon's `new` can seed its inert `systemic_failure_n` placeholder from the
/// same source of truth (production overrides it via `with_systemic_failure_n`).
pub(crate) const DEFAULT_REFRESH_SYSTEMIC_FAILURE_N: u32 = 3;

/// Default seconds bounding one whole interactive `login` capture (issue #135). Mirrors
/// [`crate::login::DEFAULT_LOGIN_TIMEOUT`] — the engine's per-call fallback — kept in sync by
/// the `default_login_timeout_matches_the_engine_default` test. Far longer than the refresh
/// timeout because a `/login` waits on a human completing a browser OAuth handoff, not a
/// headless `claude -p` spawn; the operator-tunable range (60..=600) bounds both an impatient
/// and a very patient operator.
const DEFAULT_LOGIN_TIMEOUT_SECS: u64 = 180;

/// Default raw-tier retention horizon (~14 d) for the usage-stats store (issue #161).
/// Mirrors [`crate::usage_store`]'s self-contained `DEFAULT_RAW_WINDOW_SECS` — this
/// block is the operator-facing source of truth the daemon threads into the store's
/// [`crate::usage_store::RetentionPolicy`], kept in sync by the
/// `stats_defaults_match_the_store` test.
const DEFAULT_STATS_RAW_RETENTION_SECS: u64 = 14 * 86_400;
/// Default hourly-tier retention horizon (~90 d) for the usage-stats store (issue #161).
/// Mirrors the store's `DEFAULT_HOURLY_WINDOW_SECS`.
const DEFAULT_STATS_HOURLY_RETENTION_SECS: u64 = 90 * 86_400;
/// Default daily-tier retention horizon (issue #161). **`0` = lifetime**: the daily
/// aggregate tier is kept for the store's whole lifetime by design (a summarised day
/// cannot be recovered once its raw samples age out), so the default preserves that.
/// A non-zero value opts INTO bounding the daily tier to that many seconds.
const DEFAULT_STATS_DAILY_RETENTION_SECS: u64 = 0;
/// Default reporting period for the offline `stats` verb (issue #161) — the window it
/// selects when `--period` / `--since` are both omitted. One of the `stats`
/// vocabulary (`day` | `week` | `month` | `lifetime`); `week` matches that verb's own
/// built-in default.
const DEFAULT_STATS_PERIOD: &str = "week";
/// The valid `[stats].default_period` tokens (issue #161), the SAME vocabulary the
/// `stats` verb's `--period` accepts (`crate::stats::PeriodSpec`). Validated here so a
/// bad value fails at config-load with a clear message rather than at `stats`-run.
const STATS_PERIODS: [&str; 4] = ["day", "week", "month", "lifetime"];

/// Default Argon2id memory cost (KiB) for a WRITTEN encrypted export artifact (issue #150).
/// Mirrors migration.rs's built-in [`crate::migration::KdfCost::PRODUCTION`] memory cost,
/// kept in sync by the `migration_kdf_defaults_match_the_crypto` test. Bounded `8..=1_048_576`
/// (8 KiB..1 GiB) — the upper bound is migration.rs's decrypt-time memory guard, so an artifact
/// written at any in-range cost still decrypts.
const DEFAULT_MIGRATION_KDF_MEMORY_KIB: u32 = 65_536;
/// Default Argon2id time cost (iterations) for a written encrypted export (issue #150). Mirrors
/// [`crate::migration::KdfCost::PRODUCTION`]'s iterations, bounded `1..=16` (the upper bound is
/// migration.rs's decrypt-time iteration guard).
const DEFAULT_MIGRATION_KDF_ITERATIONS: u32 = 3;
/// The FIXED Argon2id lane count (issue #150) — NOT an operator tunable. The `argon2` crate
/// derives single-threaded unless its rayon-backed `parallel` feature is enabled (which we
/// avoid), so a lane count above 1 would only add cost without the intended parallel defense
/// (see migration.rs). Threaded into every derived [`crate::migration::KdfCost`] so the config
/// maps to the built-in production cost exactly; there is deliberately no `kdf_parallelism` key.
const MIGRATION_KDF_PARALLELISM: u32 = 1;
/// Default import conflict policy token (issue #150): SKIP an account already on the target
/// (leave it untouched) unless `--overwrite`. The safe default — an import never clobbers an
/// existing account unless the operator opts in.
const DEFAULT_MIGRATION_CONFLICT_POLICY: &str = "skip";
/// The valid `[migration].conflict_policy` tokens (issue #150), validated at load so a typo
/// fails fast rather than at import-run.
const MIGRATION_CONFLICT_POLICIES: [&str; 2] = ["skip", "overwrite"];

/// The keychain service-name namespace every account's stash lives under; the
/// full name is `Sessiometer/<account_uuid>` ([`Account::stash`]). Kept as one
/// shared constant so the prefix has a single definition (issue #70).
pub(crate) const STASH_PREFIX: &str = "Sessiometer/";

/// One captured account in the roster. Keyed by `account_uuid` alone.
///
/// The fields beyond the uniqueness key are read by the write path
/// ([`Config::render`], for `capture` #4) and by `list` / `status` (#17 / #9);
/// the swap engine (#6 / #7) rotates across the roster. They are validated and
/// persisted here ahead of those consumers.
///
/// The keychain stash name is NOT stored: it is definitionally
/// `Sessiometer/<account_uuid>` (issue #70), so [`Account::stash`] derives it
/// from `account_uuid` rather than carrying a duplicate, separately-persisted
/// copy that could drift out of sync.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct Account {
    /// Stable per-account identifier (the Claude `account_uuid`); the sole roster
    /// key, and the basis for the derived [`stash`](Account::stash) name.
    pub(crate) account_uuid: String,
    /// Human-readable label shown by `list` / `status`, and the operator-facing
    /// HANDLE written **verbatim** into the daemon's durable event log — e.g. the
    /// `event=swap` line's `from=` / `to=` (see [`crate::observability`], the sole
    /// place an event becomes a log line). Those diagnostics are secret-free BY
    /// CONSTRUCTION — closed enums, rounded percents, config ints, timestamps
    /// (issue #15) — so that guarantee extends to the label ONLY while the operator
    /// keeps it a **non-PII nickname** (`work`, `spare`), never an email or username
    /// (issue #404). Non-empty is the sole invariant [`validate`](Config::validate)
    /// enforces on it; PII-freedom is deliberately left to the operator — no
    /// email-shaped rejection, which would be a behavior change out of scope for #404.
    pub(crate) label: String,
    /// Whether this account participates in the rotation (issue #36). A disabled
    /// account stays in the roster (its keychain stash is untouched), but the
    /// daemon never swaps TO it and does not poll it; `sessiometer enable` returns
    /// it to the candidate pool. Defaults to `true` — a config entry that omits the
    /// key (every pre-#36 one) loads fully enabled. The reversible sibling of
    /// removal (#13), which instead deletes the stash.
    pub(crate) enabled: bool,
}

impl Account {
    /// The keychain service name the captured credential lives under,
    /// `Sessiometer/<account_uuid>`. Derived, never stored (issue #70): the stash
    /// is definitionally a function of `account_uuid`, so the roster keeps one
    /// source of truth and `config.toml` no longer carries a redundant
    /// `stash = …` line. See [`crate::stash`] for the stash layout itself.
    pub(crate) fn stash(&self) -> String {
        format!("{STASH_PREFIX}{}", self.account_uuid)
    }
}

/// The daemon tunables, validated into their typed ranges.
///
/// `Eq` is intentionally not derived: the timing strategies (issue #38) carry
/// `f64` magnitudes, so only `PartialEq` is available — sufficient for the tests'
/// `assert_eq!` and for the render round-trip check.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tunables {
    /// Seconds between re-polling a GIVEN account (`5..=3600`) — the per-account
    /// cadence and the base of the rate-limit back-off. The daemon polls one account
    /// per `poll_secs / N` sub-interval (issue #80), so a roster of N accounts is swept
    /// once per `poll_secs` without bursting all N requests at once.
    pub(crate) poll_secs: u64,
    /// Widened re-poll cadence, in seconds, for an out-of-rotation (weekly- or
    /// session-exhausted) NON-active peer (issue #537): `poll_secs..=86400`, default 3600
    /// (one hour). Such a peer's usage can only change on a server-side window reset (a
    /// time the daemon already knows via `resets_at`) or a RARE out-of-band reset, so
    /// re-polling it every `poll_secs` wastes a request each cycle. This is the CEILING of
    /// its slow-poll window; a known `resets_at` sooner than this pulls the next poll
    /// earlier (see [`crate::daemon`]). The lower bound is `poll_secs` (a slow-polled peer
    /// must never re-poll FASTER than the normal cadence); the ACTIVE account is EXEMPT (its
    /// swap-away trigger must stay observable). Distinct from the rate-limit back-off
    /// (`poll_backoff_until`, ADR-0009), which is armed by 429/5xx and cleared on any
    /// success — an exhausted account is an HTTP-200 success, so it needs its own window.
    pub(crate) exhausted_poll_secs: u64,
    /// Tightened re-poll sub-interval, in seconds, for the ACTIVE account while its reading (or the
    /// #539 velocity projection) is in the near-limit band (issue #540): `0` or `5..=3600`, default
    /// 60. The near-limit-scoped MIRROR of `exhausted_poll_secs` — where that WIDENS an exhausted
    /// peer's cadence, this TIGHTENS the active account's on its final climb to the limit, so no
    /// long poll gap opens while it is near the limit (the poll-gap half of #363's residual, split
    /// from #539's projection per the #538 spike). Applied as `min(poll_secs / N, near_limit_poll_secs)`,
    /// so with the #366 active-interleave the active is re-observed within ~`2 ×` this near-limit.
    /// It ONLY engages while the active is in/approaching the band (keyed off the SAME signals #539
    /// uses — last reading + `usage_velocity`); BELOW the band the sub-interval is the unchanged
    /// `poll_secs / N`, so the steady-state 429 footprint stays flat. ACTIVE-account only (the
    /// #453 active-vs-peer asymmetry); a peer being near-limit does nothing. `0` disables the path
    /// (the kill-switch). A value above the base sub-interval is inert (the `min` never binds), not
    /// an error, so no cross-field bound to `poll_secs`.
    pub(crate) near_limit_poll_secs: u64,
    /// Seconds to wait after a swap before another is allowed
    /// (`COOLDOWN_SECS_FLOOR..=3600` — a non-zero floor, #272). Consumed by the
    /// cooldown logic (#10 / #11).
    #[allow(dead_code)]
    pub(crate) cooldown_secs: u64,
    /// Default-on swap-target reserve (issue #398): the most-full an account may be
    /// to receive the active session — only swap *to* an account whose session usage
    /// is below this percent (`1..=session_trigger`; an explicit `0` admits no target
    /// and is rejected), so a freshly-swapped target keeps runway before the next
    /// poll. Always valued — an absent key means [`DEFAULT_TARGET_MAX_SESSION_USAGE`], not
    /// "off" (this supersedes #10's opt-in `Option`). Raise it toward `session_trigger`
    /// to admit busier targets (equal is inert); the always-on session gate
    /// (`session < session_trigger`, [`crate::daemon`]) still prevents oscillation
    /// independently.
    pub(crate) target_max_session_usage: u8,
    /// Swap *away* from the active account at or above this session percent
    /// (`50..=99`).
    pub(crate) session_trigger: u8,
    /// Swap *away* from the active account at or above this WEEKLY percent
    /// (`50..=99`) — the second, independent trigger dimension (issue #41).
    /// Separate from `session_trigger` (no cross-field constraint), typically set
    /// higher; the daemon swaps when EITHER dimension reaches its own trigger.
    pub(crate) weekly_trigger: u8,
    /// Bounded-blindness preemptive-swap gate threshold `T` (issue #452, ADR-0017), in
    /// seconds: the active account's retained pre-blind anchor (`last_good`, #450) must
    /// be stale beyond this before the gate arms. Promoted from the interim
    /// `BLIND_GATE_SECS` daemon constant (SLI-only until #452). A value at the validated
    /// 86400 ceiling disables the path (no blind window runs that long) — the kill-switch.
    pub(crate) session_blind_swap_secs: u64,
    /// Bounded-blindness preemptive-swap `risk_band` (issue #452, ADR-0017), as a
    /// session-usage percent: the pre-blind anchor must be at/over this for the gate to
    /// arm. Promoted from the interim `BLIND_GATE_RISK_BAND` daemon constant; DISTINCT
    /// from and biased below `session_trigger` (the gate acts preemptively on a stale
    /// anchor). Conservative 60 by default (#484).
    pub(crate) session_blind_risk_band: u8,
    /// Velocity-projection preemptive-trigger horizon `H` (issue #539, ADR-0017), in seconds:
    /// the active account swaps away when its PROJECTED session usage (`last + velocity × H`,
    /// keyed off the #399 usage-velocity signal) crosses the trigger, before the observed reading
    /// does — closing the OBSERVED reactive overshoot (#363) #452's blind-window path does not.
    /// `0` disables the path (projection reduces to `last`, never crosses) — the kill-switch.
    pub(crate) session_velocity_horizon_secs: u64,
    /// Velocity-projection guard (issue #539, ADR-0017), as a session-usage percent: the projective
    /// trigger only projects when the observed reading is at/over this. The #538 spike's FREE guard
    /// (projection can't reach lower anyway); conventionally BELOW `session_trigger`, like
    /// `session_blind_risk_band`.
    pub(crate) session_velocity_min_project_above: u8,
    /// Velocity-projection EMA smoothing weight α (issue #539, ADR-0017), as a percent: the per-poll
    /// blend `ema = α·instant + (1-α)·prev` damps a single-interval velocity spike so the projection
    /// keys off SUSTAINED motion. `100` = no smoothing (raw last-interval velocity).
    pub(crate) session_velocity_ema_alpha_pct: u8,
    /// Consecutive non-scope 401s before an account is treated as DEAD (`1..=20`).
    /// Consumed by the daemon's per-account health state (issue #42): the Nth
    /// consecutive 401 on an account's stored token quarantines it (stop polling /
    /// selecting it) and, if it is the active account, triggers an emergency swap.
    /// Distinct from #13's re-auth re-stash, which is driven by canonical-change
    /// detection ([`crate::keychain::CanonicalWatch`]), not by 401s.
    pub(crate) monitor_401_n: u8,
    /// Consecutive recovery-probe successes before a quarantined (dead) account is
    /// restored to the rotation (`1..=20`, issue #42). This governs the
    /// spontaneous-revival path only: a dead ACTIVE account whose own token starts
    /// answering again (WITHOUT a re-login) must poll successfully this many times in a
    /// row before it is un-quarantined. A re-login un-quarantines immediately instead
    /// (the #13 canonical-change re-stash clears the flag directly, issue #107).
    pub(crate) monitor_recovery_m: u8,
    /// Poll-interval timing strategy (issue #38): base = `poll_secs` (seconds),
    /// normal jitter by default. The daemon draws + clamps to `5..=3600` each
    /// cycle instead of sleeping a fixed interval.
    pub(crate) poll_strategy: Strategy,
    /// Swap-away trigger timing strategy (issue #38), in the PERCENT domain:
    /// base = `session_trigger`, no jitter unless configured. Drawn + clamped to
    /// `50..=99` each cycle, then divided by 100 for the swap decision.
    pub(crate) trigger_strategy: Strategy,
    /// Weekly swap-away trigger timing strategy (issue #41), in the PERCENT
    /// domain: base = `weekly_trigger`, no jitter unless configured. Drawn +
    /// clamped to `50..=99` each cycle, then divided by 100 for the swap decision
    /// — the weekly-dimension counterpart of `trigger_strategy`.
    pub(crate) weekly_trigger_strategy: Strategy,
    /// Post-swap cooldown timing strategy (issue #38), in seconds: base =
    /// `cooldown_secs`, no jitter unless configured. Drawn + clamped to
    /// `COOLDOWN_SECS_FLOOR..=3600` each cycle — the low bound is the non-zero swap
    /// floor (issue #272), so configured jitter can never draw a sub-floor cooldown.
    pub(crate) cooldown_strategy: Strategy,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            poll_secs: DEFAULT_POLL_SECS,
            exhausted_poll_secs: DEFAULT_EXHAUSTED_POLL_SECS,
            near_limit_poll_secs: DEFAULT_NEAR_LIMIT_POLL_SECS,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
            target_max_session_usage: DEFAULT_TARGET_MAX_SESSION_USAGE,
            session_trigger: DEFAULT_SESSION_TRIGGER,
            weekly_trigger: DEFAULT_WEEKLY_TRIGGER,
            session_blind_swap_secs: DEFAULT_SESSION_BLIND_SWAP_SECS,
            session_blind_risk_band: DEFAULT_SESSION_BLIND_RISK_BAND,
            session_velocity_horizon_secs: DEFAULT_SESSION_VELOCITY_HORIZON_SECS,
            session_velocity_min_project_above: DEFAULT_SESSION_VELOCITY_MIN_PROJECT_ABOVE,
            session_velocity_ema_alpha_pct: DEFAULT_SESSION_VELOCITY_EMA_ALPHA_PCT,
            monitor_401_n: DEFAULT_MONITOR_401_N,
            monitor_recovery_m: DEFAULT_MONITOR_RECOVERY_M,
            poll_strategy: Strategy {
                base: DEFAULT_POLL_SECS as f64,
                jitter: default_poll_jitter(),
            },
            trigger_strategy: Strategy::fixed(f64::from(DEFAULT_SESSION_TRIGGER)),
            weekly_trigger_strategy: Strategy::fixed(f64::from(DEFAULT_WEEKLY_TRIGGER)),
            cooldown_strategy: Strategy::fixed(DEFAULT_COOLDOWN_SECS as f64),
        }
    }
}

/// The default poll-interval jitter: normal, so polls decorrelate out of the box
/// (issue #38). Trigger, weekly trigger, and cooldown default to [`Jitter::None`].
fn default_poll_jitter() -> Jitter {
    Jitter::Normal {
        stddev: DEFAULT_POLL_JITTER_STDDEV,
    }
}

/// The periodic isolated-refresh schedule (issue #105) — the in-daemon counterpart of the
/// one-shot `poke` (#104), wiring the #102 refresh engine into the `run` loop's idle path.
///
/// **On by default**: `enabled` defaults `true`, so the daemon keeps PARKED accounts' stored
/// tokens fresh unless an operator turns it OFF. Between poll→usage→swap ticks it lets Claude
/// Code refresh them in an isolated `CLAUDE_CONFIG_DIR` (the engine's whole job), never touching
/// the live session's canonical credential.
///
/// The tick honors the engine's Caller contract: it refreshes parked accounts only (the
/// active account and the imminent swap target are excluded; the swap lock the engine holds
/// enforces the mid-swap case), and a refresh `Err` is non-fatal — logged, with the
/// dead-credential recovery path (#13/#42) absorbing a forfeited token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefreshConfig {
    /// Whether the periodic refresh tick runs at all. **On by default**: each refresh slides the
    /// stored expiry forward, and any rotated refresh token is captured and re-stashed, so the
    /// mechanism is self-sustaining. #101's TTL question is resolved (a sliding window, not a fixed
    /// cap); an operator can still set `enabled = false` to turn it off deliberately.
    pub(crate) enabled: bool,
    /// The parked accounts the tick is allowed to refresh, each named by its `list` label OR
    /// `account_uuid` (the same resolution `poke` and `use` key on). **Empty = all** parked
    /// accounts. A non-empty list narrows the candidate set; the near-expiry horizon still
    /// gates within it (a listed account is refreshed only when actually due). An entry that
    /// matches no roster account is ignored (the selection is best-effort — a stale entry
    /// never stops the daemon).
    pub(crate) accounts: Vec<String>,
    /// Seconds between refresh ticks AND the near-expiry selection horizon (issue #105): an
    /// account is *due* when its stored token would expire within one cadence of now — i.e.
    /// it would not survive until the next tick — so the cadence IS the configurable,
    /// TTL-aware threshold (#104 left the all-accounts horizon provisional for #105 to own).
    /// **Default** ([`DEFAULT_REFRESH_CADENCE_SECS`]): a conservative one-hour cadence that keeps a
    /// parked token fresh well within its access-token TTL (#101 resolved — a sliding window).
    pub(crate) cadence_secs: u64,
    /// Seconds the daemon must idle before a refresh sweep fires (issue #105). Anchored
    /// absolutely since #260: the idle floor no longer restarts on every idle re-arm (the 15 s
    /// login-watch, a control read), so it accumulates across the idle gap instead of resetting
    /// off the poll→usage→swap seam — which bounds primarily the FIRST sweep after a (re)start,
    /// after which steady-state sweeps fire on the cadence alone. **Default**
    /// ([`DEFAULT_REFRESH_IDLE_AFTER_SECS`]).
    pub(crate) idle_after_secs: u64,
    /// Seconds bounding ONE account's whole isolated-refresh cycle (issue #105); a cycle that
    /// exceeds it is cancelled (engine RAII teardown still runs) and reported as a non-fatal
    /// error. Default ([`DEFAULT_REFRESH_TIMEOUT_SECS`]) leaves headroom over the engine's
    /// ~40 s `claude -p` spawn budget.
    pub(crate) timeout_secs: u64,
    /// The `claude` binary the engine spawns, overriding the `$CLAUDE_BIN` / `$PATH`
    /// resolution (issue #105) — `None` (the default) defers to that resolution. Resolved
    /// (absolutized, validated to exist) before any spawn by
    /// [`crate::paths::claude_binary_with_override`].
    pub(crate) claude_bin: Option<PathBuf>,
    /// Consecutive refresh sweeps that must fail with `outcome=error` across EVERY eligible
    /// account before the daemon surfaces a SYSTEMIC refresh-mechanism failure (issue #378) — the
    /// edge-triggered `refresh_systemic_failure` event + the `status` refresh-health indicator. A
    /// mechanism-down signal distinct from per-account `at_risk`, so an operator sees a stale-path
    /// (#375) style outage without waiting for an account to die. Bounds `1..=100`; **default**
    /// [`DEFAULT_REFRESH_SYSTEMIC_FAILURE_N`]. Governs detection only — never a swap/poll decision.
    pub(crate) systemic_failure_n: u32,
    /// Whether the #282 PROACTIVE keep-warm of the ACTIVE account fires (issue #468). **Off by
    /// default** — the second, tighter opt-in for a *live*-canonical rotation. Gated within
    /// `[refresh].enabled` (which wires the keep-warm engine at all, ADR-0015): with the engine off
    /// this is moot; with it on and this `false`, the active account is kept warm ONLY reactively
    /// (`should_keep_warm_retry`, on a real 401) + recovered by the #467 autonomous adopt-target —
    /// the pre-emptive near-expiry mint that rotates the live shared token every cadence is
    /// suppressed. Finding #476 (`docs/findings/0476-keep-warm-scrub-risk-tradeoff.md`, predicate C)
    /// measured that proactive mint at ~44 % of the daemon's canonical churn (4.6 rotation-yanks/day,
    /// all `rotated=true`) and — since #467 re-based the scrub it guards against from fleet-wide
    /// unrecoverable to `continue`-recoverable — recommends gating it off, leaning on the reactive
    /// backstop. Set `true` to restore the pre-#468 proactive keep-warm (finding #476 fallback A's
    /// base, or an on/off capture comparison). The reactive backstop is UNAFFECTED by this flag.
    pub(crate) proactive_keep_warm: bool,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            accounts: Vec::new(),
            cadence_secs: DEFAULT_REFRESH_CADENCE_SECS,
            idle_after_secs: DEFAULT_REFRESH_IDLE_AFTER_SECS,
            timeout_secs: DEFAULT_REFRESH_TIMEOUT_SECS,
            claude_bin: None,
            systemic_failure_n: DEFAULT_REFRESH_SYSTEMIC_FAILURE_N,
            // Issue #468 / finding #476 predicate C: proactive keep-warm of the active account is
            // OFF by default (leans on the reactive backstop + #467 recovery); an explicit opt-in
            // restores the pre-#468 pre-emptive mint.
            proactive_keep_warm: false,
        }
    }
}

impl RefreshConfig {
    /// The refresh cadence — which is also the near-expiry selection horizon — as a
    /// [`Duration`].
    pub(crate) fn cadence(&self) -> Duration {
        Duration::from_secs(self.cadence_secs)
    }

    /// The post-tick idle the daemon must accrue before a refresh fires, as a [`Duration`].
    pub(crate) fn idle_after(&self) -> Duration {
        Duration::from_secs(self.idle_after_secs)
    }

    /// The per-account whole-cycle timeout, as a [`Duration`].
    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// The `login` verb's settings (issue #135): how long one interactive `login` capture may run,
/// and an optional `claude` binary override — the user-facing counterpart of the `[refresh]`
/// block for the one-shot `sessiometer login [label]` command.
///
/// Both keys are optional with documented defaults, so a config with no `[login]` table (or none
/// at all — the first `login` runs before any `config.toml` exists) uses [`LoginConfig::default`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginConfig {
    /// Seconds bounding ONE whole interactive login capture (issue #135 AC): on expiry the
    /// isolated `claude /login` child is killed and teardown runs (the operator gets a cancelled
    /// outcome). Default [`DEFAULT_LOGIN_TIMEOUT_SECS`] (180), operator-tunable within `60..=600`
    /// — low enough to abandon an unattended login, high enough for a deliberate browser handoff.
    pub(crate) timeout_secs: u64,
    /// The `claude` binary the login engine spawns, overriding the `$CLAUDE_BIN` / `$PATH`
    /// resolution — `None` (the default) defers to that resolution. Resolved (absolutized,
    /// validated to exist) before the spawn by the SAME resolver the refresh path uses,
    /// [`crate::paths::claude_binary_with_override`] (issue #135 AC: "reusing the existing
    /// binary-override resolver; no new config mechanism").
    pub(crate) claude_bin: Option<PathBuf>,
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_LOGIN_TIMEOUT_SECS,
            claude_bin: None,
        }
    }
}

impl LoginConfig {
    /// The whole-capture timeout, as a [`Duration`] — the bound the `login` verb threads into
    /// [`crate::login::login_account`].
    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// The usage-stats subsystem's settings (issue #161): the retention horizons the daemon
/// threads into the store's [`RetentionPolicy`] when it compacts + rolls the sample store
/// (issues #155/#156), plus the default reporting period for the offline `stats` verb (#158).
///
/// All keys are optional with documented, BOUNDED defaults, so a config with no `[stats]`
/// table (or none at all) uses [`StatsConfig::default`] — the same opt-out contract as the
/// other blocks. Nothing here is secret: horizons are plain durations and the period is a
/// fixed vocabulary token, never an account handle, email, or token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatsConfig {
    /// Seconds a raw sample is kept in the append-only raw tier before its whole aged-out
    /// day is folded into the hourly/daily aggregates (`3600..=31_536_000`, i.e. 1 h..365 d).
    /// The daemon carries this into [`RetentionPolicy::raw_window_secs`]. Larger = more
    /// point-in-time detail retained, at more on-disk raw lines.
    pub(crate) raw_retention_secs: u64,
    /// Seconds an hourly-aggregate bucket is kept before it is pruned (`86_400..=315_360_000`,
    /// i.e. 1 d..10 y) — [`RetentionPolicy::hourly_window_secs`]. The mid-resolution tier: long
    /// enough to chart a season, bounded so the rollup file cannot grow without limit.
    pub(crate) hourly_retention_secs: u64,
    /// Seconds a daily-aggregate bucket is kept, or **`0` for lifetime** (`0..=315_360_000`).
    /// The daily tier is kept for the store's whole lifetime BY DESIGN (a summarised day
    /// cannot be recovered once its raw samples age out), so `0` (the default) preserves that;
    /// a non-zero value opts into bounding it — [`RetentionPolicy::daily_window_secs`].
    pub(crate) daily_retention_secs: u64,
    /// The reporting period the offline `stats` verb selects when neither `--period` nor
    /// `--since` is given (issue #158): one of `day` | `week` | `month` | `lifetime`. Validated
    /// against that vocabulary at load ([`STATS_PERIODS`]) so a typo fails fast, not at run.
    pub(crate) default_period: String,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            raw_retention_secs: DEFAULT_STATS_RAW_RETENTION_SECS,
            hourly_retention_secs: DEFAULT_STATS_HOURLY_RETENTION_SECS,
            daily_retention_secs: DEFAULT_STATS_DAILY_RETENTION_SECS,
            default_period: DEFAULT_STATS_PERIOD.to_owned(),
        }
    }
}

impl StatsConfig {
    /// Build the store's [`RetentionPolicy`] from these horizons plus the daemon's poll
    /// cadence (issue #161). `poll_interval_secs` — the `[tunables].poll_secs` the daemon
    /// actually polls at — is the coverage denominator (observed ÷ expected samples per day),
    /// so it is threaded in here rather than duplicated in `[stats]`. The daily horizon's
    /// `0 = lifetime` sentinel passes straight through to
    /// [`RetentionPolicy::daily_window_secs`] (also `0 = lifetime`).
    pub(crate) fn retention_policy(&self, poll_interval_secs: i64) -> RetentionPolicy {
        RetentionPolicy {
            raw_window_secs: self.raw_retention_secs as i64,
            hourly_window_secs: self.hourly_retention_secs as i64,
            daily_window_secs: self.daily_retention_secs as i64,
            poll_interval_secs,
        }
    }
}

/// The import conflict policy (issue #150): what `import` does when an account carried in a
/// migration artifact is already present on the target roster. The operator-facing DEFAULT
/// lives in [`MigrationConfig::conflict_policy`]; the `--overwrite` CLI flag always forces
/// [`Overwrite`](ConflictPolicy::Overwrite). Non-secret — a bare policy classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ConflictPolicy {
    /// Leave an already-present account byte-for-byte untouched (its stash AND roster entry).
    /// The safe default — an import never clobbers an existing account unless opted in.
    #[default]
    Skip,
    /// Replace an already-present account's roster entry + credential stash from the artifact.
    Overwrite,
}

impl ConflictPolicy {
    /// The config token for this policy — the value [`Config::render`] writes and
    /// [`Config::validate`] parses back (the `skip` | `overwrite` vocabulary).
    fn as_str(self) -> &'static str {
        match self {
            ConflictPolicy::Skip => "skip",
            ConflictPolicy::Overwrite => "overwrite",
        }
    }
}

/// The migration subsystem's settings (issue #150): the Argon2id KDF cost used when WRITING an
/// encrypted `export` artifact, and the default `import` conflict policy. The operator-facing
/// tunables for the migration verbs, mirroring `[stats]` (#161) / `[refresh]` (#105) / `[login]`
/// (#135).
///
/// All keys are optional with documented, BOUNDED defaults, so a config with no `[migration]`
/// table (or none at all) uses [`MigrationConfig::default`] — the same opt-out contract as the
/// other blocks. Nothing here is secret: a KDF cost is a plain integer, a conflict policy a
/// fixed vocabulary token — never an account handle, email, or token. The KDF cost bounds sit
/// WITHIN migration.rs's decrypt-time cost guards, so an artifact written at any in-range cost
/// still decrypts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MigrationConfig {
    /// Argon2id memory cost (KiB) used when `export` writes an ENCRYPTED artifact
    /// (`8..=1_048_576`, i.e. 8 KiB..1 GiB). Higher = more resistant to offline brute-force, at
    /// more time + memory to encrypt AND decrypt. The cost is recorded IN each artifact, so
    /// changing it never breaks reading an already-written file (issue #146 forward-compat).
    pub(crate) kdf_memory_kib: u32,
    /// Argon2id time cost (iterations) used when `export` writes an encrypted artifact
    /// (`1..=16`). The second cost knob alongside `kdf_memory_kib`; higher = slower to derive.
    pub(crate) kdf_iterations: u32,
    /// The default `import` conflict policy when it runs WITHOUT `--overwrite`: an account
    /// already on the target is [`Skip`](ConflictPolicy::Skip)ped (the safe default) or
    /// [`Overwrite`](ConflictPolicy::Overwrite)n. `--overwrite` always forces overwrite.
    pub(crate) conflict_policy: ConflictPolicy,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            kdf_memory_kib: DEFAULT_MIGRATION_KDF_MEMORY_KIB,
            kdf_iterations: DEFAULT_MIGRATION_KDF_ITERATIONS,
            conflict_policy: ConflictPolicy::Skip,
        }
    }
}

impl MigrationConfig {
    /// The Argon2id cost this config directs `export` to derive an encrypted artifact's key at
    /// (issue #150) — the migration-subsystem analogue of [`StatsConfig::retention_policy`]. Only
    /// memory + time are operator-tunable; the lane count is fixed at the production
    /// [`MIGRATION_KDF_PARALLELISM`] (the `argon2` crate is single-threaded without its `parallel`
    /// feature, so exposing lanes would be a misleading knob). The default maps to exactly
    /// [`crate::migration::KdfCost::PRODUCTION`], kept so by `migration_kdf_defaults_match_the_crypto`.
    pub(crate) fn kdf_cost(&self) -> crate::migration::KdfCost {
        crate::migration::KdfCost {
            memory_kib: self.kdf_memory_kib,
            iterations: self.kdf_iterations,
            parallelism: MIGRATION_KDF_PARALLELISM,
        }
    }
}

/// The validated configuration: the captured roster plus tunables.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Captured accounts (unique `account_uuid`; no fixed upper bound —
    /// #35, and possibly empty — the daemon's "at least one" precondition is
    /// [`Config::require_roster`], not a parse-time rule, so `capture` can load a
    /// tunables-only file to add the first account). Consumed by the swap engine
    /// (#6 / #7) and by `list` / `status` (#17 / #9).
    #[allow(dead_code)]
    pub(crate) roster: Vec<Account>,
    /// Poll/swap tunables.
    pub(crate) tunables: Tunables,
    /// The periodic isolated-refresh schedule (issue #105); on by default (#409).
    pub(crate) refresh: RefreshConfig,
    /// The one-shot `login` verb's settings (issue #135): capture timeout + optional `claude`
    /// binary override. Consumed by `crate::capture::login`, never by the daemon.
    pub(crate) login: LoginConfig,
    /// The usage-stats subsystem's settings (issue #161): store retention horizons + the
    /// offline `stats` verb's default period. The daemon derives its [`RetentionPolicy`] from
    /// this ([`StatsConfig::retention_policy`]) to compact + roll the sample store.
    pub(crate) stats: StatsConfig,
    /// The migration subsystem's settings (issue #150): the Argon2id KDF cost `export` writes an
    /// encrypted artifact at ([`MigrationConfig::kdf_cost`]) and the default `import` conflict
    /// policy. Consumed by the `export` / `import` verbs, never by the daemon.
    pub(crate) migration: MigrationConfig,
}

/// Whether an effective config value came from `config.toml` or a compiled-in
/// default — the provenance `config show --origin` surfaces (issue #401). An
/// absent key (or a whole absent `[section]`) reads as [`Origin::Default`], so the
/// silently-defaulted drift that motivated #401 — an externally-deleted
/// `[tunables]` block — becomes visible instead of masquerading as intentional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The key (or its whole `[section]`) is absent from the file; the daemon
    /// silently substituted the compiled-in default.
    Default,
    /// The key is present in the file.
    FromFile,
}

/// One effective config value tagged with where it came from (issue #401). `value`
/// is pre-rendered exactly as it reads in `config.toml` — through the SAME
/// `basic_string` / `render_str_array` / `render_jitter` helpers as
/// [`Config::render`] — so the view and the file speak one syntax.
#[derive(Debug)]
pub(crate) struct OriginEntry {
    /// The TOML key name (e.g. `poll_secs`).
    pub(crate) key: &'static str,
    /// The effective value, pre-rendered as TOML.
    pub(crate) value: String,
    /// File-set or compiled-in default.
    pub(crate) origin: Origin,
}

/// One `[section]` of the origin report: its TOML header, whether that section
/// header was present in the file at all, and its keyed entries (issue #401).
#[derive(Debug)]
pub(crate) struct OriginSection {
    /// The section header exactly as it reads in the file, e.g. `[tunables]`.
    pub(crate) header: &'static str,
    /// Was the `[section]` header present in the file? A `false` here with every
    /// entry [`Origin::Default`] is the deleted-block signal #401 exists to show.
    pub(crate) present: bool,
    /// The section's keyed values, in `render` order.
    pub(crate) entries: Vec<OriginEntry>,
}

/// The effective config — sectioned and origin-tagged — plus a roster summary, the
/// read-only view `config show [--origin]` renders (issue #401). Produced by
/// [`Config::load_with_origin`]; consumed by the CLI, which only formats it.
#[derive(Debug)]
pub(crate) struct OriginReport {
    /// The tunable/optional-table blocks, in `render` order.
    pub(crate) sections: Vec<OriginSection>,
    /// How many accounts the effective roster holds.
    pub(crate) roster_count: usize,
    /// Was any `[[account]]` present in the file?
    pub(crate) roster_present: bool,
}

impl Config {
    /// Load and validate `config.toml` from its standard path.
    ///
    /// Returns [`Error::ConfigNotFound`] if the file is absent (the daemon has
    /// nothing to run until `capture` writes one), [`Error::ConfigParse`] /
    /// [`Error::ConfigInvalid`] / [`Error::ConfigTargetMaxSessionAboveTrigger`] for a file
    /// that exists but is malformed. Never silently substitutes defaults for a
    /// malformed file. A well-formed file with an *empty* roster loads
    /// successfully (tunables preserved) — the "at least one account" rule is the
    /// daemon's [`Config::require_roster`] precondition, so `capture` can load a
    /// tunables-only file to add the first account.
    pub(crate) fn load() -> Result<Self> {
        Self::load_path(&paths::config_file()?)
    }

    /// [`load`](Config::load) against an explicit path — the injectable seam, so
    /// the file-I/O branches (absent → [`Error::ConfigNotFound`], other read
    /// failure → [`Error::Io`]) are testable without touching the real config
    /// location. `pub(crate)` so [`capture`](crate::capture)'s `load_existing_from`
    /// routes through the same seam rather than re-implementing the read (#59).
    pub(crate) fn load_path(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Err(Error::ConfigNotFound {
                    path: path.to_path_buf(),
                });
            }
            Err(err) => return Err(Error::Io(err)),
        };
        Self::parse(&text)
    }

    /// Parse and validate a config from its rendered TOML TEXT (not a file) — the seam
    /// the `import` verb (issue #149) uses to read the roster + tunables carried verbatim
    /// inside a migration artifact's `config_toml` ([`crate::migration::Payload::config_toml`]).
    /// Mirrors [`load_path`](Config::load_path) minus the file read, funnelling through the
    /// same [`parse`](Config::parse) validation so an artifact's config is held to the
    /// identical invariants (unique non-empty `account_uuid`, tunable ranges).
    pub(crate) fn from_toml_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }

    /// Load the effective config AND classify every value's origin (file vs default),
    /// for the read-only `config show [--origin]` diagnostics verb (issue #401).
    ///
    /// Purely additive — it changes nothing about how the daemon loads or defaults
    /// config, and every error class matches [`load`](Config::load) exactly: the file
    /// read maps [`Error::ConfigNotFound`] / [`Error::Io`] just as
    /// [`load_path`](Config::load_path) does, and the SAME [`parse`](Config::parse) →
    /// [`validate`](Config::validate) seam maps [`Error::ConfigParse`] /
    /// [`Error::ConfigInvalid`] / [`Error::ConfigTargetMaxSessionAboveTrigger`]. It then re-reads
    /// the raw text into a permissive [`toml::Table`] PURELY to detect key presence,
    /// which the typed `#[serde(default)]` layer cannot report.
    pub(crate) fn load_with_origin(path: &Path) -> Result<OriginReport> {
        // Deliberately mirrors `load_path`'s read (absent → `ConfigNotFound`, other →
        // `Io`) rather than calling it — the raw text is needed twice (typed parse +
        // presence table), and this keeps the change additive to the daemon's load path.
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Err(Error::ConfigNotFound {
                    path: path.to_path_buf(),
                });
            }
            Err(err) => return Err(Error::Io(err)),
        };
        // The effective, validated config (defaults filled). Any parse/validate error
        // surfaces here first, byte-identically to what `load_path` would return.
        let config = Self::parse(&text)?;
        // A second, permissive parse into a raw table — key presence only. `parse`
        // above already accepted `text` under `deny_unknown_fields`, so this re-parse
        // of the same input cannot fail; map defensively regardless.
        let raw: toml::Table =
            toml::from_str(&text).map_err(|err| Error::ConfigParse(err.to_string()))?;
        Ok(config.origin_report(&raw))
    }

    /// Build the origin report from the effective config (`self`) and the raw TOML
    /// `table` (the presence source). Mirrors [`render`](Config::render)'s field walk —
    /// same sections, same order, same value formatting — but emits `(key, value,
    /// origin)` triples instead of persisted TOML. The schema's single source of truth
    /// stays with the structs here; the CLI only formats what this returns.
    fn origin_report(&self, table: &toml::Table) -> OriginReport {
        // Is `[section].key` present in the raw file? An absent section (or key) →
        // the value the effective config carries is a compiled-in default.
        let present = |section: &str, key: &str| -> Origin {
            match table
                .get(section)
                .and_then(toml::Value::as_table)
                .map(|t| t.contains_key(key))
            {
                Some(true) => Origin::FromFile,
                _ => Origin::Default,
            }
        };
        let entry =
            |key: &'static str, value: String, origin: Origin| OriginEntry { key, value, origin };

        let t = &self.tunables;
        let tunables = OriginSection {
            header: "[tunables]",
            present: table.contains_key("tunables"),
            entries: vec![
                entry(
                    "poll_secs",
                    t.poll_secs.to_string(),
                    present("tunables", "poll_secs"),
                ),
                entry(
                    "exhausted_poll_secs",
                    t.exhausted_poll_secs.to_string(),
                    present("tunables", "exhausted_poll_secs"),
                ),
                entry(
                    "near_limit_poll_secs",
                    t.near_limit_poll_secs.to_string(),
                    present("tunables", "near_limit_poll_secs"),
                ),
                entry(
                    "cooldown_secs",
                    t.cooldown_secs.to_string(),
                    present("tunables", "cooldown_secs"),
                ),
                entry(
                    "target_max_session_usage",
                    t.target_max_session_usage.to_string(),
                    present("tunables", "target_max_session_usage"),
                ),
                entry(
                    "session_trigger",
                    t.session_trigger.to_string(),
                    present("tunables", "session_trigger"),
                ),
                entry(
                    "weekly_trigger",
                    t.weekly_trigger.to_string(),
                    present("tunables", "weekly_trigger"),
                ),
                entry(
                    "session_blind_swap_secs",
                    t.session_blind_swap_secs.to_string(),
                    present("tunables", "session_blind_swap_secs"),
                ),
                entry(
                    "session_blind_risk_band",
                    t.session_blind_risk_band.to_string(),
                    present("tunables", "session_blind_risk_band"),
                ),
                entry(
                    "session_velocity_horizon_secs",
                    t.session_velocity_horizon_secs.to_string(),
                    present("tunables", "session_velocity_horizon_secs"),
                ),
                entry(
                    "session_velocity_min_project_above",
                    t.session_velocity_min_project_above.to_string(),
                    present("tunables", "session_velocity_min_project_above"),
                ),
                entry(
                    "session_velocity_ema_alpha_pct",
                    t.session_velocity_ema_alpha_pct.to_string(),
                    present("tunables", "session_velocity_ema_alpha_pct"),
                ),
                entry(
                    "monitor_401_n",
                    t.monitor_401_n.to_string(),
                    present("tunables", "monitor_401_n"),
                ),
                entry(
                    "monitor_recovery_m",
                    t.monitor_recovery_m.to_string(),
                    present("tunables", "monitor_recovery_m"),
                ),
            ],
        };

        let jitter = OriginSection {
            header: "[jitter]",
            present: table.contains_key("jitter"),
            entries: vec![
                entry(
                    "poll",
                    render_jitter(&t.poll_strategy.jitter),
                    present("jitter", "poll"),
                ),
                entry(
                    "trigger",
                    render_jitter(&t.trigger_strategy.jitter),
                    present("jitter", "trigger"),
                ),
                entry(
                    "weekly_trigger",
                    render_jitter(&t.weekly_trigger_strategy.jitter),
                    present("jitter", "weekly_trigger"),
                ),
                entry(
                    "cooldown",
                    render_jitter(&t.cooldown_strategy.jitter),
                    present("jitter", "cooldown"),
                ),
            ],
        };

        let r = &self.refresh;
        let refresh = OriginSection {
            header: "[refresh]",
            present: table.contains_key("refresh"),
            entries: vec![
                entry(
                    "enabled",
                    r.enabled.to_string(),
                    present("refresh", "enabled"),
                ),
                entry(
                    "accounts",
                    render_str_array(&r.accounts),
                    present("refresh", "accounts"),
                ),
                entry(
                    "cadence_secs",
                    r.cadence_secs.to_string(),
                    present("refresh", "cadence_secs"),
                ),
                entry(
                    "idle_after_secs",
                    r.idle_after_secs.to_string(),
                    present("refresh", "idle_after_secs"),
                ),
                entry(
                    "timeout_secs",
                    r.timeout_secs.to_string(),
                    present("refresh", "timeout_secs"),
                ),
                entry(
                    "systemic_failure_n",
                    r.systemic_failure_n.to_string(),
                    present("refresh", "systemic_failure_n"),
                ),
                entry(
                    "proactive_keep_warm",
                    r.proactive_keep_warm.to_string(),
                    present("refresh", "proactive_keep_warm"),
                ),
                entry(
                    "claude_bin",
                    render_optional_bin(&r.claude_bin),
                    present("refresh", "claude_bin"),
                ),
            ],
        };

        let l = &self.login;
        let login = OriginSection {
            header: "[login]",
            present: table.contains_key("login"),
            entries: vec![
                entry(
                    "timeout_secs",
                    l.timeout_secs.to_string(),
                    present("login", "timeout_secs"),
                ),
                entry(
                    "claude_bin",
                    render_optional_bin(&l.claude_bin),
                    present("login", "claude_bin"),
                ),
            ],
        };

        let s = &self.stats;
        let stats = OriginSection {
            header: "[stats]",
            present: table.contains_key("stats"),
            entries: vec![
                entry(
                    "raw_retention_secs",
                    s.raw_retention_secs.to_string(),
                    present("stats", "raw_retention_secs"),
                ),
                entry(
                    "hourly_retention_secs",
                    s.hourly_retention_secs.to_string(),
                    present("stats", "hourly_retention_secs"),
                ),
                entry(
                    "daily_retention_secs",
                    s.daily_retention_secs.to_string(),
                    present("stats", "daily_retention_secs"),
                ),
                entry(
                    "default_period",
                    basic_string(&s.default_period),
                    present("stats", "default_period"),
                ),
            ],
        };

        let mi = &self.migration;
        let migration = OriginSection {
            header: "[migration]",
            present: table.contains_key("migration"),
            entries: vec![
                entry(
                    "kdf_memory_kib",
                    mi.kdf_memory_kib.to_string(),
                    present("migration", "kdf_memory_kib"),
                ),
                entry(
                    "kdf_iterations",
                    mi.kdf_iterations.to_string(),
                    present("migration", "kdf_iterations"),
                ),
                entry(
                    "conflict_policy",
                    basic_string(mi.conflict_policy.as_str()),
                    present("migration", "conflict_policy"),
                ),
            ],
        };

        OriginReport {
            sections: vec![tunables, jitter, refresh, login, stats, migration],
            roster_count: self.roster.len(),
            // The roster is the `[[account]]` array-of-tables (RawConfig's `account`).
            roster_present: table.contains_key("account"),
        }
    }

    /// Persist this config to the canonical `config.toml` (`0600`, parent `0700`), with the
    /// inline tunable-documenting comments. The write path for the standalone `capture` (#4).
    #[allow(dead_code)]
    pub(crate) fn save(&self) -> Result<()> {
        self.save_to(&paths::config_file()?)
    }

    /// Persist this config to an EXPLICIT `path` (`0600`, parent `0700`) — the injectable-path
    /// write seam, the counterpart of [`load_path`](Config::load_path). The daemon-routed
    /// `cmd:capture` (#359) writes back through its wired `config_path` (so a hermetic test lands
    /// the new roster in a temp file, not the real support dir), exactly as [`save`](Config::save)
    /// writes the canonical location for the standalone `capture` (#4).
    pub(crate) fn save_to(&self, path: &Path) -> Result<()> {
        paths::ensure_private_dir(
            path.parent()
                .expect("a config path always has a parent directory"),
        )?;
        paths::write_private_file(path, self.render().as_bytes())
    }

    /// Apply a `config-set` control command's edits (issue #268) to the config `text`
    /// read from disk, re-validating the WHOLE result through the same
    /// [`validate`](Config::validate) that [`load`](Config::load) runs — so every range
    /// and cross-field rule (`target_max_session_usage <= session_trigger`,
    /// `exhausted_poll_secs >= poll_secs`, the `near_limit_poll_secs` 0-or-band shape, …)
    /// is enforced atomically over the FINAL state. An invalid batch is rejected with
    /// nothing written; a batch is valid iff its resulting config is (an individually
    /// out-of-order pair — e.g. raising `poll_secs` past the old `exhausted_poll_secs`
    /// while also raising the latter — validates because both land before the check).
    ///
    /// `tunables` carries only the scalar `[tunables]` edits the settings UI may make
    /// ([`SetTunables`] is the allow-list — a credential, an `[[account]]`, or any other
    /// key is unrepresentable there). `labels` maps `account_uuid` → a new label; a uuid
    /// matching no roster account is [`Error::AccountUuidNotFound`]. ONLY an existing
    /// account's `label` is touched — the roster is never grown, shrunk, or re-keyed, and
    /// no credential is reachable (the #268 safety boundary).
    ///
    /// Returns the re-validated [`Config`] to persist (via [`save_to`](Config::save_to))
    /// plus a [`SettingsChange`] recording which classes actually changed, so the daemon
    /// picks the reload semantics: a tunable change is reload-by-restart (the daemon
    /// derives its strategy fields once at construction), a label change adopts live.
    pub(crate) fn apply_settings(
        text: &str,
        tunables: &SetTunables,
        labels: &BTreeMap<String, String>,
    ) -> Result<(Config, SettingsChange)> {
        // Baseline: the current on-disk config, fully validated. A currently-invalid file
        // (hand-broken) fails HERE, so config-set refuses rather than overwrite a file it
        // cannot understand — the daemon maps this to a `config-unreadable` rejection.
        let before = Config::parse(text)?;
        // Overlay the edits onto the raw layer so the SINGLE validate() sees the final
        // state; the file is re-parsed (tiny) rather than cloning the non-`Clone` raw.
        let mut raw: RawConfig =
            toml::from_str(text).map_err(|err| Error::ConfigParse(err.to_string()))?;
        overlay_tunables(&mut raw.tunables, tunables);
        overlay_labels(&mut raw.account, labels)?;
        let after = Config::validate(raw)?;
        let change = SettingsChange {
            tunables_changed: after.tunables != before.tunables,
            labels_changed: after.roster != before.roster,
        };
        Ok((after, change))
    }

    /// A non-secret projection of the effective config for the `config-get` control
    /// command (issue #268): the scalar tunables the settings UI edits + each roster
    /// account's non-secret `account_uuid` / `label` / `enabled`. Carries NO credential
    /// (the roster keys on uuid + label only, issue #15), so it is exactly as safe to
    /// return over the same-user control socket as the `status` / `watch` snapshots.
    pub(crate) fn view(&self) -> ConfigView {
        ConfigView {
            tunables: TunablesView::from(&self.tunables),
            accounts: self
                .roster
                .iter()
                .map(|account| AccountView {
                    account_uuid: account.account_uuid.clone(),
                    label: account.label.clone(),
                    enabled: account.enabled,
                })
                .collect(),
        }
    }

    /// Parse `text` and project it to a [`ConfigView`] (issue #268) — the `config-get` read path's
    /// one-call text→view seam, keeping [`parse`](Config::parse) private while giving the daemon a
    /// non-secret projection to serialize. Errors exactly as [`load`](Config::load) would (a parse or
    /// validation failure), which `config-get` maps to a `config unreadable` envelope.
    pub(crate) fn view_from_text(text: &str) -> Result<ConfigView> {
        Ok(Config::parse(text)?.view())
    }

    /// The base poll interval — the un-jittered `poll_secs`. The run loop now
    /// draws a jittered interval each cycle from the poll strategy (issue #38),
    /// so this is a tested accessor for the base rather than the live cadence.
    #[allow(dead_code)]
    pub(crate) fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.tunables.poll_secs)
    }

    /// The usage fraction in `[0.0, 1.0]` at or above which the active account
    /// is considered exhausted — `session_trigger` as a fraction.
    ///
    /// The daemon derives its own trigger / floor / cooldown uniformly from
    /// [`Tunables`] (issue #7), so this Config-level accessor is currently a
    /// tested seam for the `status` view (#9) rather than the run loop.
    #[allow(dead_code)]
    pub(crate) fn swap_threshold(&self) -> f64 {
        f64::from(self.tunables.session_trigger) / 100.0
    }

    /// Ensure the roster holds at least one account — the daemon's precondition.
    ///
    /// The non-empty-roster invariant belongs to the *daemon* (`run` has nothing to
    /// rotate across with an empty roster), not to parsing: `capture` and the
    /// roster-editing commands must load a possibly-empty config precisely to
    /// populate it (a brand-new tunables-only file, or one whose last account was
    /// just `remove`d). Enforcing it here — at the one consumer that requires it —
    /// lets `capture` bootstrap the first account while `run` still refuses to start
    /// on an empty roster. Maps to the friendly [`Error::RosterEmpty`], the same
    /// empty-state the offline `list` view reports.
    pub(crate) fn require_roster(&self) -> Result<()> {
        if self.roster.is_empty() {
            Err(Error::RosterEmpty)
        } else {
            Ok(())
        }
    }

    /// Stage one: deserialize TOML into the permissive raw form, then validate.
    /// Pure (no filesystem) so the whole parse-and-validate policy is testable
    /// without touching real paths.
    fn parse(text: &str) -> Result<Self> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|err| Error::ConfigParse(err.to_string()))?;
        Self::validate(raw)
    }

    /// Stage two: bounds-check every tunable and the roster, producing the typed
    /// `Config`. Each rejection names the offending field; the cross-field rule
    /// (`target_max_session_usage <= session_trigger`) gets its own distinct error.
    fn validate(raw: RawConfig) -> Result<Self> {
        let t = raw.tunables;

        range("session_trigger", t.session_trigger, 50, 99)?;
        // The weekly trigger is independent of the session trigger (issue #41):
        // its own 50..=99 bound, with NO cross-field rule — weekly may sit below
        // session (an unusual but valid operator choice), so both are configurable
        // independently (AC #3).
        range("weekly_trigger", t.weekly_trigger, 50, 99)?;
        // Issue #452 (ADR-0017) bounded-blindness preemptive-swap gate. `session_blind_swap_secs`
        // is `T` in seconds: floored at 60 (at least one poll cycle blind) and capped at 86400 — a
        // 24 h ceiling far beyond any real blind window, so setting it there disables the path (the
        // config kill-switch). `session_blind_risk_band` is a session percent, 50..=99 like the
        // triggers but conventionally set BELOW `session_trigger` (the gate fires preemptively on a
        // stale anchor). No NEW cross-field: ADR-0017's `target_max_session_usage <= session_trigger`
        // is the existing reserve invariant enforced below — the blind path's target still needs
        // runway below its own reactive trigger, which that invariant already guarantees.
        range(
            "session_blind_swap_secs",
            t.session_blind_swap_secs,
            60,
            86_400,
        )?;
        range("session_blind_risk_band", t.session_blind_risk_band, 50, 99)?;
        // Issue #539 (ADR-0017) velocity-projection preemptive trigger. `session_velocity_horizon_secs`
        // is the projection horizon `H` in seconds: `0..=600`, where `0` disables the path (the
        // projection reduces to `last`, which — the reactive path having already held — never crosses,
        // the config kill-switch), and 600 is a sanity ceiling (the #538 spike validated H ≈ 120 and
        // showed over-fire creeping in above ~150, so a large H is a foot-gun the false-projection SLI
        // surfaces; the ceiling just bounds the absurd). `session_velocity_min_project_above` is a
        // session percent (`50..=99`) conventionally set BELOW `session_trigger` — the projective peer
        // fires in the band beneath the reactive trigger — exactly like `session_blind_risk_band`, so no
        // NEW cross-field. `session_velocity_ema_alpha_pct` is the EMA weight α (`1..=100`); the `0`
        // floor is excluded because α=0 would freeze the EMA (never integrate a new sample), a
        // degenerate value, while 100 is the valid "no smoothing" (raw last-interval) end.
        range(
            "session_velocity_horizon_secs",
            t.session_velocity_horizon_secs,
            0,
            600,
        )?;
        range(
            "session_velocity_min_project_above",
            t.session_velocity_min_project_above,
            50,
            99,
        )?;
        range(
            "session_velocity_ema_alpha_pct",
            t.session_velocity_ema_alpha_pct,
            1,
            100,
        )?;
        // target_max_session_usage is default-on (#398): absent → DEFAULT_TARGET_MAX_SESSION_USAGE, clamped
        // down to session_trigger so the default honors the SAME
        // `target_max_session_usage <= session_trigger` invariant the present-value arm enforces
        // (#417 — without the clamp a `session_trigger < 80` config loads with an
        // unchecked reserve of 80 and then bricks after a render→parse round-trip, since
        // #398 renders the default as a live line; an equal reserve is inert per
        // ADR-0013). When present, its lower bound is 1 (an explicit 0 admits no
        // target, silently disabling proactive swapping) and its upper bound is
        // session_trigger (a higher reserve could never admit a target), the latter a
        // distinct cross-field error.
        let target_max_session_usage = match t.target_max_session_usage {
            None => DEFAULT_TARGET_MAX_SESSION_USAGE.min(t.session_trigger as u8),
            Some(value) => {
                if value == 0 {
                    // The swap predicate is `usage.session < target_max_session_usage`, so 0
                    // admits NO account and silently disables proactive swapping (the
                    // daemon just holds). 0 is the natural wrong guess for "no
                    // restriction" — its exact opposite (#414) — so reject it with the
                    // remedy spelled out, rather than let a live, hand-editable line
                    // brick swapping in silence.
                    return Err(Error::ConfigInvalid(format!(
                        "target_max_session_usage = 0 admits no swap target and silently disables \
                         proactive swapping; it must be in 1..={}. Raise it toward \
                         session_trigger to admit more targets.",
                        t.session_trigger
                    )));
                }
                if value < 0 {
                    return Err(Error::ConfigInvalid(format!(
                        "target_max_session_usage must be in 1..={}, got {value}",
                        t.session_trigger
                    )));
                }
                if value > t.session_trigger {
                    return Err(Error::ConfigTargetMaxSessionAboveTrigger {
                        target_max_session_usage: value,
                        trigger: t.session_trigger,
                    });
                }
                value as u8
            }
        };
        range("poll_secs", t.poll_secs, 5, 3600)?;
        // The widened exhausted-peer cadence (issue #537) is bounded BELOW by `poll_secs` (a
        // cross-field rule, checked after `poll_secs` above so the bound is the validated
        // value): a slow-polled peer must never re-poll FASTER than the normal cadence — an
        // `exhausted_poll_secs < poll_secs` would defeat the whole point (poll MORE often, not
        // less). The 86400 s (24 h) ceiling is a sanity bound far beyond any real quota window.
        // The lower bound is dynamic, so `range` cannot express it — spell the cross-field
        // remedy out, mirroring `target_max_session_usage`'s message.
        if !(t.poll_secs..=86_400).contains(&t.exhausted_poll_secs) {
            return Err(Error::ConfigInvalid(format!(
                "exhausted_poll_secs must be in {}..=86400 (>= poll_secs so a slow-polled \
                 exhausted peer never re-polls faster than the normal cadence), got {}",
                t.poll_secs, t.exhausted_poll_secs
            )));
        }
        // The near-limit active-poll sub-interval cap (issue #540). `0` disables the path (the
        // kill-switch, like `session_velocity_horizon_secs` below); a non-zero value is a poll
        // cadence in the SAME `5..=3600` s band as `poll_secs` (a sub-5 s cadence sits below the
        // daemon's own poll floor). Deliberately NOT cross-fielded to `poll_secs`: the daemon
        // applies it as `min(poll_secs / N, near_limit_poll_secs)`, so a value ABOVE the base
        // sub-interval is simply inert (the `min` never binds) rather than an error — a lowered
        // `poll_secs` whose base already sits below this cap just leaves #540 inert, which is
        // correct (the steady cadence is already tight). `range` cannot express the `0`-or-band
        // shape, so spell it out, mirroring the `exhausted_poll_secs` message above.
        if t.near_limit_poll_secs != 0 && !(5..=3600).contains(&t.near_limit_poll_secs) {
            return Err(Error::ConfigInvalid(format!(
                "near_limit_poll_secs must be 0 (disabled) or in 5..=3600, got {}",
                t.near_limit_poll_secs
            )));
        }
        // cooldown_secs has a NON-ZERO floor (issue #272): it is configurable ABOVE
        // COOLDOWN_SECS_FLOOR but not below it, so swap pacing can never be tuned down
        // to zero. The daemon's per-cycle draw clamps to the same floor, so a jitter
        // spread cannot bypass it either.
        range(
            "cooldown_secs",
            t.cooldown_secs,
            COOLDOWN_SECS_FLOOR as i64,
            3600,
        )?;
        range("monitor_401_n", t.monitor_401_n, 1, 20)?;
        range("monitor_recovery_m", t.monitor_recovery_m, 1, 20)?;

        // Jitter specs (issue #38): each optional and validated to a clear load
        // error (parse-or-error). Poll jitters normally by default; trigger and
        // cooldown are fixed unless the operator configures a strategy.
        let poll_jitter = parse_jitter("poll", raw.jitter.poll, default_poll_jitter())?;
        let trigger_jitter = parse_jitter("trigger", raw.jitter.trigger, Jitter::None)?;
        let weekly_trigger_jitter =
            parse_jitter("weekly_trigger", raw.jitter.weekly_trigger, Jitter::None)?;
        let cooldown_jitter = parse_jitter("cooldown", raw.jitter.cooldown, Jitter::None)?;

        // Ranges are checked above, so these narrowing casts cannot truncate. The
        // strategy bases are the same validated scalars (issue #38): the daemon
        // draws + clamps from the strategy each cycle.
        let tunables = Tunables {
            poll_secs: t.poll_secs as u64,
            exhausted_poll_secs: t.exhausted_poll_secs as u64,
            near_limit_poll_secs: t.near_limit_poll_secs as u64,
            cooldown_secs: t.cooldown_secs as u64,
            target_max_session_usage,
            session_trigger: t.session_trigger as u8,
            weekly_trigger: t.weekly_trigger as u8,
            session_blind_swap_secs: t.session_blind_swap_secs as u64,
            session_blind_risk_band: t.session_blind_risk_band as u8,
            session_velocity_horizon_secs: t.session_velocity_horizon_secs as u64,
            session_velocity_min_project_above: t.session_velocity_min_project_above as u8,
            session_velocity_ema_alpha_pct: t.session_velocity_ema_alpha_pct as u8,
            monitor_401_n: t.monitor_401_n as u8,
            monitor_recovery_m: t.monitor_recovery_m as u8,
            poll_strategy: Strategy {
                base: t.poll_secs as f64,
                jitter: poll_jitter,
            },
            trigger_strategy: Strategy {
                base: t.session_trigger as f64,
                jitter: trigger_jitter,
            },
            weekly_trigger_strategy: Strategy {
                base: t.weekly_trigger as f64,
                jitter: weekly_trigger_jitter,
            },
            cooldown_strategy: Strategy {
                base: t.cooldown_secs as f64,
                jitter: cooldown_jitter,
            },
        };

        // The roster has neither a lower nor an upper bound at PARSE time. An empty
        // roster is a valid intermediate state — a fresh tunables-only file, or one
        // whose last account was just `remove`d — and `capture` must be able to load
        // such a file to add the first account (otherwise it can never bootstrap).
        // The "at least one account" rule is the DAEMON's precondition, enforced by
        // its consumer via [`Config::require_roster`] (called from `run`), NOT here.
        // And there is deliberately no upper bound: the operator rotates across as
        // many accounts as they capture (#35).
        //
        // Poll-cost note (document, don't cap): the daemon polls every roster
        // account with its own `curl` each `poll_secs` tick (see
        // `daemon::Daemon::tick`), so a larger roster grows per-tick work and
        // outbound request volume linearly. The operator self-limits by choice
        // (smaller roster, or a larger `poll_secs`); the tool enforces no ceiling.
        // Uniqueness keys on `account_uuid` alone: the stash is derived from it
        // ([`Account::stash`]), so distinct uuids imply distinct stashes and a
        // non-empty uuid implies a non-empty stash — the former empty-/duplicate-
        // stash checks are now redundant (issue #70).
        let mut uuids = HashSet::new();
        let mut roster = Vec::with_capacity(raw.account.len());
        for account in raw.account {
            if account.account_uuid.trim().is_empty() {
                return Err(Error::ConfigInvalid(
                    "account_uuid must not be empty".into(),
                ));
            }
            if account.label.trim().is_empty() {
                return Err(Error::ConfigInvalid("label must not be empty".into()));
            }
            if !uuids.insert(account.account_uuid.clone()) {
                return Err(Error::ConfigInvalid(format!(
                    "duplicate account_uuid: {}",
                    account.account_uuid
                )));
            }
            roster.push(Account {
                account_uuid: account.account_uuid,
                label: account.label,
                enabled: account.enabled,
            });
        }

        // The periodic isolated-refresh schedule (issue #105). Bounds-checked like the
        // tunables; `enabled` / `accounts` / `claude_bin` are free-form (a bad `claude_bin`
        // surfaces at spawn-resolution time, an unmatched `accounts` entry at selection).
        // An empty/whitespace `claude_bin` collapses to `None` — same as omitting it — so a
        // stray `claude_bin = ""` defers to `$CLAUDE_BIN`/`$PATH` rather than erroring.
        let r = raw.refresh;
        range("refresh.cadence_secs", r.cadence_secs, 60, 86_400)?;
        range("refresh.idle_after_secs", r.idle_after_secs, 0, 3_600)?;
        range("refresh.timeout_secs", r.timeout_secs, 10, 600)?;
        range("refresh.systemic_failure_n", r.systemic_failure_n, 1, 100)?;
        let refresh = RefreshConfig {
            enabled: r.enabled,
            accounts: r.accounts,
            cadence_secs: r.cadence_secs as u64,
            idle_after_secs: r.idle_after_secs as u64,
            timeout_secs: r.timeout_secs as u64,
            claude_bin: r
                .claude_bin
                .filter(|bin| !bin.trim().is_empty())
                .map(PathBuf::from),
            systemic_failure_n: r.systemic_failure_n as u32,
            proactive_keep_warm: r.proactive_keep_warm,
        };

        // The one-shot `login` verb's settings (issue #135). The timeout is bounds-checked like the
        // refresh timeout; `claude_bin` is free-form and an empty/whitespace value collapses to
        // `None` — the SAME override-resolver contract as `[refresh].claude_bin` (a bad path
        // surfaces at spawn-resolution time, never here).
        let l = raw.login;
        range("login.timeout_secs", l.timeout_secs, 60, 600)?;
        let login = LoginConfig {
            timeout_secs: l.timeout_secs as u64,
            claude_bin: l
                .claude_bin
                .filter(|bin| !bin.trim().is_empty())
                .map(PathBuf::from),
        };

        // The usage-stats subsystem's settings (issue #161). Each retention horizon is
        // bounds-checked like the tunables; the daily horizon's lower bound is 0 (its
        // lifetime sentinel). `default_period` is validated against the fixed `stats`
        // vocabulary so a typo fails at load, not at `stats`-run. No cross-field rules.
        let s = raw.stats;
        range(
            "stats.raw_retention_secs",
            s.raw_retention_secs,
            3_600,
            31_536_000,
        )?;
        range(
            "stats.hourly_retention_secs",
            s.hourly_retention_secs,
            86_400,
            315_360_000,
        )?;
        range(
            "stats.daily_retention_secs",
            s.daily_retention_secs,
            0,
            315_360_000,
        )?;
        if !STATS_PERIODS.contains(&s.default_period.as_str()) {
            return Err(Error::ConfigInvalid(format!(
                "stats.default_period must be one of {STATS_PERIODS:?}, got {:?}",
                s.default_period
            )));
        }
        let stats = StatsConfig {
            raw_retention_secs: s.raw_retention_secs as u64,
            hourly_retention_secs: s.hourly_retention_secs as u64,
            daily_retention_secs: s.daily_retention_secs as u64,
            default_period: s.default_period,
        };

        // The migration subsystem's settings (issue #150). The KDF cost knobs are bounds-checked
        // to sit WITHIN migration.rs's decrypt-time cost guards (memory `> 1<<20`, iterations
        // `> 16`), so an artifact written at any in-range cost still decrypts. The conflict policy
        // is validated against its fixed `skip|overwrite` vocabulary so a typo fails at load, not
        // at import-run. No cross-field rules — the lane count is fixed at production (not a key).
        let m = raw.migration;
        range("migration.kdf_memory_kib", m.kdf_memory_kib, 8, 1_048_576)?;
        range("migration.kdf_iterations", m.kdf_iterations, 1, 16)?;
        let conflict_policy = match m.conflict_policy.as_str() {
            "skip" => ConflictPolicy::Skip,
            "overwrite" => ConflictPolicy::Overwrite,
            _ => {
                return Err(Error::ConfigInvalid(format!(
                    "migration.conflict_policy must be one of {MIGRATION_CONFLICT_POLICIES:?}, got {:?}",
                    m.conflict_policy
                )));
            }
        };
        let migration = MigrationConfig {
            kdf_memory_kib: m.kdf_memory_kib as u32,
            kdf_iterations: m.kdf_iterations as u32,
            conflict_policy,
        };

        Ok(Config {
            roster,
            tunables,
            refresh,
            login,
            stats,
            migration,
        })
    }

    /// Render the config back to TOML with the inline tunable-documenting
    /// comments (issue #3 N2). Emitted by hand *by design* (issue #181, ADR-0005):
    /// `serde` serialization cannot emit comments at all, and `toml_edit` (not a
    /// current dependency) would still hand-author every comment as node decor and
    /// re-express the OFF-state opt-ins as injected text — for more ceremony and a
    /// new direct dep. So the file is rendered by hand; integers need no escaping
    /// and roster strings go through [`basic_string`].
    ///
    /// `pub(crate)` so the `export` verb (issue #148) can serialize the canonical
    /// config text into a migration artifact ([`crate::migration::Payload`]).
    pub(crate) fn render(&self) -> String {
        let t = &self.tunables;
        let mut out = String::new();
        out.push_str("# sessiometer configuration.\n");
        out.push_str(
            "# The roster is managed by `sessiometer capture`; the [tunables] block is\n\
             # safe to hand-edit. Percentages are of the rolling session window.\n\n",
        );

        out.push_str("[tunables]\n");
        out.push_str(
            "# Seconds between re-polling a given account (5..=3600) — the per-account\n\
             # cadence. The default 300 (5 min) plus the normal `poll` jitter below\n\
             # decorrelates cycles; the daemon staggers the roster within it, polling one\n\
             # account per poll_secs/N sub-interval so requests do not burst all at once.\n\
             # Under sustained 429/5xx it backs off automatically — widening this and\n\
             # honouring any Retry-After — instead of re-polling at the fixed interval.\n",
        );
        out.push_str(&format!("poll_secs = {}\n", t.poll_secs));
        out.push_str(
            "# Widened re-poll cadence (poll_secs..=86400) for an out-of-rotation peer — one\n\
             # that is weekly- or session-exhausted (issue #537). Its usage can only change\n\
             # when its server-side window resets (a time the daemon already knows) or on a\n\
             # rare out-of-band reset, so re-polling it every poll_secs wastes a request. The\n\
             # default 3600 (1 h) is the ceiling; a known resets_at sooner than this polls\n\
             # earlier. The ACTIVE account is never slow-polled. Must be >= poll_secs.\n",
        );
        out.push_str(&format!(
            "exhausted_poll_secs = {}\n",
            t.exhausted_poll_secs
        ));
        out.push_str(
            "# Tightened poll sub-interval (0 to disable, else 5..=3600) for the ACTIVE account\n\
             # while it is near its limit (issue #540) — the mirror of exhausted_poll_secs, which\n\
             # WIDENS an idle peer. On the active account's final climb its cadence tightens to\n\
             # this so no long poll gap opens near the limit; below the near-limit band the cadence\n\
             # is the unchanged poll_secs/N, so the steady rate is flat. Default 60. Applied as\n\
             # min(poll_secs/N, this), so a value above the base sub-interval is inert.\n",
        );
        out.push_str(&format!(
            "near_limit_poll_secs = {}\n",
            t.near_limit_poll_secs
        ));
        out.push_str(&format!(
            "# Seconds to wait after a swap before another swap is allowed \
             ({COOLDOWN_SECS_FLOOR}..=3600; a non-zero floor — pacing can't be disabled to zero).\n"
        ));
        out.push_str(&format!("cooldown_secs = {}\n", t.cooldown_secs));
        out.push_str(
            "# The most-full an account may be to receive the active session: only swap\n\
             # TO an account whose session usage is below this percent (1..=session_trigger).\n\
             # This is NOT the level that triggers a swap. Default-on (#398); 0 is rejected\n\
             # — it admits no target and would disable proactive swapping.\n",
        );
        out.push_str(&format!(
            "target_max_session_usage = {}\n",
            t.target_max_session_usage
        ));
        out.push_str(
            "# Swap AWAY from the active account at or above this session percent (50..=99).\n",
        );
        out.push_str(&format!("session_trigger = {}\n", t.session_trigger));
        out.push_str(
            "# Swap AWAY from the active account at or above this WEEKLY percent (50..=99).\n\
             # Independent of session_trigger (typically higher): a swap fires when EITHER\n\
             # dimension reaches its own trigger.\n",
        );
        out.push_str(&format!("weekly_trigger = {}\n", t.weekly_trigger));
        out.push_str(
            "# Bounded-blindness preemptive swap (issue #452, ADR-0017): when the active\n\
             # account's usage poll stays blind (429/5xx) longer than this many seconds AND\n\
             # its last good reading was at/over session_blind_risk_band, swap it away before\n\
             # it can self-exhaust unobserved. Floor 60; set to the 86400 ceiling to disable.\n",
        );
        out.push_str(&format!(
            "session_blind_swap_secs = {}\n",
            t.session_blind_swap_secs
        ));
        out.push_str(
            "# The last-known session percent (50..=99) at/over which a blind active account\n\
             # is eligible for the preemptive swap above. Set BELOW session_trigger — it acts\n\
             # on the stale pre-blind reading, before the reactive trigger would fire.\n",
        );
        out.push_str(&format!(
            "session_blind_risk_band = {}\n",
            t.session_blind_risk_band
        ));
        out.push_str(
            "# Velocity-projection preemptive swap (issue #539, ADR-0017): swap the active\n\
             # account away when its PROJECTED session usage (last + velocity * H) crosses the\n\
             # trigger before the observed reading does — H is this horizon in seconds\n\
             # (~ the active poll cadence; 120 validated by #538). Set to 0 to disable.\n",
        );
        out.push_str(&format!(
            "session_velocity_horizon_secs = {}\n",
            t.session_velocity_horizon_secs
        ));
        out.push_str(
            "# Only project when the observed session percent (50..=99) is at/over this — the\n\
             # projection can't reach lower anyway, so it is a free guard. Set BELOW\n\
             # session_trigger (the projective peer fires in the band beneath it).\n",
        );
        out.push_str(&format!(
            "session_velocity_min_project_above = {}\n",
            t.session_velocity_min_project_above
        ));
        out.push_str(
            "# EMA smoothing weight alpha (1..=100 percent) for the session-velocity signal,\n\
             # to damp a single-interval spike so the projection keys off sustained motion.\n\
             # ~50 validated by #538; 100 means no smoothing (raw last-interval velocity).\n",
        );
        out.push_str(&format!(
            "session_velocity_ema_alpha_pct = {}\n",
            t.session_velocity_ema_alpha_pct
        ));
        out.push_str(
            "# Consecutive non-scope 401s before an account is treated as DEAD and\n\
             # quarantined (1..=20).\n",
        );
        out.push_str(&format!("monitor_401_n = {}\n", t.monitor_401_n));
        out.push_str(
            "# Consecutive recovery-probe successes before a quarantined (dead) account\n\
             # whose own token starts working again (without a re-login) is restored to\n\
             # the rotation (1..=20). A re-login restores it immediately.\n",
        );
        out.push_str(&format!("monitor_recovery_m = {}\n", t.monitor_recovery_m));

        // Per-cycle timing jitter (issue #38): drawn each cycle and clamped to the
        // tunable's valid range, to decorrelate polling/swaps across cycles.
        out.push_str("\n[jitter]\n");
        out.push_str(
            "# Randomization drawn each cycle and clamped to the tunable's range.\n\
             # kind = \"none\" | \"uniform\" (with `spread`) | \"normal\" (with `stddev`).\n\
             # poll defaults to normal jitter (stddev ~20% of poll_secs) so accounts\n\
             # decorrelate; trigger, weekly_trigger and cooldown default to none.\n",
        );
        out.push_str(&format!(
            "poll = {}\n",
            render_jitter(&t.poll_strategy.jitter)
        ));
        out.push_str(&format!(
            "trigger = {}\n",
            render_jitter(&t.trigger_strategy.jitter)
        ));
        out.push_str(&format!(
            "weekly_trigger = {}\n",
            render_jitter(&t.weekly_trigger_strategy.jitter)
        ));
        out.push_str(&format!(
            "cooldown = {}\n",
            render_jitter(&t.cooldown_strategy.jitter)
        ));

        // The periodic isolated-refresh schedule (issue #105). ON by default (#409): each refresh
        // slides the stored expiry forward and re-stashes any rotated token, so it is self-sustaining
        // (#101's TTL question is resolved — a sliding window, not a fixed cap).
        let r = &self.refresh;
        out.push_str("\n[refresh]\n");
        out.push_str(
            "# Periodically let Claude Code refresh PARKED accounts' stored tokens in an\n\
             # isolated config dir (the in-daemon counterpart of `poke`), off the\n\
             # poll/usage/swap seam — the live session's credential is never touched. The\n\
             # active account and the imminent swap target are always excluded. ON by\n\
             # default: each refresh slides the stored token's expiry forward and re-stashes\n\
             # any rotated refresh token, so the schedule is self-sustaining. Set enabled\n\
             # = false to turn it off.\n",
        );
        out.push_str(&format!("enabled = {}\n", r.enabled));
        out.push_str(
            "# Parked accounts to keep fresh, by `list` label or account-uuid. Empty = all\n\
             # parked accounts (the near-expiry horizon still applies to each).\n",
        );
        out.push_str(&format!("accounts = {}\n", render_str_array(&r.accounts)));
        out.push_str(
            "# Seconds between refresh ticks AND the near-expiry horizon (60..=86400): an\n\
             # account is refreshed when its stored token would expire within one cadence\n\
             # (i.e. before the next tick). A conservative one-hour default.\n",
        );
        out.push_str(&format!("cadence_secs = {}\n", r.cadence_secs));
        out.push_str(
            "# Seconds the daemon must idle before the first refresh sweep after start-up\n\
             # (0..=3600); anchored absolutely (#260), then sweeps recur on cadence.\n",
        );
        out.push_str(&format!("idle_after_secs = {}\n", r.idle_after_secs));
        out.push_str(
            "# Seconds bounding one account's whole refresh cycle (10..=600); a slower\n\
             # cycle is cancelled and reported (non-fatal). Keep above the ~40s spawn.\n",
        );
        out.push_str(&format!("timeout_secs = {}\n", r.timeout_secs));
        out.push_str(
            "# Consecutive refresh sweeps failing with error across ALL eligible accounts before\n\
             # the daemon flags a SYSTEMIC refresh-mechanism failure (1..=100) — a mechanism-down\n\
             # signal (event + `status` indicator) distinct from per-account at-risk.\n",
        );
        out.push_str(&format!("systemic_failure_n = {}\n", r.systemic_failure_n));
        out.push_str(
            "# Pre-emptively refresh the ACTIVE account's token in place before it nears expiry\n\
             # (issue #468). OFF by default: this rotates the live shared credential every cadence,\n\
             # and the active account is instead kept warm reactively (on a real 401) and recovered\n\
             # by autonomous adopt-target. Set true to restore the pre-emptive mint. Only takes\n\
             # effect when enabled = true. See docs/findings/0476-keep-warm-scrub-risk-tradeoff.md.\n",
        );
        out.push_str(&format!(
            "proactive_keep_warm = {}\n",
            r.proactive_keep_warm
        ));
        out.push_str(
            "# The `claude` binary to spawn, overriding $CLAUDE_BIN/$PATH. Omit (or leave\n\
             # empty) to resolve from $CLAUDE_BIN then $PATH.\n",
        );
        match &r.claude_bin {
            Some(bin) => out.push_str(&format!(
                "claude_bin = {}\n",
                basic_string(&bin.to_string_lossy())
            )),
            None => out.push_str("# claude_bin = \"/absolute/path/to/claude\"\n"),
        }

        // The one-shot `login` verb's settings (issue #135): capture timeout + optional binary
        // override. Independent of `[refresh]` (a login is interactive, not a daemon tick).
        let l = &self.login;
        out.push_str("\n[login]\n");
        out.push_str(
            "# Settings for `sessiometer login [label]`, the interactive re-auth verb: run\n\
             # `claude /login` in an isolated config dir, harvest the fresh credential, and land\n\
             # it in the roster (onboarding a new account or reviving a parked one).\n",
        );
        out.push_str(
            "# Seconds bounding one whole login capture (60..=600); on expiry the login is\n\
             # cancelled (nothing captured). Longer than the refresh timeout — a login waits on a\n\
             # human completing a browser OAuth handoff.\n",
        );
        out.push_str(&format!("timeout_secs = {}\n", l.timeout_secs));
        out.push_str(
            "# The `claude` binary to spawn, overriding $CLAUDE_BIN/$PATH. Omit (or leave empty)\n\
             # to resolve from $CLAUDE_BIN then $PATH.\n",
        );
        match &l.claude_bin {
            Some(bin) => out.push_str(&format!(
                "claude_bin = {}\n",
                basic_string(&bin.to_string_lossy())
            )),
            None => out.push_str("# claude_bin = \"/absolute/path/to/claude\"\n"),
        }

        // The usage-stats subsystem (issue #161): retention horizons the daemon threads into
        // the sample store's compaction, plus the offline `stats` verb's default period. The
        // next block ([migration], #150) renders after this one, before [[account]].
        let s = &self.stats;
        out.push_str("\n[stats]\n");
        out.push_str(
            "# The usage-stats store: the daemon records one sample per poll and periodically\n\
             # rolls aged raw samples down into hourly then daily aggregates. These horizons bound\n\
             # each tier; the `stats` verb reads the store offline.\n",
        );
        out.push_str(
            "# Seconds a raw per-poll sample is kept before its whole aged-out day is folded into\n\
             # the aggregates (3600..=31536000, i.e. 1h..365d).\n",
        );
        out.push_str(&format!("raw_retention_secs = {}\n", s.raw_retention_secs));
        out.push_str(
            "# Seconds an hourly-aggregate bucket is kept before it is pruned\n\
             # (86400..=315360000, i.e. 1d..10y).\n",
        );
        out.push_str(&format!(
            "hourly_retention_secs = {}\n",
            s.hourly_retention_secs
        ));
        out.push_str(
            "# Seconds a daily-aggregate bucket is kept, or 0 for lifetime (0..=315360000). The\n\
             # daily tier is kept for the store's lifetime by default; set non-zero to bound it.\n",
        );
        out.push_str(&format!(
            "daily_retention_secs = {}\n",
            s.daily_retention_secs
        ));
        out.push_str(
            "# Default `stats` reporting period when --period/--since are omitted:\n\
             # day | week | month | lifetime.\n",
        );
        out.push_str(&format!(
            "default_period = {}\n",
            basic_string(&s.default_period)
        ));

        // The migration subsystem (issue #150): the Argon2id KDF cost `export` writes an
        // encrypted artifact at, and the default `import` conflict policy. Renders after
        // [stats], before [[account]] — the last tunables block.
        let mi = &self.migration;
        out.push_str("\n[migration]\n");
        out.push_str(
            "# Defaults for `export` / `import`. The KDF cost is recorded IN each encrypted\n\
             # artifact, so changing it never breaks reading a file already written.\n",
        );
        out.push_str(
            "# Argon2id memory cost in KiB when `export` encrypts an artifact (8..=1048576,\n\
             # i.e. 8KiB..1GiB). Higher resists offline brute-force harder, at more time and\n\
             # memory to encrypt AND decrypt.\n",
        );
        out.push_str(&format!("kdf_memory_kib = {}\n", mi.kdf_memory_kib));
        out.push_str(
            "# Argon2id time cost in iterations when `export` encrypts an artifact (1..=16).\n",
        );
        out.push_str(&format!("kdf_iterations = {}\n", mi.kdf_iterations));
        out.push_str(
            "# Default `import` conflict policy when --overwrite is omitted: skip (leave an\n\
             # account already on the target untouched) | overwrite (replace it). --overwrite\n\
             # on the command line always forces overwrite.\n",
        );
        out.push_str(&format!(
            "conflict_policy = {}\n",
            basic_string(mi.conflict_policy.as_str())
        ));

        for account in &self.roster {
            out.push_str("\n[[account]]\n");
            out.push_str(&format!(
                "account_uuid = {}\n",
                basic_string(&account.account_uuid)
            ));
            // No `stash` line: it is derived from `account_uuid` on load
            // ([`Account::stash`]), never persisted (issue #70).
            out.push_str(&format!("label = {}\n", basic_string(&account.label)));
            // Issue #36: in the rotation? A disabled account is kept (and keeps its
            // stash) but is never polled or swapped to — `sessiometer enable`
            // returns it. Defaults to true; omitting the key leaves it enabled.
            out.push_str(
                "# In the rotation? false parks it (kept, but never polled or swapped to). Default true.\n",
            );
            out.push_str(&format!("enabled = {}\n", account.enabled));
        }
        out
    }
}

/// Reject `value` if it falls outside `lo..=hi`, naming `field` in the error.
fn range(field: &'static str, value: i64, lo: i64, hi: i64) -> Result<()> {
    if (lo..=hi).contains(&value) {
        Ok(())
    } else {
        Err(Error::ConfigInvalid(format!(
            "{field} must be in {lo}..={hi}, got {value}"
        )))
    }
}

/// Validate one tunable's optional `[jitter]` spec into a [`Jitter`], or fail at
/// load (issue #38 parse-or-error). `field` names the tunable in any error;
/// `default` applies when the spec is absent. Enforces the `none|uniform|normal`
/// vocabulary, the correct magnitude key per kind (`spread` for uniform, `stddev`
/// for normal, none for `none`), and a non-negative, finite magnitude.
fn parse_jitter(
    field: &'static str,
    spec: Option<RawJitterSpec>,
    default: Jitter,
) -> Result<Jitter> {
    let Some(spec) = spec else {
        return Ok(default);
    };
    match spec.kind.as_str() {
        "none" => {
            if spec.spread.is_some() || spec.stddev.is_some() {
                return Err(Error::ConfigInvalid(format!(
                    "{field} jitter \"none\" takes no magnitude (drop spread/stddev)"
                )));
            }
            Ok(Jitter::None)
        }
        "uniform" => {
            if spec.stddev.is_some() {
                return Err(Error::ConfigInvalid(format!(
                    "{field} jitter \"uniform\" takes `spread`, not `stddev`"
                )));
            }
            let spread = spec.spread.ok_or_else(|| {
                Error::ConfigInvalid(format!("{field} jitter \"uniform\" requires `spread`"))
            })?;
            non_negative(field, "spread", spread)?;
            Ok(Jitter::Uniform { spread })
        }
        "normal" => {
            if spec.spread.is_some() {
                return Err(Error::ConfigInvalid(format!(
                    "{field} jitter \"normal\" takes `stddev`, not `spread`"
                )));
            }
            let stddev = spec.stddev.ok_or_else(|| {
                Error::ConfigInvalid(format!("{field} jitter \"normal\" requires `stddev`"))
            })?;
            non_negative(field, "stddev", stddev)?;
            Ok(Jitter::Normal { stddev })
        }
        other => Err(Error::ConfigInvalid(format!(
            "{field} jitter kind must be none|uniform|normal, got \"{other}\""
        ))),
    }
}

/// Reject a negative or non-finite jitter magnitude, naming the field/param.
fn non_negative(field: &str, param: &str, value: f64) -> Result<()> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(Error::ConfigInvalid(format!(
            "{field} jitter {param} must be a non-negative number, got {value}"
        )))
    }
}

/// Render a [`Jitter`] as the inline TOML table [`RawJitterSpec`] parses back
/// (issue #38). Magnitudes use the float-debug form so they always carry a
/// decimal point and round-trip as TOML floats (never as integers).
#[allow(dead_code)]
fn render_jitter(jitter: &Jitter) -> String {
    match jitter {
        Jitter::None => "{ kind = \"none\" }".to_string(),
        Jitter::Uniform { spread } => format!("{{ kind = \"uniform\", spread = {spread:?} }}"),
        Jitter::Normal { stddev } => format!("{{ kind = \"normal\", stddev = {stddev:?} }}"),
    }
}

/// Render an optional `claude_bin` override for the `config show` origin view
/// (issue #401): the quoted path when set, or a `(unset)` sentinel when it defers
/// to `$CLAUDE_BIN` / `$PATH`. Diagnostic-only — this view never round-trips to a
/// file, so an absent override reads as a clear sentinel rather than a blank.
fn render_optional_bin(bin: &Option<PathBuf>) -> String {
    match bin {
        Some(path) => basic_string(&path.to_string_lossy()),
        None => "(unset)".to_string(),
    }
}

/// Render a list of strings as a single-line TOML array of basic strings, e.g.
/// `["work", "spare"]` (issue #105 `[refresh].accounts`). Each element goes through
/// [`basic_string`], so labels/uuids needing escapes round-trip; an empty list renders
/// `[]`.
fn render_str_array(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&basic_string(item));
    }
    out.push(']');
    out
}

/// Render `s` as a TOML basic string (quoted, with the required escapes). Used
/// by [`Config::render`] for roster fields, which (unlike the integer tunables)
/// may contain characters needing escaping.
///
/// Delegated to `toml_writer` (issue #403, refining ADR-0005). The *emitter* stays
/// hand-written — it interleaves doc-comments a serializer would drop — but the
/// escaping itself is a spec'd grammar (`basic-unescaped`), and `toml_writer` is the
/// reference implementation, already compiled as a dependency of `toml`. It supersedes a
/// hand-rolled `match` that had to re-derive which C0 controls take `\uXXXX` and that
/// non-ASCII stays literal.
///
/// `as_basic()` always quotes with `"` (never a literal `'…'` string), which keeps the
/// output shape identical to the hand-rolled emitter's. Pinned by
/// `basic_string_escapes_specials` and `rendered_strings_round_trip_through_the_parser`,
/// both written against the old implementation and re-run unchanged against this one.
fn basic_string(s: &str) -> String {
    TomlStringBuilder::new(s).as_basic().to_toml_value()
}

// ── Settings-edit surface (issue #268): the daemon-routed config-get / config-set backend. ──
//
// The menubar settings UI (a pure control-socket client) reads the effective config via the
// `config-get` command → [`ConfigView`] and writes tunable/label edits via `config-set` →
// [`Config::apply_settings`]. `config.toml` stays the single source of truth (the daemon
// load→mutate→save's through the SAME tested `render`/`validate` path); tunables are
// reload-by-restart, labels adopt live. The SAFETY boundary — tunables + existing-account
// labels ONLY, never a credential or roster STRUCTURE — is enforced STRUCTURALLY: the
// editable surface IS these types, so a forbidden key is unrepresentable, not merely unshown.

/// The scalar `[tunables]` edits a `config-set` may carry (issue #268), mirroring the
/// scalar [`RawTunables`] keys 1:1. Every field is `Option` — an omitted key is an
/// UNEDITED key — and `#[serde(deny_unknown_fields)]` rejects ANY other key (a credential,
/// an `[[account]]`, a `[jitter]` / `[refresh]` / … block, or a mistyped tunable) as a hard
/// parse error. This type IS the settable allow-list: the roster structure and every
/// credential are unrepresentable, so the #268 safety boundary holds by construction, not by
/// convention. The `[jitter]` strategy specs are deliberately excluded (structured, not
/// scalar form fields); `enabled` is excluded from v1 (it stays the CLI `enable`/`disable`
/// verb, mirroring "add/remove routes to CLI").
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetTunables {
    #[serde(default)]
    pub(crate) poll_secs: Option<i64>,
    #[serde(default)]
    pub(crate) exhausted_poll_secs: Option<i64>,
    #[serde(default)]
    pub(crate) near_limit_poll_secs: Option<i64>,
    #[serde(default)]
    pub(crate) cooldown_secs: Option<i64>,
    #[serde(default)]
    pub(crate) target_max_session_usage: Option<i64>,
    #[serde(default)]
    pub(crate) session_trigger: Option<i64>,
    #[serde(default)]
    pub(crate) weekly_trigger: Option<i64>,
    #[serde(default)]
    pub(crate) session_blind_swap_secs: Option<i64>,
    #[serde(default)]
    pub(crate) session_blind_risk_band: Option<i64>,
    #[serde(default)]
    pub(crate) session_velocity_horizon_secs: Option<i64>,
    #[serde(default)]
    pub(crate) session_velocity_min_project_above: Option<i64>,
    #[serde(default)]
    pub(crate) session_velocity_ema_alpha_pct: Option<i64>,
    #[serde(default)]
    pub(crate) monitor_401_n: Option<i64>,
    #[serde(default)]
    pub(crate) monitor_recovery_m: Option<i64>,
}

/// Which classes of edit a [`Config::apply_settings`] actually changed (issue #268), so the
/// daemon picks reload semantics: `tunables_changed` ⇒ reload-by-restart (the daemon derives
/// its strategy fields once at construction, with no re-derivation primitive), `labels_changed`
/// ⇒ adopt live via the daemon's roster reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingsChange {
    pub(crate) tunables_changed: bool,
    pub(crate) labels_changed: bool,
}

/// The `config-get` control-command reply (issue #268): a non-secret projection of the
/// effective config — the scalar tunables the settings UI edits + the roster's non-secret
/// per-account fields. Produced by [`Config::view`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConfigView {
    pub(crate) tunables: TunablesView,
    pub(crate) accounts: Vec<AccountView>,
}

/// The scalar tunables in a [`ConfigView`] (issue #268) — the effective values the settings
/// UI displays and edits. Mirrors [`Tunables`]' scalar fields; the `[jitter]` strategy
/// fields are omitted (not form-editable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TunablesView {
    pub(crate) poll_secs: u64,
    pub(crate) exhausted_poll_secs: u64,
    pub(crate) near_limit_poll_secs: u64,
    pub(crate) cooldown_secs: u64,
    pub(crate) target_max_session_usage: u8,
    pub(crate) session_trigger: u8,
    pub(crate) weekly_trigger: u8,
    pub(crate) session_blind_swap_secs: u64,
    pub(crate) session_blind_risk_band: u8,
    pub(crate) session_velocity_horizon_secs: u64,
    pub(crate) session_velocity_min_project_above: u8,
    pub(crate) session_velocity_ema_alpha_pct: u8,
    pub(crate) monitor_401_n: u8,
    pub(crate) monitor_recovery_m: u8,
}

impl From<&Tunables> for TunablesView {
    fn from(t: &Tunables) -> Self {
        Self {
            poll_secs: t.poll_secs,
            exhausted_poll_secs: t.exhausted_poll_secs,
            near_limit_poll_secs: t.near_limit_poll_secs,
            cooldown_secs: t.cooldown_secs,
            target_max_session_usage: t.target_max_session_usage,
            session_trigger: t.session_trigger,
            weekly_trigger: t.weekly_trigger,
            session_blind_swap_secs: t.session_blind_swap_secs,
            session_blind_risk_band: t.session_blind_risk_band,
            session_velocity_horizon_secs: t.session_velocity_horizon_secs,
            session_velocity_min_project_above: t.session_velocity_min_project_above,
            session_velocity_ema_alpha_pct: t.session_velocity_ema_alpha_pct,
            monitor_401_n: t.monitor_401_n,
            monitor_recovery_m: t.monitor_recovery_m,
        }
    }
}

/// One roster account in a [`ConfigView`] (issue #268): its non-secret `account_uuid` (the
/// stable label-edit key), `label`, and `enabled` flag. No credential — the roster holds
/// none (issue #15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AccountView {
    pub(crate) account_uuid: String,
    pub(crate) label: String,
    pub(crate) enabled: bool,
}

/// Overlay a `config-set`'s scalar tunable edits (issue #268) onto the raw layer — each
/// `Some(v)` replaces that key, each `None` leaves it. `target_max_session_usage` maps to
/// the raw `Option` (its absence sentinel); every other key to its plain scalar. Ranges are
/// NOT checked here — [`Config::validate`] does that atomically over the overlaid result.
fn overlay_tunables(raw: &mut RawTunables, edits: &SetTunables) {
    if let Some(v) = edits.poll_secs {
        raw.poll_secs = v;
    }
    if let Some(v) = edits.exhausted_poll_secs {
        raw.exhausted_poll_secs = v;
    }
    if let Some(v) = edits.near_limit_poll_secs {
        raw.near_limit_poll_secs = v;
    }
    if let Some(v) = edits.cooldown_secs {
        raw.cooldown_secs = v;
    }
    if let Some(v) = edits.target_max_session_usage {
        raw.target_max_session_usage = Some(v);
    }
    if let Some(v) = edits.session_trigger {
        raw.session_trigger = v;
    }
    if let Some(v) = edits.weekly_trigger {
        raw.weekly_trigger = v;
    }
    if let Some(v) = edits.session_blind_swap_secs {
        raw.session_blind_swap_secs = v;
    }
    if let Some(v) = edits.session_blind_risk_band {
        raw.session_blind_risk_band = v;
    }
    if let Some(v) = edits.session_velocity_horizon_secs {
        raw.session_velocity_horizon_secs = v;
    }
    if let Some(v) = edits.session_velocity_min_project_above {
        raw.session_velocity_min_project_above = v;
    }
    if let Some(v) = edits.session_velocity_ema_alpha_pct {
        raw.session_velocity_ema_alpha_pct = v;
    }
    if let Some(v) = edits.monitor_401_n {
        raw.monitor_401_n = v;
    }
    if let Some(v) = edits.monitor_recovery_m {
        raw.monitor_recovery_m = v;
    }
}

/// Overlay a `config-set`'s label edits (issue #268): each `account_uuid` → new label is
/// written onto the MATCHING existing raw account. A uuid matching none is
/// [`Error::AccountUuidNotFound`]. Never appends/removes an entry — only an existing
/// account's `label` field is touched, so the roster structure (and every credential keyed
/// off it) is out of reach (the #268 safety boundary). The new label's non-emptiness is
/// enforced downstream by [`Config::validate`].
fn overlay_labels(accounts: &mut [RawAccount], labels: &BTreeMap<String, String>) -> Result<()> {
    for (account_uuid, new_label) in labels {
        let account = accounts
            .iter_mut()
            .find(|account| &account.account_uuid == account_uuid)
            .ok_or_else(|| Error::AccountUuidNotFound {
                account_uuid: account_uuid.clone(),
            })?;
        account.label = new_label.clone();
    }
    Ok(())
}

/// Permissive deserialization target: every key optional (documented default),
/// integers kept wide so out-of-range values reach [`Config::validate`] with a
/// clear message rather than failing as a `serde` type error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    account: Vec<RawAccount>,
    #[serde(default)]
    tunables: RawTunables,
    #[serde(default)]
    jitter: RawJitter,
    #[serde(default)]
    refresh: RawRefresh,
    #[serde(default)]
    login: RawLogin,
    #[serde(default)]
    stats: RawStats,
    #[serde(default)]
    migration: RawMigration,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccount {
    account_uuid: String,
    /// Legacy back-compat (issue #70): pre-#70 `config.toml` files persisted a
    /// `stash = "Sessiometer/<account_uuid>"` line. The stash is now derived
    /// ([`Account::stash`]) and never re-written, but the key is still ACCEPTED
    /// here — and ignored — so existing files keep parsing under
    /// `deny_unknown_fields`; the next `save` drops it. Absent (a post-#70 file) →
    /// empty via `#[serde(default)]`.
    #[serde(default)]
    #[allow(dead_code)]
    stash: String,
    label: String,
    /// In the rotation? (issue #36) Absent → `true`: a pre-#36 `[[account]]` entry
    /// omits the key and must stay fully enabled (backward-compatible default).
    #[serde(default = "default_account_enabled")]
    enabled: bool,
}

/// The backward-compatible default for [`RawAccount::enabled`] (issue #36): an
/// account entry that omits `enabled` is enabled.
fn default_account_enabled() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTunables {
    #[serde(default = "default_poll_secs")]
    poll_secs: i64,
    #[serde(default = "default_exhausted_poll_secs")]
    exhausted_poll_secs: i64,
    #[serde(default = "default_near_limit_poll_secs")]
    near_limit_poll_secs: i64,
    #[serde(default = "default_cooldown_secs")]
    cooldown_secs: i64,
    /// Default-on (#398): absent → `None` here, mapped to `DEFAULT_TARGET_MAX_SESSION_USAGE`
    /// in [`Config::validate`] (the raw layer keeps `Option` to detect absence).
    /// Accepts the two pre-rename keys `target_max_usage` (#415) and `session_floor`
    /// (pre-#415) as deprecation aliases (ADR-0006): an existing `config.toml` written with
    /// either old key still parses to this field, and `save` re-emits it under the new key.
    /// #443 is the LAST rename of this key — the alias stack stops at these two. Any two of
    /// the three spellings present in one file → serde's duplicate-field parse error, so an
    /// operator mid-migration gets no silent winner.
    #[serde(default, alias = "target_max_usage", alias = "session_floor")]
    target_max_session_usage: Option<i64>,
    #[serde(default = "default_session_trigger")]
    session_trigger: i64,
    #[serde(default = "default_weekly_trigger")]
    weekly_trigger: i64,
    #[serde(default = "default_session_blind_swap_secs")]
    session_blind_swap_secs: i64,
    #[serde(default = "default_session_blind_risk_band")]
    session_blind_risk_band: i64,
    #[serde(default = "default_session_velocity_horizon_secs")]
    session_velocity_horizon_secs: i64,
    #[serde(default = "default_session_velocity_min_project_above")]
    session_velocity_min_project_above: i64,
    #[serde(default = "default_session_velocity_ema_alpha_pct")]
    session_velocity_ema_alpha_pct: i64,
    #[serde(default = "default_monitor_401_n")]
    monitor_401_n: i64,
    #[serde(default = "default_monitor_recovery_m")]
    monitor_recovery_m: i64,
}

impl Default for RawTunables {
    fn default() -> Self {
        Self {
            poll_secs: default_poll_secs(),
            exhausted_poll_secs: default_exhausted_poll_secs(),
            near_limit_poll_secs: default_near_limit_poll_secs(),
            cooldown_secs: default_cooldown_secs(),
            target_max_session_usage: None,
            session_trigger: default_session_trigger(),
            weekly_trigger: default_weekly_trigger(),
            session_blind_swap_secs: default_session_blind_swap_secs(),
            session_blind_risk_band: default_session_blind_risk_band(),
            session_velocity_horizon_secs: default_session_velocity_horizon_secs(),
            session_velocity_min_project_above: default_session_velocity_min_project_above(),
            session_velocity_ema_alpha_pct: default_session_velocity_ema_alpha_pct(),
            monitor_401_n: default_monitor_401_n(),
            monitor_recovery_m: default_monitor_recovery_m(),
        }
    }
}

fn default_poll_secs() -> i64 {
    DEFAULT_POLL_SECS as i64
}
fn default_exhausted_poll_secs() -> i64 {
    DEFAULT_EXHAUSTED_POLL_SECS as i64
}
fn default_near_limit_poll_secs() -> i64 {
    DEFAULT_NEAR_LIMIT_POLL_SECS as i64
}
fn default_cooldown_secs() -> i64 {
    DEFAULT_COOLDOWN_SECS as i64
}
fn default_session_trigger() -> i64 {
    i64::from(DEFAULT_SESSION_TRIGGER)
}
fn default_weekly_trigger() -> i64 {
    i64::from(DEFAULT_WEEKLY_TRIGGER)
}
fn default_session_blind_swap_secs() -> i64 {
    DEFAULT_SESSION_BLIND_SWAP_SECS as i64
}
fn default_session_blind_risk_band() -> i64 {
    i64::from(DEFAULT_SESSION_BLIND_RISK_BAND)
}
fn default_session_velocity_horizon_secs() -> i64 {
    DEFAULT_SESSION_VELOCITY_HORIZON_SECS as i64
}
fn default_session_velocity_min_project_above() -> i64 {
    i64::from(DEFAULT_SESSION_VELOCITY_MIN_PROJECT_ABOVE)
}
fn default_session_velocity_ema_alpha_pct() -> i64 {
    i64::from(DEFAULT_SESSION_VELOCITY_EMA_ALPHA_PCT)
}
fn default_monitor_401_n() -> i64 {
    i64::from(DEFAULT_MONITOR_401_N)
}
fn default_monitor_recovery_m() -> i64 {
    i64::from(DEFAULT_MONITOR_RECOVERY_M)
}

/// Permissive deserialization of the optional `[refresh]` table (issue #105): every key
/// optional with a documented default, integers kept wide so an out-of-range value reaches
/// [`Config::validate`] with a clear message rather than a bare `serde` type error.
/// `deny_unknown_fields` rejects a stray key as a parse error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRefresh {
    #[serde(default = "default_refresh_enabled")]
    enabled: bool,
    #[serde(default)]
    accounts: Vec<String>,
    #[serde(default = "default_refresh_cadence_secs")]
    cadence_secs: i64,
    #[serde(default = "default_refresh_idle_after_secs")]
    idle_after_secs: i64,
    #[serde(default = "default_refresh_timeout_secs")]
    timeout_secs: i64,
    #[serde(default)]
    claude_bin: Option<String>,
    #[serde(default = "default_refresh_systemic_failure_n")]
    systemic_failure_n: i64,
    // Issue #468: an absent key resolves to `false` (proactive keep-warm OFF, predicate C) — the
    // bool `Default`, so a plain `#[serde(default)]` is the right default here (unlike `enabled`,
    // whose default is `true` and needs a named default fn).
    #[serde(default)]
    proactive_keep_warm: bool,
}

impl Default for RawRefresh {
    fn default() -> Self {
        Self {
            enabled: default_refresh_enabled(),
            accounts: Vec::new(),
            cadence_secs: default_refresh_cadence_secs(),
            idle_after_secs: default_refresh_idle_after_secs(),
            timeout_secs: default_refresh_timeout_secs(),
            claude_bin: None,
            systemic_failure_n: default_refresh_systemic_failure_n(),
            proactive_keep_warm: false,
        }
    }
}

/// The default for [`RawRefresh::enabled`] (issue #409): an absent `enabled` key — whether the
/// whole `[refresh]` section is omitted (via [`RawRefresh::default`]) or only the key is (via this
/// field default) — resolves to refresh ON, the self-sustaining sliding-window default. An operator
/// opts OUT with an explicit `enabled = false`, which serde parses verbatim (a present key never
/// takes this default).
fn default_refresh_enabled() -> bool {
    true
}
fn default_refresh_cadence_secs() -> i64 {
    DEFAULT_REFRESH_CADENCE_SECS as i64
}
fn default_refresh_idle_after_secs() -> i64 {
    DEFAULT_REFRESH_IDLE_AFTER_SECS as i64
}
fn default_refresh_timeout_secs() -> i64 {
    DEFAULT_REFRESH_TIMEOUT_SECS as i64
}
fn default_refresh_systemic_failure_n() -> i64 {
    i64::from(DEFAULT_REFRESH_SYSTEMIC_FAILURE_N)
}

/// Permissive deserialization of the optional `[login]` table (issue #135): both keys optional
/// with a documented default, the timeout kept wide (`i64`) so an out-of-range value reaches
/// [`Config::validate`] with a clear message rather than a bare `serde` type error.
/// `deny_unknown_fields` rejects a stray key as a parse error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLogin {
    #[serde(default = "default_login_timeout_secs")]
    timeout_secs: i64,
    #[serde(default)]
    claude_bin: Option<String>,
}

impl Default for RawLogin {
    fn default() -> Self {
        Self {
            timeout_secs: default_login_timeout_secs(),
            claude_bin: None,
        }
    }
}

fn default_login_timeout_secs() -> i64 {
    DEFAULT_LOGIN_TIMEOUT_SECS as i64
}

/// Permissive deserialization of the optional `[stats]` table (issue #161): every key
/// optional with a documented default, the retention horizons kept wide (`i64`) so an
/// out-of-range value reaches [`Config::validate`] with a clear message rather than a bare
/// `serde` type error. `deny_unknown_fields` rejects a stray key as a parse error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStats {
    #[serde(default = "default_stats_raw_retention_secs")]
    raw_retention_secs: i64,
    #[serde(default = "default_stats_hourly_retention_secs")]
    hourly_retention_secs: i64,
    #[serde(default = "default_stats_daily_retention_secs")]
    daily_retention_secs: i64,
    #[serde(default = "default_stats_period")]
    default_period: String,
}

impl Default for RawStats {
    fn default() -> Self {
        Self {
            raw_retention_secs: default_stats_raw_retention_secs(),
            hourly_retention_secs: default_stats_hourly_retention_secs(),
            daily_retention_secs: default_stats_daily_retention_secs(),
            default_period: default_stats_period(),
        }
    }
}

fn default_stats_raw_retention_secs() -> i64 {
    DEFAULT_STATS_RAW_RETENTION_SECS as i64
}
fn default_stats_hourly_retention_secs() -> i64 {
    DEFAULT_STATS_HOURLY_RETENTION_SECS as i64
}
fn default_stats_daily_retention_secs() -> i64 {
    DEFAULT_STATS_DAILY_RETENTION_SECS as i64
}
fn default_stats_period() -> String {
    DEFAULT_STATS_PERIOD.to_owned()
}

/// Permissive deserialization target for the optional `[migration]` table (issue #150): the
/// KDF cost kept wide (`i64`) so an out-of-range value reaches [`Config::validate`] with a clear
/// bounds message rather than a bare `serde` type error, and the conflict policy kept a `String`
/// so an unknown token is a validation error naming the vocabulary, not a `serde` enum failure.
/// `deny_unknown_fields` rejects a stray key as a parse error, like the other tables.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMigration {
    #[serde(default = "default_migration_kdf_memory_kib")]
    kdf_memory_kib: i64,
    #[serde(default = "default_migration_kdf_iterations")]
    kdf_iterations: i64,
    #[serde(default = "default_migration_conflict_policy")]
    conflict_policy: String,
}

impl Default for RawMigration {
    fn default() -> Self {
        Self {
            kdf_memory_kib: default_migration_kdf_memory_kib(),
            kdf_iterations: default_migration_kdf_iterations(),
            conflict_policy: default_migration_conflict_policy(),
        }
    }
}

fn default_migration_kdf_memory_kib() -> i64 {
    i64::from(DEFAULT_MIGRATION_KDF_MEMORY_KIB)
}
fn default_migration_kdf_iterations() -> i64 {
    i64::from(DEFAULT_MIGRATION_KDF_ITERATIONS)
}
fn default_migration_conflict_policy() -> String {
    DEFAULT_MIGRATION_CONFLICT_POLICY.to_owned()
}

/// Permissive deserialization of the optional `[jitter]` table (issue #38): each
/// tunable's spec is optional (absent → its default jitter). `deny_unknown_fields`
/// rejects a stray tunable name as a parse error.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawJitter {
    #[serde(default)]
    poll: Option<RawJitterSpec>,
    #[serde(default)]
    trigger: Option<RawJitterSpec>,
    #[serde(default)]
    weekly_trigger: Option<RawJitterSpec>,
    #[serde(default)]
    cooldown: Option<RawJitterSpec>,
}

/// One tunable's jitter spec: a `kind` plus its magnitude (`spread` for uniform,
/// `stddev` for normal). Both magnitudes are kept optional and wide here so a
/// kind/magnitude mismatch reaches [`parse_jitter`] as a clear domain error
/// rather than a bare `serde` type error. Magnitudes are TOML floats (write a
/// decimal, e.g. `spread = 2.0`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJitterSpec {
    kind: String,
    #[serde(default)]
    spread: Option<f64>,
    #[serde(default)]
    stddev: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[tunables]
poll_secs = 30
cooldown_secs = 45
target_max_session_usage = 70
session_trigger = 90
weekly_trigger = 97
monitor_401_n = 5
monitor_recovery_m = 4

[[account]]
account_uuid = "11111111-1111-1111-1111-111111111111"
label = "work"

[[account]]
account_uuid = "22222222-2222-2222-2222-222222222222"
label = "personal"
"#;

    /// A minimal valid roster body with one account and the given `[tunables]`
    /// fragment spliced in.
    fn with_tunables(fragment: &str) -> String {
        format!(
            "[tunables]\n{fragment}\n\
             [[account]]\n\
             account_uuid = \"u\"\n\
             label = \"l\"\n"
        )
    }

    // ── config-set / config-get backend (issue #268) ──

    /// A `BTreeMap<uuid, label>` for the `config-set` label edits (fully qualified so the
    /// test needs no extra `use`).
    fn labels(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(uuid, label)| (uuid.to_string(), label.to_string()))
            .collect()
    }

    #[test]
    fn apply_settings_overlays_a_tunable_and_revalidates() {
        let (after, change) = Config::apply_settings(
            VALID,
            &SetTunables {
                poll_secs: Some(300),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap();
        assert_eq!(after.tunables.poll_secs, 300);
        assert!(change.tunables_changed);
        assert!(!change.labels_changed);
        // A tunables-only edit leaves the roster untouched.
        assert_eq!(after.roster.len(), 2);
    }

    #[test]
    fn apply_settings_relabels_an_account_by_uuid() {
        let (after, change) = Config::apply_settings(
            VALID,
            &SetTunables::default(),
            &labels(&[("11111111-1111-1111-1111-111111111111", "day-job")]),
        )
        .unwrap();
        assert!(change.labels_changed);
        assert!(!change.tunables_changed);
        let renamed = after
            .roster
            .iter()
            .find(|a| a.account_uuid == "11111111-1111-1111-1111-111111111111")
            .unwrap();
        assert_eq!(renamed.label, "day-job");
    }

    #[test]
    fn apply_settings_rejects_an_out_of_range_tunable() {
        // poll_secs floor is 5; 4 is out of range → the whole batch is rejected.
        let err = Config::apply_settings(
            VALID,
            &SetTunables {
                poll_secs: Some(4),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)));
    }

    #[test]
    fn apply_settings_validates_the_final_batch_not_intermediate_states() {
        // Current: poll_secs=30, exhausted_poll_secs=60. Raising poll_secs to 300 AND
        // exhausted to 7200 is valid as a WHOLE (300 <= 7200), even though applying poll
        // first would transiently violate `exhausted >= poll` (60 < 300). Atomic validation
        // over the final state is what lets a settings-form "Apply" move coupled fields.
        let base = with_tunables("poll_secs = 30\nexhausted_poll_secs = 60");
        let (after, _) = Config::apply_settings(
            &base,
            &SetTunables {
                poll_secs: Some(300),
                exhausted_poll_secs: Some(7200),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap();
        assert_eq!(after.tunables.poll_secs, 300);
        assert_eq!(after.tunables.exhausted_poll_secs, 7200);
    }

    #[test]
    fn apply_settings_rejects_a_cross_field_invalid_batch() {
        // exhausted_poll_secs must be >= poll_secs; 200 < 300 → rejected as a whole.
        let err = Config::apply_settings(
            VALID,
            &SetTunables {
                poll_secs: Some(300),
                exhausted_poll_secs: Some(200),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)));
    }

    #[test]
    fn apply_settings_rejects_target_max_above_session_trigger() {
        // VALID session_trigger=90; target_max_session_usage=95 > 90 → the distinct cross-field error.
        let err = Config::apply_settings(
            VALID,
            &SetTunables {
                target_max_session_usage: Some(95),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::ConfigTargetMaxSessionAboveTrigger { .. }
        ));
    }

    #[test]
    fn apply_settings_rejects_an_unknown_account_uuid() {
        let err = Config::apply_settings(
            VALID,
            &SetTunables::default(),
            &labels(&[("no-such-uuid", "x")]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::AccountUuidNotFound { .. }));
    }

    #[test]
    fn apply_settings_rejects_an_empty_label() {
        let err = Config::apply_settings(
            VALID,
            &SetTunables::default(),
            &labels(&[("11111111-1111-1111-1111-111111111111", "  ")]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)));
    }

    #[test]
    fn apply_settings_reports_no_change_for_a_noop_edit() {
        // Submitting the current value + current label changes nothing.
        let (_, change) = Config::apply_settings(
            VALID,
            &SetTunables {
                poll_secs: Some(30), // VALID's current poll_secs
                ..SetTunables::default()
            },
            &labels(&[("11111111-1111-1111-1111-111111111111", "work")]),
        )
        .unwrap();
        assert!(!change.tunables_changed);
        assert!(!change.labels_changed);
    }

    #[test]
    fn apply_settings_refuses_a_currently_unreadable_config() {
        // A hand-broken file fails at the baseline parse — config-set never overwrites a
        // file it cannot understand.
        let err = Config::apply_settings(
            "this is not toml [[[",
            &SetTunables::default(),
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigParse(_)));
    }

    #[test]
    fn set_tunables_rejects_a_forbidden_key() {
        // SAFETY invariant: only the scalar tunable keys are representable. A credential, a
        // roster field, or any other key is a hard parse error (deny_unknown_fields), so the
        // credential/roster-structure boundary cannot be crossed through config-set.
        for forbidden in [
            r#"{"account_uuid":"x"}"#,
            r#"{"credential":"secret"}"#,
            r#"{"label":"x"}"#,
            r#"{"enabled":true}"#,
            r#"{"poll_secs":300,"roster":[]}"#,
        ] {
            assert!(
                serde_json::from_str::<SetTunables>(forbidden).is_err(),
                "forbidden key accepted: {forbidden}"
            );
        }
        // A bare scalar tunable parses; unset keys stay None.
        let ok: SetTunables = serde_json::from_str(r#"{"poll_secs":300}"#).unwrap();
        assert_eq!(ok.poll_secs, Some(300));
        assert_eq!(ok.session_trigger, None);
    }

    #[test]
    fn config_view_projects_tunables_and_roster() {
        let view = Config::parse(VALID).unwrap().view();
        assert_eq!(view.tunables.poll_secs, 30);
        assert_eq!(view.tunables.session_trigger, 90);
        assert_eq!(view.accounts.len(), 2);
        assert_eq!(
            view.accounts[0].account_uuid,
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(view.accounts[0].label, "work");
        assert!(view.accounts[0].enabled);
    }

    #[test]
    fn config_view_serde_round_trips() {
        let view = Config::parse(VALID).unwrap().view();
        let json = serde_json::to_string(&view).unwrap();
        let back: ConfigView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
    }

    #[test]
    fn parses_a_valid_config() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.roster.len(), 2);
        assert_eq!(
            config.tunables,
            Tunables {
                poll_secs: 30,
                // VALID omits exhausted_poll_secs → the compiled-in default (issue #537).
                exhausted_poll_secs: 3600,
                // VALID omits near_limit_poll_secs → the compiled-in default (issue #540). 60 > the
                // configured poll_secs (30) here, so it is inert for this config (min never binds) —
                // valid, not an error (no cross-field bound to poll_secs).
                near_limit_poll_secs: 60,
                cooldown_secs: 45,
                target_max_session_usage: 70,
                session_trigger: 90,
                weekly_trigger: 97,
                // VALID sets no blind-swap keys → the compiled-in defaults (issue #452).
                session_blind_swap_secs: 300,
                session_blind_risk_band: 60,
                // VALID sets no velocity-projection keys → the compiled-in defaults (issue #539).
                session_velocity_horizon_secs: 120,
                session_velocity_min_project_above: 85,
                session_velocity_ema_alpha_pct: 50,
                monitor_401_n: 5,
                monitor_recovery_m: 4,
                // No [jitter] table in VALID → default strategies: poll jitters
                // normally (base from poll_secs), trigger/weekly_trigger/cooldown
                // are fixed at their respective bases.
                poll_strategy: Strategy {
                    base: 30.0,
                    jitter: default_poll_jitter(),
                },
                trigger_strategy: Strategy::fixed(90.0),
                weekly_trigger_strategy: Strategy::fixed(97.0),
                cooldown_strategy: Strategy::fixed(45.0),
            }
        );
        assert_eq!(config.roster[0].label, "work");
        // The stash name is derived from `account_uuid`, not parsed from the file.
        assert_eq!(
            config.roster[1].stash(),
            "Sessiometer/22222222-2222-2222-2222-222222222222"
        );
    }

    #[test]
    fn tunables_default_when_table_absent() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    label = \"only\"\n";
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.tunables, Tunables::default());
        assert_eq!(config.tunables.session_trigger, 95);
        // #398: the target_max_session_usage reserve is default-on at 80.
        assert_eq!(
            config.tunables.target_max_session_usage,
            DEFAULT_TARGET_MAX_SESSION_USAGE
        );
    }

    #[test]
    fn missing_tunable_key_takes_its_default() {
        let toml = with_tunables("poll_secs = 120");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.cooldown_secs, DEFAULT_COOLDOWN_SECS);
    }

    #[test]
    fn malformed_toml_is_a_parse_error() {
        assert!(matches!(Config::parse("]["), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let toml = with_tunables("poll_secs = 60\nbogus = 1");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn rejects_out_of_range_trigger() {
        for trigger in ["49", "100", "120"] {
            let toml = with_tunables(&format!("session_trigger = {trigger}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "session_trigger = {trigger} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_out_of_range_weekly_trigger() {
        // #41: the weekly trigger carries the same 50..=99 bound as the session one.
        for trigger in ["49", "100", "120"] {
            let toml = with_tunables(&format!("weekly_trigger = {trigger}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "weekly_trigger = {trigger} should be rejected"
            );
        }
    }

    #[test]
    fn session_and_weekly_triggers_are_independently_configurable() {
        // AC #3: the two triggers are set independently — there is NO cross-field
        // rule, so weekly may even sit BELOW session (unlike target_max_session_usage, which
        // is capped at session_trigger).
        let t = Config::parse(&with_tunables("session_trigger = 90\nweekly_trigger = 99"))
            .unwrap()
            .tunables;
        assert_eq!(t.session_trigger, 90);
        assert_eq!(t.weekly_trigger, 99);
        assert_eq!(t.trigger_strategy.base, 90.0);
        assert_eq!(t.weekly_trigger_strategy.base, 99.0);

        // weekly BELOW session is accepted (no target_max_session_usage-style cross-field constraint).
        let inverted = Config::parse(&with_tunables("session_trigger = 95\nweekly_trigger = 60"))
            .unwrap()
            .tunables;
        assert_eq!(inverted.session_trigger, 95);
        assert_eq!(inverted.weekly_trigger, 60);
    }

    #[test]
    fn weekly_trigger_defaults_higher_than_session_when_absent() {
        // An absent weekly_trigger takes its (higher-than-session) default.
        let t = Config::parse(&with_tunables("session_trigger = 95"))
            .unwrap()
            .tunables;
        assert_eq!(t.weekly_trigger, DEFAULT_WEEKLY_TRIGGER);
        assert!(t.weekly_trigger > t.session_trigger);
        assert_eq!(
            t.weekly_trigger_strategy.base,
            f64::from(DEFAULT_WEEKLY_TRIGGER)
        );
    }

    #[test]
    fn rejects_target_max_above_trigger_with_a_distinct_error() {
        let toml = with_tunables("target_max_session_usage = 95\nsession_trigger = 90");
        assert!(matches!(
            Config::parse(&toml),
            Err(Error::ConfigTargetMaxSessionAboveTrigger {
                target_max_session_usage: 95,
                trigger: 90
            })
        ));
    }

    #[test]
    fn rejects_negative_target_max() {
        let toml = with_tunables("target_max_session_usage = -1");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn rejects_zero_target_max_naming_the_consequence() {
        // #414: target_max_session_usage = 0 makes the swap predicate `usage.session < 0` admit no
        // account, so proactive swapping is silently disabled and the daemon just holds.
        // Since #398 made target_max_session_usage a live, hand-editable line, 0 is the natural
        // (wrong) guess for "no restriction" — its exact opposite. validate must reject it
        // with a message that names the consequence AND points at the remedy (raise it
        // toward session_trigger to admit more targets).
        let toml = with_tunables("target_max_session_usage = 0\nsession_trigger = 90");
        match Config::parse(&toml) {
            Err(Error::ConfigInvalid(msg)) => assert!(
                msg.contains("disables proactive swapping") && msg.contains("session_trigger"),
                "rejection must name the consequence and the remedy, got: {msg}"
            ),
            Ok(_) => panic!("target_max_session_usage = 0 must be rejected, not accepted"),
            Err(e) => panic!("target_max_session_usage = 0 must be ConfigInvalid, got: {e}"),
        }

        // The reject is precisely 0, not "any low value": 1 is the valid lower edge and
        // still parses (inert-but-valid — admits only accounts at 0% session).
        let one = Config::parse(&with_tunables(
            "target_max_session_usage = 1\nsession_trigger = 90",
        ))
        .expect("target_max_session_usage = 1 is the valid lower bound and must parse");
        assert_eq!(one.tunables.target_max_session_usage, 1);

        // …and the absent-key default path (#417 clamp) is untouched by the reject: an
        // absent target_max_session_usage still yields the default-on reserve, never 0.
        let absent = Config::parse(&with_tunables("session_trigger = 90")).unwrap();
        assert_eq!(
            absent.tunables.target_max_session_usage,
            DEFAULT_TARGET_MAX_SESSION_USAGE
        );
    }

    #[test]
    fn target_max_session_usage_defaults_to_80_when_absent() {
        // #398: an absent target_max_session_usage takes the default-on reserve (80), even when
        // other tunables are set…
        let absent = Config::parse(&with_tunables("session_trigger = 95")).unwrap();
        assert_eq!(
            absent.tunables.target_max_session_usage,
            DEFAULT_TARGET_MAX_SESSION_USAGE
        );
        // …and a present value overrides it at that percent.
        let set = Config::parse(&with_tunables(
            "target_max_session_usage = 90\nsession_trigger = 95",
        ))
        .unwrap();
        assert_eq!(set.tunables.target_max_session_usage, 90);
    }

    #[test]
    fn rendered_default_config_documents_target_max_session_usage_as_a_live_value() {
        // #398: render emits a LIVE target_max_session_usage line (default-on) that round-trips
        // back to the same value — never a commented-out opt-in.
        let mut config = Config::parse(VALID).unwrap();
        config.tunables.target_max_session_usage = DEFAULT_TARGET_MAX_SESSION_USAGE;
        let text = config.render();
        assert!(text.contains("target_max_session_usage = 80"), "got {text}");
        assert!(!text.contains("# target_max_session_usage ="), "got {text}");
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(
            reparsed.tunables.target_max_session_usage,
            DEFAULT_TARGET_MAX_SESSION_USAGE
        );
    }

    #[test]
    fn absent_target_max_default_clamps_to_trigger_below_80_and_survives_round_trip() {
        // #417 (regression from #398): with session_trigger < 80 and NO target_max_session_usage
        // key, the absent-key default (80) MUST clamp down to session_trigger — honoring
        // the same target_max_session_usage <= session_trigger invariant the present-value arm
        // already enforces (ADR-0013 Decision 1). Without the clamp the first load
        // silently yields a reserve of 80 (> trigger — the cross-field check is skipped on
        // the absent-key arm), render() then emits it as a LIVE line (#398), and the SECOND
        // parse rejects the config with ConfigTargetMaxSessionAboveTrigger — bricking a valid config
        // after any save/export round-trip (enable/disable/remove account, capture
        // write-back, export→import). The existing round-trip test above only covers the
        // default trigger = 95 (where 80 < 95), so it never reached this corner.
        let toml = with_tunables("session_trigger = 70"); // no target_max_session_usage key
        let config = Config::parse(&toml).unwrap();
        // The default is clamped to the trigger — the maximally-permissive inert value
        // (ADR-0013: an equal reserve admits exactly what the always-on gate admits),
        // never left at 80.
        assert_eq!(config.tunables.target_max_session_usage, 70);
        assert!(config.tunables.target_max_session_usage <= config.tunables.session_trigger);

        // …and it survives a render → parse round-trip: the exact path that bricked.
        let text = config.render();
        assert!(text.contains("target_max_session_usage = 70"), "got {text}");
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.tunables.target_max_session_usage, 70);
        assert_eq!(reparsed.tunables.session_trigger, 70);
    }

    #[test]
    fn deprecated_aliases_parse_and_render_as_target_max_session_usage() {
        // Schema-migration guard (ADR-0006). The target-reserve key has been renamed twice:
        // `session_floor` → `target_max_usage` (#415) → `target_max_session_usage` (#443, the
        // unqualified `usage` hid the session axis). Each rename kept the prior key as a serde
        // deprecation alias, and #443 is the LAST rename (the alias stack stops at two). Every
        // existing config.toml carries a persisted, operator-visible line, so BOTH deprecated
        // keys MUST still parse onto the new field, and render MUST rewrite them to the new
        // canonical key.

        // All three spellings load onto the new field with the same value (AC: assert all three).
        for key in [
            "session_floor",
            "target_max_usage",
            "target_max_session_usage",
        ] {
            let cfg = Config::parse(&with_tunables(&format!("{key} = 70\nsession_trigger = 90")))
                .unwrap_or_else(|e| panic!("a config written with `{key}` must still parse: {e}"));
            assert_eq!(
                cfg.tunables.target_max_session_usage, 70,
                "`{key}` must map onto target_max_session_usage",
            );
        }

        // A deprecated-key file is REWRITTEN to the new key on render (the one-way key rewrite,
        // mirroring the #70 stash drop): the emitted file carries `target_max_session_usage`,
        // never either old key.
        let old = Config::parse(&with_tunables("session_floor = 70\nsession_trigger = 90"))
            .expect("a config written with the deprecated `session_floor` key must still parse");
        let rendered = old.render();
        assert!(
            rendered.contains("target_max_session_usage = 70"),
            "render must emit the new key: {rendered}"
        );
        assert!(
            !rendered.contains("session_floor") && !rendered.contains("target_max_usage"),
            "render must NOT emit either deprecated key: {rendered}"
        );

        // Export → import round-trip survives the deprecated-key input: parsing the render
        // of an old-key file yields the same value under the new field.
        let reimported = Config::parse(&rendered).expect("the rendered new-key file re-imports");
        assert_eq!(reimported.tunables.target_max_session_usage, 70);
    }

    #[test]
    fn multiple_reserve_key_spellings_present_is_a_parse_error() {
        // Mid-migration an operator might leave more than one spelling of the reserve key in
        // one file. serde maps the canonical `target_max_session_usage` and both deprecated
        // aliases (`target_max_usage` #415, `session_floor` pre-#415) onto the same field, so
        // ANY two present at once is a duplicate-field parse error rather than a silent winner
        // — the operator is told to pick one (the issue's precedence choice). Cover every
        // collision-capable pair plus all three at once.
        let collisions = [
            "session_floor = 70\ntarget_max_usage = 80",
            "session_floor = 70\ntarget_max_session_usage = 80",
            "target_max_usage = 70\ntarget_max_session_usage = 80",
            "session_floor = 70\ntarget_max_usage = 75\ntarget_max_session_usage = 80",
        ];
        for combo in collisions {
            let toml = with_tunables(&format!("{combo}\nsession_trigger = 90"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigParse(_))),
                "multiple reserve-key spellings present must be a ConfigParse error for `{combo}`, got: {:?}",
                Config::parse(&toml)
            );
        }
    }

    #[test]
    fn rejects_each_out_of_range_tunable() {
        for (key, value) in [
            ("poll_secs", "4"),
            ("poll_secs", "3601"),
            ("exhausted_poll_secs", "86401"), // above the 24 h ceiling (#537)
            ("near_limit_poll_secs", "4"), // below the 5 s floor, yet not the 0 kill-switch (#540)
            ("near_limit_poll_secs", "3601"), // above the 3600 s ceiling (#540)
            ("session_velocity_horizon_secs", "601"), // above the 600 s sanity ceiling (#539)
            ("session_velocity_min_project_above", "49"), // below the 50 % floor (#539)
            ("session_velocity_min_project_above", "100"), // above the 99 % ceiling (#539)
            ("session_velocity_ema_alpha_pct", "0"), // alpha=0 freezes the EMA — degenerate (#539)
            ("session_velocity_ema_alpha_pct", "101"), // above 100 % (#539)
            ("cooldown_secs", "0"),        // below the non-zero floor (#272)
            ("cooldown_secs", "4"),        // still below the floor (#272)
            ("cooldown_secs", "3601"),
            ("monitor_401_n", "0"),
            ("monitor_401_n", "21"),
            ("monitor_recovery_m", "0"),
            ("monitor_recovery_m", "21"),
        ] {
            let toml = with_tunables(&format!("{key} = {value}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "{key} = {value} should be rejected"
            );
        }
    }

    #[test]
    fn exhausted_poll_secs_defaults_to_one_hour() {
        // Issue #537: an absent `exhausted_poll_secs` defaults to the compiled-in 3600 s (1 h)
        // ceiling — the slow-poll cadence is on by default without an operator opting in.
        let config = Config::parse(&with_tunables("poll_secs = 300")).unwrap();
        assert_eq!(config.tunables.exhausted_poll_secs, 3600);
    }

    #[test]
    fn exhausted_poll_secs_must_be_at_least_poll_secs_and_below_the_ceiling() {
        // Issue #537: the widened cadence is bounded BELOW by `poll_secs` (a slow-polled peer
        // must never re-poll FASTER than the normal cadence — that would defeat the point) and
        // ABOVE by 86400 s. The lower bound is a CROSS-FIELD rule, so the rejection names both
        // the field and `poll_secs`, mirroring `target_max_session_usage`'s message.
        let below = with_tunables("poll_secs = 600\nexhausted_poll_secs = 599");
        match Config::parse(&below) {
            Err(Error::ConfigInvalid(msg)) => assert!(
                msg.contains("exhausted_poll_secs") && msg.contains("600"),
                "rejection must name the field and poll_secs, got: {msg}"
            ),
            other => panic!("exhausted_poll_secs < poll_secs must be rejected, got: {other:?}"),
        }

        // The lower edge (== poll_secs) LOADS — an equal cadence is the inert boundary, not a
        // slow-down, but it is a valid operator choice and threads through to the tunable.
        let at_floor = Config::parse(&with_tunables("poll_secs = 600\nexhausted_poll_secs = 600"))
            .expect("exhausted_poll_secs == poll_secs is the valid lower edge");
        assert_eq!(at_floor.tunables.exhausted_poll_secs, 600);

        // The upper edge (the 24 h ceiling) loads; one over is rejected.
        let at_ceiling = Config::parse(&with_tunables("exhausted_poll_secs = 86400"))
            .expect("the 86400 s ceiling is valid");
        assert_eq!(at_ceiling.tunables.exhausted_poll_secs, 86_400);
        assert!(matches!(
            Config::parse(&with_tunables("exhausted_poll_secs = 86401")),
            Err(Error::ConfigInvalid(_))
        ));

        // A mid-range value >= poll_secs loads and threads through verbatim.
        let mid = Config::parse(&with_tunables(
            "poll_secs = 300\nexhausted_poll_secs = 7200",
        ))
        .expect("a value in poll_secs..=86400 loads");
        assert_eq!(mid.tunables.exhausted_poll_secs, 7200);
    }

    #[test]
    fn near_limit_poll_secs_accepts_zero_disabled_or_the_5_to_3600_band() {
        // Issue #540: the near-limit fast-poll cap is `0` (disabled — the kill-switch) OR in the
        // 5..=3600 s band. The `0`-OR-band shape is the load-bearing subtlety: a naive
        // `(5..=3600).contains()` WITHOUT the `!= 0` guard would reject the documented kill-switch.
        // There is deliberately NO cross-field bound against `poll_secs` (unlike #537's
        // `exhausted_poll_secs`): an above-base value is INERT via the `min(poll_secs / N, cap)` in
        // `next_subinterval`, not a load-time error — so no default-vs-configured footgun.

        // Absent → the compiled-in 60 s default.
        let default = Config::parse(&with_tunables("poll_secs = 300")).unwrap();
        assert_eq!(default.tunables.near_limit_poll_secs, 60);

        // 0 is the disabled kill-switch and MUST load — it is NOT a sub-floor rejection.
        let disabled = Config::parse(&with_tunables("near_limit_poll_secs = 0"))
            .expect("0 is the valid disabled kill-switch, not a sub-floor rejection");
        assert_eq!(disabled.tunables.near_limit_poll_secs, 0);

        // Both band edges load and thread through verbatim.
        for edge in [5u64, 3600] {
            let cfg = Config::parse(&with_tunables(&format!("near_limit_poll_secs = {edge}")))
                .unwrap_or_else(|e| panic!("near_limit_poll_secs = {edge} is a valid edge: {e:?}"));
            assert_eq!(cfg.tunables.near_limit_poll_secs, edge);
        }

        // An above-base cap LOADS (no cross-field bound): with poll_secs = 30 the base sub-interval
        // is already < 60, so a 60 s cap can never bind — but it is inert, not a rejection.
        let inert = Config::parse(&with_tunables("poll_secs = 30\nnear_limit_poll_secs = 60"))
            .expect("an above-base cap is inert, not an error (no cross-field bound)");
        assert_eq!(inert.tunables.near_limit_poll_secs, 60);
    }

    #[test]
    fn cooldown_secs_has_a_non_zero_floor_it_cannot_be_configured_below() {
        // Issue #272: the swap cooldown is tunable ABOVE a non-zero floor but can
        // never be disabled to zero. A sub-floor `cooldown_secs` (including 0) is a
        // load-time rejection whose message names the field and the floor, and the
        // floor edge itself loads and is preserved through to the drawn strategy base.
        // (The floor's non-zero-ness is a compile-time guard at COOLDOWN_SECS_FLOOR.)
        for below in [0, COOLDOWN_SECS_FLOOR - 1] {
            let toml = with_tunables(&format!("cooldown_secs = {below}"));
            match Config::parse(&toml) {
                Err(Error::ConfigInvalid(msg)) => assert!(
                    msg.contains("cooldown_secs") && msg.contains(&COOLDOWN_SECS_FLOOR.to_string()),
                    "rejection must name the field and the floor, got: {msg}"
                ),
                Ok(_) => panic!("cooldown_secs = {below} must be rejected, not accepted"),
                Err(e) => panic!("cooldown_secs = {below} must be ConfigInvalid, got: {e}"),
            }
        }

        // The floor edge loads and threads through to the timing-strategy base.
        let at_floor = with_tunables(&format!("cooldown_secs = {COOLDOWN_SECS_FLOOR}"));
        let config = Config::parse(&at_floor).unwrap();
        assert_eq!(config.tunables.cooldown_secs, COOLDOWN_SECS_FLOOR);
        assert_eq!(
            config.tunables.cooldown_strategy.base,
            COOLDOWN_SECS_FLOOR as f64
        );
    }

    #[test]
    fn rendered_config_documents_the_cooldown_floor_on_one_clean_line() {
        // Operator-facing (#272): the generated `config.toml` cooldown comment states
        // the non-zero floor range and is a single, cleanly-joined `#` line — a guard
        // that the source line-continuation did not leave a torn double-space.
        let text = Config::parse(VALID).unwrap().render();
        let comment = text
            .lines()
            .find(|l| l.contains("Seconds to wait after a swap"))
            .expect("the cooldown comment must be rendered");
        assert!(
            comment.starts_with("# ") && !comment.contains("  "),
            "cooldown comment must be one clean line, got: {comment:?}"
        );
        assert!(
            comment.contains(&format!("{COOLDOWN_SECS_FLOOR}..=3600")),
            "cooldown comment must document the floor range, got: {comment:?}"
        );
    }

    #[test]
    fn accepts_a_roster_less_config_and_preserves_tunables() {
        // Regression (the `capture` bootstrap bug, #58): a well-formed tunables-only
        // file must PARSE (empty roster) and PRESERVE the operator's tunables, so
        // `capture` can load it to add the first account. The "at least one account"
        // rule is the daemon's `require_roster` precondition, not a parse rejection.
        let config =
            Config::parse("[tunables]\npoll_secs = 120\ntarget_max_session_usage = 80\n").unwrap();
        assert!(config.roster.is_empty());
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.target_max_session_usage, 80);
    }

    #[test]
    fn require_roster_rejects_an_empty_roster_with_the_friendly_empty_state() {
        // The daemon's precondition (#58): an empty roster is the friendly
        // `RosterEmpty` ("nothing captured yet"), the same state the offline `list`
        // reports — not a raw parse/validation error.
        let config = Config::parse("[tunables]\npoll_secs = 60\n").unwrap();
        assert!(matches!(config.require_roster(), Err(Error::RosterEmpty)));
    }

    #[test]
    fn require_roster_accepts_a_populated_roster() {
        let config = Config::parse(VALID).unwrap();
        assert!(config.require_roster().is_ok());
    }

    #[test]
    fn accepts_a_roster_larger_than_the_former_five_cap() {
        // #35: the roster has no fixed upper bound — a config well beyond the
        // former 5-account cap loads and validates.
        let mut toml = String::new();
        for i in 0..8 {
            toml.push_str(&format!(
                "[[account]]\naccount_uuid = \"u{i}\"\nlabel = \"l{i}\"\n"
            ));
        }
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.roster.len(), 8);
    }

    #[test]
    fn rejects_duplicate_uuid() {
        let toml = "[[account]]\naccount_uuid = \"same\"\nlabel = \"a\"\n\
                    [[account]]\naccount_uuid = \"same\"\nlabel = \"b\"\n";
        assert!(matches!(Config::parse(toml), Err(Error::ConfigInvalid(_))));
    }

    // (Pre-#70 there was a `rejects_duplicate_stash` test; the stash is now derived
    // from `account_uuid`, so duplicate stashes cannot occur independently of
    // duplicate uuids — the check, and its test, are gone. See
    // `stash_is_derived_from_account_uuid` and `legacy_stash_field_is_ignored`.)

    #[test]
    fn rejects_empty_label() {
        let toml = "[[account]]\naccount_uuid = \"u\"\nlabel = \"\"\n";
        assert!(matches!(Config::parse(toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn round_trips_render_then_parse() {
        let original = Config::parse(VALID).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.tunables, reparsed.tunables);
        assert_eq!(original.roster, reparsed.roster);
        // The (default) refresh schedule round-trips too (issue #105).
        assert_eq!(original.refresh, reparsed.refresh);
        // …and the (default) [login] settings (issue #135).
        assert_eq!(original.login, reparsed.login);
        // …and the (default) [migration] settings (issue #150).
        assert_eq!(original.migration, reparsed.migration);
    }

    // --- [refresh] schedule (issue #105) ------------------------------------

    #[test]
    fn refresh_defaults_when_table_absent() {
        // No [refresh] table → the feature is ON by default (#409) with its standard defaults.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.refresh, RefreshConfig::default());
        assert!(config.refresh.enabled);
        assert!(config.refresh.accounts.is_empty());
        assert_eq!(config.refresh.cadence_secs, DEFAULT_REFRESH_CADENCE_SECS);
        assert_eq!(
            config.refresh.idle_after_secs,
            DEFAULT_REFRESH_IDLE_AFTER_SECS
        );
        assert_eq!(config.refresh.timeout_secs, DEFAULT_REFRESH_TIMEOUT_SECS);
        assert_eq!(config.refresh.claude_bin, None);
    }

    #[test]
    fn parses_a_custom_refresh_table() {
        let toml = format!(
            "{VALID}\n[refresh]\n\
             enabled = true\n\
             accounts = [\"work\", \"22222222-2222-2222-2222-222222222222\"]\n\
             cadence_secs = 7200\n\
             idle_after_secs = 120\n\
             timeout_secs = 60\n\
             claude_bin = \"/opt/claude/bin/claude\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        assert_eq!(
            config.refresh,
            RefreshConfig {
                systemic_failure_n: 3,
                enabled: true,
                accounts: vec![
                    "work".to_owned(),
                    "22222222-2222-2222-2222-222222222222".to_owned()
                ],
                cadence_secs: 7200,
                idle_after_secs: 120,
                timeout_secs: 60,
                claude_bin: Some(PathBuf::from("/opt/claude/bin/claude")),
                // Absent from the parsed TOML above → the #468 default (proactive keep-warm off).
                proactive_keep_warm: false,
            }
        );
        // The cadence is also the near-expiry horizon, exposed as a Duration.
        assert_eq!(config.refresh.cadence(), Duration::from_secs(7200));
        assert_eq!(config.refresh.idle_after(), Duration::from_secs(120));
        assert_eq!(config.refresh.timeout(), Duration::from_secs(60));
    }

    #[test]
    fn refresh_missing_key_takes_its_default() {
        // A partial [refresh] table fills only the named keys; the rest default — and an absent
        // `enabled` key now takes the on-by-default (#409), not off.
        let toml = format!("{VALID}\n[refresh]\ncadence_secs = 7200\n");
        let config = Config::parse(&toml).unwrap();
        assert!(config.refresh.enabled);
        assert_eq!(config.refresh.cadence_secs, 7200);
        assert_eq!(config.refresh.timeout_secs, DEFAULT_REFRESH_TIMEOUT_SECS);
    }

    #[test]
    fn refresh_explicit_false_still_disables() {
        // Backward-compat (#409): an operator can still opt OUT — an explicit `enabled = false`
        // parses to a disabled refresh even though the default is now on. A present key is never
        // overridden by the on-by-default serde default.
        let toml = format!("{VALID}\n[refresh]\nenabled = false\n");
        let config = Config::parse(&toml).unwrap();
        assert!(!config.refresh.enabled);
    }

    #[test]
    fn refresh_proactive_keep_warm_defaults_off() {
        // Issue #468 / finding #476 predicate C: an absent `proactive_keep_warm` key resolves to
        // OFF even with `[refresh]` maintenance ON — the active account is then kept warm reactively
        // (on a real 401) + recovered by #467, not by the pre-emptive live-canonical mint.
        let toml = format!("{VALID}\n[refresh]\nenabled = true\n");
        let config = Config::parse(&toml).unwrap();
        assert!(config.refresh.enabled);
        assert!(
            !config.refresh.proactive_keep_warm,
            "proactive keep-warm is off by default (#468)"
        );
    }

    #[test]
    fn refresh_proactive_keep_warm_opt_in_parses_and_round_trips() {
        // An operator restores the pre-#468 pre-emptive mint (finding #476 fallback A's base) with
        // an explicit `proactive_keep_warm = true`; a present key is never overridden by the
        // off-by-default serde default, and the opt-in survives the render->parse round trip.
        let toml = format!("{VALID}\n[refresh]\nenabled = true\nproactive_keep_warm = true\n");
        let config = Config::parse(&toml).unwrap();
        assert!(config.refresh.proactive_keep_warm);
        let reparsed = Config::parse(&config.render()).unwrap();
        assert!(
            reparsed.refresh.proactive_keep_warm,
            "the opt-in survives emit->parse (#468)"
        );
    }

    #[test]
    fn refresh_unknown_field_is_rejected() {
        // deny_unknown_fields: a stray key is a parse error, not a silent ignore.
        let toml = format!("{VALID}\n[refresh]\nenabled = true\nthreshold_secs = 99\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn refresh_cadence_out_of_range_is_rejected() {
        // Below the 60 s floor and above the 1-day ceiling both fail, naming the field.
        for bad in ["cadence_secs = 30", "cadence_secs = 100000"] {
            let toml = format!("{VALID}\n[refresh]\n{bad}\n");
            let err = Config::parse(&toml).unwrap_err();
            assert!(
                matches!(&err, Error::ConfigInvalid(msg) if msg.contains("refresh.cadence_secs")),
                "expected a refresh.cadence_secs range error, got {err:?}"
            );
        }
    }

    #[test]
    fn refresh_idle_after_and_timeout_ranges_are_enforced() {
        let idle = format!("{VALID}\n[refresh]\nidle_after_secs = 5000\n");
        assert!(
            matches!(Config::parse(&idle), Err(Error::ConfigInvalid(msg)) if msg.contains("refresh.idle_after_secs"))
        );
        // 0 idle is allowed (refresh as soon as the tick settles).
        let zero = format!("{VALID}\n[refresh]\nidle_after_secs = 0\n");
        assert_eq!(Config::parse(&zero).unwrap().refresh.idle_after_secs, 0);
        let timeout = format!("{VALID}\n[refresh]\ntimeout_secs = 5\n");
        assert!(
            matches!(Config::parse(&timeout), Err(Error::ConfigInvalid(msg)) if msg.contains("refresh.timeout_secs"))
        );
    }

    #[test]
    fn refresh_systemic_failure_n_out_of_range_is_rejected() {
        // The #378 systemic threshold is bounded `1..=100`: a `0` (which would arm the detector
        // before a single failed sweep) and an above-ceiling value both fail, naming the field.
        for bad in ["systemic_failure_n = 0", "systemic_failure_n = 101"] {
            let toml = format!("{VALID}\n[refresh]\n{bad}\n");
            let err = Config::parse(&toml).unwrap_err();
            assert!(
                matches!(&err, Error::ConfigInvalid(msg) if msg.contains("refresh.systemic_failure_n")),
                "expected a refresh.systemic_failure_n range error, got {err:?}"
            );
        }
        // Both inclusive endpoints parse.
        for ok in ["systemic_failure_n = 1", "systemic_failure_n = 100"] {
            let toml = format!("{VALID}\n[refresh]\n{ok}\n");
            assert!(Config::parse(&toml).is_ok(), "{ok} should parse");
        }
    }

    #[test]
    fn empty_claude_bin_collapses_to_none() {
        // A stray `claude_bin = ""` defers to $CLAUDE_BIN/$PATH (None), like omitting it.
        let toml = format!("{VALID}\n[refresh]\nclaude_bin = \"   \"\n");
        assert_eq!(Config::parse(&toml).unwrap().refresh.claude_bin, None);
    }

    #[test]
    fn refresh_round_trips_render_then_parse() {
        // A fully-customised refresh schedule survives render → parse byte-equivalently.
        let toml = format!(
            "{VALID}\n[refresh]\n\
             enabled = true\n\
             accounts = [\"work\"]\n\
             cadence_secs = 5400\n\
             idle_after_secs = 90\n\
             timeout_secs = 120\n\
             claude_bin = \"/usr/local/bin/claude\"\n"
        );
        let original = Config::parse(&toml).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.refresh, reparsed.refresh);
    }

    #[test]
    fn rendered_default_refresh_is_on_with_commented_claude_bin() {
        // The rendered default [refresh] block is enabled (#409) and leaves claude_bin commented
        // (so a fresh `capture` writes a self-documenting, on-by-default block) yet round-trips.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(
            text.contains("[refresh]"),
            "render must emit [refresh]: {text}"
        );
        assert!(
            text.contains("enabled = true"),
            "default refresh must render enabled: {text}"
        );
        assert!(
            text.contains("# claude_bin ="),
            "an unset claude_bin must render commented: {text}"
        );
        assert_eq!(
            Config::parse(&text).unwrap().refresh,
            RefreshConfig::default()
        );
    }

    // --- [login] settings (issue #135) --------------------------------------

    #[test]
    fn login_defaults_when_table_absent() {
        // No [login] table → the default 180 s timeout and no binary override.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.login, LoginConfig::default());
        assert_eq!(config.login.timeout_secs, DEFAULT_LOGIN_TIMEOUT_SECS);
        assert_eq!(config.login.claude_bin, None);
    }

    #[test]
    fn default_login_timeout_matches_the_engine_default() {
        // The config default and the engine's per-call fallback ([`login::DEFAULT_LOGIN_TIMEOUT`])
        // must agree, so an operator who never writes a [login] block gets the SAME 180 s the
        // engine would use standalone. A drift guard: if either constant moves, this fails.
        assert_eq!(
            LoginConfig::default().timeout(),
            crate::login::DEFAULT_LOGIN_TIMEOUT
        );
    }

    #[test]
    fn parses_a_custom_login_table() {
        let toml = format!(
            "{VALID}\n[login]\n\
             timeout_secs = 300\n\
             claude_bin = \"/opt/claude/bin/claude\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        assert_eq!(
            config.login,
            LoginConfig {
                timeout_secs: 300,
                claude_bin: Some(PathBuf::from("/opt/claude/bin/claude")),
            }
        );
        assert_eq!(config.login.timeout(), Duration::from_secs(300));
    }

    #[test]
    fn login_missing_key_takes_its_default() {
        // A partial [login] table fills only the named keys; the rest default.
        let toml = format!("{VALID}\n[login]\ntimeout_secs = 240\n");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.login.timeout_secs, 240);
        assert_eq!(config.login.claude_bin, None);
    }

    #[test]
    fn login_timeout_out_of_range_is_rejected() {
        // Below the 60 s floor and above the 600 s ceiling both fail, naming the field.
        for bad in ["timeout_secs = 59", "timeout_secs = 601"] {
            let toml = format!("{VALID}\n[login]\n{bad}\n");
            let err = Config::parse(&toml).unwrap_err();
            assert!(
                matches!(&err, Error::ConfigInvalid(msg) if msg.contains("login.timeout_secs")),
                "expected a login.timeout_secs range error, got {err:?}"
            );
        }
        // The inclusive bounds themselves are accepted.
        for ok in ["timeout_secs = 60", "timeout_secs = 600"] {
            let toml = format!("{VALID}\n[login]\n{ok}\n");
            assert!(
                Config::parse(&toml).is_ok(),
                "an inclusive bound must be accepted: {ok}"
            );
        }
    }

    #[test]
    fn login_empty_claude_bin_collapses_to_none() {
        // A stray `claude_bin = ""` defers to $CLAUDE_BIN/$PATH (None), like omitting it —
        // the same override-resolver contract as [refresh].claude_bin (issue #135 AC).
        let toml = format!("{VALID}\n[login]\nclaude_bin = \"   \"\n");
        assert_eq!(Config::parse(&toml).unwrap().login.claude_bin, None);
    }

    #[test]
    fn login_unknown_field_is_rejected() {
        // deny_unknown_fields: a stray key is a parse error, not a silent ignore.
        let toml = format!("{VALID}\n[login]\ntimeout_secs = 200\nwait_loop = true\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn login_round_trips_render_then_parse() {
        // A fully-customised [login] block survives render → parse byte-equivalently.
        let toml = format!(
            "{VALID}\n[login]\n\
             timeout_secs = 420\n\
             claude_bin = \"/usr/local/bin/claude\"\n"
        );
        let original = Config::parse(&toml).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.login, reparsed.login);
    }

    #[test]
    fn rendered_default_login_documents_timeout_and_commented_claude_bin() {
        // The rendered default [login] block carries the 180 s timeout and leaves claude_bin
        // commented (a self-documenting, inert override), and round-trips to the default.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(text.contains("[login]"), "render must emit [login]: {text}");
        assert!(
            text.contains("timeout_secs = 180"),
            "default login must render the 180 s timeout: {text}"
        );
        assert!(
            text.contains("# claude_bin ="),
            "an unset login claude_bin must render commented: {text}"
        );
        assert_eq!(Config::parse(&text).unwrap().login, LoginConfig::default());
    }

    #[test]
    fn stash_is_derived_from_account_uuid() {
        // The stash name is a pure function of `account_uuid` (issue #70): there is
        // no stored field to read, so a roster entry with only the required keys
        // still resolves its keychain service name.
        let config =
            Config::parse("[[account]]\naccount_uuid = \"abc-123\"\nlabel = \"work\"\n").unwrap();
        assert_eq!(config.roster[0].stash(), "Sessiometer/abc-123");
    }

    #[test]
    fn legacy_stash_field_is_ignored() {
        // Back-compat (issue #70): a pre-#70 file carrying a `stash = …` line still
        // PARSES (the key is accepted, not rejected by `deny_unknown_fields`), and
        // the stored value is IGNORED — the stash is derived from `account_uuid`. A
        // deliberately mismatched stored value proves the field is not read.
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    stash = \"Sessiometer/STALE-IGNORED\"\n\
                    label = \"work\"\n";
        let config = Config::parse(toml).expect("a legacy stash line must still parse");
        assert_eq!(config.roster[0].stash(), "Sessiometer/u");
    }

    #[test]
    fn rendered_config_omits_the_derived_stash() {
        // `render` no longer emits a `stash = …` line (issue #70), so the next save
        // of a legacy file drops it. The derived stash survives the render→parse
        // round-trip because it rides on `account_uuid`.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(
            !text.contains("stash ="),
            "render must not emit a stash line: {text}"
        );
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.roster[0].stash(), config.roster[0].stash());
    }

    // --- account enable/disable (issue #36) --------------------------------

    #[test]
    fn account_enabled_defaults_to_true_when_the_key_is_absent() {
        // Backward-compat AC: every pre-#36 `[[account]]` omits `enabled`, so an
        // absent key must load fully enabled — VALID's two accounts have no key.
        let config = Config::parse(VALID).unwrap();
        assert!(
            config.roster.iter().all(|a| a.enabled),
            "default is enabled"
        );
    }

    #[test]
    fn account_enabled_false_parses_as_disabled() {
        let toml = "[[account]]\naccount_uuid = \"u\"\nlabel = \"l\"\nenabled = false\n";
        let config = Config::parse(toml).unwrap();
        assert!(!config.roster[0].enabled);
    }

    #[test]
    fn rendered_config_documents_and_round_trips_the_enabled_flag() {
        // The renderer writes `enabled` for every account (capture writes it; #36)
        // with an inline doc, and a disabled account survives a render→parse cycle.
        let mut config = Config::parse(VALID).unwrap();
        config.roster[1].enabled = false;
        let text = config.render();
        assert!(text.contains("enabled = true"), "got {text}");
        assert!(text.contains("enabled = false"), "got {text}");
        assert!(
            text.contains("# In the rotation?"),
            "documents enabled: {text}"
        );
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.roster, config.roster);
        assert!(reparsed.roster[0].enabled);
        assert!(!reparsed.roster[1].enabled);
    }

    // --- timing jitter strategies (issue #38) ------------------------------

    /// A minimal valid roster body with one account and the given `[jitter]`
    /// fragment spliced in (the `[tunables]` table is absent → its defaults).
    fn with_jitter(fragment: &str) -> String {
        format!(
            "[jitter]\n{fragment}\n\
             [[account]]\n\
             account_uuid = \"u\"\n\
             label = \"l\"\n"
        )
    }

    #[test]
    fn poll_jitter_defaults_to_normal_trigger_and_cooldown_stay_fixed() {
        // AC: poll interval uses normal jitter by default; trigger, weekly_trigger
        // and cooldown are fixed unless the operator configures a strategy. Bases
        // mirror the validated scalar tunables.
        let t = Config::parse(VALID).unwrap().tunables;
        assert_eq!(
            t.poll_strategy.jitter,
            Jitter::Normal {
                stddev: DEFAULT_POLL_JITTER_STDDEV
            }
        );
        assert_eq!(t.trigger_strategy.jitter, Jitter::None);
        assert_eq!(t.weekly_trigger_strategy.jitter, Jitter::None);
        assert_eq!(t.cooldown_strategy.jitter, Jitter::None);
        assert_eq!(t.poll_strategy.base, 30.0);
        assert_eq!(t.trigger_strategy.base, 90.0);
        assert_eq!(t.weekly_trigger_strategy.base, 97.0);
        assert_eq!(t.cooldown_strategy.base, 45.0);
    }

    #[test]
    fn default_poll_base_is_longer_than_the_original_sixty_seconds() {
        // AC: the poll interval moves to a LONGER base than the original fixed
        // 60 s, with normal jitter.
        let t = Tunables::default();
        assert!(
            t.poll_secs > 60,
            "default poll base must exceed the old 60 s"
        );
        assert_eq!(t.poll_strategy.base, t.poll_secs as f64);
        assert!(matches!(t.poll_strategy.jitter, Jitter::Normal { .. }));
    }

    #[test]
    fn parses_a_full_jitter_table() {
        let toml = with_jitter(
            "poll = { kind = \"normal\", stddev = 25.0 }\n\
             trigger = { kind = \"uniform\", spread = 2.5 }\n\
             weekly_trigger = { kind = \"normal\", stddev = 1.0 }\n\
             cooldown = { kind = \"none\" }",
        );
        let t = Config::parse(&toml).unwrap().tunables;
        assert_eq!(t.poll_strategy.jitter, Jitter::Normal { stddev: 25.0 });
        assert_eq!(t.trigger_strategy.jitter, Jitter::Uniform { spread: 2.5 });
        assert_eq!(
            t.weekly_trigger_strategy.jitter,
            Jitter::Normal { stddev: 1.0 }
        );
        assert_eq!(t.cooldown_strategy.jitter, Jitter::None);
    }

    #[test]
    fn rejects_every_malformed_jitter_spec() {
        // parse-or-error: each malformed spec is rejected at load.
        for fragment in [
            "poll = { kind = \"gaussian\", stddev = 1.0 }", // unknown kind
            "poll = { kind = \"normal\", stddev = -1.0 }",  // negative magnitude
            "poll = { kind = \"uniform\", spread = -0.1 }", // negative magnitude
            "poll = { kind = \"normal\", spread = 1.0 }",   // wrong key for kind
            "poll = { kind = \"uniform\", stddev = 1.0 }",  // wrong key for kind
            "poll = { kind = \"none\", stddev = 1.0 }",     // none takes no magnitude
            "poll = { kind = \"normal\" }",                 // missing magnitude
            "poll = { kind = \"uniform\" }",                // missing magnitude
        ] {
            assert!(
                matches!(
                    Config::parse(&with_jitter(fragment)),
                    Err(Error::ConfigInvalid(_))
                ),
                "jitter spec should be rejected: {fragment}"
            );
        }
    }

    #[test]
    fn rejects_an_unknown_jitter_field_or_tunable() {
        // deny_unknown_fields: a stray key in a spec is a parse error…
        assert!(matches!(
            Config::parse(&with_jitter(
                "poll = { kind = \"normal\", stddev = 1.0, bogus = 2.0 }"
            )),
            Err(Error::ConfigParse(_))
        ));
        // …and so is an unrecognized tunable name. The jitter tunables are
        // poll/trigger/weekly_trigger/cooldown (issue #41 added weekly_trigger); a
        // bare `weekly` (≠ the actual `weekly_trigger` key) is still unknown.
        assert!(matches!(
            Config::parse(&with_jitter("weekly = { kind = \"none\" }")),
            Err(Error::ConfigParse(_))
        ));
    }

    #[test]
    fn round_trips_a_configured_jitter_table() {
        let toml = with_jitter(
            "poll = { kind = \"uniform\", spread = 12.5 }\n\
             trigger = { kind = \"normal\", stddev = 1.5 }\n\
             weekly_trigger = { kind = \"uniform\", spread = 0.5 }\n\
             cooldown = { kind = \"none\" }",
        );
        let original = Config::parse(&toml).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.tunables, reparsed.tunables);
    }

    #[test]
    fn rendered_config_documents_the_jitter_table() {
        let text = Config::parse(VALID).unwrap().render();
        assert!(text.contains("[jitter]"));
        for key in ["poll", "trigger", "weekly_trigger", "cooldown"] {
            assert!(
                text.contains(key),
                "rendered config must mention jitter.{key}"
            );
        }
        // The default poll jitter renders as a normal strategy with a decimal
        // magnitude (so it re-parses as a TOML float).
        assert!(text.contains("kind = \"normal\""));
        assert!(text.contains("stddev = 60.0"));
    }

    #[test]
    fn round_trips_a_label_that_needs_escaping() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    label = \"tab\\there \\\"quote\\\" and \\\\ slash\"\n";
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.roster[0].label, "tab\there \"quote\" and \\ slash");
        let reparsed = Config::parse(&config.render()).unwrap();
        assert_eq!(reparsed.roster[0].label, config.roster[0].label);
    }

    #[test]
    fn rendered_config_documents_the_tunables() {
        let text = Config::parse(VALID).unwrap().render();
        // AC #5: the written file carries the inline tunable docs, in particular
        // the target_max_session_usage "most-full a target may be to receive the session" semantics.
        assert!(text.contains("The most-full an account may be to receive"));
        for key in [
            "poll_secs",
            "exhausted_poll_secs",
            "near_limit_poll_secs",
            "cooldown_secs",
            "target_max_session_usage",
            "session_trigger",
            "weekly_trigger",
            "session_velocity_horizon_secs",
            "session_velocity_min_project_above",
            "session_velocity_ema_alpha_pct",
            "monitor_401_n",
            "monitor_recovery_m",
        ] {
            assert!(text.contains(key), "rendered config must mention {key}");
        }
        // Issue #76 AC3: the poll_secs comment documents the default cadence + jitter
        // AND the rate-limit / transient back-off (incl. Retry-After) — so an operator
        // hand-editing poll_secs learns the spacing widens automatically under 429/5xx.
        assert!(
            text.contains("The default 300 (5 min)"),
            "poll_secs comment must document the default cadence: {text:?}"
        );
        assert!(
            text.contains("backs off automatically"),
            "poll_secs comment must document the back-off: {text:?}"
        );
        assert!(
            text.contains("Retry-After"),
            "poll_secs comment must document honouring Retry-After: {text:?}"
        );
    }

    #[test]
    fn accessors_map_tunables_to_daemon_inputs() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.poll_interval(), Duration::from_secs(30));
        assert!((config.swap_threshold() - 0.90).abs() < 1e-9);
    }

    /// Pins the full escape surface of [`basic_string`], not just the common cases.
    ///
    /// Written to characterize the hand-rolled emitter BEFORE #403 delegated it to
    /// `toml_writer`, then re-run unchanged against the delegated one: an identical
    /// pass across every escape class is the empirical evidence that the swap is
    /// behavior-preserving. Do not thin it out — each arm below is a distinct branch
    /// of the TOML `basic-unescaped` grammar.
    #[test]
    fn basic_string_escapes_specials() {
        assert_eq!(basic_string("plain"), "\"plain\"");
        assert_eq!(basic_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(basic_string("tab\tnl\n"), "\"tab\\tnl\\n\"");
        assert_eq!(basic_string("\u{0}"), "\"\\u0000\"");

        // The named escapes TOML defines, each on its own.
        assert_eq!(basic_string("\u{08}"), "\"\\b\"");
        assert_eq!(basic_string("\u{0c}"), "\"\\f\"");
        assert_eq!(basic_string("\r"), "\"\\r\"");

        // Remaining C0 controls and DEL take the \uXXXX form, upper-case hex.
        assert_eq!(basic_string("\u{1}"), "\"\\u0001\"");
        assert_eq!(basic_string("\u{1f}"), "\"\\u001F\"");
        assert_eq!(basic_string("\u{7f}"), "\"\\u007F\"");

        // Non-ASCII is valid literally in a basic string — never escaped. This is the
        // arm an operator's label most plausibly exercises (issue #176 wide glyphs).
        assert_eq!(basic_string("café"), "\"café\"");
        assert_eq!(basic_string("работа"), "\"работа\"");
        assert_eq!(basic_string("🟢 work"), "\"🟢 work\"");

        // Space and `'` stay literal; only `"` and `\` are structural.
        assert_eq!(basic_string("a b 'c'"), "\"a b 'c'\"");

        // Empty renders as an empty basic string, not a bare pair of nothing.
        assert_eq!(basic_string(""), "\"\"");
    }

    /// Every string [`Config::render`] emits must survive a render → parse round-trip.
    /// Guards the #403 delegation at the level that actually matters: the emitted file
    /// re-parses to the same values, for the whole escape surface at once.
    ///
    /// The empty string is deliberately absent: `""` escapes fine (pinned above) but
    /// `validate` rejects an empty `label` outright, which is a roster invariant, not an
    /// escaping property.
    #[test]
    fn rendered_strings_round_trip_through_the_parser() {
        for label in [
            "plain",
            "a\"b\\c",
            "tab\there",
            "nl\nhere",
            "cr\rhere",
            "\u{08}\u{0c}",
            "\u{0}\u{1f}\u{7f}",
            "café ☕",
            "🟢 work",
        ] {
            let rendered = basic_string(label);
            let toml = format!("[[account]]\naccount_uuid = \"u\"\nlabel = {rendered}\n");
            let config = Config::parse(&toml)
                .unwrap_or_else(|e| panic!("{label:?} rendered as {rendered} must parse: {e}"));
            assert_eq!(
                config.roster[0].label, label,
                "{label:?} must survive render -> parse unchanged"
            );
        }
    }

    #[test]
    fn accepts_inclusive_bounds() {
        // Each bound's edge is valid: trigger 50/99, target_max_session_usage 1 (the non-zero lower bound;
        // 0 admits no target) and floor == trigger, poll 5/3600, cooldown 5/3600 (5 =
        // the non-zero floor, #272), monitor 1/20.
        for fragment in [
            "session_trigger = 50\ntarget_max_session_usage = 1",
            "session_trigger = 99\ntarget_max_session_usage = 99", // target_max_session_usage == trigger is allowed
            "weekly_trigger = 50",
            "weekly_trigger = 99",
            "poll_secs = 5",
            "poll_secs = 3600",
            "cooldown_secs = 5",
            "cooldown_secs = 3600",
            "monitor_401_n = 1",
            "monitor_401_n = 20",
            "monitor_recovery_m = 1",
            "monitor_recovery_m = 20",
        ] {
            assert!(
                Config::parse(&with_tunables(fragment)).is_ok(),
                "inclusive bound should be accepted: {fragment:?}"
            );
        }
    }

    #[test]
    fn load_path_reports_not_found_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(matches!(
            Config::load_path(&path),
            Err(Error::ConfigNotFound { .. })
        ));
    }

    #[test]
    fn load_path_surfaces_a_malformed_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"][").unwrap();
        assert!(matches!(
            Config::load_path(&path),
            Err(Error::ConfigParse(_))
        ));
    }

    // --- config show --origin (issue #401) ---------------------------------

    /// The provenance test #401 exists for: a file that sets ONLY `session_trigger`
    /// must show that one key `FromFile` and EVERY other tunable — plus every absent
    /// optional section — `Default`, so a silently-defaulted (absent) block is visible.
    #[test]
    fn origin_report_tags_absent_keys_default_and_present_keys_from_file() {
        let text = "[tunables]\nsession_trigger = 90\n";
        let config = Config::from_toml_str(text).expect("a lone session_trigger is valid");
        let table: toml::Table = toml::from_str(text).expect("valid TOML");
        let report = config.origin_report(&table);

        let tunables = &report.sections[0];
        assert_eq!(tunables.header, "[tunables]");
        assert!(tunables.present, "[tunables] is present");
        let by_key = |k: &str| {
            tunables
                .entries
                .iter()
                .find(|e| e.key == k)
                .unwrap_or_else(|| panic!("no `{k}` entry"))
        };
        assert_eq!(by_key("session_trigger").origin, Origin::FromFile);
        assert_eq!(by_key("session_trigger").value, "90");
        // Every OTHER tunable in the present section is still a compiled-in default.
        assert_eq!(by_key("poll_secs").origin, Origin::Default);
        assert_eq!(by_key("target_max_session_usage").origin, Origin::Default);
        assert_eq!(by_key("monitor_401_n").origin, Origin::Default);

        // Every optional section is absent → not present, all values Default.
        for header in ["[jitter]", "[refresh]", "[login]", "[stats]", "[migration]"] {
            let section = report
                .sections
                .iter()
                .find(|s| s.header == header)
                .unwrap_or_else(|| panic!("no `{header}` section"));
            assert!(!section.present, "{header} is absent");
            assert!(
                section.entries.iter().all(|e| e.origin == Origin::Default),
                "{header} keys are all Default when the section is absent",
            );
        }
        assert_eq!(report.roster_count, 0);
        assert!(!report.roster_present, "no [[account]] in the file");
    }

    /// Keys and sections PRESENT in the file read `FromFile`; a key omitted from an
    /// otherwise-present section still reads `Default`; a populated roster is counted
    /// and flagged present.
    #[test]
    fn origin_report_marks_present_sections_keys_and_roster_from_file() {
        let text = "\
[tunables]
poll_secs = 45

[refresh]
enabled = true

[[account]]
account_uuid = \"11111111-1111\"
label = \"work\"
";
        let config = Config::from_toml_str(text).expect("valid config");
        let table: toml::Table = toml::from_str(text).expect("valid TOML");
        let report = config.origin_report(&table);

        let tunables = report
            .sections
            .iter()
            .find(|s| s.header == "[tunables]")
            .unwrap();
        let poll = tunables
            .entries
            .iter()
            .find(|e| e.key == "poll_secs")
            .unwrap();
        assert_eq!(poll.origin, Origin::FromFile);
        assert_eq!(poll.value, "45");
        // Present section, absent key → still Default.
        let cooldown = tunables
            .entries
            .iter()
            .find(|e| e.key == "cooldown_secs")
            .unwrap();
        assert_eq!(cooldown.origin, Origin::Default);

        let refresh = report
            .sections
            .iter()
            .find(|s| s.header == "[refresh]")
            .unwrap();
        assert!(refresh.present);
        let enabled = refresh.entries.iter().find(|e| e.key == "enabled").unwrap();
        assert_eq!(enabled.origin, Origin::FromFile);
        assert_eq!(enabled.value, "true");

        assert_eq!(report.roster_count, 1);
        assert!(report.roster_present);
    }

    /// `load_with_origin` funnels through the SAME parse→validate seam as `load`, so a
    /// bad value fails identically (never a silent default) and an absent file is
    /// `ConfigNotFound` — the read-only diagnostics verb inherits the daemon's contract.
    #[test]
    fn load_with_origin_surfaces_the_same_errors_as_load_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        std::fs::write(&path, "[tunables]\npoll_secs = 1\n").unwrap();
        let err = Config::load_with_origin(&path).expect_err("poll_secs=1 is out of range");
        assert!(matches!(err, Error::ConfigInvalid(_)), "got {err:?}");

        let missing = dir.path().join("nope.toml");
        assert!(matches!(
            Config::load_with_origin(&missing),
            Err(Error::ConfigNotFound { .. })
        ));
    }

    /// End-to-end through disk. A rendered config reports every value `FromFile`
    /// (render writes every key live), so the ONLY way a tunable reads `Default` is a
    /// genuinely absent key — which is exactly why the drift #401 surfaces is real.
    #[test]
    fn load_with_origin_reports_a_rendered_config_all_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let rendered = Config::parse(VALID).unwrap().render();
        std::fs::write(&path, &rendered).unwrap();

        let report = Config::load_with_origin(&path).unwrap();
        for section in &report.sections {
            // `claude_bin` is the one key `render` leaves COMMENTED when unset, so it
            // is legitimately absent (`Default`); every other rendered key is live.
            for entry in &section.entries {
                if entry.key == "claude_bin" {
                    continue;
                }
                assert_eq!(
                    entry.origin,
                    Origin::FromFile,
                    "rendered {}.{} should read FromFile",
                    section.header,
                    entry.key,
                );
            }
        }
    }

    /// The externally-deleted-block scenario #401 names verbatim: a config that OMITS
    /// `[tunables]` entirely (but is otherwise valid) loads fine, and every tunable
    /// reads `Default` with the section flagged absent — the drift made visible.
    #[test]
    fn load_with_origin_surfaces_a_missing_tunables_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[refresh]\nenabled = true\n\n[[account]]\naccount_uuid = \"11111111-1111\"\nlabel = \"work\"\n",
        )
        .unwrap();

        let report = Config::load_with_origin(&path).unwrap();
        let tunables = report
            .sections
            .iter()
            .find(|s| s.header == "[tunables]")
            .unwrap();
        assert!(!tunables.present, "[tunables] is absent from the file");
        assert!(
            tunables.entries.iter().all(|e| e.origin == Origin::Default),
            "a missing [tunables] block reads as all-Default — the #401 drift signal",
        );
        // The present [refresh].enabled still reads FromFile — absence is per-section.
        let refresh = report
            .sections
            .iter()
            .find(|s| s.header == "[refresh]")
            .unwrap();
        assert!(refresh.present);
    }

    /// #401 drift guard, the complement of `..._all_from_file` above: every key `render`
    /// writes for a full config MUST also appear in `origin_report`. Without this, a tunable
    /// added to `render` but forgotten in `origin_report` would be silently DROPPED from
    /// `config show` — the drift most likely as the schema grows (jitter #38, refresh #105,
    /// stats #161, migration #150, target_max_session_usage #398). Asserts `live ⊆ reported`.
    #[test]
    fn origin_report_reports_every_key_render_writes() {
        let config = Config::parse(VALID).unwrap();
        let table: toml::Table = toml::from_str(&config.render()).unwrap();
        let report = config.origin_report(&table);
        for (name, live) in &table {
            // The `[[account]]` roster is summarized, not key-listed — skip the array.
            let Some(live) = live.as_table() else {
                continue;
            };
            let want = format!("[{name}]");
            let section = report
                .sections
                .iter()
                .find(|s| s.header == want.as_str())
                .unwrap_or_else(|| panic!("render writes {want} but origin_report has no section"));
            let reported: std::collections::BTreeSet<&str> =
                section.entries.iter().map(|e| e.key).collect();
            for key in live.keys() {
                assert!(
                    reported.contains(key.as_str()),
                    "render writes {name}.{key} but origin_report omits it — config show would drop it",
                );
            }
        }
    }

    /// AC #3 + #4 end-to-end: a config written the way `capture` will write it
    /// (rendered → `write_private_file`) is read back identically by the daemon's
    /// `load`, and the on-disk file is `0600`.
    #[test]
    fn written_config_round_trips_through_disk_at_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let original = Config::parse(VALID).unwrap();
        paths::write_private_file(&path, original.render().as_bytes()).unwrap();

        let loaded = Config::load_path(&path).unwrap();
        assert_eq!(loaded.tunables, original.tunables);
        assert_eq!(loaded.roster, original.roster);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // --- [stats] block (issue #161) -----------------------------------------

    #[test]
    fn stats_defaults_when_the_table_is_absent() {
        // A config with no `[stats]` table (VALID has none) loads the documented defaults —
        // the same opt-out contract as `[refresh]` / `[login]`.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.stats, StatsConfig::default());
        assert_eq!(config.stats.raw_retention_secs, 14 * 86_400);
        assert_eq!(config.stats.hourly_retention_secs, 90 * 86_400);
        assert_eq!(config.stats.daily_retention_secs, 0); // 0 = lifetime
        assert_eq!(config.stats.default_period, "week");
    }

    #[test]
    fn parses_a_full_stats_override() {
        // Every key set to a non-default the operator chose, all within bounds.
        let toml = format!(
            "{VALID}\n[stats]\n\
             raw_retention_secs = 604800\n\
             hourly_retention_secs = 2592000\n\
             daily_retention_secs = 31536000\n\
             default_period = \"month\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        assert_eq!(
            config.stats,
            StatsConfig {
                raw_retention_secs: 604_800,
                hourly_retention_secs: 2_592_000,
                daily_retention_secs: 31_536_000,
                default_period: "month".to_owned(),
            }
        );
    }

    #[test]
    fn a_partial_stats_table_fills_only_named_keys() {
        // Like `[refresh]`, a partial table sets only the named key; the rest default.
        let toml = format!("{VALID}\n[stats]\ndefault_period = \"lifetime\"\n");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.stats.default_period, "lifetime");
        assert_eq!(config.stats.raw_retention_secs, 14 * 86_400); // untouched → default
        assert_eq!(config.stats.daily_retention_secs, 0);
    }

    #[test]
    fn rendered_default_config_round_trips_the_stats_block() {
        // The rendered default config carries a `[stats]` block that reparses to the same
        // settings — the render → parse round-trip the other blocks hold to.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(text.contains("[stats]"), "render must emit [stats]: {text}");
        assert!(
            text.contains("raw_retention_secs ="),
            "render must document raw retention: {text}"
        );
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.stats, config.stats);
    }

    #[test]
    fn rendered_stats_round_trips_operator_overrides() {
        // Operator-set non-defaults survive render → parse unchanged (defaults + overrides,
        // the issue's round-trip AC).
        let mut config = Config::parse(VALID).unwrap();
        config.stats = StatsConfig {
            raw_retention_secs: 3_600,          // the lower bound
            hourly_retention_secs: 315_360_000, // the upper bound
            daily_retention_secs: 7_776_000,
            default_period: "day".to_owned(),
        };
        let text = config.render();
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.stats, config.stats);
    }

    #[test]
    fn rejects_each_out_of_range_stats_horizon() {
        for (key, value) in [
            ("raw_retention_secs", "3599"),     // below 1h
            ("raw_retention_secs", "31536001"), // above 365d
            ("hourly_retention_secs", "86399"), // below 1d
            ("hourly_retention_secs", "315360001"),
            ("daily_retention_secs", "-1"), // below the 0 = lifetime floor
            ("daily_retention_secs", "315360001"), // above the cap
        ] {
            let toml = format!("{VALID}\n[stats]\n{key} = {value}\n");
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "stats.{key} = {value} should be rejected"
            );
        }
    }

    #[test]
    fn accepts_zero_daily_retention_as_lifetime() {
        // The 0 = lifetime sentinel is IN range (it is the default), not rejected.
        let toml = format!("{VALID}\n[stats]\ndaily_retention_secs = 0\n");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.stats.daily_retention_secs, 0);
    }

    #[test]
    fn rejects_an_unknown_stats_default_period() {
        let toml = format!("{VALID}\n[stats]\ndefault_period = \"fortnight\"\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn rejects_an_unknown_stats_key() {
        // `deny_unknown_fields` rejects a stray key as a parse error, like the other tables.
        let toml = format!("{VALID}\n[stats]\nbogus = 1\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn stats_defaults_match_the_store_retention_policy() {
        // The `[stats]` defaults are the operator-facing source of truth for the store's
        // RetentionPolicy — so the default `[stats]` maps to exactly RetentionPolicy::default()
        // (raw ~14d, hourly ~90d, daily 0 = lifetime), threading the store's own default poll
        // cadence in as the coverage denominator. Guards the two from drifting apart.
        let store_default = RetentionPolicy::default();
        let derived = StatsConfig::default().retention_policy(store_default.poll_interval_secs);
        assert_eq!(derived, store_default);
    }

    #[test]
    fn stats_retention_policy_maps_every_horizon() {
        let stats = StatsConfig {
            raw_retention_secs: 100,
            hourly_retention_secs: 200,
            daily_retention_secs: 300,
            default_period: "week".to_owned(),
        };
        let policy = stats.retention_policy(42);
        assert_eq!(policy.raw_window_secs, 100);
        assert_eq!(policy.hourly_window_secs, 200);
        assert_eq!(policy.daily_window_secs, 300);
        assert_eq!(policy.poll_interval_secs, 42);
        // The daily lifetime sentinel passes straight through.
        assert_eq!(
            StatsConfig::default()
                .retention_policy(42)
                .daily_window_secs,
            0
        );
    }

    // --- [migration] block (issue #150) -------------------------------------

    #[test]
    fn migration_defaults_when_the_table_is_absent() {
        // A config with no `[migration]` table (VALID has none) loads the documented defaults —
        // the same opt-out contract as `[stats]` / `[refresh]` / `[login]`.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.migration, MigrationConfig::default());
        assert_eq!(config.migration.kdf_memory_kib, 65_536);
        assert_eq!(config.migration.kdf_iterations, 3);
        assert_eq!(config.migration.conflict_policy, ConflictPolicy::Skip);
    }

    #[test]
    fn parses_a_full_migration_override() {
        // Every key set to a non-default the operator chose, all within bounds.
        let toml = format!(
            "{VALID}\n[migration]\n\
             kdf_memory_kib = 131072\n\
             kdf_iterations = 4\n\
             conflict_policy = \"overwrite\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        assert_eq!(
            config.migration,
            MigrationConfig {
                kdf_memory_kib: 131_072,
                kdf_iterations: 4,
                conflict_policy: ConflictPolicy::Overwrite,
            }
        );
    }

    #[test]
    fn a_partial_migration_table_fills_only_named_keys() {
        // Like the other blocks, a partial table sets only the named key; the rest default.
        let toml = format!("{VALID}\n[migration]\nconflict_policy = \"overwrite\"\n");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.migration.conflict_policy, ConflictPolicy::Overwrite);
        assert_eq!(config.migration.kdf_memory_kib, 65_536); // untouched → default
        assert_eq!(config.migration.kdf_iterations, 3);
    }

    #[test]
    fn rendered_default_config_round_trips_the_migration_block() {
        // The rendered default config carries a `[migration]` block that reparses to the same
        // settings — the render → parse round-trip the other blocks hold to.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(
            text.contains("[migration]"),
            "render must emit [migration]: {text}"
        );
        assert!(
            text.contains("kdf_memory_kib ="),
            "render must document the KDF cost: {text}"
        );
        assert!(
            text.contains("conflict_policy ="),
            "render must document the conflict policy: {text}"
        );
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.migration, config.migration);
    }

    #[test]
    fn rendered_migration_round_trips_operator_overrides() {
        // Operator-set non-defaults survive render → parse unchanged (defaults + overrides, the
        // issue's round-trip AC). Uses the exact range bounds to prove they render/reparse.
        let mut config = Config::parse(VALID).unwrap();
        config.migration = MigrationConfig {
            kdf_memory_kib: 1_048_576, // the upper bound
            kdf_iterations: 16,        // the upper bound
            conflict_policy: ConflictPolicy::Overwrite,
        };
        let text = config.render();
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.migration, config.migration);

        // …and the lower bounds round-trip too.
        config.migration = MigrationConfig {
            kdf_memory_kib: 8,
            kdf_iterations: 1,
            conflict_policy: ConflictPolicy::Skip,
        };
        let reparsed = Config::parse(&config.render()).unwrap();
        assert_eq!(reparsed.migration, config.migration);
    }

    #[test]
    fn rejects_each_out_of_range_migration_kdf_cost() {
        for (key, value) in [
            ("kdf_memory_kib", "7"),       // below the 8 KiB floor
            ("kdf_memory_kib", "1048577"), // above the 1 GiB decrypt-time guard
            ("kdf_iterations", "0"),       // below 1
            ("kdf_iterations", "17"),      // above the 16 decrypt-time guard
        ] {
            let toml = format!("{VALID}\n[migration]\n{key} = {value}\n");
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "migration.{key} = {value} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_an_unknown_migration_conflict_policy() {
        let toml = format!("{VALID}\n[migration]\nconflict_policy = \"merge\"\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn rejects_an_unknown_migration_key() {
        // `deny_unknown_fields` rejects a stray key as a parse error, like the other tables. In
        // particular there is deliberately no `kdf_parallelism` key (lanes are fixed).
        let toml = format!("{VALID}\n[migration]\nkdf_parallelism = 2\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn migration_kdf_defaults_match_the_crypto() {
        // The `[migration]` KDF defaults are the operator-facing source of truth for the cost
        // `export` derives an encrypted artifact's key at — so the default `[migration]` maps to
        // exactly migration.rs's built-in production cost (`KdfCost::PRODUCTION`). Guards the two
        // from drifting apart (the sibling of `stats_defaults_match_the_store_retention_policy`).
        assert_eq!(
            MigrationConfig::default().kdf_cost(),
            crate::migration::KdfCost::PRODUCTION
        );
    }

    #[test]
    fn migration_kdf_cost_maps_memory_and_time_and_fixes_parallelism() {
        // The operator's memory + time knobs thread into the derived cost; the lane count is
        // fixed at the production 1 (not a config key), so `export` maps to a valid single-lane
        // Argon2id cost regardless of the operator's memory/time choice.
        let migration = MigrationConfig {
            kdf_memory_kib: 131_072,
            kdf_iterations: 5,
            conflict_policy: ConflictPolicy::Skip,
        };
        let cost = migration.kdf_cost();
        assert_eq!(cost.memory_kib, 131_072);
        assert_eq!(cost.iterations, 5);
        assert_eq!(cost.parallelism, 1, "the lane count is fixed at production");
    }
}
