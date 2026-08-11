// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Status-snapshot assembly: the non-secret `status` snapshot + wire types and the pure
//! projections that build them (issue #203, the #195 per-concern decomposition).
//!
//! [`StatusSnapshot`] is the daemon's per-cycle reading set; [`status_response`] projects it
//! into the [`StatusResponse`] wire reply — handles + percentages + the `next_swap` candidate,
//! never a token or email (the #15 discipline). [`credential_health`] is the pure 5-state
//! rollup the display snapshot and the transition-event diff share. Each item is re-exported
//! under `crate::daemon::*`, so relocating them is source-compatible for every existing consumer
//! (cli / poke / use_account) and for the in-module test suite (`mod tests`' `use super::*`).

use serde::{Deserialize, Serialize};

use super::*;

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
    /// Wall-clock epoch SECONDS at which the daemon assembled this snapshot (issue #164) — the
    /// freshness stamp the frozen wire contract carries so a read-only client (e.g. a menubar
    /// app) can tell a LIVE snapshot from a STALE one: a healthy daemon advances it every cycle,
    /// a wedged or dead one stops, so a client compares it against its own clock and greys out
    /// once the gap grows. Stamped in [`Daemon::snapshot`] from the same `now_secs` the #119
    /// health rollup reads, so ONE wall-clock read backs the whole cycle. Epoch seconds — the
    /// unit the rest of the wire already speaks (`access_expires_at`, `session_resets_at`).
    /// `0` by `Default` (an all-defaults snapshot has no generation instant).
    pub(crate) generated_at: i64,
    /// The daemon-level SYSTEMIC refresh-health indicator (issue #378): `Some(n)` while the
    /// refresh MECHANISM is down — `n` consecutive sweeps failed with `outcome=error` across every
    /// eligible account, past the configured threshold — else `None`. Copied from
    /// [`SystemicRefreshHealth::status`](crate::systemic_refresh::SystemicRefreshHealth::status) at
    /// build. Distinct from the per-account [`AccountReading::health`] `at_risk` rollup: it
    /// reflects the whole mechanism, visible without waiting for an account to die. `None` by
    /// `Default` (an all-defaults snapshot reads as healthy). A COUNT only — never a token,
    /// path, or email (the #15 discipline).
    pub(crate) systemic_refresh: Option<u32>,
    /// WHICH opening bracket opened the active systemic-refresh episode (issue #813): `Some(Sweep |
    /// Preflight)` exactly when [`Self::systemic_refresh`] is `Some`, else `None`. Copied from
    /// [`SystemicRefreshHealth::source`](crate::systemic_refresh::SystemicRefreshHealth::source) at
    /// build, in lockstep with the count — both read the SAME latch, so they cannot disagree about
    /// whether an episode is active. Lets each surface phrase a preflight-opened episode without
    /// asserting a sweep that never ran. `None` by `Default` (an all-defaults snapshot reads as
    /// healthy). A FIXED-TOKEN classification only — never a path, token, or email (the #15
    /// discipline).
    pub(crate) systemic_refresh_source: Option<SystemicRefreshSource>,
    /// The daemon-level CANONICAL-SCRUB rollup (issue #516): `Some(Recovering | Exhausted)` while the
    /// shared canonical item is scrubbed, else `None` when healthy. Computed in [`Daemon::snapshot`]
    /// from the edge-latched scrub signals (`signaled_canonical_scrubbed` / `signaled_scrub_adopt_exhausted`);
    /// [`status_response`] copies it straight onto the wire. `None` by `Default` (an all-defaults
    /// snapshot reads as healthy). A STATE discriminant only — never a token or email (the #15 discipline).
    pub(crate) canonical_scrub: Option<CanonicalScrub>,
    /// The daemon-level KEYCHAIN-LOCKED rollup (issue #498): `true` while the macOS login keychain is
    /// LOCKED, so the daemon cannot READ the shared credential item at ALL (access denied) — distinct
    /// from `canonical_scrub`, where the item IS readable but its token was scrubbed/emptied (#469/#463).
    /// Computed in [`Daemon::snapshot`] from the edge-latched `signaled_keychain_locked` signal;
    /// [`status_response`] copies it straight onto the wire. `false` by `Default` (an all-defaults
    /// snapshot reads as unlocked/healthy). A bare BINARY state discriminant — never a token or email
    /// (the #15 discipline). The remedy the operator sees (unlock the keychain) is the surfacing
    /// consumer's concern (the menubar #498 card), NOT this wire increment's.
    pub(crate) keychain_locked: bool,
    /// A just-fired #452 bounded-blindness preemptive swap to NARRATE (issue #479), or `None` when
    /// no such swap is recent-and-still-current. Resolved daemon-side in [`Daemon::snapshot`] from the
    /// retained `last_blind_preempt_swap` record, projected `Some` only within the
    /// `BLIND_PREEMPT_NOTICE_SECS` window AND while the swap's target is still the active account (a
    /// superseding swap self-invalidates it); [`status_response`] copies it straight onto the wire.
    /// `None` by `Default`.
    pub(crate) recent_blind_preempt_swap: Option<BlindPreemptSwap>,
    /// A just-observed RUNTIME landing overshoot to surface (issue #613), or `None` when none is
    /// recent. Resolved daemon-side in [`Daemon::snapshot`] from the retained `last_landing_overshoot`
    /// record, projected `Some` only within the `LANDING_OVERSHOOT_NOTICE_SECS` window;
    /// [`status_response`] copies it straight onto the wire. `None` by `Default`.
    pub(crate) recent_landing_overshoot: Option<LandingOvershoot>,
    /// The behavioral canary's LAST verdict (issue #714), or `None` until the first canary run
    /// concludes (and on a canary that could not run at all — e.g. a boot under a locked
    /// keychain: no evidence is not a verdict). Copied from the daemon's carried canary state in
    /// [`Daemon::snapshot`]; [`status_response`] copies it straight onto the wire. `None` by
    /// `Default`. Labels only — never a token or email (the #15 discipline).
    pub(crate) canary: Option<CanaryStatus>,
    /// The fleet-level synchronized-expiry cohort condition (issue #879), or `None` when no cohort
    /// reaches the foresight horizon. Resolved in [`Daemon::snapshot`] by [`expiry_cohorts`] over
    /// the deadlines every account's [`AccountReading::expiry`] already carries — ONE walk that also
    /// stamps each member's [`AccountExpiry::cohort_id`], so the row grouping and the fleet
    /// statement cannot disagree about who is in what. [`status_response`] copies it straight onto
    /// the wire. `None` by `Default`. Counts and instants only (the #15 discipline).
    pub(crate) expiry_cohort: Option<ExpiryCohort>,
}

/// The non-secret refresh-health inputs `status` surfaces in `--json` (issue #119): the
/// daemon's reduced projection of the refresh observations its per-account health state
/// carries — whether the last refresh kept the credential alive, whether CC rotated the
/// refresh-token VALUE where an exchange ran at all, and the consecutive-failure streak. `None`
/// (the whole struct) until the refresh engine has observed the account at least once (e.g. the
/// `[refresh]` feature is off, or the account has not yet been swept). Every field is a boolean /
/// count — never a token or expiry (the #15 discipline). Derives `Deserialize` so the `status`
/// client can read it back; `#[serde(default)]` on the carrying field handles a pre-#119 daemon.
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
    ///
    /// `None`, and OMITTED from the wire, on every outcome that ran no token exchange —
    /// `no_change`, `dead`, `error` (issue #1070). Rotation is decided by comparing the seeded
    /// refresh token against the one that came back, so an outcome with no exchange has no such
    /// pair to compare and any value here would be fabricated, not observed. `Some(false)` is
    /// therefore a real measurement ("an exchange ran and returned the same token"), which is
    /// exactly what absence is not — the distinction the pre-#1070 `bool` could not draw, and the
    /// reason R-5a called the derived `false` "the exact uninformative value R-5 removes, now on a
    /// versioned surface".
    ///
    /// The sole constructor is [`refresh_health_view`], which reads it off
    /// [`RefreshEventOutcome::rotated`] — the same accessor the three log lines use, so the wire
    /// and the log cannot disagree about which outcomes carry the signal. Note what the shape does
    /// and does not buy: the LITERAL `RefreshHealth { last_ok: false, rotated: true }` the issue
    /// names no longer type-checks, but `last_ok` and `rotated` remain independent fields, so
    /// `rotated: Some(_)` beside `last_ok: false` is still expressible by a hand-written
    /// construction. Nothing in the daemon writes one — and
    /// `refresh_health_view_never_pairs_a_rotation_with_a_non_refreshed_outcome` pins that across
    /// every variant — but the guarantee is a constructor invariant, not a type-level one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rotated: Option<bool>,
    /// Consecutive refresh FAILURES (`dead` / `error` outcomes), reset to 0 by the next
    /// alive refresh — the rollup's at-risk input.
    pub(crate) consecutive_failures: u32,
}

/// The active account's BOUNDED-BLINDNESS state (issue #479, umbrella #363 Path B) — present only
/// when the active account has gone blind (its `/oauth/usage` poll is failing / backing off, so its
/// live reading is cleared) AND the daemon still holds a retained pre-blind anchor (`last_good`,
/// #450). Surfaced so `status` renders a SEMANTIC line — blind duration, last-known session %, and
/// whether ADR-0017 auto-protection is OK or DEGRADED — instead of the content-free `n/a … 🟡` a
/// bare failed-poll row shows. The surface only REFLECTS this daemon-pushed state; it never
/// self-polls or self-swaps (the #169 UI-never-acts invariant). Non-secret — a duration and two
/// small numbers, never a token or email (issue #15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BlindActive {
    /// Seconds the active account has been blind — `blind_elapsed`, measured from the retained
    /// pre-blind anchor's observation instant (`last_good.at`, #450) to snapshot assembly on the
    /// daemon's MONOTONIC clock (the SAME clock #452's gate and the swap cooldown use). A DURATION
    /// (not an absolute instant, which an [`std::time::Instant`] cannot cross the socket as), so the
    /// client renders it verbatim against nothing.
    pub(crate) blind_secs: u64,
    /// The retained pre-blind SESSION-window usage percent (`0..=100`) the anchor holds
    /// (`last_good.session`, #450) — the last-known reading before the account went blind. This is
    /// why the row stops reporting "no data": the daemon DID retain a reading.
    pub(crate) last_known_session_pct: u8,
    /// Whether ADR-0017 preemptive auto-protection is DEGRADED for this blind account — `true` when ANY
    /// of the daemon's three report arms is active (see `blind_active_view`): the ANCHOR arm (blind past
    /// `BLIND_GATE_SECS` AND the anchor at/over `BLIND_GATE_RISK_BAND` — the #452 swap's premise), the
    /// #582 SERVER-directed arm (a `Retry-After` still holding the account off its poll), or the #584
    /// VELOCITY-projection arm (the anchor, projected forward over the blind window at its retained #539
    /// rate, could plausibly reach the trigger — a below-band burn the anchor arm cannot see). `false` =
    /// OK: blind, but not yet past the gate threshold and with no arm tripped — auto-protection is
    /// nominally intact. The first two arms front a swap the daemon fires; the velocity arm is
    /// report-only (issue #584), so DEGRADED here means "compromised", not always "a swap is acting".
    pub(crate) auto_protection_degraded: bool,
}

