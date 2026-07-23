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

// Per-concern submodules split off from the single ~1,253-line `impl Config` along its
// responsibility seams (issue #638; the same decomposition `daemon` took in #203). Inherent
// impls are collected crate-wide, so every `Config::…` method relocated below stays reachable
// under its existing path — the split is source-compatible for every consumer (cli / daemon /
// capture / migration), and needs none of the re-export blocks `daemon` carries. `config`
// retains the type schema itself and the derived accessors; issue #653 then split the test
// block along these SAME seams, so each child owns the tests for its own concern and only the
// schema / accessor tests stay here.
//
// The seams, in the order a config travels them: `load` — the file/text doors, all funnelling
// through the staging `parse`; `validate` — the one bounds gate every door crosses; `render` —
// the TOML emitter, the `0600` write seam, and `origin_report`; `settings` — the #268
// daemon-routed `config-get` / `config-set` backend; `test_support` — the cfg(test) fixtures
// those test modules share. Declared alphabetically below because `rustfmt` sorts `mod`
// statements, so the lifecycle order lives here.
//
// NOTE: several imports above now have no consumer in THIS file's own body — they are reached
// by the children through `use super::*` (`ToTomlValue` is the subtle one: consumed as a trait
// method, so it greps as unused).
mod load;
mod render;
mod settings;
#[cfg(test)]
mod test_support;
mod validate;

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
/// Default `session_ceiling` percent — the CEILING (issue #597): the settled session line
/// the active account must not cross, NOT a fire-at trigger. Both swap arms (the reactive
/// `observed` estimator and the issue #539 projection) derive their fire point BACKWARD from
/// it — `ceiling − tail_margin − velocity × lookahead` — so a swap lands the outgoing account
/// BELOW the ceiling even after its post-swap committed tail (up to +5 pp, issue #595 — the
/// parked account keeps billing in-flight work). 99 makes the strict `P100 < 99` landing SLO
/// (issue #455 / #595) reachable: the ceiling is the SLO boundary and the tail margin
/// (`swap::TAIL_MARGIN`, 6 pp) keeps the landing under it. See ADR-0023 (the ceiling redesign,
/// superseding ADR-0022).
const DEFAULT_SESSION_CEILING: u8 = 95;
/// Default `target_max_session_usage` percent (issue #398): the default-on swap-target
/// reserve — only swap TO an account whose session usage is below this. Sits
/// below `session_ceiling` so a swapped-to target keeps runway before the next
/// poll; supersedes #10's opt-in (an absent key now means this, not "off").
const DEFAULT_TARGET_MAX_SESSION_USAGE: u8 = 80;
/// Default `weekly_ceiling` percent — the weekly CEILING (issue #607): the settled weekly line the
/// active account must not cross, NOT a fire-at trigger. Separate from and higher than
/// `session_ceiling` (issue #41): the weekly window is the longer, harder limit, so the active
/// account is allowed closer to full on it before a swap-away. The swap fires BACKWARD from this
/// ceiling at `ceiling − swap::WEEKLY_TAIL_MARGIN` (1 pp) so the outgoing account lands BELOW it
/// after its post-swap committed tail — the weekly analogue of what #597 did for `session_ceiling`.
/// 98 leaves the weekly fire point at 97, still well above the session ceiling, preserving the
/// "weekly is the harder limit, allowed closer to full" intent this default has always encoded.
const DEFAULT_WEEKLY_CEILING: u8 = 98;
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
/// arm. Deliberately LOWER than `session_ceiling` — the gate acts preemptively on a
/// stale anchor, before the account would trip the reactive trigger. 60 is the low end
/// of the conservative 60-to-65 band #484 ratified: the #451 replay is flat from 68 %
/// down to 50 %, so the data bounds only the ceiling and the fire-early-is-cheaper
/// asymmetry (a late swap hits the wall on an unattended run; an early one only spends a
/// recoverable-target swap) picks the low end.
const DEFAULT_SESSION_BLIND_RISK_BAND: u8 = 60;
/// Default `session_velocity_horizon_secs` (issue #539, ADR-0017): the projection horizon `H`
/// (seconds) for the velocity-projection preemptive trigger — the active account swaps away when
/// its PROJECTED session usage (`last + velocity × H`) crosses the effective ceiling
/// (`session_ceiling` minus the tail margin, issue #597), before the observed
/// reading does. 120 s ≈ one active poll interval (the post-#366 interleave cadence, NOT the
/// `poll_secs=300` peer cadence), the horizon the #538 spike validated on 22,022 real samples
/// (P50=94 / P100=98 covered-swap, 0 over-fire at H ≤ 150 s). Setting it to `0` disables the path
/// — the projection reduces to `last`, which never crosses (the reactive path already held) — the
/// kill-switch.
const DEFAULT_SESSION_VELOCITY_HORIZON_SECS: u64 = 120;
/// Default `session_velocity_min_project_above` percent (issue #539, ADR-0017): the projective
/// trigger only projects when the observed session reading is at/over this. The #538 spike's FREE
/// guard — projection can't reach below it anyway (max reach ≤ 14 pp at H ≤ 150 s) — so it costs no
/// benefit while excluding spurious low-usage projections. Conventionally BELOW `session_ceiling`
/// (the projection fires in the band beneath the reactive trigger).
const DEFAULT_SESSION_VELOCITY_MIN_PROJECT_ABOVE: u8 = 85;
/// Default `session_velocity_ema_alpha_pct` (issue #539, ADR-0017): the EMA smoothing weight α
/// (percent) applied to the per-account session-velocity signal (#399) — `ema = α·instant +
/// (1-α)·prev` — to damp a single-interval velocity spike so the projection keys off SUSTAINED
/// motion. α ≈ 0.5 (the #538-validated value); 100 means no smoothing (raw last-interval velocity).
const DEFAULT_SESSION_VELOCITY_EMA_ALPHA_PCT: u8 = 50;
/// Default `fleet_runway_warn_secs` (issue #650): the aggregate fleet-runway threshold (seconds)
/// below which the daemon emits the proactive edge-triggered `fleet_runway_low` warning. **`0` =
/// OFF** — the feature is opt-in (the issue's conservative default): the warning keys off the #544
/// fleet-runway aggregate, whose honest-degradation gates make it meaningful only once the usage
/// store has real velocity coverage, so a fresh install should not warn out of the box. A non-zero
/// value opts in at that threshold (e.g. `86400` warns when the roster's combined weekly head-room
/// covers less than a day at the observed burn).
const DEFAULT_FLEET_RUNWAY_WARN_SECS: u64 = 0;

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
    /// is below this percent (`1..=session_ceiling`; an explicit `0` admits no target
    /// and is rejected), so a freshly-swapped target keeps runway before the next
    /// poll. Always valued — an absent key means [`DEFAULT_TARGET_MAX_SESSION_USAGE`], not
    /// "off" (this supersedes #10's opt-in `Option`). Raise it toward `session_ceiling`
    /// to admit busier targets (equal is inert); the always-on session gate
    /// (`session < session_ceiling`, [`crate::daemon`]) still prevents oscillation
    /// independently.
    pub(crate) target_max_session_usage: u8,
    /// The session CEILING (issues #597, #609): the settled session percent (`50..=99`) the active
    /// account must not cross — NOT a fire-at trigger. ONE ceiling; BOTH swap-away estimators of the
    /// same quantity derive their fire point BACKWARD from it, so neither is a separate knob:
    ///   - the reactive arm fires at `observed >= ceiling − tail_margin − velocity × poll_gap`;
    ///   - the issue #539 projection fires at `observed + velocity × H >= ceiling − tail_margin`.
    ///
    /// The two estimators cover DIFFERENT unseen windows — the reactive arm the re-observation gap
    /// (`poll_gap`), the projection the velocity horizon (`H`) — and the daemon composes them (fire at
    /// the earlier), so the swap fires at `observed >= ceiling − tail_margin − velocity × max(poll_gap,
    /// H)`, covering the larger window (issue #609, superseding #597's strict-early-fire framing). This
    /// stays ONE knob, never two: both derive from this single ceiling and share the reserve.
    ///
    /// This IS (near) the landing point now — the point of the redesign: the `tail_margin`
    /// (`swap::TAIL_MARGIN`, 6 pp) is subtracted so the outgoing account lands BELOW the ceiling even
    /// after its post-swap committed tail (measured mean +1.08 pp, max +5 pp, issue #595 — real
    /// in-flight drain, issue #596), and a `velocity × lookahead` term absorbs the climb during the
    /// reactive re-observation gap (`swap::reactive_poll_gap_secs` = `max(2 × near_limit_poll_secs,
    /// REACTIVE_REOBSERVATION_GAP_SECS)`, the measured p90 313 s as a floor since #609) / the projection
    /// horizon `session_velocity_horizon_secs` (`H`). The default ceiling **95** stays *below* the
    /// `P100 < 99` landing SLO (issue #455 / #595) as a conservative lever, but #609 makes 99 REACHABLE:
    /// the reactive `poll_gap` term now looks ahead over at least the real p90 re-observation gap (313 s
    /// floor) rather than the theoretical ~120 s, so the margin is earned by the lookahead, and an
    /// operator who trusts it may raise the ceiling to 99. Full rationale +
    /// tail/coupling evidence: ADR-0023 (the ceiling redesign, superseding ADR-0022) and ADR-0024 (the
    /// gap-percentile lookahead + max-window coverage + downward-only ceiling jitter).
    pub(crate) session_ceiling: u8,
    /// The settled WEEKLY CEILING (`50..=99` percent) — the weekly line the active account must not
    /// cross — and the second, independent ceiling dimension (issue #41). Separate from
    /// `session_ceiling` (no cross-field constraint), typically set higher; the daemon swaps when
    /// EITHER dimension reaches its own fire point.
    ///
    /// Since issue #607 this is a CEILING, not a fire-*at* trigger — the weekly analogue of what
    /// #597 did for `session_ceiling`. The swap fires BACKWARD from it at `ceiling −
    /// swap::WEEKLY_TAIL_MARGIN` (1 pp), so the outgoing account LANDS below this line after its
    /// post-swap committed tail: the same in-flight work that keeps billing the parked account's
    /// session window bills its weekly window too (issue #595 measured the tail on the session axis;
    /// #596 confirmed it is real in-flight drain and saw weekly co-move in 8/13 episodes). The
    /// margin is 1 pp — NOT the session dimension's 6 pp — because the same committed tail is a far
    /// smaller fraction of the weekly BUDGET than of a session window (worst-case `5 pp / k` for the
    /// quota ratio `k = weekly_quota / session_quota`, covered by 1 pp under the stated `k ≥ 5`
    /// assumption — NOT the window-duration ratio, which would justify the margin in the wrong
    /// direction; see `swap::WEEKLY_TAIL_MARGIN` for the full provenance). The two dimensions carry
    /// independently calibrated margins by design.
    pub(crate) weekly_ceiling: u8,
    /// Bounded-blindness preemptive-swap gate threshold `T` (issue #452, ADR-0017), in
    /// seconds: the active account's retained pre-blind anchor (`last_good`, #450) must
    /// be stale beyond this before the gate arms. Promoted from the interim
    /// `BLIND_GATE_SECS` daemon constant (SLI-only until #452). A value at the validated
    /// 86400 ceiling disables the path (no blind window runs that long) — the kill-switch.
    pub(crate) session_blind_swap_secs: u64,
    /// Bounded-blindness preemptive-swap `risk_band` (issue #452, ADR-0017), as a
    /// session-usage percent: the pre-blind anchor must be at/over this for the gate to
    /// arm. Promoted from the interim `BLIND_GATE_RISK_BAND` daemon constant; DISTINCT
    /// from and biased below `session_ceiling` (the gate acts preemptively on a stale
    /// anchor). Conservative 60 by default (#484).
    pub(crate) session_blind_risk_band: u8,
    /// Velocity-projection preemptive-trigger horizon `H` (issue #539, ADR-0017), in seconds:
    /// the active account swaps away when its PROJECTED session usage (`last + velocity × H`,
    /// keyed off the #399 usage-velocity signal) crosses the effective ceiling (`session_ceiling`
    /// minus the tail margin, issue #597), before the observed reading does — closing the OBSERVED
    /// reactive overshoot (#363) #452's blind-window path does not.
    /// `0` disables the path (projection reduces to `last`, never crosses) — the kill-switch.
    pub(crate) session_velocity_horizon_secs: u64,
    /// Velocity-projection guard (issue #539, ADR-0017), as a session-usage percent: the projective
    /// trigger only projects when the observed reading is at/over this. The #538 spike's FREE guard
    /// (projection can't reach lower anyway); conventionally BELOW `session_ceiling`, like
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
    /// Proactive fleet-runway warning threshold, in seconds (issue #650): when the #544
    /// fleet-runway aggregate — the roster's combined weekly head-room over its combined observed
    /// burn — drops BELOW this, the daemon emits ONE edge-triggered `fleet_runway_low` event (the
    /// operator's lead-time signal BEFORE the all-exhausted terminal state, #11). **`0` disables
    /// the path (the kill-switch, and the opt-in default)**; a non-zero value is bounded
    /// `60..=2_592_000` (1 min..30 d — a warn line above 30 days is always-on noise, not a
    /// warning). Purely an operator-visibility signal: no swap decision reads it.
    pub(crate) fleet_runway_warn_secs: u64,
    /// The documented operator override for a FALSE behavioral-canary drift (issue #714). The
    /// canary refuses credential writes on a positive Layer-2 DRIFT (the resolved canonical
    /// credential byte-matches a DIFFERENT account's stash than the one Claude Code's own state
    /// names active); `true` lets those writes PROCEED — logged (`overridden=true` on the durable
    /// `canary_drift` event) and surfaced on `status` — so a false drift on an unattended daemon
    /// is recoverable in minutes (set the key, restart the daemon; re-clear it once the cause is
    /// understood). Overrides Layer-2 DRIFT ONLY: Layer-1 refusals (zero / ambiguous items under
    /// the derived service) have no false-positive story and stay fail-closed. `false` (refuse on
    /// drift) is the default and the safe posture.
    pub(crate) canary_drift_override: bool,
    /// Poll-interval timing strategy (issue #38): base = `poll_secs` (seconds),
    /// normal jitter by default. The daemon draws + clamps to `5..=3600` each
    /// cycle instead of sleeping a fixed interval.
    pub(crate) poll_strategy: Strategy,
    /// Swap-away trigger timing strategy (issue #38), in the PERCENT domain:
    /// base = `session_ceiling`, no jitter unless configured. Drawn + clamped to
    /// `50..=99` each cycle, then divided by 100 for the swap decision.
    pub(crate) session_ceiling_strategy: Strategy,
    /// Weekly swap-away trigger timing strategy (issue #41), in the PERCENT
    /// domain: base = `weekly_ceiling`, no jitter unless configured. Drawn +
    /// clamped to `50..=99` each cycle, then divided by 100 for the swap decision
    /// — the weekly-dimension counterpart of `session_ceiling_strategy`.
    pub(crate) weekly_ceiling_strategy: Strategy,
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
            session_ceiling: DEFAULT_SESSION_CEILING,
            weekly_ceiling: DEFAULT_WEEKLY_CEILING,
            session_blind_swap_secs: DEFAULT_SESSION_BLIND_SWAP_SECS,
            session_blind_risk_band: DEFAULT_SESSION_BLIND_RISK_BAND,
            session_velocity_horizon_secs: DEFAULT_SESSION_VELOCITY_HORIZON_SECS,
            session_velocity_min_project_above: DEFAULT_SESSION_VELOCITY_MIN_PROJECT_ABOVE,
            session_velocity_ema_alpha_pct: DEFAULT_SESSION_VELOCITY_EMA_ALPHA_PCT,
            monitor_401_n: DEFAULT_MONITOR_401_N,
            monitor_recovery_m: DEFAULT_MONITOR_RECOVERY_M,
            fleet_runway_warn_secs: DEFAULT_FLEET_RUNWAY_WARN_SECS,
            // The #714 canary-drift override: OFF (refuse on drift) — the safe posture.
            canary_drift_override: false,
            poll_strategy: Strategy {
                base: DEFAULT_POLL_SECS as f64,
                jitter: default_poll_jitter(),
            },
            session_ceiling_strategy: Strategy::fixed(f64::from(DEFAULT_SESSION_CEILING)),
            weekly_ceiling_strategy: Strategy::fixed(f64::from(DEFAULT_WEEKLY_CEILING)),
            cooldown_strategy: Strategy::fixed(DEFAULT_COOLDOWN_SECS as f64),
        }
    }
}