/// A just-fired #452 bounded-blindness PREEMPTIVE swap (issue #479, umbrella #363 Path B), retained
/// so `status` can NARRATE it — present only for a bounded window after the daemon swapped a BLIND
/// active account away on its stale pre-blind anchor (ADR-0017), and only while that swap's TARGET is
/// still the active account (a superseding swap self-invalidates it, projected daemon-side in
/// [`Daemon::snapshot`]). A swap off a blind account on a stale reading is exactly the event an
/// operator most needs narrated — so they can UNDO it (`use <from_label>`) if the swapped-away account
/// turns out to have recovered. Carried on the wire so `status` renders the SAME information the durable
/// `event=swap … reason=blind_preempt` log line already holds — source, last-known session %, target —
/// each medium in its own idiom (R-2 STATE-parity, as `canonical_scrub` / `next_swap` do). The undo
/// verb is DERIVED (`use <from_label>`), never stored — the surface only REFLECTS this daemon-pushed
/// state, it never self-swaps (the #169 UI-never-acts invariant). Non-secret — two operator handles
/// and a small number, never a token or email (issue #15).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BlindPreemptSwap {
    /// The operator handle (label) the daemon swapped AWAY FROM — the blind account. The undo the
    /// surface names is `use <from_label>` (derived, not stored). Never the email (issue #15).
    pub(crate) from_label: String,
    /// The operator handle (label) the daemon swapped TO — the account now active. Never the email
    /// (issue #15).
    pub(crate) to_label: String,
    /// The stale pre-blind SESSION-window usage percent (`0..=100`) the gate FIRED on — the same
    /// `to_pct(anchor.session)` the `event=swap … session_pct=` log line records and the Part-1
    /// [`BlindActive::last_known_session_pct`] shows, captured at swap-time (by projection time the
    /// anchor `last_good` has been reset to `None`). Gives R-2 content-parity across all three surfaces.
    pub(crate) last_known_session_pct: u8,
}

/// A just-observed RUNTIME landing-point overshoot (issue #613), retained so `status` can surface it —
/// present only for a bounded window ([`crate::landing::LANDING_OVERSHOOT_NOTICE_SECS`]) after THIS
/// daemon observed a recently-parked (`reason=session`) account climb to the SLO ceiling within the
/// landing window ([`crate::landing::LANDING_WINDOW`]). It is the RUNTIME mirror of the offline #595
/// landing SLI's `P100 < 99` breach: the swap redirects only NEW requests, so the parked account's
/// in-flight committed tail keeps billing and can land it over the SLO after an on-target swap —
/// invisible to SLI 1 (the swap-DECISION reading) and, until this, visible only in a later offline
/// `sessiometer reliability` run. Carried on the wire so `status` renders the local breach the same
/// way the durable landing SLI would (each medium in its own idiom, R-2 STATE-parity, as
/// `canonical_scrub` / `recent_blind_preempt_swap` do). Best-available, still per-machine: a
/// co-consuming second machine's tail is invisible to it (the single-machine-sync boundary — see
/// [`crate::landing`]). The surface only REFLECTS this daemon-pushed state; it never self-acts (the
/// #169 UI-never-acts invariant). Non-secret — one operator handle and two small percents, never a
/// token or email (issue #15).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LandingOvershoot {
    /// The operator handle (label) of the parked account that overshot — the one the daemon swapped
    /// AWAY FROM and then observed climb past the ceiling. Never the email (issue #15).
    pub(crate) from_label: String,
    /// The session percent (`0..=100`) the swap FIRED on — the swap-DECISION reading (`session_pct=`
    /// on the `event=swap … reason=session` line). Below the SLO ceiling for a true post-swap tail;
    /// paired with `landing_pct` it shows the tail's size (fired at X, landed at Y).
    pub(crate) decision_pct: u8,
    /// The session percent (`0..=100`) the parked account actually LANDED at — the live peak this
    /// machine observed within the landing window, at/over the [`crate::landing::LANDING_SLO_CEILING_PCT`]
    /// SLO ceiling (that crossing is what fired this notice).
    pub(crate) landing_pct: u8,
}

/// The daemon-level CANONICAL-SCRUB rollup (issue #516, umbrella #463) — present only while the
/// shared `Claude Code-credentials` canonical item is SCRUBBED (its refresh token cleared): the
/// fleet-wide lockout NO per-account `credential_dead` fires for (the shared item is emptied while
/// account rows can still read perfectly healthy). Surfaced so `status` + the menubar (issue #469)
/// can render the scrubbed / un-recoverable state that no per-account `auth` rollup, and no #479
/// `blind_active`, reflects — a signal only the DAEMON holds (it lives in the durable event log,
/// #464/#467, never on the frozen wire until this field). Distinct from the ADR-0016
/// `ActiveDeadNoTarget` case (which IS wire-derivable from `next_swap` + a dead active row).
///
/// Internally tagged on `state` (mirroring [`NextSwap`]), so a future per-variant field — e.g. the
/// roster handle the `canonical_scrubbed` / `canonical_recovery_exhausted` events already carry — is
/// an ADDITIVE change rather than a breaking `string → object` reshape. A fleet-wide STATE
/// discriminant only: never per-account, never a token or email (issue #15).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum CanonicalScrub {
    /// The canonical is scrubbed, but the daemon's autonomous adopt-recovery is still in progress
    /// (issue #467): a viable spare's known-live token may yet be adopted into the emptied canonical,
    /// healing the fleet with no operator action. The lower-severity, self-may-heal state.
    Recovering,
    /// The canonical is scrubbed AND recovery is EXHAUSTED (issue #467): the bounded adopt churn hit
    /// its cap (or no viable adopt target exists), so the daemon has BACKED OFF and the canonical
    /// stays empty until a `claude /login` re-authenticates it. The residual UN-RECOVERABLE state
    /// #469 renders with that remedy. Ranks above [`Self::Recovering`] (most-severe wins).
    Exhausted,
}

/// WHICH of a systemic-refresh episode's two opening brackets opened the ACTIVE episode (issue
/// #813) — the wire's half of a distinction the event log has drawn since issue #787 but the
/// snapshot could not.
///
/// An episode opens EITHER by the #378 sweep crossing (`refresh_systemic_failure`) or by the #787
/// startup preflight failing to resolve the `claude` binary (`refresh_preflight_unresolved`); it
/// closes one way, on the first working sweep. Without this discriminant a client holding only the
/// [`StatusResponse::systemic_refresh_failure`] count cannot tell them apart, and the count alone
/// does not carry it: the preflight path SEEDS the count at one (see
/// [`SystemicRefreshHealth::note_preflight`](crate::systemic_refresh::SystemicRefreshHealth::note_preflight))
/// so pre-#813 renderers stay grammatical, which is indistinguishable on the wire from a genuine
/// one-sweep crossing under `systemic_failure_n = 1`. Every renderer therefore asserted "1
/// consecutive sweep failed" for an episode in which ZERO sweeps had run — a fabricated
/// observation in a signal whose entire purpose is diagnosability.
///
/// The count STAYS alongside this field rather than being re-derived: an older client keeps
/// decoding exactly the bytes it decodes today (the field it knows is unchanged, the one it does
/// not know it ignores), so the accuracy fix costs no compatibility. What changes is that a
/// #813-aware renderer now branches on PROVENANCE and stops citing the count as a sweep count on
/// the preflight arm.
///
/// Externally tagged as a bare string (`"sweep"` / `"preflight"`) rather than internally tagged
/// like [`CanonicalScrub`]: the two variants are fieldless CLASSIFICATIONS with no per-variant
/// payload to grow into — deliberately so, per issue #15. A FIXED TOKEN from a closed two-value
/// set, never a path, a binary location, a token, or an email; in particular the preflight's
/// unresolvable-binary evidence stays on the daemon log, and only the fact of its class reaches
/// the wire.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SystemicRefreshSource {
    /// The episode opened on the #378 SWEEP crossing: `systemic_failure_n` consecutive sweeps in a
    /// row failed with `outcome=error` across every eligible account. The count is then a true
    /// sweep count, and a renderer may cite it as one.
    Sweep,
    /// The episode opened on the #787 startup PREFLIGHT: the `claude` binary could not be resolved,
    /// so every eligible account's refresh cycle is guaranteed to fail before a single one runs. No
    /// sweep produced this verdict, so a renderer must NOT cite the count as a sweep count — that
    /// is the entire point of this discriminant (issue #813).
    Preflight,
}

/// The behavioral canary's LAST verdict (issue #714) — did the reverse-engineered #100 keychain
/// derivation still point at the credential Claude Code is actually using, the last time the
/// canary ran (daemon boot + every pre-swap re-check)? Layer 1 is the fresh service-resolution
/// uniqueness probe; Layer 2 is the offline stash-token identity cross-check (the decided
/// option-C oracle — see [`crate::canary`]).
///
/// Internally tagged on `verdict` (mirroring [`CanonicalScrub`]'s `state`), so future per-variant
/// fields are ADDITIVE rather than a breaking `string → object` reshape. Doubles as the daemon's
/// own carried canary state (`DecisionState`) — one type, resolved to operator LABELS at
/// detection time, so no surface downstream of it can leak a token, email, or account-uuid
/// (issue #15).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "verdict")]
pub(crate) enum CanaryStatus {
    /// Positive Layer-2 pass: the resolved canonical token byte-matches the displayed active
    /// account's own stash.
    Ok,
    /// No positive identity evidence either way (the canonical matches no stash — overwhelmingly
    /// an in-place token refresh — or `~/.claude.json` names no roster account): the canary
    /// FAILS OPEN to Layer-1-only protection. The honest surface of the offline oracle's
    /// documented residual (see [`crate::canary`] Layer 3): identity is UNVERIFIED, not verified.
    Inconclusive,
    /// Layer 1: ZERO items under the derived service — a service-name derivation change, or a
    /// scrubbed keychain (the [`CanonicalScrub`] machinery carries the scrub remedy; this verdict
    /// records that the canary saw it too).
    NotFound,
    /// Layer 1: MORE THAN ONE item under the derived service — the #100 uniqueness rule fails,
    /// so credential writes are refused (no unique, safe target for the atomic in-place write).
    Ambiguous {
        /// How many service-matching items the fresh enumeration found.
        count: usize,
    },
    /// Layer 2 DRIFT: the resolved canonical token byte-matches a DIFFERENT account's stash than
    /// the one Claude Code's own state names active. Credential writes are refused pre-mutation
    /// (zero writes) unless `overridden`.
    Drift {
        /// Label of the account `~/.claude.json` names active.
        displayed: String,
        /// Label of the account whose stashed token the canonical actually matches.
        matched: String,
        /// Whether the documented `canary_drift_override` tunable was set, letting credential
        /// writes proceed despite the drift.
        overridden: bool,
    },
    /// Layer 2 REFUSE (issue #730, surfaced by issue #738): the resolved canonical token matches
    /// NO account's stash AND does not parse as a Claude Code credential — overwhelmingly an
    /// UNRELATED secret sitting under the derived service, which the atomic `-U` upsert would
    /// clobber unrecoverably. Credential writes are refused pre-mutation (zero writes).
    ///
    /// A distinct verdict rather than the quiet [`Inconclusive`](Self::Inconclusive) it reused
    /// before #738: the *identity* answer is genuinely inconclusive either way, but the daemon's
    /// fail-CLOSED policy makes this case a REFUSAL an operator must see — the wire carries the
    /// operator-visible consequence, not just the identity verdict. Emitted ONLY while the refuse
    /// is live: with `canary_nostashmatch_override` set, the gate restores the pre-#730 fail-OPEN
    /// and the verdict is `Inconclusive` again, because nothing is being refused. Carries no
    /// fields — an unparseable canonical has no #15-safe detail to name (the bytes are the
    /// unrelated secret itself); the remedy rides the render text, not the wire.
    RefusedUnparseableCanonical,
}

/// One account's latest reading.
#[derive(Debug, Clone, Default)]
pub(crate) struct AccountReading {
    pub(crate) label: String,
    pub(crate) active: bool,
    /// Whether the account is in the rotation (issue #36) — surfaced so `status`
    /// can mark a parked account. A disabled account is shown but never swapped to.
    pub(crate) enabled: bool,
    /// Whether the account is QUARANTINED — its stored ACCESS token was rejected (a #42
    /// 401-streak), so it is out of rotation. NON-TERMINAL (issue #427): the remedy is a
    /// refresh (`poke`), not necessarily a re-login. Non-secret (a plain flag on the handle).
    pub(crate) quarantined: bool,
    /// Whether a quarantined account is mid-RECOVERY — its credential is currently
    /// answering again (`quarantined && recovery_successes > 0`), climbing toward the
    /// un-quarantine threshold on the spontaneous-revival path (issue #109). A refinement
    /// of `quarantined` (always implies it), surfaced so `status` can render `recovering`
    /// instead of the alarming `needs re-login` for a healing account. Derived from the
    /// health counter (where it lives); non-secret — a plain flag, no raw count exposed.
    pub(crate) recovering: bool,
    /// Whether the account's WEEKLY window is EXHAUSTED — `weekly >= weekly_ceiling`
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
    /// The daemon-computed 5-state credential-health rollup (issue #119) — the verdict the
    /// thin `status` client projects to a glyph. Computed in [`Daemon::snapshot`] from this
    /// account's health state and the wall clock.
    pub(crate) health: CredentialHealth,
    /// The active account's bounded-blindness projection (issue #479), or `None` when this is not
    /// the active account, or the active account is not blind, or there is no retained anchor.
    /// Computed in [`Daemon::snapshot`] from the retained `last_good` anchor (#450) and the ADR-0017
    /// gate thresholds; copied straight to the wire ([`status_response`]).
    pub(crate) blind_active: Option<BlindActive>,
    /// The REFRESH-token expiry modifier (issue #878), or `None` until this account has been
    /// POLLED at all. Keyed on the poll latch, NOT on a deadline being present: an unreadable
    /// credential (a locked keychain) on a poll that DID happen still carries `Some(_)` holding
    /// [`ExpiryHorizon::Unknown`], because *observed, no deadline in the credential* and *never
    /// observed* are different facts and neither means "not expiring" (issue #137).
    ///
    /// Carried ORTHOGONALLY to [`health`](Self::health): the two are independent axes, and an
    /// account is routinely [`CredentialHealth::Healthy`] while its refresh token is
    /// [`ExpiryHorizon::Within`] the horizon.
    ///
    /// Computed in [`Daemon::snapshot`] by [`account_expiry`] and copied straight to the wire
    /// ([`AccountStatusLine::expiry`], issue #882).
    pub(crate) expiry: Option<AccountExpiry>,
}

/// The status-snapshot wire contract's version (issue #164): a `major.minor` the daemon stamps
/// on every reply so an independently-released read-only client (a menubar app) can bind to it
/// safely. Semver-for-a-wire-struct: a MAJOR bump is a BREAKING change (a field removed /
/// renamed / re-typed / re-meant) an older client MUST refuse to render rather than mis-read; a
/// MINOR bump is ADDITIVE (a new optional field) an older client tolerates by ignoring what it
/// does not know. Non-secret — two integers.
///
/// Derives `Default` (`{0, 0}`) so a `#[serde(default)]` decode of a PRE-#164 daemon's reply
/// (which omits the field) yields major `0` — an "unknown, pre-freeze" version the client treats
/// as a mismatched major and DEGRADES on, rather than assuming compatibility (fail-safe).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct SchemaVersion {
    pub(crate) major: u32,
    pub(crate) minor: u32,
}

/// The status-snapshot contract version THIS build speaks (as the daemon) and understands (as
/// the reference `status` client) — issue #164. `1.0` is the FIRST frozen contract: the 0.1.0
/// status snapshot settled by #137–#143. Bump MAJOR on any breaking field change, MINOR on an
/// additive one (see [`SchemaVersion`]). `1.1` ADDED the daemon-level
/// [`StatusResponse::systemic_refresh_failure`] indicator (issue #378) — an optional field an
/// older client tolerates by ignoring. `1.2` ADDED the [`NextSwap::Target`] `reason`
/// ([`NextSwapReason`], issue #393) — the daemon's own selection rationale, likewise optional and
/// tolerated-by-ignoring. `1.3` ADDED the [`NextSwap::NoViableTarget`] `cause` + `resets_at`
/// fleet-capacity relief hint ([`NoTargetCause`], issue #405) — two more optional
/// tolerated-by-ignoring fields on a variant that was previously payload-free. `1.4` ADDED the
/// per-account [`AccountStatusLine::blind_active`] bounded-blindness projection ([`BlindActive`],
/// issue #479) — an optional field an older client tolerates by ignoring, and (via
/// `skip_serializing_if`) omitted entirely except on a blind active account, so a non-blind frame's
/// per-line bytes are unchanged. `1.5` ADDED the daemon-level
/// [`StatusResponse::canonical_scrub`] canonical-scrub rollup ([`CanonicalScrub`], issue #516) — a
/// fleet-wide scrubbed / recovery-exhausted signal, likewise optional and (via `skip_serializing_if`)
/// omitted entirely when healthy, so a non-scrub frame's bytes are unchanged. Like
/// `systemic_refresh_failure` it is daemon-level, but it takes `blind_active`'s `skip_serializing_if`
/// omit-when-healthy pattern rather than `systemic_refresh_failure`'s always-emitted `null`. `1.6`
/// ADDED the daemon-level [`StatusResponse::keychain_locked`] flag (issue #498) — a fleet-wide
/// "the login keychain is LOCKED so the shared credential is unreadable" signal, a bare `bool`
/// (via `skip_serializing_if`) omitted entirely when unlocked, so a non-locked frame's bytes are
/// unchanged. The daemon-level sibling of `canonical_scrub`, but for an UNREADABLE item rather than
/// a readable-but-scrubbed one; the wire prerequisite for the menubar #498 surface. `1.7` ADDED the
/// daemon-level [`StatusResponse::recent_blind_preempt_swap`] narrated-swap notice ([`BlindPreemptSwap`],
/// issue #479): a just-fired #452 bounded-blindness preemptive swap (source + last-known % + target),
/// so `status` can narrate the swap-away and its `use <from>` undo — likewise optional and (via
/// `skip_serializing_if`) omitted entirely except in the bounded window after such a swap, so a
/// no-recent-preempt-swap frame's bytes are unchanged. Takes `blind_active`'s / `canonical_scrub`'s
/// omit-when-absent pattern; a pre-#479 client ignores the unknown key (the minor-bump
/// tolerate-by-ignoring convention). `1.8` ADDED the daemon-level
/// [`StatusResponse::recent_landing_overshoot`] runtime landing-overshoot notice ([`LandingOvershoot`],
/// issue #613): a just-observed LOCAL landing overshoot (a recently-parked `reason=session` account
/// climbing to the SLO ceiling within the landing window — the post-swap committed tail caught LIVE,
/// not only in a later offline `reliability` run), so `status` can surface the breach in the moment —
/// likewise optional and (via `skip_serializing_if`) omitted entirely except in the bounded window
/// after such an overshoot, so an unaffected frame's bytes are unchanged. Takes
/// `recent_blind_preempt_swap`'s omit-when-absent pattern; a pre-#613 client ignores the unknown key.
/// `1.9` ADDED the daemon-level [`StatusResponse::canary`] behavioral-canary verdict
/// ([`CanaryStatus`], issue #714): whether the reverse-engineered #100 keychain derivation still
/// points at the credential Claude Code is actually using (the fresh Layer-1 uniqueness probe + the
/// offline Layer-2 stash-token identity cross-check, run at boot and pre-swap) — likewise optional
/// and (via `skip_serializing_if`) omitted only until the first canary run concludes, so a pre-#714
/// client ignores the unknown key. UNLIKE the omit-when-healthy siblings, a healthy verdict is
/// still carried once known: "verified ok" and "unverified" are different operator facts (the
/// canary's Layer-3 residual surface). `1.10` ADDED the
/// [`CanaryStatus::RefusedUnparseableCanonical`] VARIANT (issue #738) — the #730 fail-CLOSED
/// refuse (an orphan canonical that parses as no Claude Code credential) had been reusing the
/// quiet `inconclusive` verdict, so the daemon refused the swap while every surface rendered
/// "nothing to see". Additive by construction: the enum is internally tagged on `verdict`, so a
/// new variant adds no key to any EXISTING frame — a healthy or drifted snapshot's bytes are
/// unchanged. Forward-compat is asymmetric BY DESIGN here: a pre-#738 client that meets the new
/// verdict rejects the frame rather than mis-decoding it (an alarm state must never silently
/// degrade to "all clear"), which is exactly why this earns a minor bump the client can gate on.
/// `1.11` ADDED the daemon-level [`StatusResponse::systemic_refresh_source`] episode-provenance
/// discriminant ([`SystemicRefreshSource`], issue #813) — WHICH of the two opening brackets opened
/// the active systemic-refresh episode (the #378 sweep crossing or the #787 startup preflight), a
/// distinction the event log has drawn since #787 while the wire could not. Additive alongside the
/// existing `systemic_refresh_failure` count rather than a reshape of it, and (via
/// `skip_serializing_if`) omitted entirely while the mechanism is healthy — so a healthy frame's
/// bytes are unchanged AND an episode frame keeps the exact count an older client already renders.
/// A pre-#813 client ignores the unknown key (the minor-bump tolerate-by-ignoring convention).
/// `1.12` ADDED the per-account [`AccountStatusLine::expiry`] REFRESH-token expiry modifier
/// ([`AccountExpiry`], issue #882) — the observed `refreshTokenExpiresAt` deadline plus its
/// [`ExpiryHorizon`] classification against `[credential].expiry_horizon_secs`, so the menubar and
/// `status` can warn BEFORE a refresh token lapses instead of after. Takes `blind_active`'s
/// per-account omit-when-absent pattern: optional and (via `skip_serializing_if`) omitted entirely
/// until an account has been polled, so an unpolled row's per-line bytes are unchanged. ORTHOGONAL
/// to the `auth` rollup rather than a variant of it (the ADR-0017 modifier posture) — an account is
/// routinely healthy AND inside its expiry horizon. A pre-#882 client ignores the unknown key.
/// `1.13` ADDED the daemon-level [`StatusResponse::expiry_cohort`] SYNCHRONIZED-EXPIRY cohort
/// condition ([`ExpiryCohort`], issue #879) AND made the per-account
/// [`AccountExpiry::cohort_id`] grouping key REACHABLE — the field shipped at 1.12 but was
/// unconditionally `None`, so it never appeared on the wire until #879's populator landed. Both
/// halves of one fact: `cohort_id` says WHICH group a row is in, `expiry_cohort` states the
/// fleet-level condition no single row can express (the sibling of `keychain_locked` /
/// `canonical_scrub`, both likewise fleet-wide). Takes the omit-when-absent pattern: via
/// `skip_serializing_if` an ungrouped roster's bytes are unchanged, so a fleet with no synchronized
/// deadlines still serializes exactly as it did at 1.12. A pre-#879 client ignores both keys.
/// `1.14` made the per-account [`RefreshHealth::rotated`] OPTIONAL and (via `skip_serializing_if`)
/// omitted entirely on every outcome that ran no token exchange (issue #1070, closing the fourth
/// surface #1004 deferred). The three log lines already carry `rotated=` only where an exchange
/// happened, because #1004 moved the payload INSIDE the refreshed variants; this brings the wire to
/// the same rule, so the uninformative `"rotated": false` R-5a objected to on a versioned surface is
/// gone. A refreshed account still carries the AC-3 durability signal in both directions
/// (`"rotated": true` / `false`) — the signal is preserved, only the fabricated value is dropped.
///
/// **This is the one entry that RESHAPES an existing key rather than adding a new one, so its
/// forward-compat is asymmetric — deliberately, and unlike the omit-when-absent siblings above.** A
/// pre-#1070 client typed the key as REQUIRED, so a 1.14 daemon's non-refreshed frame does not
/// decode against it (Swift `WatchStatusStore` drops the line). The Rust `status` client is immune
/// by construction — it ships in the same binary as the daemon it reads — so the exposure is exactly
/// one consumer, the separately-installed menubar app, which this bump updates in lockstep. The
/// bump is MINOR because [`SchemaVersion`] support gates on the MAJOR and the reshape is the
/// narrowest one that removes the false value; an operator running a 1.14 daemon against a
/// pre-#1070 app must update the app, which is the same lockstep every wire change here already
/// assumes. The reverse direction is clean: a 1.14 client reading a ≤1.13 daemon decodes the
/// always-present key as `Some(_)`.
pub(crate) const STATUS_SCHEMA_VERSION: SchemaVersion = SchemaVersion {
    major: 1,
    minor: 14,
};