/// The default poll-interval jitter: normal, so polls decorrelate out of the box
/// (issue #38). Session ceiling, weekly ceiling, and cooldown default to [`Jitter::None`].
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

/// The non-fatal peak-velocity runway finding (issue #608): the configured swap-target reserve
/// admits accounts with no runway at the assumed peak velocity, though a lower reserve would keep
/// some. Produced by [`Config::peak_runway_advisory`] and rendered by `sessiometer config validate`;
/// see that method for why this is an advisory rather than a load error. All three fields are bare
/// tunable values — never secrets (issue #15).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PeakRunwayAdvisory {
    /// The configured reserve, as a whole percent.
    pub(crate) target_max_session_usage: u8,
    /// The highest reserve that keeps peak-velocity runway, as a whole percent (floored).
    pub(crate) bound_pct: u8,
    /// The composed swap lookahead the bound was derived over — `max(reactive poll gap, velocity
    /// horizon)` in seconds — so the operator can see WHICH tunable to lower.
    pub(crate) window_secs: u64,
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
    /// The base poll interval — the un-jittered `poll_secs`. The run loop now
    /// draws a jittered interval each cycle from the poll strategy (issue #38),
    /// so this is a tested accessor for the base rather than the live cadence.
    #[allow(dead_code)]
    pub(crate) fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.tunables.poll_secs)
    }

    /// The session CEILING as a fraction in `[0.0, 1.0]` (issue #597) — `session_ceiling`
    /// / 100. The settled line the active account must not cross; the daemon's swap arms
    /// derive their fire point backward from it (see the `session_ceiling` field).
    ///
    /// The daemon derives its own ceiling / floor / cooldown uniformly from
    /// [`Tunables`] (issue #7), so this Config-level accessor is currently a
    /// tested seam for the `status` view (#9) rather than the run loop.
    #[allow(dead_code)]
    pub(crate) fn swap_threshold(&self) -> f64 {
        f64::from(self.tunables.session_ceiling) / 100.0
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

    /// The non-fatal half of the issue #608 peak-velocity runway coupling: `Some` when the
    /// configured `target_max_session_usage` sits ABOVE the bound
    /// ([`crate::swap::peak_runway_reserve_bound`]) yet the bound is still SATISFIABLE — i.e. a
    /// swapped-to target admitted by this reserve has no runway at the assumed peak velocity, but
    /// some LOWER reserve would. `None` when the reserve already honors the bound.
    ///
    /// Deliberately an accessor, not a parse-time error. `Config::validate` rejects only the
    /// UNSATISFIABLE stack ([`Error::ConfigPeakRunwayUnsatisfiable`]); this case is the one the
    /// SHIPPED DEFAULT is in by design — `target_max_session_usage` 80 against a ~52 bound at the
    /// default ceiling — because ADR-0023 recorded that looseness as accepted (the reserve is
    /// bounded by the ceiling, and the #595 landing SLI plus `TAIL_MARGIN` are the interim guard).
    /// Erroring would brick every stock install; warning on every *load* would fire on every stock
    /// install and so be trained away. It is surfaced instead where an operator goes to ask exactly
    /// this question — `sessiometer config validate` — and nowhere else.
    ///
    /// Returns the bound as a whole PERCENT, floored, so it reads in the same unit as the
    /// `target_max_session_usage` it is compared against (flooring keeps the advisory's suggested
    /// value strictly inside the bound rather than one rounding step past it).
    pub(crate) fn peak_runway_advisory(&self) -> Option<PeakRunwayAdvisory> {
        let t = &self.tunables;
        let bound = crate::swap::peak_runway_reserve_bound(
            f64::from(t.session_ceiling) / 100.0,
            t.near_limit_poll_secs,
            t.session_velocity_horizon_secs,
        );
        // A non-positive bound is the parse-time error's business, not this advisory's — it cannot
        // reach a loaded `Config`. Guarding anyway keeps the accessor total if it is ever called on
        // a hand-built value (the tests do), and keeps the `as u8` floor below non-negative.
        if bound <= 0.0 {
            return None;
        }
        let bound_pct = (bound * 100.0).floor() as u8;
        (t.target_max_session_usage > bound_pct).then_some(PeakRunwayAdvisory {
            target_max_session_usage: t.target_max_session_usage,
            bound_pct,
            // The SAME lookahead the bound above was derived over — one source of truth, so the
            // reported window and the bound can never describe different composed lookaheads.
            window_secs: crate::swap::composed_swap_lookahead_secs(
                t.near_limit_poll_secs,
                t.session_velocity_horizon_secs,
            ) as u64,
        })
    }
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
    pub(crate) session_ceiling: Option<i64>,
    #[serde(default)]
    pub(crate) weekly_ceiling: Option<i64>,
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
    #[serde(default)]
    pub(crate) fleet_runway_warn_secs: Option<i64>,
    /// Issue #714: the canary-drift override switch — the one non-integer settable (a plain
    /// bool; there is no range to validate, so the overlay writes it straight through).
    #[serde(default)]
    pub(crate) canary_drift_override: Option<bool>,
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
    pub(crate) session_ceiling: u8,
    pub(crate) weekly_ceiling: u8,
    pub(crate) session_blind_swap_secs: u64,
    pub(crate) session_blind_risk_band: u8,
    pub(crate) session_velocity_horizon_secs: u64,
    pub(crate) session_velocity_min_project_above: u8,
    pub(crate) session_velocity_ema_alpha_pct: u8,
    pub(crate) monitor_401_n: u8,
    pub(crate) monitor_recovery_m: u8,
    pub(crate) fleet_runway_warn_secs: u64,
    /// Issue #714: whether the canary-drift override is set (a plain bool; `#[serde(default)]`
    /// so a pre-#714 daemon's view decodes to `false` — refuse on drift).
    #[serde(default)]
    pub(crate) canary_drift_override: bool,
}