/// The control socket's `status` reply PAYLOAD — handles + percentages + the forward-looking
/// `next_swap` candidate, and nothing else (issue #15: never a token or email).
/// Derives both `Serialize` (the daemon writes it) and `Deserialize` (the `status`
/// client reads it). This is the payload the frozen wire envelope ([`VersionedStatus`], issue
/// #164) carries; the durable, timestamped swap HISTORY remains the event-log view (#9), not
/// `status`.
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
    /// The daemon-level SYSTEMIC refresh-failure indicator (issue #378): `Some(n)` while the
    /// refresh MECHANISM is down (`n` consecutive all-eligible-account `outcome=error` sweeps past
    /// the configured threshold), else `None`/absent when healthy. Lets `sessiometer status` show
    /// the mechanism is down — a signal distinct from the per-account `auth` rollup, visible
    /// without waiting for an account to die. `Option` + `#[serde(default)]` per the added-field
    /// convention (this is the MINOR [`STATUS_SCHEMA_VERSION`] bump 1.0 → 1.1): a pre-#378 daemon
    /// omits the field → `None`, which the client renders as healthy. A COUNT only — never a token,
    /// path, or email (issue #15).
    #[serde(default)]
    pub(crate) systemic_refresh_failure: Option<u32>,
    /// WHICH opening bracket opened the active systemic-refresh episode (issue #813):
    /// `Some(Sweep | Preflight)` alongside a `Some` [`Self::systemic_refresh_failure`], else absent.
    /// Lets each surface phrase what actually happened instead of asserting a sweep that never ran —
    /// the preflight path seeds the count at one (issue #787) purely for renderer grammar, which is
    /// indistinguishable on the wire from a genuine one-sweep crossing.
    ///
    /// `Option` + `#[serde(default, skip_serializing_if = "Option::is_none")]` per the added-field
    /// convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump 1.10 → 1.11, taking `canonical_scrub`'s
    /// omit-when-healthy pattern rather than `systemic_refresh_failure`'s always-emitted `null`):
    /// a pre-#813 daemon omits the field → `None`, AND a healthy snapshot omits it entirely, so a
    /// healthy frame's bytes are byte-for-byte unchanged. ADDITIVE alongside the count, never a
    /// reshape of it — an older client keeps decoding the exact bytes it decodes today and ignores
    /// this unknown key (the minor-bump tolerate-by-ignoring convention).
    ///
    /// Set in lockstep with the count from the SAME latch
    /// ([`crate::systemic_refresh::SystemicRefreshHealth`], whose latch IS
    /// the source), so the two can never disagree about whether an episode is active. A FIXED-TOKEN
    /// classification only — never a path, binary location, token, or email (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) systemic_refresh_source: Option<SystemicRefreshSource>,
    /// The daemon-level CANONICAL-SCRUB rollup (issue #516): `Some(Recovering | Exhausted)` while the
    /// shared canonical item is scrubbed (recovering vs recovery-exhausted / un-recoverable), else
    /// absent when healthy. Lets `sessiometer status` + the menubar (#469) surface the fleet-wide
    /// scrubbed lockout that no per-account `auth` rollup reflects — the daemon-LEVEL sibling of the
    /// per-account `blind_active`. `Option` + `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// per the added-field convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump 1.4 → 1.5, mirroring
    /// `blind_active`): a pre-#516 daemon omits the field → `None`, AND a HEALTHY snapshot omits it
    /// entirely, so a non-scrub frame's bytes are byte-for-byte unchanged (a pre-#516 client ignores
    /// the unknown key, the minor-bump tolerate-by-ignoring convention). A STATE discriminant only —
    /// never a token or email (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) canonical_scrub: Option<CanonicalScrub>,
    /// The daemon-level KEYCHAIN-LOCKED flag (issue #498): `true` while the macOS login keychain is
    /// LOCKED, so the daemon cannot READ the shared credential item at ALL — the daemon-LEVEL sibling
    /// of `canonical_scrub`, but for an UNREADABLE item (access denied) rather than a readable-but-
    /// scrubbed one, so the operator remedy differs (unlock the keychain, not `claude /login`). Lets
    /// `sessiometer status` + the menubar (#498) surface a fleet-wide unreadable-credential lockout no
    /// per-account `auth` rollup reflects. A bare `bool` + `#[serde(default, skip_serializing_if =
    /// "std::ops::Not::not")]` per the added-field convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump
    /// 1.5 → 1.6, taking `canonical_scrub`'s omit-when-healthy pattern): a pre-#498 daemon omits the
    /// field → `false`, AND an unlocked snapshot omits it entirely, so a non-locked frame's bytes are
    /// byte-for-byte unchanged (a pre-#498 client ignores the unknown key, the minor-bump
    /// tolerate-by-ignoring convention). A bare BINARY state discriminant — never a token or email
    /// (issue #15).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) keychain_locked: bool,
    /// The daemon-level NARRATED bounded-blindness preemptive-swap notice (issue #479): `Some` for a
    /// bounded window after #452 swapped a BLIND active account away on its stale anchor (ADR-0017),
    /// carrying source + last-known session % + target so `sessiometer status` can narrate the
    /// swap-away and its `use <from>` undo — the SAME information the durable `event=swap …
    /// reason=blind_preempt` log line holds, reflected in `status` (the surface has no other way to see
    /// a just-happened swap — `render_status` reads only this wire, never the event log). Absent when
    /// no such swap is recent-and-still-current. `Option` + `#[serde(default, skip_serializing_if =
    /// "Option::is_none")]` per the added-field convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump
    /// 1.6 → 1.7, mirroring `blind_active` / `canonical_scrub`): a pre-#479 daemon omits the field →
    /// `None`, AND a no-recent-swap snapshot omits it entirely, so an unaffected frame's bytes are
    /// byte-for-byte unchanged (a pre-#479 client ignores the unknown key). The surface only REFLECTS
    /// this daemon-pushed state; it never self-swaps (the #169 UI-never-acts invariant). Non-secret —
    /// two handles and a `u8`, never a token or email (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recent_blind_preempt_swap: Option<BlindPreemptSwap>,
    /// The daemon-level RUNTIME landing-overshoot notice (issue #613): `Some` for a bounded window
    /// after THIS machine observed a recently-parked (`reason=session`) account climb to the SLO
    /// ceiling within the landing window — the post-swap committed tail (#595) the swap-DECISION
    /// reading is blind to, caught LIVE instead of only in a later offline `reliability` run. Carries
    /// the parked account's handle + the decision % it swapped out at + the % it actually LANDED at,
    /// so `sessiometer status` can surface the local breach (like the `systemic_refresh_failure`
    /// banner). Absent when no overshoot is recent. `Option` + `#[serde(default, skip_serializing_if
    /// = "Option::is_none")]` per the added-field convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump
    /// 1.7 → 1.8, mirroring `recent_blind_preempt_swap`): a pre-#613 daemon omits the field → `None`,
    /// AND a no-overshoot snapshot omits it entirely, so an unaffected frame's bytes are byte-for-byte
    /// unchanged (a pre-#613 client ignores the unknown key). Best-available, still per-machine — a
    /// co-consuming second machine's tail is invisible to it. Non-secret — one handle and two `u8`s,
    /// never a token or email (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recent_landing_overshoot: Option<LandingOvershoot>,
    /// The behavioral canary's LAST verdict (issue #714): did the reverse-engineered #100
    /// keychain derivation still point at the credential Claude Code is actually using, the last
    /// time the canary ran (boot + every pre-swap re-check)? `Some(Drift { .. })` /
    /// `Some(Ambiguous { .. })` are the refusing states (`sessiometer status` renders a fault
    /// banner: credential writes are refused while they hold); `Some(Ok)` is the positive
    /// identity pass; `Some(Inconclusive)` is the honest fail-open Layer-1-only state; `None`
    /// means the canary has not concluded a run (pre-#714 daemon, or it could not run — e.g. a
    /// locked keychain at boot). `Option` + `#[serde(default, skip_serializing_if =
    /// "Option::is_none")]` per the added-field convention (the MINOR [`STATUS_SCHEMA_VERSION`]
    /// bump 1.8 → 1.9, mirroring `canonical_scrub`): a pre-#714 daemon omits the field → `None`
    /// (a pre-#714 client ignores the unknown key, the minor-bump tolerate-by-ignoring
    /// convention). Unlike the omit-when-healthy siblings this is omitted only when NEVER RUN —
    /// a healthy verdict is still carried, because "verified ok" and "unverified" are different
    /// operator facts (the Layer-3 residual surface). Labels only — never a token, email, or
    /// account-uuid (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) canary: Option<CanaryStatus>,
    /// The daemon-level SYNCHRONIZED-EXPIRY COHORT condition (issue #879, REQ-CC-B-004): `Some`
    /// when two or more accounts' refresh-token deadlines fall within one
    /// `[credential].expiry_cohort_window_secs` window AND the soonest of them is inside the
    /// foresight horizon, else absent.
    ///
    /// The fleet-level half of the expiry feature, and the half the upstream client structurally
    /// cannot provide: it warns only for the ACTIVE account, so a parked account's deadline is
    /// invisible to it until swap-in — possibly after death. The per-account
    /// [`AccountStatusLine::expiry`] modifier beside it shows four rows that each look individually
    /// survivable; only this states that the swap pool loses several members at once.
    ///
    /// A DAEMON-LEVEL field, deliberately, exactly like [`Self::keychain_locked`] and
    /// [`Self::canonical_scrub`] — a condition distinct from any single account's state, never a
    /// per-account modifier and never a footer list keyed per account. Membership stays on the rows
    /// themselves via [`AccountExpiry::cohort_id`], so this field carries no handles.
    ///
    /// `Option` + `#[serde(default, skip_serializing_if = "Option::is_none")]` per the added-field
    /// convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump 1.12 → 1.13, mirroring
    /// `canonical_scrub`'s omit-when-absent pattern): a pre-#879 daemon omits the field → `None`,
    /// AND an unsynchronized roster omits it entirely, so an unaffected frame's bytes are
    /// byte-for-byte unchanged (a pre-#879 client ignores the unknown key, the minor-bump
    /// tolerate-by-ignoring convention).
    ///
    /// Absence means "no cohort was DETECTED among the deadlines actually observed" — it is never
    /// a claim that the fleet is unsynchronized. A roster whose credentials carry no
    /// `refreshTokenExpiresAt` at all produces no condition and no per-account cell, so no surface
    /// reports a reassuring zero for a fleet it could not measure (the issue #137 invariant). That
    /// is also why [`ExpiryCohort::observed`] rides along: it names the denominator rather than
    /// letting a reader assume the roster.
    ///
    /// Counts and instants only — never a token, email, or handle (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expiry_cohort: Option<ExpiryCohort>,
}

/// The FROZEN status-snapshot wire contract (issue #164): the [`StatusResponse`] payload plus the
/// two envelope fields that make it safe for an independently-released read-only client to bind
/// to — the contract [`SchemaVersion`] and the `generated_at` freshness stamp. This is the exact
/// struct the daemon serializes onto the control socket for a `status` request.
///
/// The payload is `#[serde(flatten)]`ed, so the wire JSON stays FLAT —
/// `{"schema_version":…,"generated_at":…,"accounts":…,"next_swap":…,"refresh_enabled":…}` — the
/// settled #137–#143 payload shape unchanged at top level, only PREFIXED with the two meta
/// fields. So existing internal readers that decode a bare [`StatusResponse`] (`poke`,
/// `use_account`) keep working: serde ignores the two extra top-level keys they do not name.
/// Non-secret by construction: the envelope adds a version object and a timestamp, and the
/// payload is the same redacted [`StatusResponse`] (issue #15).
///
/// `#[serde(default)]` on the two meta fields makes a PRE-#164 daemon's reply (which omits them)
/// decode to `SchemaVersion { major: 0, minor: 0 }` / `generated_at: 0` — a mismatched major the
/// client degrades on — rather than a decode error.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct VersionedStatus {
    /// The contract version the payload conforms to ([`STATUS_SCHEMA_VERSION`] from a current
    /// daemon). The reference client gates on `major` before rendering.
    #[serde(default)]
    pub(crate) schema_version: SchemaVersion,
    /// Wall-clock epoch SECONDS at which the daemon assembled this snapshot — the client's
    /// live-vs-stale signal. Copied from [`StatusSnapshot::generated_at`].
    #[serde(default)]
    pub(crate) generated_at: i64,
    /// The redacted per-account payload (issue #15), flattened so its fields sit at the top
    /// level of the wire JSON alongside the two envelope fields above.
    #[serde(flatten)]
    pub(crate) status: StatusResponse,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AccountStatusLine {
    /// The operator-chosen handle (label) — never the email (issue #15).
    pub(crate) label: String,
    pub(crate) active: bool,
    /// Whether the account is in the rotation (issue #36); `false` for a parked
    /// account, which `status` marks. Non-secret — a plain flag.
    pub(crate) enabled: bool,
    /// Whether the account is QUARANTINED — its stored ACCESS token was rejected (a #42
    /// 401-streak), so it is out of rotation. NON-TERMINAL — the remedy is a refresh, not
    /// necessarily a re-login (issue #427); `false` for a healthy account. Non-secret — a
    /// plain flag.
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
    /// Whether the account's WEEKLY window is exhausted (`weekly >= weekly_ceiling`),
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
    /// The daemon-computed 5-state credential-auth rollup (issue #119): the verdict the
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
    /// The active account's bounded-blindness projection (issue #479, umbrella #363 Path B): blind
    /// duration + last-known session % + whether ADR-0017 auto-protection is DEGRADED — or absent
    /// when the active account is not blind (or this is not the active account). The client renders
    /// it as a SEMANTIC status line in place of the bare `n/a … 🟡` active row. `#[serde(default)]`
    /// decodes an omitting daemon to `None`; `skip_serializing_if` OMITS it whenever absent, so a
    /// non-blind account's per-line wire bytes are byte-for-byte unchanged — the additive MINOR
    /// `1.3 → 1.4` field appears ONLY on a blind active account (a pre-#479 client ignores the
    /// unknown key, the minor-bump tolerate-by-ignoring convention). Non-secret (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blind_active: Option<BlindActive>,
    /// This account's REFRESH-token expiry MODIFIER ([`AccountExpiry`], issue #882): the observed
    /// `refreshTokenExpiresAt` deadline plus its [`ExpiryHorizon`] classification against the
    /// operator's `[credential].expiry_horizon_secs` foresight window — or absent until the account
    /// has been POLLED at all.
    ///
    /// ORTHOGONAL to [`health`](Self::health) and never folded into it — an account is routinely
    /// [`CredentialHealth::Healthy`] *right now* while its refresh token is already
    /// [`ExpiryHorizon::Within`] the horizon, so the two are INDEPENDENT cells. [`AccountExpiry`]
    /// carries the full ADR-0017 modifier-rather-than-a-new-auth-state rationale.
    ///
    /// `Option` + `#[serde(default, skip_serializing_if = "Option::is_none")]` per the added-field
    /// convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump 1.11 → 1.12, mirroring `blind_active`):
    /// a pre-#882 daemon omits the field → `None`, AND a never-polled account omits it entirely, so
    /// an unpolled row's wire bytes are byte-for-byte unchanged (a pre-#882 client ignores the
    /// unknown key, the minor-bump tolerate-by-ignoring convention).
    ///
    /// Absent is "never observed", NOT "not expiring" — and neither is
    /// [`ExpiryHorizon::Unknown`], which is what a POLLED account whose credential carried no
    /// deadline reports (issue #137). Only [`ExpiryHorizon::Beyond`] means "further out than the
    /// horizon". Non-secret — one timestamp and one classification, never a token or email
    /// (issue #15).
    ///
    /// Tolerance is ASYMMETRIC between that unknown key and an unknown TOKEN inside it:
    /// [`ExpiryHorizon`] has no catch-all variant, so a LATER minor adding a fifth classification
    /// costs a client built before it the whole [`VersionedStatus`] decode rather than one cell —
    /// the hard reject every RUST decoder of this wire takes, none of these enums carrying a
    /// `#[serde(other)]` catch-all. Harmless in the two internal readers (`poke` and `use_account`
    /// both swallow any decode error into a benign fall-back), NOT in [`crate::cli`]'s
    /// `gate_status`, where the matching major bypasses the visible schema-mismatch degrade. The
    /// menubar's posture is deliberately SPLIT rather than uniform, so the #884 mirror has a real
    /// choice to make: an unknown `canonical_scrub.state` / `next_swap.state` / `canary.verdict`
    /// rejects, while an unknown `next_swap.reason.kind` degrades to `nil` (issue #412) and an
    /// unknown `systemic_refresh_source` to `unrecognized` (issue #813) — the rule being whether
    /// the unknown value IS the fault or only decorates one that decodes fine. Either way that
    /// variant earns the minor bump a client COULD gate on, as
    /// [`CanaryStatus::RefusedUnparseableCanonical`] earned one at `1.10` for the same asymmetry.
    /// Nothing gates on the minor today: `gate_status` and the menubar's
    /// `WireContract.isSupported` both key on the MAJOR alone.
    ///
    /// A partial `expiry` OBJECT is the one case that degrades instead of throwing: `horizon_state`
    /// defaults to the fail-safe [`ExpiryHorizon::Unknown`] (see
    /// [`AccountExpiry::horizon_state`]), deliberately unlike [`BlindActive`], whose fields have no
    /// honest default and so throw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expiry: Option<AccountExpiry>,
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
    /// A viable target exists — [`pick_target`]'s choice, by roster label, plus the daemon's own
    /// `reason` for choosing it ([`NextSwapReason`], issue #393). The reason is DAEMON-AUTHORITATIVE:
    /// a client cannot re-derive it (the session trigger / floor `pick_target` consumes are
    /// daemon-only, never on the wire), so it is carried here rather than guessed client-side. An
    /// ADDITIVE `Option` field (`#[serde(default)]`, the #164 minor-bump convention): a current
    /// daemon always sends `Some`, but a pre-#393 daemon omits it → `None`, which a renderer shows
    /// as a bare target label with no rationale (mirroring the `health` / `refresh_enabled`
    /// pre-freeze-compat posture).
    Target {
        to: String,
        #[serde(default)]
        reason: Option<NextSwapReason>,
    },
    /// No sound swap destination — [`pick_target`] picked nothing AND this is not the
    /// post-restart all-unpolled moment (`AwaitingData`). Reached when at least one
    /// *live* (enabled, non-quarantined) other account has already been polled and none
    /// qualifies (weekly-exhausted, or over the `target_max_session_usage` reserve) — even while other
    /// live accounts are still unpolled (the staggered-warm-up #80 mixed case) — or when
    /// there is no live other account at all (every other disabled #36 or quarantined #42,
    /// its reading masked away by `decision_readings`, or there is simply no other account).
    ///
    /// Carries the same fleet-capacity RELIEF hint the durable `all_exhausted` /
    /// `active_dead_no_target` events do (issue #405): `cause` names WHY the fleet is blocked
    /// ([`NoTargetCause`]) and `resets_at` WHEN capacity returns — the [`all_exhausted_relief`]
    /// classification, so BOTH the CLI footer and the menubar can tell the operator "out of
    /// capacity, resets in ⟨dur⟩ — add an account" instead of a content-free "no viable target".
    /// This surfaces the SAME hint whether the active is alive-and-over-trigger or DEAD-and-stranded
    /// (the dead active's 🔴 health shows separately on its own account row, so the composite —
    /// re-login the dead credential AND wait for / add capacity — emerges). Both fields are ADDITIVE
    /// `Option`s (`#[serde(default)]`, the #164 minor-bump convention): a current daemon always
    /// sends `Some(cause)` (relief always classifies a cause), and `resets_at` whenever the relevant
    /// window reported a parseable reset; a pre-#405 daemon omits both → `None`, which a renderer
    /// falls back to the bare "no viable target" on (the pre-freeze-compat posture `reason` on
    /// [`Self::Target`] and `health` share).
    NoViableTarget {
        #[serde(default)]
        cause: Option<NoTargetCause>,
        #[serde(default)]
        resets_at: Option<i64>,
    },
    /// No reading yet for any *live* (enabled, non-quarantined) other account — the
    /// post-restart moment, before the staggered poll loop (#80) has read the rotation.
    /// Kept distinct from `NoViableTarget` because it is exactly the moment an operator
    /// checks `status`; a quarantined account's masked-away reading does NOT count here
    /// (its data needs a re-login, not a poll).
    AwaitingData,
}