impl From<&Tunables> for TunablesView {
    fn from(t: &Tunables) -> Self {
        Self {
            poll_secs: t.poll_secs,
            exhausted_poll_secs: t.exhausted_poll_secs,
            near_limit_poll_secs: t.near_limit_poll_secs,
            cooldown_secs: t.cooldown_secs,
            target_max_session_usage: t.target_max_session_usage,
            session_ceiling: t.session_ceiling,
            weekly_ceiling: t.weekly_ceiling,
            session_blind_swap_secs: t.session_blind_swap_secs,
            session_blind_risk_band: t.session_blind_risk_band,
            session_velocity_horizon_secs: t.session_velocity_horizon_secs,
            session_velocity_min_project_above: t.session_velocity_min_project_above,
            session_velocity_ema_alpha_pct: t.session_velocity_ema_alpha_pct,
            monitor_401_n: t.monitor_401_n,
            monitor_recovery_m: t.monitor_recovery_m,
            fleet_runway_warn_secs: t.fleet_runway_warn_secs,
            canary_drift_override: t.canary_drift_override,
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
    #[serde(default = "default_session_ceiling")]
    session_ceiling: i64,
    #[serde(default = "default_weekly_ceiling")]
    weekly_ceiling: i64,
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
    // Issue #650: an absent key resolves to `0` — the proactive fleet-runway warning OFF, the
    // opt-in default.
    #[serde(default = "default_fleet_runway_warn_secs")]
    fleet_runway_warn_secs: i64,
    // Issue #714: the documented canary-drift override — an absent key resolves to `false`
    // (refuse credential writes on a positive Layer-2 drift), the safe posture. A plain bool
    // (not an integer knob): it is a switch with no range to validate.
    #[serde(default)]
    canary_drift_override: bool,
}

impl Default for RawTunables {
    fn default() -> Self {
        Self {
            poll_secs: default_poll_secs(),
            exhausted_poll_secs: default_exhausted_poll_secs(),
            near_limit_poll_secs: default_near_limit_poll_secs(),
            cooldown_secs: default_cooldown_secs(),
            target_max_session_usage: None,
            session_ceiling: default_session_ceiling(),
            weekly_ceiling: default_weekly_ceiling(),
            session_blind_swap_secs: default_session_blind_swap_secs(),
            session_blind_risk_band: default_session_blind_risk_band(),
            session_velocity_horizon_secs: default_session_velocity_horizon_secs(),
            session_velocity_min_project_above: default_session_velocity_min_project_above(),
            session_velocity_ema_alpha_pct: default_session_velocity_ema_alpha_pct(),
            monitor_401_n: default_monitor_401_n(),
            monitor_recovery_m: default_monitor_recovery_m(),
            fleet_runway_warn_secs: default_fleet_runway_warn_secs(),
            canary_drift_override: false,
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
fn default_session_ceiling() -> i64 {
    i64::from(DEFAULT_SESSION_CEILING)
}
fn default_weekly_ceiling() -> i64 {
    i64::from(DEFAULT_WEEKLY_CEILING)
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
fn default_fleet_runway_warn_secs() -> i64 {
    DEFAULT_FLEET_RUNWAY_WARN_SECS as i64
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
    session_ceiling: Option<RawJitterSpec>,
    #[serde(default)]
    weekly_ceiling: Option<RawJitterSpec>,
    #[serde(default)]
    cooldown: Option<RawJitterSpec>,
}

/// One tunable's jitter spec: a `kind` plus its magnitude (`spread` for uniform,
/// `stddev` for normal). Both magnitudes are kept optional and wide here so a
/// kind/magnitude mismatch reaches [`parse_jitter`](validate::parse_jitter) as a clear
/// domain error rather than a bare `serde` type error. Magnitudes are TOML floats (write a
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
    use crate::config::test_support::*;

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
    fn accessors_map_tunables_to_daemon_inputs() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.poll_interval(), Duration::from_secs(30));
        assert!((config.swap_threshold() - 0.90).abs() < 1e-9);
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