/// WHY [`pick_target`] chose the [`NextSwap::Target`] it did (issue #393) — the daemon's own
/// selection rationale, carried on the wire so BOTH the panel footer and `sessiometer status`
/// render the ONE reason the daemon actually used, each in its own idiom (R-2 STATE-parity: a
/// structured discriminant, not a pre-formatted string that would force identical wording on both
/// media). Distinct from [`crate::observability::SwapReason`], which records why a swap FIRED
/// (session / weekly / manual / forced); this records why a particular TARGET won selection.
///
/// The variants track [`pick_target`]'s ACTUAL axis (issue #37 — soonest weekly reset among
/// viable accounts), NOT the superseded "most headroom" rule the client used to assert. Internally
/// tagged on `kind` (NOT `reason`, which is the carrying field name on [`NextSwap::Target`], nor
/// `state`, [`NextSwap`]'s own tag), so the nested wire shape is unambiguous:
/// `{"state":"target","to":"…","reason":{"kind":"soonest_reset","resets_at":…}}`. Non-secret — a
/// discriminant plus one epoch timestamp, never a token or email (issue #15).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(crate) enum NextSwapReason {
    /// Two or more accounts qualified and this one's WEEKLY window resets SOONEST — the live #37
    /// selection axis. `resets_at` is the winner's weekly-reset epoch (the `min_by_key` key
    /// `pick_target` sorts on, previously computed then discarded before serialization).
    SoonestReset { resets_at: i64 },
    /// Exactly ONE account qualified, so nothing discriminated the winner — it is the sole viable
    /// target. Carries no epoch: its weekly reset, known or not, decided nothing.
    OnlyCandidate,
    /// Two or more accounts qualified but NONE reported a weekly reset, so no soonest-reset
    /// tiebreak existed and selection fell to the earliest roster index (`min_by_key` keeps the
    /// first of equal keys). Deliberately DISTINCT from [`Self::OnlyCandidate`]: several targets
    /// were viable here, so a renderer must never claim this one was the only one — that would be
    /// the very false-rationale bug #393 exists to remove. Carries no epoch because none exists.
    RosterOrder,
}

/// WHY [`NextSwap::NoViableTarget`] has no target — the fleet-capacity RELIEF cause (issue #405),
/// the forward-looking sibling of the durable `all_exhausted` / `active_dead_no_target` events'
/// `cause`. Carried on the wire so BOTH the CLI footer and the menubar render the ONE cause the
/// daemon's [`all_exhausted_relief`] classification produced, each in its own idiom (R-2
/// STATE-parity: a structured discriminant, not a pre-formatted string).
///
/// Deliberately a WIRE-LOCAL enum distinct from [`crate::observability::SwapReason`] — exactly as
/// [`NextSwapReason`] is — for two reasons: `SwapReason` additionally carries `Manual` / `Forced`
/// (operator-swap reasons that cannot arise from a no-target verdict), and it is not `serde`, so
/// putting it on the wire would both widen the contract's value set nonsensically and couple the
/// diagnostic enum to the wire. This carries ONLY the two causes relief can report. `snake_case`,
/// so a value is `"session"` or `"weekly"`. Non-secret — a bare discriminant (issue #15).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoTargetCause {
    /// The SESSION window (the block line `min(session_ceiling, target_max_session_usage)`) is what
    /// gates the soonest-returning spare — relief arrives at that account's session reset.
    Session,
    /// The WEEKLY window (`weekly >= weekly_ceiling`) is what gates the soonest-returning spare —
    /// relief arrives at that account's weekly reset. The #11 default, and the ONLY cause reachable
    /// on the emergency/dead-active path, which bypasses the session gate entirely.
    ///
    /// Note both variants name the dimension gating the WINNING account, not a fleet-wide property:
    /// on a mixed fleet some spares may be blocked the other way, or on both (issue #665). The
    /// operator-facing prose that reads these is the sibling concern, issue #666.
    Weekly,
}

/// Project a [`StatusSnapshot`] into the wire [`StatusResponse`]. Sourced solely
/// from non-secret fields, so it can never carry a token or email (issue #15).
pub(crate) fn status_response(snapshot: &StatusSnapshot) -> StatusResponse {
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
                // The bounded-blindness projection (issue #479), already resolved daemon-side in
                // `Daemon::snapshot`; copied straight to the wire. `None` for every non-active or
                // non-blind account, so `skip_serializing_if` omits it there.
                blind_active: account.blind_active,
                // The refresh-token expiry modifier (issue #882), classified daemon-side in
                // `Daemon::snapshot` by `account_expiry`; copied straight to the wire. `None` until
                // the account has been polled at all, so `skip_serializing_if` omits it there — and
                // that absence means "never observed", never "not expiring" (issue #137).
                expiry: account.expiry,
            })
            .collect(),
        // Already computed at snapshot build (issue #88); copy it to the wire.
        next_swap: snapshot.next_swap.clone(),
        // The config `[refresh].enabled` (#105) for the #138 advisory; wrapped `Some` since a
        // current daemon always knows it (the `Option` is purely pre-#138 wire compat, mirroring
        // `health`).
        refresh_enabled: Some(snapshot.refresh_enabled),
        // The daemon-level systemic refresh-failure indicator (issue #378), copied straight to the
        // wire: `Some(n)` while the mechanism is down, `None` when healthy.
        systemic_refresh_failure: snapshot.systemic_refresh,
        // WHICH opening bracket opened that episode (issue #813), copied straight to the wire
        // beside the count it qualifies: `Some(Sweep | Preflight)` while the mechanism is down,
        // `None` when healthy (and then omitted entirely via `skip_serializing_if`). Both come
        // from the one latch, so an episode frame always carries both or neither.
        systemic_refresh_source: snapshot.systemic_refresh_source,
        // The daemon-level canonical-scrub rollup (issue #516), copied straight to the wire:
        // `Some(Recovering | Exhausted)` while the shared canonical is scrubbed, `None` when healthy.
        canonical_scrub: snapshot.canonical_scrub,
        // The daemon-level keychain-locked flag (issue #498), copied straight to the wire: `true`
        // while the login keychain is locked (the shared credential is unreadable), `false` when
        // unlocked (and then omitted from the wire via `skip_serializing_if`).
        keychain_locked: snapshot.keychain_locked,
        // The daemon-level narrated preemptive-swap notice (issue #479), already resolved daemon-side
        // in `Daemon::snapshot` (windowed + target-still-active): `Some` for a bounded window after a
        // #452 blind-preempt swap, `None` otherwise (and then omitted via `skip_serializing_if`).
        recent_blind_preempt_swap: snapshot.recent_blind_preempt_swap.clone(),
        // The daemon-level runtime landing-overshoot notice (issue #613), already resolved daemon-side
        // in `Daemon::snapshot` (windowed): `Some` for a bounded window after a local landing overshoot,
        // `None` otherwise (and then omitted via `skip_serializing_if`).
        recent_landing_overshoot: snapshot.recent_landing_overshoot.clone(),
        canary: snapshot.canary.clone(),
        // The daemon-level synchronized-expiry cohort condition (issue #879), already resolved
        // daemon-side in `Daemon::snapshot`: `Some` when two or more deadlines share a window and
        // the soonest is inside the horizon, `None` otherwise (and then omitted via
        // `skip_serializing_if`). Its absence is "none detected among what was observed", never a
        // claim that the fleet is unsynchronized.
        expiry_cohort: snapshot.expiry_cohort,
    }
}

/// Wrap a [`StatusSnapshot`] into the FROZEN wire envelope (issue #164): stamp the current
/// [`STATUS_SCHEMA_VERSION`] and copy the snapshot's `generated_at`, around the same
/// [`status_response`] payload projection. This is the single function the control socket
/// serializes for a `status` request, so EVERY reply carries the contract version + freshness
/// stamp. Non-secret for the same reason `status_response` is — the envelope adds only a version
/// object and a timestamp (issue #15).
pub(crate) fn versioned_status_response(snapshot: &StatusSnapshot) -> VersionedStatus {
    VersionedStatus {
        schema_version: STATUS_SCHEMA_VERSION,
        generated_at: snapshot.generated_at,
        status: status_response(snapshot),
    }
}

/// A usage fraction in `[0.0, 1.0]` as a rounded, clamped `0..=100` percent.
pub(crate) fn to_pct(fraction: f64) -> u8 {
    (fraction * 100.0).round().clamp(0.0, 100.0) as u8
}

/// A usage fraction as an EXACT percent — neither rounded nor clamped, unlike [`to_pct`].
///
/// The conversion for values that are NOT readings: a PROJECTION (routinely over `1.0` — a steep
/// rate carried across the horizon), a per-second RATE (far below the `u8` resolution — rounding it
/// to a whole percent would erase the signal entirely), and a jittered CEILING draw (not a round
/// number). Rounding or clamping any of those would destroy exactly the precision that makes the
/// #634 projection ingredients reconstructable, so they take this conversion and carry their own
/// decimals at render time.
///
/// The single place the daemon's internal fraction-per-unit domain becomes the log's percent domain
/// for those values — the unit boundary is load-bearing (issue #634), so it lives in one function
/// beside [`to_pct`] rather than as scattered `× 100.0`.
pub(crate) fn to_pct_exact(fraction: f64) -> f64 {
    fraction * 100.0
}

/// The daemon-side credential-health rollup (issue #119, extended by #137) — a PURE function
/// of one account's health inputs, its fresh-reading liveness signal, and the wall clock, so
/// it is unit-tested directly and computed identically for the display snapshot and the
/// transition-event diff. The thin `status` client just projects the returned verdict to a
/// glyph.
///
/// A SEVERITY ladder (most-severe wins), matching the issue's 🟢→🟡→🟠→🔴 ordering, plus a
/// distinct ⚪ `Unknown` for the no-evidence case (#137):
/// - **Dead** — the last refresh outcome was `Dead`: a sweep-refresh actually rejected the
///   REFRESH token (the #261 / `CredentialUnrecoverable` cue). This is PROVEN death and the
///   ONLY 🔴 / `claude /login` case (issue #427). A DISPLAY rollup — it never flips the
///   quarantine machinery; surfacing a refresh-detected death is more honest than hiding it.
/// - **Degraded** — `quarantined` (the #42 access-token 401-streak verdict) but NOT proven
///   dead (issue #427). A usage-endpoint 401 rejects the ACCESS token and says nothing about
///   the REFRESH token (a resource server never sees it), so the account is out of rotation
///   right now yet `poke` / a restart revive it — it needs a REFRESH, not a re-login. 🟠
///   NON-TERMINAL; checked AFTER proven `Dead` so a quarantined account whose refresh has
///   ALSO returned `Dead` still reads the terminal 🔴.
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
pub(crate) fn credential_health(
    quarantined: bool,
    last_refresh_outcome: Option<RefreshEventOutcome>,
    consecutive_refresh_failures: u32,
    access_expires_at: Option<i64>,
    has_fresh_reading: bool,
    now_secs: i64,
) -> CredentialHealth {
    if last_refresh_outcome == Some(RefreshEventOutcome::Dead) {
        // PROVEN death: a refresh actually rejected the REFRESH token (#261). The only 🔴
        // `claude /login` case — checked FIRST so it wins over a co-occurring quarantine.
        CredentialHealth::Dead
    } else if quarantined {
        // The ACCESS token 401-streaked into quarantine (#42) but the refresh token is
        // unproven — NON-TERMINAL (issue #427): needs a REFRESH (`poke` / restart), not a
        // re-login. Ranks above `AtRisk`: the account is out of rotation NOW, not merely
        // trending, so a quarantine wins even alongside a refresh-failure streak.
        CredentialHealth::Degraded
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

/// One account's REFRESH-token expiry MODIFIER (issue #878) — the forward-looking axis carried
/// ALONGSIDE [`CredentialHealth`], never folded into it.
///
/// Follows the ratified ADR-0017 `blind_active` precedent ([`BlindActive`]): a per-account
/// condition hung on an otherwise-connected row rather than a new state in an existing ramp. The
/// two axes answer different questions and are rendered as INDEPENDENT cells — an account can be
/// (and typically is) `Healthy` **and** `Within` its expiry horizon at the same time, which is
/// precisely the case the operator needs to see and which no single ordinal ladder can express.
///
/// ONE type for both the daemon-internal reading ([`AccountReading::expiry`]) and the wire
/// ([`AccountStatusLine::expiry`], issue #882) — the same single-type posture [`BlindActive`] and
/// [`RefreshHealth`] take, and safe for the same reason: every field is already non-secret, so
/// there is nothing a wire-local twin would have to redact.
///
/// Non-secret: one timestamp, one classification, and a group id — never a token or email (#15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct AccountExpiry {
    /// The observed `refreshTokenExpiresAt` deadline as epoch SECONDS (converted from the
    /// credential's milliseconds at the read boundary, as [`AccountReading::access_expires_at`] is),
    /// or `None` when the credential carried no parseable value.
    ///
    /// `None` here ALWAYS pairs with [`ExpiryHorizon::Unknown`] in
    /// [`horizon_state`](Self::horizon_state): [`account_expiry`] admits no other combination.
    ///
    /// Emitted as an explicit `null` rather than omitted: inside a PRESENT `expiry` object, "polled
    /// and the credential carried no deadline" is a positive observation, not a missing key.
    #[serde(default)]
    pub(crate) expires_at: Option<i64>,
    /// Where [`expires_at`](Self::expires_at) sits relative to the operator's configured horizon.
    /// [`ExpiryHorizon::Unknown`] whenever no deadline was observed — never "not expiring" (the
    /// issue #137 invariant).
    #[serde(default)]
    pub(crate) horizon_state: ExpiryHorizon,
    /// The synchronized-expiry COHORT this account belongs to (issue #879), or `None` when this
    /// deadline groups with no other — which means "not grouped", never "alone at the front".
    ///
    /// [`account_expiry`] cannot fill this in: cohort membership is a property of the WHOLE fleet's
    /// deadlines, invisible to a per-account classifier. [`Daemon::snapshot`](crate::daemon::Daemon)
    /// overwrites it from [`expiry_cohorts`] once every account has been classified, which is why
    /// this type's own constructor still writes `None`.
    ///
    /// The id is a per-SNAPSHOT ordinal, assigned in ascending-deadline order and stable only
    /// within the frame that carried it. It identifies co-membership — two rows sharing an id
    /// expire together — and nothing beyond that; it is not a handle to look a cohort up by, and
    /// comparing one across frames is meaningless.
    ///
    /// `skip_serializing_if` keeps an ungrouped account's key OFF the wire, so the common
    /// unsynchronized fleet still serializes byte-identically to a pre-#879 frame and the
    /// status/watch goldens that carry no cohort gained no key across this bump — only their
    /// version stamp moved. **The key's mere APPEARANCE was the wire-contract change**,
    /// and #879 paid the full ritual for it: the MINOR [`STATUS_SCHEMA_VERSION`] bump to 1.13, the
    /// golden regeneration, and the current-daemon Swift fixture lockstep. Note what that
    /// byte-identity means for anyone extending this: a gate CANNOT tell you when a new grouping
    /// reaches the wire, because the goldens go on passing. The same is true of the
    /// `assert!(!frame.contains("cohort_id"))` in
    /// `account_line_encodes_the_expiry_modifier_only_once_the_account_has_been_polled`, which
    /// hand-builds its own `AccountExpiry` and so pins only that an ungrouped account stays quiet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cohort_id: Option<u32>,
}

/// Classify one account's REFRESH-token deadline against the operator's foresight horizon
/// (issue #878) — the orthogonal sibling of [`credential_health`], and the ONLY constructor of
/// [`AccountExpiry`].
///
/// `refresh_expires_at` is this account's `refreshTokenExpiresAt` deadline in epoch SECONDS — the
/// poll path is its only source, extracting it with
/// [`crate::refresh::refresh_token_expires_at`] and folding the credential's MILLISECONDS to
/// seconds at that boundary; `horizon_secs` is `[credential].expiry_horizon_secs`.
///
/// The classification, in order:
/// - **`None` ⇒ [`ExpiryHorizon::Unknown`]** — the load-bearing branch. An absent
///   `refreshTokenExpiresAt` (an older Claude Code, a changed upstream policy, a non-first-party
///   credential) is reported as UNKNOWN, **never** as "not expiring". This is the issue #137
///   invariant — *absence of a NEGATIVE signal alone is not health* — and it is what makes the whole
///   feature degrade safely should upstream ever drop the field: the daemon goes quiet-but-honest
///   rather than confidently wrong.
/// - **`deadline <= now` ⇒ [`ExpiryHorizon::Lapsed`]** — already past; only a re-login recovers it.
/// - **`deadline <= now + horizon` ⇒ [`ExpiryHorizon::Within`]** — inside the lookahead: still
///   working, but act before it lapses.
/// - **otherwise ⇒ [`ExpiryHorizon::Beyond`]** — further out than the horizon.
///
/// The deadline is ALWAYS read from the credential and never inferred: the ~30-day refresh-token
/// lifetime observed in the field is a server-side default, not a contract, so no lifetime constant
/// appears anywhere in this path. `horizon_secs` bounds the LOOKAHEAD only.
///
/// A pure function of explicit inputs (no clock, no I/O), so every branch — including the absent
/// field — is deterministically testable. The `Within` edge comes from the shared [`horizon_edge`],
/// whose saturating arithmetic keeps an extreme horizon from wrapping the comparison into a false
/// `Beyond`.
pub(crate) fn account_expiry(
    refresh_expires_at: Option<i64>,
    horizon_secs: u64,
    now_secs: i64,
) -> AccountExpiry {
    let horizon_state = match refresh_expires_at {
        // Absent ⇒ UNKNOWN, never "not expiring" (issue #137).
        None => ExpiryHorizon::Unknown,
        Some(deadline) if deadline <= now_secs => ExpiryHorizon::Lapsed,
        Some(deadline) if deadline <= horizon_edge(now_secs, horizon_secs) => ExpiryHorizon::Within,
        Some(_) => ExpiryHorizon::Beyond,
    };
    AccountExpiry {
        expires_at: refresh_expires_at,
        horizon_state,
        // Ungrouped by construction: cohort membership is a property of the WHOLE fleet's
        // deadlines, which this per-account classifier cannot see. `Daemon::snapshot` overwrites
        // this from [`expiry_cohorts`] once every account has been classified (issue #879).
        cohort_id: None,
    }
}

/// The instant the operator's foresight reaches — `now + horizon_secs`, saturating so an extreme
/// horizon cannot wrap it back into the past.
///
/// ONE definition, shared by [`account_expiry`] and [`expiry_cohorts`] deliberately: the
/// per-account [`ExpiryHorizon::Within`] boundary and the fleet condition's RAISE boundary are the
/// SAME instant, so a deadline sitting exactly on it must classify and raise together rather than
/// one but not the other. Two open-coded copies of this expression could only agree by intent.
fn horizon_edge(now_secs: i64, horizon_secs: u64) -> i64 {
    now_secs.saturating_add(i64::try_from(horizon_secs).unwrap_or(i64::MAX))
}

/// The fleet-level SYNCHRONIZED-EXPIRY COHORT condition (issue #879, REQ-CC-B-004): two or more
/// accounts whose `refreshTokenExpiresAt` deadlines fall within one grouping window, so the swap
/// pool loses several members at once.
///
/// **A FLEET fact, not a per-account one — that distinction is the whole point of the issue.** Each
/// member row already looks individually survivable; what no single row can show is that several of
/// them go together. So this is a DAEMON-LEVEL field of [`StatusResponse`], a sibling of
/// [`StatusResponse::keychain_locked`] and [`StatusResponse::canonical_scrub`], never a per-account
/// modifier and never a band listing handles-with-deadlines (the shape issues #543/#544 were retired
/// for). The upstream Claude Code client cannot see this at all: it warns only for the ACTIVE
/// account, so a parked account's deadline is invisible to it until swap-in.
///
/// Carries COUNTS and INSTANTS only — deliberately no member handles. Membership is already
/// recoverable from the per-account [`AccountExpiry::cohort_id`], so repeating it here would add
/// nothing while handing a renderer the per-account list the issue forbids. Non-secret by
/// construction: four numbers, never a token, email, or handle (issue #15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExpiryCohort {
    /// How many accounts are in this cohort. ALWAYS `>= 2` — a lone expiring account is not a
    /// cohort, which is the acceptance criterion most easily got wrong (REQ-CC-B-004 says "two or
    /// more"). [`expiry_cohorts`] is the only constructor and enforces it.
    pub(crate) size: u32,
    /// How many accounts carried a PARSED deadline at all — the denominator `size` is out of.
    ///
    /// Deliberately NOT the roster size. An account whose credential carried no
    /// `refreshTokenExpiresAt` is [`ExpiryHorizon::Unknown`], and the daemon cannot say whether it
    /// belongs to this cohort or not; quoting the roster would silently claim the unobserved
    /// accounts are OUTSIDE the cohort. Naming the observed denominator states the coverage instead
    /// of assuming it — the issue #137 invariant applied to an aggregate.
    pub(crate) observed: u32,
    /// The SOONEST member's deadline, epoch seconds — the instant the pool starts losing members.
    pub(crate) earliest: i64,
    /// Seconds between the soonest and latest member deadlines. `0` when they coincide, and never
    /// greater than the configured window (the anchored grouping in [`expiry_cohorts`] guarantees
    /// it), so "these fall within one window" is literally true of the members rather than an
    /// approximation.
    pub(crate) span_secs: i64,
}

/// Group a fleet's REFRESH-token deadlines into synchronized cohorts (issue #879) — the detector
/// REQ-CC-B-004 asks for, and the populator [`AccountExpiry::cohort_id`] was carved out for.
///
/// `deadlines` is one entry per account IN ROSTER ORDER, `None` where no deadline was observed;
/// `window_secs` is `[credential].expiry_cohort_window_secs`. Returns a per-account cohort
/// assignment aligned with the input, plus the fleet-level [`ExpiryCohort`] condition.
///
/// **Grouping is ANCHORED, not single-linkage.** Deadlines are sorted; the earliest ungrouped one
/// ANCHORS a cohort and every deadline up to `anchor + window` joins it; the next ungrouped deadline
/// anchors the next cohort. This guarantees every cohort's own span is `<= window`, so the claim
/// "these deadlines fall within one window" holds for the members themselves. Single-linkage
/// (extend while each CONSECUTIVE gap is within the window) was rejected for exactly that reason: it
/// chains, so ten accounts a day apart under a one-day window would form one "cohort" spanning nine
/// days and be reported as expiring together, which is false.
///
/// **The cardinality guard is load-bearing.** A group of one is not a cohort — it gets no
/// `cohort_id` and raises no condition. A cohort is by definition a MULTI-account synchronization,
/// and a per-account expiry warning is already the job of [`ExpiryHorizon`].
///
/// **Which cohort becomes the condition**: the one whose earliest deadline is soonest — it is the
/// one that bites first — and ONLY when that deadline is inside `horizon_secs`, the operator's
/// existing foresight knob, reused rather than joined by a second urgency threshold. Grouping is
/// unconditional, so `cohort_id` stays populated for a distant cohort and a client can group rows by
/// it; what waits for the horizon is only the RAISED condition. Without that gate a fleet onboarded
/// in one sitting would fly the banner permanently for the weeks it sits `Beyond`, and a condition
/// that is always on carries no information.
///
/// A pure function of explicit inputs — no clock, no I/O, no config read — so every branch is
/// deterministically testable, the same posture [`account_expiry`] takes.
pub(crate) fn expiry_cohorts(
    deadlines: &[Option<i64>],
    window_secs: u64,
    horizon_secs: u64,
    now_secs: i64,
) -> (Vec<Option<u32>>, Option<ExpiryCohort>) {
    let mut assignment: Vec<Option<u32>> = vec![None; deadlines.len()];

    // Sort the OBSERVED deadlines, carrying each one's roster index so the assignment can be
    // written back in place. The pair is DEADLINE-FIRST so a plain sort orders by deadline, the
    // index only breaking ties. Unobserved accounts never enter the walk: an absent deadline is not
    // a deadline "at time zero", and letting one anchor a cohort would invent a grouping out of the
    // very absence issue #137 forbids reading as information.
    let mut observed: Vec<(i64, usize)> = deadlines
        .iter()
        .enumerate()
        .filter_map(|(i, d)| d.map(|deadline| (deadline, i)))
        .collect();
    observed.sort_unstable();

    // The two bounds the walk reads, both drawn once: the grouping window is a SPAN added to each
    // anchor below, the foresight edge an INSTANT that does not move between cohorts.
    let window = i64::try_from(window_secs).unwrap_or(i64::MAX);
    let horizon_edge = horizon_edge(now_secs, horizon_secs);
    let mut condition: Option<ExpiryCohort> = None;
    let mut next_id: u32 = 0;
    let mut start = 0usize;

    while start < observed.len() {
        let anchor = observed[start].0;
        // Saturating, so an extreme window cannot wrap the edge into a false non-member.
        let window_edge = anchor.saturating_add(window);
        // The sort makes membership a contiguous RUN: once one deadline is past the edge, so is
        // every later one.
        let mut end = start + 1;
        while end < observed.len() && observed[end].0 <= window_edge {
            end += 1;
        }

        let size = end - start;
        if size >= 2 {
            let id = next_id;
            next_id += 1;
            for (_, idx) in &observed[start..end] {
                assignment[*idx] = Some(id);
            }
            let latest = observed[end - 1].0;
            let candidate = ExpiryCohort {
                // Saturating rather than asserting a ceiling: the roster has NO configured upper
                // bound (`Config::validate` documents that it enforces none), so these casts
                // degrade instead of claiming a bound nothing keeps. Truncation would take four
                // billion accounts.
                size: u32::try_from(size).unwrap_or(u32::MAX),
                observed: u32::try_from(observed.len()).unwrap_or(u32::MAX),
                earliest: anchor,
                span_secs: latest.saturating_sub(anchor),
            };
            // Raise only what the operator can act on: the anchor must reach into the foresight
            // horizon. Both this and `account_expiry`'s own `Within` test read the one
            // [`horizon_edge`] with the same `<=`, so a deadline sitting exactly on the edge
            // classifies and raises together rather than one but not the other. An already-LAPSED
            // cohort is past that edge in the other direction and is likewise inside — hence
            // comparing the deadline itself, not its distance.
            if anchor <= horizon_edge {
                // Cohorts are walked in ascending anchor order, so the FIRST one to qualify is
                // already the soonest; a later cohort can only be further out. Recording just the
                // first is therefore the soonest-wins rule, not an approximation of it.
                condition.get_or_insert(candidate);
            }
        }

        start = end;
    }

    (assignment, condition)
}

/// Reduce one account's stored refresh observations into the non-secret [`RefreshHealth`]
/// the wire surfaces (issue #119), or `None` when no refresh has been observed yet. `last_ok`
/// collapses the full outcome to alive-vs-not (`Refreshed` / `NoChange` ⇒ ok; `Dead` /
/// `Error` ⇒ not), the rollup's finer `Dead`-vs-`Error` distinction having already been
/// applied by [`credential_health`].
pub(crate) fn refresh_health_view(health: &AccountHealth) -> Option<RefreshHealth> {
    let outcome = health.last_refresh_outcome?;
    Some(RefreshHealth {
        last_ok: matches!(
            outcome,
            RefreshEventOutcome::Refreshed { .. }
                | RefreshEventOutcome::RefreshedNotReStashed { .. }
                | RefreshEventOutcome::NoChange
        ),
        // Read straight off the carried outcome, `None` and all (issue #1070) — the SAME accessor
        // the three `rotated=` log lines use, so no fourth rule exists for the wire to drift from.
        // #1004 removed the FALSE claim here by deriving the value instead of carrying it beside
        // the outcome, but stopped at `.unwrap_or(false)`: R-5a reserved the bump-or-keep call
        // ("which path is taken is a decision this scope has not made; it is not resolvable by an
        // implementer choosing the compiling one"), and the surviving `false` on every
        // `no_change` / `dead` / `error` account was "the exact uninformative value R-5 removes,
        // now on a versioned surface".
        //
        // That decision was made on 2026-08-06 (issue #1070): make it optional, present only where
        // an exchange ran. Dropping the `.unwrap_or` is the whole of it here — the accessor already
        // returns exactly the `Option` the wire now wants — and it completes AC-5 across all four
        // emitting surfaces. The cost is a MINOR `STATUS_SCHEMA_VERSION` bump (1.13 → 1.14) with
        // the status/watch goldens regenerated and the Swift mirror re-typed to `Bool?`; see
        // [`STATUS_SCHEMA_VERSION`] for why minor, and for the one asymmetric consequence.
        rotated: outcome.rotated(),
        consecutive_failures: health.consecutive_refresh_failures,
    })
}
