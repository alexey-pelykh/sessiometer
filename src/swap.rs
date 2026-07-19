// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The swap decision and the out-of-band swap engine.
//!
//! Two concerns live here: [`decide`] turns a usage reading into a
//! [`SwapDecision`] (the poll loop's per-tick verdict), and [`swap`] is the
//! out-of-band swap **engine** that *acts* on a decision — the callable unit that
//! rotates the active credential from one account to another. The poll→decision
//! loop that calls them (#7) and the cooldown / terminal state (#10 / #11) wire
//! this engine in; the engine itself is account-identity-agnostic, moving blobs
//! between stashes and the canonical keychain item addressed only by
//! `Sessiometer/<account_uuid>` stash-service name. (Issue #13's re-auth re-stash
//! is a sibling path: it refreshes a single stash through the same `AccountStash`
//! seam on a detected canonical change, without driving this swap engine.)
//!
//! ## The swap sequence (outgoing A → incoming B, one tick, this order)
//!
//! Replicates what `claude /login` writes — the canonical keychain token (the
//! functional reroute) then `~/.claude.json`'s `oauthAccount` (honest display) —
//! to inherit its proven cross-session propagation (H2; `build/version-compat.md`):
//!   1. read A's CURRENT (silently-refreshed) canonical blob;
//!   2. **re-stash A** to its `Sessiometer/<account_uuid>` stash BEFORE overwriting the
//!      canonical item — the token-refresh-rotation drift guard (#6 added
//!      acceptance). A's token has drifted (it refreshes in place while active),
//!      so the fresh blob is re-stashed; its `oauthAccount` is display-only and
//!      stable, so it is PRESERVED from A's existing stash, never fabricated;
//!   3. write B's token to the canonical item (atomic `-U`: a reader sees
//!      old-or-new, never empty / torn);
//!   4. co-write B's `oauthAccount` into `~/.claude.json` (best-effort display);
//!   5. re-read the canonical item to confirm B; a third writer (a concurrent
//!      `/login` or a refresh) that changed it leaves the swap unconfirmed, to be
//!      reconciled on the next cycle (re-read each cycle, never cache).
//!
//! Steps 1–3 are the swap proper (they must succeed — a failure aborts before or
//! at the atomic canonical write, leaving A safely re-stashed and the canonical
//! item un-torn); steps 4–5 are best-effort, display / diagnostic — the keychain
//! token is the authoritative bearer, so a clobbered `oauthAccount` self-heals on
//! the next reconcile (last-writer-wins).
//!
//! ## Mid-turn correctness (issue #12)
//!
//! Because the target app re-reads the canonical credential **per request**, a swap
//! that lands mid-turn must present a clean cut: the next request picks up the
//! incoming account, the in-flight request is unaffected, and no reader ever
//! observes a torn / half-written blob. That cut rests entirely on step 3's atomic
//! `-U` canonical write ("old-or-new, never empty / torn"). The
//! `tests::mid_turn_live` oracle demonstrates it against the real `security` CLI:
//! a concurrent reader re-reading the canonical item across a forced swap sees the
//! outgoing account, then the incoming one, and never anything in between. The
//! remaining live-only tail — the in-flight request's at-most-one
//! transparently-retried 401 — is the *target's* own retry, not ours, and stays a
//! deferred manual check (below).
//!
//! ## Deferred live checks (need a live token; cannot run in CI)
//!
//! These oracles need the real login keychain plus a live Claude token, so they are
//! verified manually rather than in CI (re-run on Claude Code auth bumps):
//!   - the end-to-end LIVE oracle — after a swap, an *independent* usage read
//!     reports the new account (the functional reroute actually took effect);
//!   - the mid-turn live tail (#12) — a running session adopts the incoming account
//!     on its next request, and the in-flight request absorbs at most one
//!     transparently-retried 401; `tests::mid_turn_live` proves the
//!     credential-cut half in CI, this is the target-behaviour half;
//!   - the `apple-tool:`-ride version check — the CLI write still rides the
//!     `apple-tool:` ACL entry on the current Claude Code version (#2).
//!
//! ## Single-machine-sync boundary (issue #613)
//!
//! Every lock this crate takes is a *per-machine* `flock`: the single-owner roster
//! lock, and [`SwapLock`] serialising this engine against a concurrent `/login`. Nothing
//! coordinates ACROSS machines — Sessiometer has no shared backend — so two machines each
//! running `sessiometer` against the SAME account roster is possible, and each daemon is
//! blind to the other's consumption. Two consequences follow, and they are load-bearing
//! for the swap math above:
//!   - **Co-consumption.** Both machines bill one account's session/weekly quota at the
//!     same time. [`TAIL_MARGIN`] is calibrated for a SINGLE machine's post-swap
//!     committed tail; two machines' tails stacked on one parked account can exceed it,
//!     landing the account past its ceiling even when each machine swapped on target.
//!   - **Per-machine visibility.** The landing check is per-machine — the offline SLI 5
//!     in [`crate::reliability`] and its runtime mirror [`crate::landing`] (issue #613)
//!     both only see what THIS daemon observed. A second machine pushing a parked account
//!     over the ceiling is invisible to them.
//!
//! The one guard that DOES cross the boundary is velocity-spike detection (the reactive
//! arm [`reactive_session_threshold`] and the daemon's projection arm `velocity_swap`):
//! both read each account's usage from the account-global `/oauth/usage` endpoint, which
//! already reflects BOTH machines' combined burn, so a co-consumption spike shows up as a
//! faster-than-modelled climb and can fire an earlier swap. It REDUCES the exposure — it
//! does not remove it: the committed tail and the shared per-account rate limits still
//! apply, and two machines can briefly stack usage between polls. Running one roster per
//! machine avoids the boundary entirely; spanning two treats velocity spikes as the
//! safety net, not a guarantee.

use std::fs::OpenOptions;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::claude_state;
use crate::error::{Error, Result};
use crate::keychain::CredentialStore;
use crate::stash::{AccountStash, StashedAccount};
use crate::usage::Usage;

/// What the poll loop decided to do about the active account this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapDecision {
    /// Stay on the current account.
    Hold,
    /// One of the active account's usage dimensions reached its own trigger
    /// (issue #41: session or weekly); the swap engine should rotate to the next
    /// account.
    Swap,
}

/// Decide whether to swap: trigger when EITHER dimension reaches its OWN
/// threshold (issue #41) — the active account's session usage at/above
/// `session_threshold`, OR its weekly usage at/above the separate (typically
/// higher) `weekly_threshold`. The two thresholds are independent: either
/// crossing alone forces a swap-away, and neither subsumes the other.
///
/// Since issues #597 (session) and #607 (weekly), BOTH thresholds are derived
/// BACKWARD from their own dimension's ceiling rather than being fire-*at* values:
/// the caller passes `effective_ceiling(session_ceiling) − velocity × lookahead` and
/// [`weekly_effective_ceiling`]`(weekly_ceiling)` respectively, so a swap LANDS each
/// dimension below its own ceiling after that dimension's post-swap committed tail.
/// This function stays a pure two-threshold predicate — which ceiling each threshold
/// came from, and how far back it was derived, is the daemon's business.
pub(crate) fn decide(usage: &Usage, session_threshold: f64, weekly_threshold: f64) -> SwapDecision {
    if usage.session >= session_threshold || usage.weekly >= weekly_threshold {
        SwapDecision::Swap
    } else {
        SwapDecision::Hold
    }
}

/// The post-swap committed-tail margin, as a usage FRACTION: how far BELOW the ceiling a
/// swap must land the outgoing account so its in-flight drain does not carry the peak past
/// the ceiling. The parked account keeps billing already-committed work after the swap
/// redirects only NEW requests (issue #595 measured the tail at the landing point; #596
/// confirmed it is real in-flight drain, not a cache artifact), so the LANDING sits above
/// the swap-decision point by this tail.
///
/// Set to 0.06 (6 pp): STRICTLY above the measured max committed tail of +5 pp (re-verified
/// against fresh landing data 2026-07-17), so the strict `P100 < 99` landing SLO (issue
/// #455 / #595) keeps ~1 pp headroom even on the worst observed tail. Deliberately larger
/// than the p90 (+2 pp) — the SLO is defined by the MAX, not the median, and capacity is not
/// binding (`all_exhausted` fired 2× in 17 days), so buying margin is cheap while overshoot
/// is expensive (issue #597).
pub(crate) const TAIL_MARGIN: f64 = 0.06;

/// The EFFECTIVE ceiling both swap arms derive their fire point backward from (issue #597):
/// the configured `ceiling` (the settled line the outgoing account must not cross) less the
/// [`TAIL_MARGIN`]. A swap that lands the outgoing account AT this effective ceiling leaves
/// exactly the tail margin of headroom, so the post-swap committed tail lands below the true
/// ceiling. Clamped at 0 so a pathologically low `ceiling` can never yield a negative fire
/// point. `ceiling` is a fraction in `[0.0, 1.0]` (`session_ceiling` / 100).
pub(crate) fn effective_ceiling(ceiling: f64) -> f64 {
    (ceiling - TAIL_MARGIN).max(0.0)
}

/// The WEEKLY post-swap committed-tail margin, as a usage FRACTION — the weekly analogue of
/// [`TAIL_MARGIN`] (issue #607). The same in-flight work that keeps billing the parked account's
/// SESSION window after a swap bills its WEEKLY window too, so the weekly dimension has the same
/// blind spot the session dimension lost in issue #597: a swap that fires exactly AT the weekly
/// line still lands above it.
///
/// # Provenance — SCALED from the #595 session measurement under ONE stated assumption
///
/// The weekly tail has NOT been measured directly; no weekly landing SLI exists yet (building one
/// is the follow-up this constant's re-verification note names). What follows is the scaling
/// argument the value rests on, stated so the assumption is auditable rather than buried.
///
/// The committed tail is ONE fixed quantity of already-committed work `Δ` (issue #596 confirmed it
/// is real in-flight drain, not a cache artifact). It bills BOTH windows, so it is the same `Δ`
/// expressed against two different denominators:
///
/// ```text
/// weekly_tail_fraction = session_tail_fraction × (session_quota / weekly_quota)
/// ```
///
/// The #595 landing SLI measured the session side over 2026-07-01…07-17: mean +1.08 pp, p90 +2 pp,
/// **max +5 pp** of the SESSION window. Writing `k = weekly_quota / session_quota` (how many full
/// session windows the weekly budget is worth), the worst-case weekly tail is therefore `5 pp / k`,
/// and this margin covers it exactly when:
///
/// ```text
/// k >= 5      i.e. the weekly budget is worth at least five full session windows
/// ```
///
/// **That inequality is the assumption — it is not derived, and it is what to re-check.** Note what
/// this margin deliberately does NOT assume: that `k` equals the *window-duration* ratio
/// (`168 h / 5 h ≈ 33.6`). That ratio is an upper bound on `k`, not a safe one — it describes a
/// weekly budget large enough to run every session window back-to-back at full tilt, i.e. a weekly
/// limit that never binds. This tool exists because the weekly limit DOES bind
/// (`weekly_exhausted` is a reachable, surfaced state), so the real `k` is materially below 33.6
/// and a margin justified by the duration ratio alone would be justified in the wrong direction.
/// The `k >= 5` breakeven is the honest statement of what 1 pp buys.
///
/// **Why 1 pp is nonetheless the right value now.** `k >= 5` is a weak requirement — it fails only
/// if a whole week's budget is worth under five 5-hour sessions, which would make the weekly limit
/// bind within a single heavy day. Three further properties bound the cost of being wrong: there is
/// no weekly SLO analogous to session's `P100 < 99` (so an under-margin degrades runway, it does
/// not breach a committed target); the default ceiling 98 leaves 2 pp of slack to the real 100 wall
/// beneath this margin; and the error asymmetry is the one [`TAIL_MARGIN`] cites — an early swap is
/// the cheap error. Firing 1 pp early on a 7-day window is a small, bounded runway cost.
///
/// **Why not session's 6 pp.** Copying it would fire the weekly arm at 92 under the default 98
/// ceiling — surrendering 6 pp of a 7-day window to guard a tail that is `5 pp / k` for a `k` well
/// above 1. The two dimensions measure different quantities against different denominators (issue
/// #41) and so carry independently-calibrated margins; that independence is the point.
///
/// Kept honest the same way [`TAIL_MARGIN`] is, and this constant needs it MORE because it is
/// scaled rather than measured: build the WEEKLY landing SLI — the peak `weekly_pct` a parked
/// account reaches within `reliability::LANDING_WINDOW_SECS` of a swap, the weekly analogue of #595
/// — and re-calibrate against the observed distribution. Until then this value rests on the stated
/// `k >= 5` assumption. Do not widen it silently, and do not restate the assumption as a finding.
pub(crate) const WEEKLY_TAIL_MARGIN: f64 = 0.01;

// Compile-time calibration guards (issue #607), in the style of `config::COOLDOWN_SECS_FLOOR`'s
// (#272): both invariants below are pure constant relationships, so a regression is a BUILD
// failure here rather than a test failure later. Neither is evidence that the VALUE is right —
// they pin the two structural properties the derivation needs; the empirical question is the
// `k >= 5` assumption above, which only a weekly landing SLI can settle.
//
// 1. The margin must stay strictly positive, or the weekly arm degenerates back to the fire-AT
//    trigger this issue replaced (fire point == ceiling, landing above it).
const _: () = assert!(WEEKLY_TAIL_MARGIN > 0.0);
// 2. The two dimensions carry INDEPENDENTLY calibrated margins (issue #41), and the weekly one is
//    necessarily the smaller: the same committed tail is a smaller fraction of the longer weekly
//    window for any `k > 1`. Equality would mean someone copied the session constant — the
//    specific mistake this constant's provenance rejects.
const _: () = assert!(WEEKLY_TAIL_MARGIN < TAIL_MARGIN);

/// The EFFECTIVE WEEKLY ceiling the swap decision derives its weekly fire point backward from
/// (issue #607) — the weekly analogue of [`effective_ceiling`]: the configured weekly `ceiling`
/// (the settled weekly line the outgoing account must not cross) less [`WEEKLY_TAIL_MARGIN`]. A
/// swap that lands the outgoing account AT this effective ceiling leaves exactly the weekly tail
/// margin of headroom, so the post-swap committed tail lands below the true weekly ceiling.
///
/// Kept a SEPARATE function from [`effective_ceiling`] rather than a shared one parameterised by
/// margin, because the two dimensions are independent by design (issue #41): each carries its own
/// measured margin, and a single knob would invite the copy this constant's provenance rejects.
///
/// Clamped at 0 so a pathologically low `ceiling` can never yield a negative fire point. `ceiling`
/// is a fraction in `[0.0, 1.0]` (`weekly_ceiling` / 100).
pub(crate) fn weekly_effective_ceiling(ceiling: f64) -> f64 {
    (ceiling - WEEKLY_TAIL_MARGIN).max(0.0)
}

/// The reactive arm's re-observation-gap lookahead, in seconds: how long the active account can
/// climb UNSEEN between the daemon's successive observations of it. The measured **p90** of the
/// active-account re-observation gap.
///
/// # Provenance (issue #609, implementing ADR-0023 § Alternatives 6)
///
/// The active-account re-observation gap — swap-decision to next reading, over the #366 staggered
/// interleave (ADR-0012) — was measured at **p50 112 s / p90 313 s / max 972 s**. The pre-#609
/// reactive lookahead used the THEORETICAL near-limit round-trip `2 × near_limit_poll_secs` (~120 s
/// at the default cadence), which UNDER-models that real gap: on the p90 tail the account climbs
/// ~200 s longer than modeled — past `effective_ceiling` before the next poll — and the post-swap
/// committed tail (#595) then lands it over the `P100 < 99` SLO. That under-modeling is precisely
/// why ADR-0023 set the default ceiling BELOW the SLO (a second, gap-independent margin). Widening
/// the reactive lookahead to the p90 gap makes the reactive arm honest about how long it is blind,
/// so the margin is earned by the lookahead rather than by a sub-SLO ceiling and the ceiling could
/// return to the SLO line (issue #609 leaves the DEFAULT ceiling at 95 — the operator's lever —
/// while making 99 reachable).
///
/// **p90, not max.** `max` (972 s) over-fits to the worst gap ever seen (a backoff outlier) yet
/// would apply every cycle: at the observed peak velocity (~6.95 %/min ≈ 0.00116 frac/s) a 972 s
/// lookahead pulls the fire point down by `0.00116 × 972 ≈ 1.13` — to/below 0, a swap at any usage —
/// while even the MEDIAN account (0.63 %/min) fires ~10 pp early. p90 keeps the median's early-fire
/// to ~3 pp while still pulling the fast tail in; the gap beyond p90 is absorbed by the
/// defence-in-depth already present ([`TAIL_MARGIN`] plus the sub-SLO ceiling headroom). The error
/// asymmetry favours it: an early swap is the CHEAP error (`all_exhausted` fired 2× in 17 days —
/// capacity is not binding), an overshoot the expensive one.
///
/// A calibrated CONSTANT, not a runtime-adaptive estimate — the same discipline as [`TAIL_MARGIN`].
/// The re-observation gap is a slow-moving property of the polling architecture, not a fast signal;
/// a runtime percentile would close an adaptive loop with its own failure modes (a transient backoff
/// incident inflating the observed gap would make every account swap far too early exactly when
/// capacity is tightest). Re-verify against the #595 landing SLI if the gap distribution is suspected
/// to have shifted (re-measure the active re-observation gap; the #538/#540 replay recipe).
///
/// Used as the measured **floor** beneath the cadence-scaled proxy in [`reactive_poll_gap_secs`]
/// (the daemon looks ahead over `max(2 × near_limit_poll_secs, this)`), not as the lookahead
/// outright — so a slower configured near-limit poll only WIDENS the lookahead past this gap, never
/// shrinks below it.
pub(crate) const REACTIVE_REOBSERVATION_GAP_SECS: f64 = 313.0;

/// The reactive arm's re-observation-gap lookahead window fed to [`reactive_session_threshold`], in
/// seconds, derived from the near-limit poll cadence (issue #609):
///
/// ```text
/// near_limit_poll_secs == 0  →  0    (fast-poll disabled — no near-limit gap to cover; the #539
///                                     projection is then the sole velocity-aware estimator, #584)
/// near_limit_poll_secs  > 0  →  max(2 × near_limit_poll_secs, REACTIVE_REOBSERVATION_GAP_SECS)
/// ```
///
/// The window is the LARGER of two lower bounds on the true re-observation gap: the measured p90
/// FLOOR ([`REACTIVE_REOBSERVATION_GAP_SECS`], 313 s — the scheduling / back-off / interleave
/// overhead that dominates at the default cadence) and the cadence-scaled round-trip `2 ×
/// near_limit_poll_secs` (a slower configured poll widens the real gap proportionally — the pre-#609
/// model kept as the scaling term, not discarded). Taking the `max` keeps the #609 substitution an
/// UNCONDITIONAL widening of the pre-#609 lookahead `min(2 × near_limit_poll_secs, H)` — `max(2c,
/// floor) ≥ 2c ≥ min(2c, H)` for every cadence `c` and horizon `H` — so the reactive `poll_gap` never
/// SHRINKS versus before, and (the composed fire point being monotone non-increasing in `poll_gap`,
/// see [`reactive_session_threshold`] § Max-window coverage) the landing point never rises: the
/// `P100 < 99` SLO stays reachable BY CONSTRUCTION for ALL configs, not only those where the bare
/// floor already exceeds the prior lookahead. A bare `poll_gap = REACTIVE_REOBSERVATION_GAP_SECS`
/// would NARROW the lookahead — a latent SLO regression — whenever `2 × near_limit_poll_secs > 313`
/// (`near_limit_poll_secs > 156`) with `H > 313`; the `max` closes that corner (ADR-0024 § Decision 1
/// / § Alternatives 6).
pub(crate) fn reactive_poll_gap_secs(near_limit_poll_secs: u64) -> f64 {
    if near_limit_poll_secs == 0 {
        0.0
    } else {
        (2.0 * near_limit_poll_secs as f64).max(REACTIVE_REOBSERVATION_GAP_SECS)
    }
}

/// The reactive arm's session fire threshold (issues #597, #609): the OBSERVED session fraction at
/// or above which [`decide`] should swap away, derived BACKWARD from `effective_ceiling` so the
/// account lands AT the effective ceiling by the time the swap actually executes — one
/// re-observation gap later, having climbed at `velocity`:
///
/// ```text
/// effective_ceiling − velocity × poll_gap_secs
/// ```
///
/// `poll_gap_secs` is how long the active account climbs UNSEEN between successive observations —
/// the reactive re-observation-gap lookahead ([`reactive_poll_gap_secs`]): `max(2 ×
/// near_limit_poll_secs, REACTIVE_REOBSERVATION_GAP_SECS)` (the measured p90 313 s as a floor) when
/// the near-limit fast-poll is on, or `0` when it is disabled (`near_limit_poll_secs == 0`),
/// collapsing the threshold to the bare `effective_ceiling` so the #539 projection is then the sole
/// velocity-aware estimator (the pre-#597 division of labour, now landing-margined).
///
/// # Max-window coverage (issue #609, superseding #597's strict-early-fire framing)
///
/// The reactive arm covers ONE unseen window — the **re-observation gap** (`poll_gap_secs`: how long
/// until the account is next SEEN). The projection arm (the daemon's `velocity_swap`, #539 /
/// ADR-0017) covers a DIFFERENT one — the **velocity horizon** `H` (`session_velocity_horizon_secs`:
/// how far the EMA reaches), firing at observed `effective_ceiling − velocity × H`. Neither window
/// bounds the other, so this function is deliberately INDEPENDENT of `H`. The two arms are composed
/// by the daemon (reactive checked first in [`decide`], projection consulted on `Hold`), and the
/// composed swap fires at the EARLIER of the two thresholds:
///
/// ```text
/// min(effective_ceiling − velocity × poll_gap_secs,  effective_ceiling − velocity × H)
///   = effective_ceiling − velocity × max(poll_gap_secs, H)
/// ```
///
/// — early enough to cover the LARGER unseen window, monotone non-increasing in BOTH `poll_gap_secs`
/// and `H` (widening either window never delays the swap, so it can only LOWER the landing point —
/// the `P100 < 99` SLO stays reachable by construction). Which arm leads is config-dependent, and
/// both stay live:
///   - `poll_gap_secs > H` (the default: 313 s gap vs 120 s horizon): the reactive arm is the
///     earlier fire — the gap-tail specialist.
///   - `H > poll_gap_secs`, or `poll_gap_secs == 0` (fast-poll disabled): the projection arm is the
///     earlier fire — the horizon / fast-poll-off fallback (#584).
///
/// This REPLACES #597's `max`-clamp form, whose "the projection never fires later than the reactive
/// arm" (strict-early-fire) invariant capped the reactive lookahead at `H` — silently DISCARDING any
/// widening of the re-observation gap beyond `H` (the #609 bug: the real gap runs to p90 313 s, but
/// the clamp pinned the reactive lookahead at the 120 s horizon). The composed-min identity above is
/// the honest generalization: each arm covers its own window; the swap covers their union.
///
/// `velocity` is a FRACTION per second (matching the `session` reading). `velocity == 0` (an
/// unwarmed / reset EMA — gated identically in both arms by `MIN_VELOCITY_SAMPLES`) collapses the
/// threshold to the bare `effective_ceiling`, so an idle account fires AT the effective ceiling,
/// never early.
///
/// # Unbounded below — by design
///
/// The threshold is NOT floored (unlike `effective_ceiling`, which clamps at 0): under a large
/// `velocity × poll_gap_secs` it drops well below `effective_ceiling` and, at the extreme (sustained
/// peak velocity over the p90-or-wider gap), to/below 0. That is the intended early-protective fire:
/// the account is climbing so fast RELATIVE TO ITS RE-OBSERVATION GAP that it must swap now to land
/// at `effective_ceiling` by the next poll — the CHEAP error under issue #597's asymmetry (an early
/// swap wastes runway; an overshoot breaches the SLO, and capacity is not binding). Widening the
/// lookahead to the p90 gap (#609) DEEPENS this reach and correspondingly SHARPENS the disclosed
/// retained-EMA staleness window (a burst that ended keeps the EMA high for a few decay ticks, so a
/// swap can fire at moderate usage a current EMA would not warrant) — an accepted property of the
/// council-chosen retained-EMA velocity, bounded and self-correcting. A below-0 value is
/// behaviourally identical to 0 in the sole consumer (`decide`'s `usage.session >= threshold`, and
/// `session` is never negative), so it is left unclamped rather than carrying a cosmetic `.max(0.0)`
/// that would fix no behaviour.
///
/// This is the one place the two arms deliberately DIVERGE: the projection arm carries a
/// `session_velocity_min_project_above` observed-usage floor (its small ≤~14 pp lookahead makes a
/// low reading unable to cross, so a low-usage projection is spurious), whereas the reactive arm's
/// lookahead spans the full re-observation gap and CAN legitimately cross from a lower reading — so
/// it carries no such floor. The config-LOAD bound on the absurd combos is
/// [`peak_runway_reserve_bound`] (issue #608, discharging ADR-0023 § Alternatives 3): a config whose
/// bound is UNSATISFIABLE — the peak-velocity fire point at/below 0, i.e. every account swapping at
/// any usage — is rejected at load, so the unbounded-below reach documented here stays a runtime
/// property of *live* velocity and never a stacked-config pathology.
pub(crate) fn reactive_session_threshold(
    effective_ceiling: f64,
    velocity: f64,
    poll_gap_secs: f64,
) -> f64 {
    effective_ceiling - velocity * poll_gap_secs
}

/// The assumed PEAK session velocity, in the measurement's own unit (percent per minute) — the
/// worst climb rate a config's swap-target reserve must keep runway against (issue #608).
///
/// # Provenance (issue #608, discharging ADR-0023 § Alternatives 3)
///
/// Calibrated from the measured `session_pct_per_min` distribution (the durable log's
/// `event=usage_velocity` line, issue #399/#449): **p50 0.63 / p90 1.86 / max 6.95 %/min** — an ~11×
/// spread, the same distribution ADR-0022 § Context cited to reject a single fire-*at* trigger.
///
/// **max, not p90** — the opposite of the [`REACTIVE_REOBSERVATION_GAP_SECS`] choice, and for the
/// reason that constant states. p90 was chosen *there* because `max` "over-fits to the worst gap ever
/// seen yet **would apply every cycle**": that constant is a RUNTIME term in the fire point, so
/// over-fitting spends real early swaps on every account forever. `v_peak` is a CONFIG-LOAD bound —
/// evaluated once per config parse, never in the fire path — so over-conservatism costs nothing at
/// runtime and the p90-rejection rationale does not transfer. What DOES transfer is
/// [`TAIL_MARGIN`]'s: the quantity guarded is a `P100` SLO, "defined by the MAX, not the median",
/// under an error asymmetry where buying margin is cheap. A bound calibrated at p90 would call a
/// config safe that a single observed-peak burst empties.
///
/// Kept honest by the observed-peak SLI (`sessiometer reliability`, issue #608): it re-measures the
/// live `session_pct_per_min` distribution against this constant and flags when the real peak
/// outruns it — the "measure, don't trust the constant" discipline [`TAIL_MARGIN`] has via the #595
/// landing SLI. A sustained breach means re-calibrate this value, not widen the bound silently.
pub(crate) const V_PEAK_SESSION_PCT_PER_MIN: f64 = 6.95;

/// [`V_PEAK_SESSION_PCT_PER_MIN`] in the fire-point math's unit: a usage FRACTION per SECOND, the
/// same unit as the `velocity` [`reactive_session_threshold`] multiplies by a lookahead in seconds.
///
/// Derived from the percent-per-minute constant rather than written as a literal `0.00115833…`, so
/// the unit conversion is auditable and the two forms can never drift: the SLI compares observed
/// `%/min` against the former, the bound multiplies seconds by the latter, and both name one
/// calibrated number.
pub(crate) const V_PEAK_SESSION_FRAC_PER_SEC: f64 = V_PEAK_SESSION_PCT_PER_MIN / 100.0 / 60.0;

/// The tighter swap-target reserve bound (issue #608, discharging ADR-0023 § Alternatives 3): the
/// highest `target_max_session_usage` — as a usage FRACTION — that still leaves a swapped-to account
/// runway when it is climbing at the assumed peak velocity [`V_PEAK_SESSION_FRAC_PER_SEC`].
///
/// ```text
/// effective_ceiling(ceiling) − V_PEAK × max(reactive_poll_gap_secs(near_limit), horizon_secs)
/// ```
///
/// # Why this is tighter than the ADR-0013 reserve invariant
///
/// `Config::validate` enforces `target_max_session_usage <= session_ceiling`
/// (`Error::ConfigTargetMaxSessionAboveTrigger`, ADR-0013). Under the issue #597 ceiling semantics
/// that bound is correct but **loose**: it bounds the reserve by the CEILING, whereas the newly
/// active account is judged against the composed *fire point*, which sits a whole lookahead below the
/// ceiling. `target_max_session_usage`'s own doc promises a swapped-to target "keeps runway before
/// the next poll" — this function is that promise stated in arithmetic.
///
/// # Why `max(poll_gap, horizon)` and not `poll_gap` alone
///
/// ADR-0023 § Alternatives 3 wrote the coupling as `… − v_peak × poll_gap`, but that predates issue
/// #609 / ADR-0024, which DECOUPLED the reactive arm from the horizon `H`. Post-#609 the two arms
/// cover different unseen windows and the composed swap fires at the EARLIER of the two thresholds —
/// `effective_ceiling − velocity × max(poll_gap, H)` (see [`reactive_session_threshold`]
/// § Max-window coverage). Bounding against `poll_gap` alone would therefore MISS the projection arm
/// entirely whenever `H > poll_gap`, and completely when the near-limit fast-poll is disabled
/// (`near_limit_poll_secs == 0` → `poll_gap == 0`, while `H` may be up to its 600 s ceiling) — which
/// is precisely the `session_velocity_horizon_secs` half of the stacking issue #608 names. The
/// composed `max` is the honest post-#609 restatement of the same coupling.
///
/// # Unsatisfiable is the enforced case
///
/// The result can go to/below 0 — the config stacks lookahead so wide that at peak velocity NO
/// reserve in the legal `1..=session_ceiling` range keeps runway. That is ADR-0023 § Consequences'
/// "absurd-config corner" (a config pairing a high `near_limit_poll_secs` with `horizon_secs` at its
/// ceiling pulls the reactive fire point to/below 0 — every account swapping at any usage), and it is
/// what `Config::validate` REJECTS (`Error::ConfigPeakRunwayUnsatisfiable`). A merely EXCEEDED bound
/// (positive, but below the configured reserve) is NOT rejected — the shipped default is in exactly
/// that state by deliberate design (ADR-0023 recorded the looseness), so rejecting it would brick
/// every stock install; it surfaces as a non-fatal advisory in `sessiometer config validate`.
///
/// `ceiling` is a fraction in `[0.0, 1.0]` (`session_ceiling` / 100), matching
/// [`effective_ceiling`]. Pure and total — no clamping, so the caller can distinguish
/// "unsatisfiable" (`<= 0`) from "tight" (a small positive).
pub(crate) fn peak_runway_reserve_bound(
    ceiling: f64,
    near_limit_poll_secs: u64,
    horizon_secs: u64,
) -> f64 {
    let window = composed_swap_lookahead_secs(near_limit_poll_secs, horizon_secs);
    effective_ceiling(ceiling) - V_PEAK_SESSION_FRAC_PER_SEC * window
}

/// The composed swap lookahead, in seconds: how long the active account climbs UNSEEN before EITHER
/// arm fires — `max(reactive re-observation gap, velocity horizon)` (issue #608, the post-#609
/// max-window form). The single source of truth for the window: [`peak_runway_reserve_bound`]
/// *computes* the bound over it, and `Config::validate` / `Config::peak_runway_advisory` *report* it
/// to the operator (the `window_secs` in the load error and the advisory). Extracted so those
/// reporting sites cannot drift from the value the bound was actually derived over — if the
/// composition ever gains a third term, all three move together. See [`reactive_session_threshold`]
/// § Max-window coverage for why the two arms' windows compose by `max`.
pub(crate) fn composed_swap_lookahead_secs(near_limit_poll_secs: u64, horizon_secs: u64) -> f64 {
    reactive_poll_gap_secs(near_limit_poll_secs).max(horizon_secs as f64)
}

/// The highest SESSION fraction observed within ONE session window (issue #614) — the
/// plausibility baseline both swap arms measure a suspiciously LOW reading against.
///
/// # Why a per-window high-water mark
///
/// Session usage is MONOTONIC within a window: the 5 h window only accumulates, so a reading that
/// falls while the window is demonstrably the same one cannot be real drain — it is a stale /
/// cache-lagged `/oauth/usage` response. Both swap arms trust `active_usage.session` at face value
/// (`decide` here, [`crate::daemon::Daemon::velocity_swap`] there), so such a reading sits below the
/// fire threshold and cancels an otherwise-due swap while the account is actually higher; two in a
/// row also under-estimate the #539 velocity EMA, so BOTH arms then fire late. Retaining the window's
/// high-water mark gives the arms a floor to fall back on, and it is a genuine LOWER BOUND on the
/// true usage rather than a synthesized number.
///
/// The window is identified by `session_resets_at` — matched with a tolerance, NOT byte-equality
/// (see [`SESSION_WINDOW_MATCH_SECS`]) — so a DROP inside one window is implausible, while a drop
/// paired with a stamp a whole window away is the legitimate roll (the mark then restarts at the
/// fresh reading, per [`Self::fold`]).
///
/// # Bounded, and fail-safe when the evidence is missing
///
/// The mark cannot pin an account indefinitely: it is scoped to one window and released as soon as
/// `session_resets_at` moves a window away. And it is only ever consulted on POSITIVE evidence — a
/// reading whose `session_resets_at` is absent (`None`, the API did not report a parseable stamp) or
/// names a different window carries no evidence that the window is unchanged, so it is trusted as-is
/// and the pre-#614 behaviour stands. That residual trust boundary is deliberate: the guard fires
/// only where the drop is provably impossible, never on a guess. In practice the absent-stamp case is
/// the COMMON one (the API omits `session_resets_at` on most zero-session readings), so a
/// window-reset drop is usually released by the stamp VANISHING rather than by it moving.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SessionHighWater {
    /// The session window this mark belongs to — the FIRST `Usage::session_resets_at` seen for it, in
    /// epoch seconds. Deliberately NOT refreshed by later readings of the same window: the stamp
    /// jitters ±1 s poll to poll (see [`SESSION_WINDOW_MATCH_SECS`]), so re-anchoring on every fold
    /// would let the reference walk. A fixed anchor plus the match tolerance keeps every reading in
    /// the window comparing against one stable value.
    window: i64,
    /// The highest `session` fraction observed within that window.
    session: f64,
}

/// How far apart two `session_resets_at` stamps may be and still name the SAME session window, in
/// seconds (issue #614).
///
/// # Provenance (measured, not assumed)
///
/// The stamp is NOT stable byte-for-byte within one window. Measured over the daemon's own usage
/// sample store (24,244 samples / 6 accounts, 17,978 consecutive stamped pairs), the delta between
/// successive stamps of the SAME window is:
///
/// ```text
///    0 s   50.1%          ±1 s   49.8%          |Δ| ≤ 1 s   99.8%
/// ```
///
/// The API renders `resets_at` with sub-second precision and [`crate::usage::epoch_from_rfc3339`]
/// truncates the fraction, so a value straddling a second boundary alternates between `N` and `N+1`.
/// A byte-equality window key would therefore have failed to match on ~HALF of all same-window polls
/// — and worse, each mismatch would restart the mark at the very stale-low reading it exists to
/// reject, so the guard would have fired essentially NEVER in production while every hand-pinned unit
/// test stayed green.
///
/// A genuine window roll is ~5 h away: the smallest non-jitter delta observed in the same corpus is
/// 3600 s, and the roll cluster sits at 17999–18001 s. **60 s** therefore separates the two
/// populations with ~2 orders of magnitude of margin on BOTH sides — 60× the observed jitter, and
/// 60× below the nearest real change. It is a calibrated constant, not a tuned knob; re-measure
/// against the sample store if the API's `resets_at` rendering is suspected to have changed.
pub(crate) const SESSION_WINDOW_MATCH_SECS: i64 = 60;

impl SessionHighWater {
    /// Fold one LIVE reading into the mark, returning the mark to retain.
    ///
    /// - No `session_resets_at` on the reading → `None`: without a window identity no later drop can
    ///   be judged implausible, so carrying a mark forward would risk holding a floor across an
    ///   unseen window roll.
    /// - Same window as the current mark (within [`SESSION_WINDOW_MATCH_SECS`]) → the running
    ///   maximum, keeping the mark's ORIGINAL anchor stamp; a stale-low reading therefore leaves the
    ///   mark standing at the higher, plausible value instead of dragging the baseline down.
    /// - A DIFFERENT window, or no mark yet → start fresh at this reading, which is how a legitimate
    ///   window reset releases the previous window's floor.
    ///
    /// Call on successful polls ONLY. A failed poll is blindness, not evidence the window rolled, so
    /// it must leave the mark untouched.
    pub(crate) fn fold(mark: Option<Self>, usage: &Usage) -> Option<Self> {
        let window = usage.session_resets_at?;
        Some(match mark {
            Some(prev) if prev.matches_window(window) => Self {
                // The ORIGINAL anchor, not `window` — see the field doc: re-anchoring each fold would
                // let the reference walk with the per-poll jitter.
                window: prev.window,
                session: prev.session.max(usage.session),
            },
            _ => Self {
                window,
                session: usage.session,
            },
        })
    }

    /// Whether `stamp` names this mark's session window, tolerating the per-poll rendering jitter
    /// ([`SESSION_WINDOW_MATCH_SECS`]).
    fn matches_window(self, stamp: i64) -> bool {
        self.window.abs_diff(stamp) <= SESSION_WINDOW_MATCH_SECS.unsigned_abs()
    }

    /// The retained high-water fraction that APPLIES to `usage` — `Some` only when the mark and the
    /// reading name the same session window, so a rolled (or unstamped) window yields `None` and the
    /// reading stands on its own.
    fn applies_to(self, usage: &Usage) -> Option<f64> {
        usage
            .session_resets_at
            .filter(|&stamp| self.matches_window(stamp))
            .map(|_| self.session)
    }
}

/// Whether `usage`'s session reading is IMPLAUSIBLY LOW (issue #614): it sits BELOW the high-water
/// mark of the very window it claims to be in. Usage cannot fall within a window, so this is a
/// stale / cache-lagged reading, not real drain.
///
/// `None` mark, an unstamped reading, or a rolled window → `false` (no evidence of implausibility;
/// see [`SessionHighWater`] § Bounded, and fail-safe when the evidence is missing).
pub(crate) fn is_stale_low(mark: Option<SessionHighWater>, usage: &Usage) -> bool {
    mark.and_then(|m| m.applies_to(usage))
        .is_some_and(|high_water| usage.session < high_water)
}

/// `usage` with its SESSION fraction raised to the window's high-water mark (issue #614) — the
/// reading the swap arms should decide on, so a stale-low response cannot cancel an otherwise-due
/// swap. A plausible reading (at or above the mark, or carrying no window evidence) is returned
/// UNCHANGED, so this is a no-op on every normal tick.
///
/// Only the `session` fraction is corrected. `weekly` has its own (weekly) window and is left
/// verbatim, and the caller keeps the raw reading in its own carried state — the correction is
/// applied to the swap DECISION, never written back over what the API actually reported.
pub(crate) fn plausible_session(mark: Option<SessionHighWater>, usage: &Usage) -> Usage {
    match mark.and_then(|m| m.applies_to(usage)) {
        Some(high_water) if high_water > usage.session => Usage {
            session: high_water,
            ..*usage
        },
        _ => *usage,
    }
}

/// The plausibility-corrected session fraction of a retained PRE-BLIND ANCHOR (issue #619) — the
/// blind-gate peer of [`plausible_session`]. Returns `anchor_session` raised to `mark`'s high-water
/// fraction when it sits below it (the anchor's own pre-blind reading came back stale/cache-lagged),
/// else `anchor_session` unchanged (a plausible anchor, a `None` mark → the pre-#619 value stands).
///
/// The #450 anchor is the last reading before the active account went blind. If THAT reading was
/// stale-low, keying the #452 bounded-blindness gate off it under-reads the account and declines an
/// otherwise-due preemptive swap — the exact stale-low failure #614 fixed for the LIVE swap arms,
/// reached one path over through the blind gate. The correction is computed for the gate DECISION
/// only and NEVER stored: `last_good` and every surface that reads it as a MEASUREMENT (the
/// `blind_preempt` swap line's `session_pct`, `status`'s last-known %, `Event::BlindWindow`) keep
/// the RAW value — the same read-time-only contract [`plausible_session`] draws for the
/// reactive/projective arms, so no synthesized value lands in a raw-measurement surface.
///
/// One anchor consumer is a DECISION read yet deliberately left RAW: the #584 velocity projection
/// ([`crate::daemon`]'s `blind_velocity_projected_armed`), which bases `status`'s third degradation
/// arm on the anchor. It is out of #619's scope (the #452 gate only), NOT a measurement. Residual:
/// an anchor whose CORRECTED value still sits below the risk band (so the #452 arm stays disarmed)
/// projects that arm off the stale-low RAW base, so `status` can under-report degradation there.
/// Bounded to the `status` projection; no swap path reads that predicate.
///
/// Unlike [`plausible_session`] this takes NO window stamp and does NO window match — sound for the
/// ANCHOR case SPECIFICALLY: the caller consults the ACTIVE account's own `session_high_water`, which
/// is folded ONLY on that account's successful polls, in the same poll-fold iteration that refreshes
/// the anchor. A blind episode has NO successful active polls, so through it the mark and the anchor
/// are BOTH frozen at the last live poll and name the SAME session window by construction. The
/// cross-window case [`plausible_session`]'s stamp match exists to reject — a FRESH low reading in a
/// NEW window whose mark still holds the OLD window's floor — is therefore unreachable here (`fold`'s
/// own max keeps the mark from being dragged below the true high-water by the stale-low reading it
/// corrects). Do NOT call this on a fresh reading; use [`plausible_session`], which window-matches.
pub(crate) fn plausible_anchor_session(mark: Option<SessionHighWater>, anchor_session: f64) -> f64 {
    match mark {
        Some(m) if m.session > anchor_session => m.session,
        _ => anchor_session,
    }
}

/// The result of a completed [`swap`]. The token reroute (the swap proper)
/// succeeded; these two fields report the best-effort, display-only follow-ups so
/// the caller (#7) can log or act without re-reading the keychain itself.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SwapReport {
    /// Whether the post-swap re-read of the canonical item still matched the token
    /// written for the incoming account. `false` means a third writer (a
    /// concurrent `/login` or a token refresh) changed it between the write and
    /// the re-read; the keychain is authoritative, so the daemon reconciles on the
    /// next cycle. The token reroute itself already succeeded — this is a
    /// display / diagnostic signal, not a swap failure.
    pub(crate) canonical_confirmed: bool,
    /// Whether the `~/.claude.json` `oauthAccount` co-write succeeded. `false` is
    /// tolerated (best-effort display correctness): a missed co-write self-heals
    /// on the next reconcile, since the keychain blob is the authoritative bearer.
    pub(crate) oauth_cowritten: bool,
}

/// Run one out-of-band swap, rotating the active credential from the outgoing
/// account to the incoming account. Both are addressed by their `Sessiometer/<account_uuid>`
/// stash-service name; the engine is account-identity-agnostic (the daemon, #7,
/// maps roster accounts to stash names and picks the pair).
///
/// See the module docs for the five-step sequence and its invariants. Steps 1–3
/// (read outgoing, re-stash outgoing, write incoming) must succeed — a failure
/// there aborts the swap before or at the atomic canonical write, leaving the
/// outgoing account safely re-stashed and the canonical item un-torn. Steps 4–5
/// (co-write, confirm) are best-effort and reported in the returned [`SwapReport`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn swap<C, S>(
    store: &C,
    stash: &S,
    outgoing_stash: &str,
    incoming_stash: &str,
    claude_json: &Path,
) -> Result<SwapReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Read every input up front, BEFORE any mutation — so a failure to read any of
    // them (a locked keychain, an absent / corrupt stash) aborts the swap as a true
    // no-op, touching neither stash nor the canonical item.
    //
    // 1. The outgoing account's CURRENT (silently-refreshed) canonical blob.
    let outgoing_current = store.read().await?;
    // The outgoing account's existing stash — for its display-only `oauthAccount`
    // half, which is stable and PRESERVED across the re-stash (only the token
    // drifts; a full `StashedAccount` is required to write, so the half is sourced
    // here rather than fabricated).
    let outgoing_prev = stash.read(outgoing_stash).await?;
    // The incoming account's stash — its token (→ canonical) and `oauthAccount`
    // (→ `~/.claude.json`). Read here, before the re-stash write below, so an
    // absent / corrupt incoming stash aborts before the outgoing stash is rewritten.
    let incoming = stash.read(incoming_stash).await?;

    // Identity-consistency guard (issue #211): the step-2 re-stash below writes the
    // LIVE canonical token under the OUTGOING account's stash key + its PRESERVED
    // identity. Refuse — BEFORE any write (ZERO writes) — when the engine has POSITIVE
    // evidence the caller mis-resolved the outgoing account: the live canonical does
    // NOT match the outgoing account's own stashed token, yet DOES match the incoming
    // account's. Re-stashing then would staple the incoming account's credential onto
    // the outgoing stash — silent corruption (a stale `~/.claude.json` naming an
    // account that is no longer active: the failure mode #207 fixes at the caller;
    // this is the engine-level safety net UNDERNEATH it, also covering `--force` and
    // adopt-target callers). Mirrors the daemon's token-first ownership check +
    // "never staple a different account's identity" (`restash_account`, `src/daemon.rs`).
    //
    // Both halves are load-bearing:
    //   - The canonical MATCHING the outgoing stash is the safe no-drift case (and the
    //     self-swap / shared-token case) — never refuse it.
    //   - The canonical matching NEITHER stash is the legitimate in-place token-refresh
    //     DRIFT the re-stash exists to capture (the outgoing account's OWN freshly-
    //     refreshed token, stashed nowhere yet) — allow it, so a normal swap is
    //     unaffected. Only a canonical that is DEMONSTRABLY the incoming account's token
    //     (matches its stash) while NOT the outgoing's is a wrong-identity staple.
    if !outgoing_current.matches(&outgoing_prev.credential)
        && outgoing_current.matches(&incoming.credential)
    {
        return Err(Error::SwapWrongIdentityRestash);
    }

    // 2. Re-stash the outgoing account BEFORE overwriting the canonical item — the
    //    token-refresh-rotation drift guard: re-stash its FRESH canonical token
    //    with its PRESERVED `oauthAccount` half (never fabricated).
    stash
        .write(
            outgoing_stash,
            &StashedAccount {
                credential: outgoing_current,
                oauth_account: outgoing_prev.oauth_account,
            },
        )
        .await?;

    // 3. Write the incoming account's token to the canonical item (atomic `-U`).
    store.write(&incoming.credential).await?;

    // 4. Co-write the incoming account's `oauthAccount` into `~/.claude.json`
    //    (best-effort display correctness — a failure is tolerated and self-heals).
    let oauth_cowritten =
        claude_state::write_oauth_account(claude_json, &incoming.oauth_account).is_ok();

    // 5. Post-swap re-read to confirm the canonical item still holds the token we
    //    wrote (re-read each cycle, never cache). A read failure or a third-writer
    //    change leaves it unconfirmed; the token reroute already succeeded.
    let canonical_confirmed = store
        .read()
        .await
        .is_ok_and(|current| current.matches(&incoming.credential));

    Ok(SwapReport {
        canonical_confirmed,
        oauth_cowritten,
    })
}

/// Run one out-of-band **adopt-target** recovery (issue #212): install `incoming`
/// as the active account by writing ONLY its credential to the canonical item and
/// co-writing its `oauthAccount` into `~/.claude.json` — the swap sequence's steps
/// 3–5, SKIPPING the outgoing read + re-stash (steps 1–2).
///
/// This is the recovery path for when the canonical credential is **gone or rotated**
/// — a forced Claude logout that scrubbed / rotated the keychain token (issue #209) —
/// so the normal [`swap`] cannot run: its step 1 reads the outgoing canonical and step
/// 2 re-stashes it, but there is no sound outgoing token to re-stash. Adopt-target needs
/// NEITHER: the departing (dead / absent) token is not required, and because it
/// re-stashes nothing, no credential can be stapled under a wrong identity — the #211
/// failure mode is structurally impossible here (nothing is written to any stash).
///
/// SAFETY is preserved, matching [`swap`]'s discipline:
///   - **read-everything-before-mutate** — the incoming account's stash (its token
///     → canonical, its `oauthAccount` → `~/.claude.json`) is read FIRST; an absent /
///     corrupt incoming stash aborts with ZERO writes;
///   - **could not read ≠ gone** — the canonical item is PROBED before any write, and
///     the write proceeds ONLY on positive evidence it is safe: a CONFIRMED-absent
///     canonical ([`Error::CredentialNotFound`], the scrubbed token) or a PRESENT-but-
///     readable one (`Ok`, a rotated / orphan token the caller vetted as un-attributable)
///     — the two states adopt-target recovers from. EVERY other read outcome aborts with
///     ZERO writes: a LOCKED keychain (transient — "locked ≠ gone", retry when unlocked),
///     but equally an ACL / auth-deny or other [`Error::Keychain`], ambiguity, or I/O
///     error, because a canonical we merely *could not read* is not proven *gone* —
///     clobbering it would lose a present token without re-stashing it. This mirrors the
///     normal [`swap`]'s step-1 read, which aborts on any error (`?`) identically.
///
/// Gated by `--force` at the sole caller ([`crate::use_account`]); the autonomous daemon
/// never adopts (it only rotates between known, re-stashed accounts). Steps 4–5 are
/// best-effort and reported in the returned [`SwapReport`], exactly as in [`swap`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn adopt_target<C, S>(
    store: &C,
    stash: &S,
    incoming_stash: &str,
    claude_json: &Path,
) -> Result<SwapReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Read the essential input FIRST — the incoming account's stash. An absent /
    // corrupt stash aborts before any mutation (ZERO writes), exactly as [`swap`]
    // reads all its inputs up front.
    let incoming = stash.read(incoming_stash).await?;

    // Probe the canonical before any write. Proceed ONLY on positive evidence it is safe
    // to overwrite: a CONFIRMED-absent canonical (the scrubbed token) or a PRESENT-but-
    // readable one (a rotated / orphan token the caller vetted as un-attributable) — the
    // two states adopt-target recovers from. EVERY other read outcome aborts with ZERO
    // writes, because a canonical we merely *could not read* is NOT proven *gone*:
    // clobbering it would lose a present token without re-stashing it (the #211 loss the
    // normal [`swap`] avoids by aborting its step-1 read on any error).
    match store.read().await {
        // Present + readable: the caller routes here only when this token matches no
        // known account, so overwriting the orphan is the intended rotation recovery.
        Ok(_) => {}
        // Confirmed absent (errSecItemNotFound): nothing to clobber — the recovery case.
        Err(Error::CredentialNotFound) => {}
        // LOCKED (transient — "locked ≠ gone"), or an ACL / auth-deny or other
        // `security` error, ambiguity, or I/O: "could not read" is not "gone". Abort.
        Err(err) => return Err(err),
    }

    // 3. Write the incoming account's token to the canonical item (atomic `-U`: a
    //    reader sees old-or-new, never empty / torn).
    store.write(&incoming.credential).await?;

    // 4. Co-write the incoming account's `oauthAccount` into `~/.claude.json`
    //    (best-effort display correctness — a failure is tolerated and self-heals).
    let oauth_cowritten =
        claude_state::write_oauth_account(claude_json, &incoming.oauth_account).is_ok();

    // 5. Post-write re-read to confirm the canonical still holds the token we wrote
    //    (re-read each cycle, never cache). A read failure or a third-writer change
    //    leaves it unconfirmed; the token reroute already succeeded.
    let canonical_confirmed = store
        .read()
        .await
        .is_ok_and(|current| current.matches(&incoming.credential));

    Ok(SwapReport {
        canonical_confirmed,
        oauth_cowritten,
    })
}

/// How long a contended swap-lock acquire ([`SwapLock::acquire`]) waits before
/// failing closed (issue #64). Comfortably exceeds one swap's keychain work (a
/// handful of `security` subprocesses, sub-second to ~2 s), so the ordinary
/// contention — the OTHER writer simply mid-swap — resolves with margin; only a
/// genuinely wedged holder reaches the ceiling, where failing closed (ZERO writes)
/// beats blocking forever.
pub(crate) const SWAP_LOCK_MAX_WAIT: Duration = Duration::from_secs(10);

/// Poll interval while waiting on a contended swap lock (issue #64). Short enough
/// that the wait ends within ~one interval of the holder releasing, small enough
/// that the few polls during a typical sub-second swap are negligible.
const SWAP_LOCK_RETRY: Duration = Duration::from_millis(50);

/// A held single-WRITER swap lock: a kernel advisory `flock(LOCK_EX)` on the
/// native-local `swap.lock`, held only for the DURATION of one swap (issue #64).
/// The file is held open for the critical section; the kernel releases the lock on
/// drop (or process death), so there is no stale-lock reaping.
///
/// DISTINCT from the daemon's single-INSTANCE lock ([`crate::daemon::InstanceLock`]),
/// which is held NON-blocking for the whole process lifetime to reject a second
/// `run`. This lock is BLOCKING (bounded) and per-swap: both the manual `use` swap
/// and the daemon's own swap routine acquire it, collapsing their two-step swaps
/// (canonical keychain write → `~/.claude.json` co-write) into mutually-exclusive
/// critical sections so the two writers can never interleave into a split state
/// (canonical = one account while `~/.claude.json` = another).
#[derive(Debug)]
pub(crate) struct SwapLock {
    // Held open purely to keep the lock; dropping it (or the process dying)
    // releases it.
    _file: std::fs::File,
}

impl SwapLock {
    /// Acquire the swap lock at `path` (creating the file `0600` if needed),
    /// bounded-blocking up to `max_wait`.
    ///
    /// FAIL-CLOSED: if the lock cannot be taken within `max_wait` — another swap
    /// held it the whole time — returns [`Error::SwapLockBusy`] so the caller
    /// aborts with ZERO writes, rather than writing without it and reopening the
    /// torn-write race. Polls `flock(LOCK_EX|LOCK_NB)` and yields the runtime
    /// between tries (an async sleep, never a busy-spin or a blocked OS thread), so
    /// the current-thread runtime keeps cooperating while it waits — the daemon
    /// stays responsive, and `use` stays interruptible.
    pub(crate) async fn acquire(path: &Path, max_wait: Duration) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let deadline = Instant::now() + max_wait;
        loop {
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
            // EWOULDBLOCK (== EAGAIN) means another swap holds the lock; anything
            // else is a genuine I/O failure (a broken fd / filesystem), surfaced
            // as itself rather than masqueraded as contention.
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(Error::Io(err));
            }
            // Out of patience: fail closed (the caller aborts with ZERO writes).
            if Instant::now() >= deadline {
                return Err(Error::SwapLockBusy);
            }
            tokio::time::sleep(SWAP_LOCK_RETRY).await;
        }
    }
}

/// Run one out-of-band [`swap`], wrapped in the single-writer swap lock (issue
/// #64) when `lock` is `Some((path, max_wait))`.
///
/// The lock is acquired BEFORE the swap reads any input and held across the WHOLE
/// two-step sequence, so the manual `use` writer and the daemon's swap routine —
/// the two real swap writers — are serialized on one keychain item and can never
/// interleave into a split canonical/`~/.claude.json` pair. Whoever waits proceeds
/// on FRESH state once the holder releases. A `lock` of `None` runs the swap
/// unlocked: the hermetic single-process test path, where there is no second
/// writer to serialize against.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn swap_locked<C, S>(
    lock: Option<(&Path, Duration)>,
    store: &C,
    stash: &S,
    outgoing_stash: &str,
    incoming_stash: &str,
    claude_json: &Path,
) -> Result<SwapReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Bind the guard here so it outlives the entire swap and drops only on return.
    // A contended acquire fails closed (`Err`) BEFORE any swap input is read, so a
    // refusal is a true no-op (ZERO writes), exactly like the swap engine's own
    // read-everything-before-mutating discipline.
    let _guard = match lock {
        Some((path, max_wait)) => Some(SwapLock::acquire(path, max_wait).await?),
        None => None,
    };
    swap(store, stash, outgoing_stash, incoming_stash, claude_json).await
}

/// Run one out-of-band [`adopt_target`] recovery (issue #212), wrapped in the
/// single-writer swap lock (issue #64) when `lock` is `Some((path, max_wait))` —
/// the locked counterpart of [`swap_locked`], for the adopt path.
///
/// The lock is acquired BEFORE the adopt reads any input and held across the whole
/// write, so the manual `use` recovery cannot interleave with a concurrent daemon
/// swap into a split canonical / `~/.claude.json` pair. A contended acquire fails
/// closed (`Err`) BEFORE any input is read, so a refusal is a true no-op (ZERO
/// writes), matching [`swap_locked`]. A `lock` of `None` runs unlocked — the
/// hermetic single-process test path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn adopt_target_locked<C, S>(
    lock: Option<(&Path, Duration)>,
    store: &C,
    stash: &S,
    incoming_stash: &str,
    claude_json: &Path,
) -> Result<SwapReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Bind the guard here so it outlives the entire adopt and drops only on return.
    // A contended acquire fails closed (`Err`) BEFORE any input is read, so a refusal
    // is a true no-op (ZERO writes), exactly like [`swap_locked`].
    let _guard = match lock {
        Some((path, max_wait)) => Some(SwapLock::acquire(path, max_wait).await?),
        None => None,
    };
    adopt_target(store, stash, incoming_stash, claude_json).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::rc::Rc;

    use crate::claude_state::OauthAccount;
    use crate::error::Error;
    use crate::keychain::{Credential, FakeCredentialStore};
    use crate::stash::FakeAccountStash;

    #[test]
    fn holds_when_both_dimensions_are_below_their_thresholds() {
        let usage = Usage {
            session: 0.5,
            weekly: 0.5,
            weekly_resets_at: None,
            session_resets_at: None,
        };
        // Session below 0.95 AND weekly below 0.98 → hold.
        assert_eq!(decide(&usage, 0.95, 0.98), SwapDecision::Hold);
    }

    #[test]
    fn swaps_when_session_reaches_its_threshold() {
        // AC #1 (regression preserved): session at its threshold → swap, even
        // with weekly far below its separate (higher) threshold.
        let usage = Usage {
            session: 0.95,
            weekly: 0.1,
            weekly_resets_at: None,
            session_resets_at: None,
        };
        assert_eq!(decide(&usage, 0.95, 0.98), SwapDecision::Swap);
    }

    #[test]
    fn swaps_when_weekly_reaches_its_threshold_while_session_is_below() {
        // AC #2: weekly at its threshold while session sits below its own → swap.
        let usage = Usage {
            session: 0.50,
            weekly: 0.98,
            weekly_resets_at: None,
            session_resets_at: None,
        };
        assert_eq!(decide(&usage, 0.95, 0.98), SwapDecision::Swap);
    }

    #[test]
    fn the_two_thresholds_gate_their_dimensions_independently() {
        // AC #3: each dimension is gated by its OWN threshold. A weekly reading of
        // 0.96 — between the two thresholds — does NOT trigger while the weekly
        // threshold is the higher 0.98, but the SAME reading DOES trigger once the
        // weekly threshold is lowered to 0.95. Session is held below its threshold
        // throughout, isolating the weekly axis.
        let usage = Usage {
            session: 0.50,
            weekly: 0.96,
            weekly_resets_at: None,
            session_resets_at: None,
        };
        assert_eq!(decide(&usage, 0.95, 0.98), SwapDecision::Hold);
        assert_eq!(decide(&usage, 0.95, 0.95), SwapDecision::Swap);
    }

    // --- #597 ceiling derivation (effective ceiling + reactive threshold) ---

    #[test]
    fn effective_ceiling_subtracts_the_tail_margin() {
        // The default ceiling 0.99 yields an effective ceiling of 0.93 — the 6 pp tail
        // margin below the settled line, so a landing here plus the ≤5 pp committed tail
        // stays under 0.99 (the strict P100 < 99 landing SLO).
        assert!((effective_ceiling(0.99) - 0.93).abs() < 1e-9);
        assert!((effective_ceiling(0.95) - 0.89).abs() < 1e-9);
        // Clamped at 0 for a pathologically low ceiling — never negative.
        assert_eq!(effective_ceiling(0.04), 0.0);
        assert_eq!(effective_ceiling(0.0), 0.0);
    }

    // --- #607 weekly ceiling derivation (the weekly analogue of #597) ---

    #[test]
    fn weekly_effective_ceiling_subtracts_the_weekly_tail_margin() {
        // The default weekly ceiling 0.98 yields an effective ceiling of 0.97 — the 1 pp weekly
        // tail margin below the settled weekly line, so a landing here plus the worst-case weekly
        // committed tail (`5 pp / k`, ≤ 1 pp under the `k ≥ 5` assumption) stays under 0.98.
        assert!((weekly_effective_ceiling(0.98) - 0.97).abs() < 1e-9);
        assert!((weekly_effective_ceiling(0.95) - 0.94).abs() < 1e-9);
        // Clamped at 0 for a pathologically low ceiling — never negative.
        assert_eq!(weekly_effective_ceiling(0.005), 0.0);
        assert_eq!(weekly_effective_ceiling(0.0), 0.0);
    }

    #[test]
    fn the_weekly_fire_point_is_strictly_early_across_the_operator_range() {
        // The per-dimension strict-early-fire invariant (issue #607): for every weekly ceiling the
        // operator can set (`50..=99`, config-clamped), the weekly fire point sits STRICTLY below
        // the ceiling — so a weekly swap always lands with tail headroom, never AT the line. This
        // is the weekly half of what `effective_ceiling` guarantees for session; the two are
        // asserted independently because the dimensions are independent (issue #41).
        //
        // Scope note: this pins the STRUCTURAL property (fire < ceiling, and by exactly one margin).
        // It deliberately does NOT re-assert a worst-case-tail bound — the tail magnitude rests on
        // the `k >= 5` assumption documented on `WEEKLY_TAIL_MARGIN`, and restating that assumption
        // as a loop-invariant comparison of two constants would dress one unverified premise up as
        // independent corroboration. Only a weekly landing SLI can settle the magnitude.
        for ceiling_pct in 50..=99u8 {
            let ceiling = f64::from(ceiling_pct) / 100.0;
            let fire = weekly_effective_ceiling(ceiling);
            assert!(
                fire < ceiling,
                "weekly ceiling {ceiling_pct}: fire point {fire} must be strictly below the ceiling",
            );
            assert!(
                (ceiling - fire - WEEKLY_TAIL_MARGIN).abs() < 1e-9,
                "weekly ceiling {ceiling_pct}: fire point must sit exactly one margin below",
            );
        }
    }

    #[test]
    fn reactive_threshold_collapses_to_the_effective_ceiling_at_zero_velocity() {
        // v == 0 (an unwarmed / reset EMA) → the threshold is the bare effective ceiling,
        // so an idle account fires exactly AT it, never early (issue #597's "don't fire
        // idle accounts early"). Both estimators gate velocity identically, so a low-signal
        // account behaves the same in either arm.
        let eff = effective_ceiling(0.99);
        assert_eq!(reactive_session_threshold(eff, 0.0, 120.0), eff);
        assert_eq!(reactive_session_threshold(eff, 0.0, 313.0), eff);
    }

    #[test]
    fn reactive_threshold_fires_one_reobservation_gap_early() {
        // Issue #609: the reactive arm fires one re-observation gap early — at `eff - v*poll_gap` —
        // so the account is AT the effective ceiling by the time the swap executes one gap later,
        // having climbed velocity*poll_gap. It is INDEPENDENT of the projection horizon (it takes
        // no H): the composition with the projection arm lives in the daemon, not this function.
        let eff = effective_ceiling(0.99); // 0.93
        let v = 0.001; // fraction/sec

        // At the measured p90 gap (313 s) the reactive arm looks ahead the full gap, not the 120 s
        // horizon the pre-#609 min-clamp would have pinned it to.
        let thr = reactive_session_threshold(eff, v, REACTIVE_REOBSERVATION_GAP_SECS);
        assert!(
            (thr - (eff - v * 313.0)).abs() < 1e-12,
            "the reactive arm fires one re-observation gap early"
        );
        // Fast-poll disabled → poll_gap 0 → the bare effective ceiling (projection is then sole arm).
        assert_eq!(reactive_session_threshold(eff, v, 0.0), eff);
    }

    #[test]
    fn the_composed_swap_covers_the_larger_unseen_window_across_the_grid() {
        // Issue #609 INVARIANT (superseding #597's strict-early-fire framing): the reactive arm
        // covers the re-observation gap and the projection arm (`eff - v*H`) covers the velocity
        // horizon; the daemon composes them (reactive checked first, projection on Hold), so the
        // swap fires at the EARLIER of the two — `eff - v*max(poll_gap, H)` — covering the LARGER
        // unseen window. Exhaustive grid over ceiling, velocity, poll_gap and horizon, INCLUDING
        // poll_gap > H (the case #597's min-clamp silently discarded) and the measured poll-gap
        // percentiles (p50 112, p90 313, max 972) and peak velocity (~6.95 %/min ≈ 0.00116 frac/s).
        for &ceiling in &[0.50, 0.80, 0.95, 0.99] {
            let eff = effective_ceiling(ceiling);
            for &v in &[0.0, 0.0002, 0.001, 0.00116, 0.005] {
                for &poll_gap in &[0.0, 30.0, 60.0, 112.0, 120.0, 313.0, 972.0] {
                    let reactive = reactive_session_threshold(eff, v, poll_gap);
                    // The reactive arm is INDEPENDENT of the horizon — it is exactly `eff - v*gap`.
                    assert!((reactive - (eff - v * poll_gap)).abs() < 1e-12);
                    for &h in &[0.0, 60.0, 120.0, 150.0, 300.0] {
                        let projection = eff - v * h; // the daemon's velocity_swap fire point
                        let composed = reactive.min(projection); // reactive-first, projection-on-Hold
                        let expected = eff - v * poll_gap.max(h);
                        assert!(
                            (composed - expected).abs() < 1e-12,
                            "max-window coverage violated: composed {composed} != eff - v*max = {expected} \
                             (ceiling={ceiling}, v={v}, poll_gap={poll_gap}, h={h})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_old_min_clamp_would_discard_a_gap_wider_than_the_horizon() {
        // FALSIFIER / regression-lock (issue #609). The pre-#609 form clamped the reactive lookahead
        // to `min(poll_gap, H)`, so a re-observation gap WIDER than the horizon was silently
        // discarded — the #609 bug (the real gap runs to p90 313 s, but the clamp pinned the
        // lookahead at H=120). The new reactive arm fires STRICTLY EARLIER than that discarded-gap
        // clamp when poll_gap > H, and the composed swap covers the true `max(gap, H)`.
        let eff = effective_ceiling(0.95); // 0.89, the default ceiling
        let v = 0.001;
        let poll_gap = REACTIVE_REOBSERVATION_GAP_SECS; // 313
        let h = 120.0; // the default horizon; poll_gap > h
        let new_reactive = reactive_session_threshold(eff, v, poll_gap); // eff - v*313
        let old_clamped = eff - v * poll_gap.min(h); // the pre-#609 min-clamp form: eff - v*120
        assert!(
            new_reactive < old_clamped - 1e-12,
            "the widened lookahead must fire earlier than the discarded-gap clamp: {new_reactive} !< {old_clamped}"
        );
        // The composed swap covers the LARGER window (313 s), not the horizon (120 s).
        let projection = eff - v * h;
        let composed = new_reactive.min(projection);
        assert!((composed - (eff - v * poll_gap.max(h))).abs() < 1e-12);
        assert!((composed - (eff - v * 313.0)).abs() < 1e-12);
    }

    #[test]
    fn widening_the_reobservation_gap_never_raises_the_fire_point() {
        // Issue #609 SAFETY property: the reactive fire threshold is monotone NON-INCREASING in the
        // re-observation gap, so widening the lookahead can only LOWER the fire point — hence lower
        // the landing point — hence the `P100 < 99` landing SLO stays reachable BY CONSTRUCTION
        // (widening never worsens overshoot; its only cost is earlier swaps = the cheap error under
        // #597's asymmetry). This is why "verify against the #595 landing SLI" holds analytically.
        for &ceiling in &[0.50, 0.95, 0.99] {
            let eff = effective_ceiling(ceiling);
            for &v in &[0.0, 0.001, 0.00116] {
                let gaps = [0.0, 60.0, 112.0, 120.0, 313.0, 972.0];
                for pair in gaps.windows(2) {
                    let (narrower, wider) = (pair[0], pair[1]);
                    let thr_narrow = reactive_session_threshold(eff, v, narrower);
                    let thr_wide = reactive_session_threshold(eff, v, wider);
                    assert!(
                        thr_wide <= thr_narrow + 1e-12,
                        "widening the gap raised the fire point: gap {wider} → {thr_wide} > gap {narrower} → {thr_narrow}"
                    );
                }
            }
        }
    }

    #[test]
    fn reactive_poll_gap_is_zero_when_the_near_limit_fast_poll_is_disabled() {
        // #584 / #609: `near_limit_poll_secs == 0` disables the near-limit fast poll, so there is no
        // tight re-observation gap to look ahead over — the reactive poll_gap is 0 (the threshold
        // then collapses to the bare effective ceiling and the #539 projection is the sole
        // velocity-aware arm). The `max`-floor MUST NOT fire here (`max(2·0, 313) = 313` would break
        // #584); the explicit zero guard keeps the projection-only path intact.
        assert_eq!(reactive_poll_gap_secs(0), 0.0);
    }

    #[test]
    fn reactive_poll_gap_floors_at_the_measured_reobservation_gap() {
        // #609: at the default (and any tight) cadence, `2 × near_limit_poll_secs` UNDER-models the
        // real gap (the measured p90 is 313 s, dominated by scheduling / back-off / interleave
        // overhead), so the measured floor governs. Default cadence 60 → 2·60 = 120 < 313 → 313. The
        // floor still governs right up to the crossover (156 s: 2·156 = 312 < 313).
        assert_eq!(reactive_poll_gap_secs(60), REACTIVE_REOBSERVATION_GAP_SECS);
        assert_eq!(reactive_poll_gap_secs(30), REACTIVE_REOBSERVATION_GAP_SECS);
        assert_eq!(reactive_poll_gap_secs(156), REACTIVE_REOBSERVATION_GAP_SECS);
    }

    #[test]
    fn reactive_poll_gap_scales_past_the_floor_for_a_slower_configured_poll() {
        // #609: a SLOWER near-limit poll widens the real re-observation gap proportionally, so above
        // the crossover the cadence-scaled `2 × near_limit_poll_secs` governs — the lookahead tracks
        // the operator's actual blindness rather than pinning to the default-cadence measurement.
        assert_eq!(reactive_poll_gap_secs(157), 314.0); // 2·157 = 314 > 313
        assert_eq!(reactive_poll_gap_secs(200), 400.0);
        assert_eq!(reactive_poll_gap_secs(600), 1200.0);
    }

    #[test]
    fn reactive_poll_gap_never_narrows_the_pre_609_lookahead() {
        // #609 SAFETY regression-lock (the daemon-level counterpart of
        // `widening_the_reobservation_gap_never_raises_the_fire_point`, which locks the property for
        // the ISOLATED threshold in its poll_gap argument — this locks the FORMULA substitution that
        // feeds that argument). The pre-#609 reactive lookahead was `min(2 × near_limit_poll_secs, H)`;
        // #609 substitutes the poll_gap formula, so the substitution must never SHRINK the lookahead —
        // a shrink would RAISE the fire point and could push a landing over the SLO. That is exactly
        // the corner a BARE `poll_gap = 313` leaves open (`2 × near_limit_poll_secs > 313 ∧ H > 313`);
        // `max(2c, floor) ≥ 2c ≥ min(2c, H)` closes it UNCONDITIONALLY. Grid the reachable ranges
        // INCLUDING that corner (near_limit > 156 with H > 313).
        for &nl in &[0_u64, 5, 30, 60, 156, 157, 200, 600, 1800, 3600] {
            let new_gap = reactive_poll_gap_secs(nl);
            for &h in &[0.0, 60.0, 120.0, 150.0, 313.0, 400.0, 600.0] {
                let old_lookahead = (2.0 * nl as f64).min(h); // pre-#609 min(2c, H)
                assert!(
                    new_gap >= old_lookahead - 1e-12,
                    "the #609 poll_gap narrowed the pre-#609 lookahead: near_limit={nl} H={h} → \
                     new {new_gap} < old {old_lookahead}"
                );
            }
        }
    }

    #[test]
    fn the_reactive_threshold_is_unbounded_below_by_design_at_an_extreme_lookahead() {
        // Issues #597/#609: the reactive threshold is intentionally NOT floored (unlike
        // `effective_ceiling`). A large `velocity × poll_gap` — sustained peak velocity over a wide
        // re-observation gap — pulls it far below the effective ceiling and past 0. That is the
        // intended early-protective fire (climbing too fast for the re-observation gap → swap now to
        // land at the effective ceiling), the CHEAP error under #597's asymmetry. A below-0 value is
        // behaviourally identical to 0 in `decide` (`session >= threshold`, session never negative),
        // so it is left unclamped. Documented here so the unbounded-below is read as design, not an
        // oversight (the config-load bound on the absurd combos is now `peak_runway_reserve_bound`,
        // issue #608 — which rejects only the UNSATISFIABLE stack, leaving this runtime reach intact).
        let eff = effective_ceiling(0.50); // 0.44 — a low ceiling maximises the below-0 reach
        let v = 0.00116; // ~6.95 %/min, the observed peak
        let thr = reactive_session_threshold(eff, v, 7200.0);
        assert!(thr < 0.0, "the threshold is unbounded below: {thr}");
        // And at the default ceiling with peak velocity over the p90 gap it fires well below the
        // projection arm's ~0.85 min-project-above floor — the deliberate arm divergence, DEEPENED by
        // the #609 gap widening (0.89 - 0.00116*313 ≈ 0.53).
        let thr_default_ceiling =
            reactive_session_threshold(effective_ceiling(0.95), v, REACTIVE_REOBSERVATION_GAP_SECS);
        assert!(
            thr_default_ceiling < 0.85,
            "peak-velocity reactive fire ({thr_default_ceiling}) is below the projection floor, by design"
        );
    }

    // --- #608 peak-velocity runway coupling bound ---

    #[test]
    fn v_peak_fraction_per_second_is_the_percent_per_minute_constant_converted() {
        // The two forms name ONE calibrated number: the SLI compares observed %/min against the
        // former, the bound multiplies seconds by the latter. Assert the conversion so they cannot
        // drift — 6.95 %/min ÷ 100 ÷ 60 ≈ 0.0011583 frac/s.
        assert!(
            (V_PEAK_SESSION_FRAC_PER_SEC - 6.95 / 100.0 / 60.0).abs() < 1e-15,
            "the frac/s form must be the %/min constant converted, got {V_PEAK_SESSION_FRAC_PER_SEC}"
        );
        // And the sanity magnitude the ADR cites (~0.00116 frac/s at the observed peak).
        assert!((V_PEAK_SESSION_FRAC_PER_SEC - 0.00116).abs() < 1e-4);
    }

    #[test]
    fn peak_runway_bound_at_the_defaults_is_positive_but_below_the_default_reserve() {
        // The SHIPPED default (ceiling 0.95 → effective 0.89, near_limit 60 → poll_gap 313, H 120 →
        // window max(313,120)=313): the bound is 0.89 − 0.00116×313 ≈ 0.527. Positive (so the
        // default LOADS — no unsatisfiable rejection), yet below the default reserve 0.80 — exactly
        // the exceeded-but-satisfiable state ADR-0023 recorded as deliberate. This is the anchor
        // fact the whole severity split rests on: erroring here would brick every stock install.
        let bound = peak_runway_reserve_bound(0.95, 60, 120);
        assert!(
            bound > 0.0,
            "the default config must remain loadable: {bound}"
        );
        assert!(
            (bound * 100.0).floor() < 80.0,
            "the default reserve 80 must sit ABOVE the bound (the advisory case): {bound}"
        );
        // Concretely ~52 pp.
        assert_eq!((bound * 100.0).floor() as i64, 52);
    }

    #[test]
    fn peak_runway_bound_uses_the_max_of_poll_gap_and_horizon_not_poll_gap_alone() {
        // Post-#609 the composed fire point is `effective_ceiling − v × max(poll_gap, H)` — the
        // reactive arm decoupled from H (ADR-0024). A bound against poll_gap ALONE would miss the
        // projection arm whenever H > poll_gap, and entirely when fast-poll is off (poll_gap == 0).
        // With fast-poll disabled (near_limit 0 → poll_gap 0) but a wide horizon, the bound MUST
        // still tighten by H, not collapse to the bare effective ceiling.
        let eff = effective_ceiling(0.95); // 0.89
        let wide_h = peak_runway_reserve_bound(0.95, 0, 600);
        assert!(
            (wide_h - (eff - V_PEAK_SESSION_FRAC_PER_SEC * 600.0)).abs() < 1e-12,
            "with poll_gap 0 the bound must still tighten by the 600 s horizon, got {wide_h}"
        );
        // And where poll_gap dominates (near_limit 200 → poll_gap 400 > H 120), the bound uses 400.
        let gap_dominant = peak_runway_reserve_bound(0.95, 200, 120);
        assert!(
            (gap_dominant - (eff - V_PEAK_SESSION_FRAC_PER_SEC * 400.0)).abs() < 1e-12,
            "where poll_gap 400 > H 120 the bound must use 400, got {gap_dominant}"
        );
    }

    #[test]
    fn peak_runway_bound_goes_nonpositive_at_the_absurd_stack() {
        // ADR-0023 § Consequences' absurd corner: near_limit toward its 3600 s max pulls the
        // reactive poll_gap to 7200 s; at peak velocity 0.00116×7200 ≈ 8.3 ≫ any effective ceiling,
        // so the bound is deeply negative — no reserve keeps runway. This is the UNSATISFIABLE case
        // `Config::validate` rejects.
        assert!(
            peak_runway_reserve_bound(0.95, 3600, 120) < 0.0,
            "the absurd near-limit stack must yield a non-positive bound"
        );
        // The threshold where it crosses 0 at the default ceiling: window = 0.89 / 0.0011583 ≈
        // 768.4 s, i.e. near_limit ≈ 384 (2×near_limit = poll_gap once it clears the 313 floor).
        // 384 → window 768 stays barely satisfiable; 385 → window 770 goes unsatisfiable — the
        // boundary is real and granular, not all-or-nothing.
        assert!(
            peak_runway_reserve_bound(0.95, 384, 120) > 0.0,
            "near_limit 384 (window 768) is just satisfiable at the default ceiling"
        );
        assert!(
            peak_runway_reserve_bound(0.95, 385, 120) < 0.0,
            "near_limit 385 (window 770) crosses into unsatisfiable"
        );
    }

    // --- #614 stale-reading plausibility guard (session high-water mark) ---

    /// An epoch-second session-window stamp; `WINDOW_NEXT` is the one the 5 h window rolls to.
    const WINDOW: i64 = 1_800_000_000;
    const WINDOW_NEXT: i64 = WINDOW + 18_000;

    /// A reading at `session` claiming session window `window` (`None` = the API reported no
    /// parseable `resets_at`).
    fn reading(session: f64, window: Option<i64>) -> Usage {
        Usage {
            session,
            weekly: 0.20,
            weekly_resets_at: None,
            session_resets_at: window,
        }
    }

    #[test]
    fn session_high_water_runs_to_the_max_within_one_window() {
        // The mark tracks the HIGHEST session fraction seen in the window, so a stale-low reading
        // folded into it leaves the plausible value standing rather than dragging the baseline down
        // (which would let a second stale reading look plausible relative to the first).
        let mark = SessionHighWater::fold(None, &reading(0.70, Some(WINDOW)));
        let mark = SessionHighWater::fold(mark, &reading(0.88, Some(WINDOW)));
        let mark = SessionHighWater::fold(mark, &reading(0.40, Some(WINDOW))); // the stale-low one
        assert_eq!(
            mark.and_then(|m| m.applies_to(&reading(0.40, Some(WINDOW)))),
            Some(0.88),
            "the mark holds the window's maximum, not its latest reading",
        );
    }

    #[test]
    fn session_high_water_restarts_when_the_session_window_rolls() {
        // Issue #614 AC 2: a NEW window is the legitimate reason usage falls. The mark restarts at the
        // fresh reading, so the previous window's floor is released — it can never pin an account
        // across windows.
        let mark = SessionHighWater::fold(None, &reading(0.92, Some(WINDOW)));
        let rolled = SessionHighWater::fold(mark, &reading(0.05, Some(WINDOW_NEXT)));
        assert_eq!(
            rolled.and_then(|m| m.applies_to(&reading(0.05, Some(WINDOW_NEXT)))),
            Some(0.05),
            "the rolled window starts a fresh mark at the post-reset reading",
        );
        // And the OLD window's mark no longer applies to a reading in the new window.
        assert!(
            !is_stale_low(rolled, &reading(0.05, Some(WINDOW_NEXT))),
            "a post-reset reading is plausible, not stale-low",
        );
    }

    #[test]
    fn session_high_water_needs_a_window_stamp() {
        // Without a `session_resets_at` there is no window identity, so no later drop could be judged
        // implausible — carrying a mark forward would risk holding a floor across an UNSEEN roll.
        assert_eq!(SessionHighWater::fold(None, &reading(0.90, None)), None);
        let stamped = SessionHighWater::fold(None, &reading(0.90, Some(WINDOW)));
        assert_eq!(
            SessionHighWater::fold(stamped, &reading(0.95, None)),
            None,
            "an unstamped reading releases the mark rather than silently retaining it",
        );
    }

    #[test]
    fn a_drop_inside_an_unchanged_window_is_stale_low() {
        // Issue #614 AC 1, the predicate: usage is monotonic within a session window, so a value BELOW
        // the same window's high-water mark cannot be real drain — it is a stale / cache-lagged read.
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        assert!(is_stale_low(mark, &reading(0.40, Some(WINDOW))));
        // At or above the mark is plausible — the guard is strictly a below-the-mark test.
        assert!(!is_stale_low(mark, &reading(0.88, Some(WINDOW))));
        assert!(!is_stale_low(mark, &reading(0.91, Some(WINDOW))));
    }

    #[test]
    fn the_window_key_survives_the_real_per_poll_stamp_jitter() {
        // Issue #614, the defect a byte-equality window key would have shipped: `session_resets_at` is
        // NOT stable within one window. Measured over the daemon's own sample store (17,978 consecutive
        // stamped pairs) the same-window delta is 0 s only 50.1 % of the time and ±1 s the other 49.8 %
        // — the API renders sub-second precision and `epoch_from_rfc3339` truncates it, so a value on a
        // second boundary alternates. This replays a REAL captured sequence (a contiguous window whose
        // session climbed monotonically while the stamp flipped every poll):
        //
        //     ts=1783139244 session=0.03 resets_at=1783156799
        //     ts=1783139495 session=0.06 resets_at=1783156800
        //     ts=1783139751 session=0.06 resets_at=1783156799
        //     ts=1783140052 session=0.07 resets_at=1783156800
        //
        // Under exact equality every other poll restarts the mark AT the newest reading — so a
        // stale-low reading both escapes the guard and destroys the baseline, and the guard fires
        // essentially never in production while hand-pinned unit tests stay green.
        let jittered = [
            (0.03, 1_783_156_799),
            (0.06, 1_783_156_800),
            (0.06, 1_783_156_799),
            (0.07, 1_783_156_800),
        ];
        let mark = jittered.iter().fold(None, |mark, &(session, stamp)| {
            SessionHighWater::fold(mark, &reading(session, Some(stamp)))
        });
        // The mark accumulated the window's true max across the jitter, rather than being restarted.
        assert_eq!(
            mark.and_then(|m| m.applies_to(&reading(0.07, Some(1_783_156_800)))),
            Some(0.07),
            "the ±1 s stamp jitter is one window, so the mark accumulates across it",
        );
        // And a stale-low reading arriving on the OTHER side of the jitter is still caught.
        assert!(
            is_stale_low(mark, &reading(0.01, Some(1_783_156_799))),
            "a stale-low reading is caught even when its stamp jitters against the mark's anchor",
        );
    }

    #[test]
    fn the_window_match_tolerance_separates_jitter_from_a_real_roll() {
        // The tolerance must admit the ±1 s jitter WITHOUT admitting a genuine ~5 h roll. The two
        // populations are ~2 orders of magnitude apart (observed jitter ≤ 1 s; smallest real change
        // 3600 s; the roll cluster 17999–18001 s), so 60 s sits with wide margin on both sides.
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        for delta in [
            -1,
            0,
            1,
            -SESSION_WINDOW_MATCH_SECS,
            SESSION_WINDOW_MATCH_SECS,
        ] {
            assert!(
                is_stale_low(mark, &reading(0.40, Some(WINDOW + delta))),
                "a stamp {delta} s from the anchor is the SAME window",
            );
        }
        for delta in [SESSION_WINDOW_MATCH_SECS + 1, 3600, 17_999, 18_000, 18_001] {
            assert!(
                !is_stale_low(mark, &reading(0.40, Some(WINDOW + delta))),
                "a stamp {delta} s away is a DIFFERENT window — the guard must not fire",
            );
        }
    }

    #[test]
    fn the_window_anchor_does_not_walk_with_the_jitter() {
        // The mark keeps its ORIGINAL anchor stamp rather than re-anchoring on each fold: with a
        // tolerance-based match, re-anchoring would let the reference drift one jitter step at a time
        // and eventually walk out of the true window. Folding a long run of +1-biased stamps must leave
        // the anchor put.
        let mut mark = SessionHighWater::fold(None, &reading(0.10, Some(WINDOW)));
        for step in 1..=200 {
            mark = SessionHighWater::fold(mark, &reading(0.10, Some(WINDOW + (step % 2))));
        }
        assert_eq!(
            mark.map(|m| m.window),
            Some(WINDOW),
            "the anchor stays at the first stamp seen for the window",
        );
    }

    #[test]
    fn a_drop_across_a_rolled_window_is_not_stale_low() {
        // Issue #614 AC 2 (the no-misfire half): the SAME drop that is implausible inside one window is
        // entirely expected once `session_resets_at` has moved on. The guard must not fire, so the
        // velocity EMA still resets and the reading is still trusted at face value.
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        assert!(
            !is_stale_low(mark, &reading(0.40, Some(WINDOW_NEXT))),
            "a drop paired with a CHANGED window stamp is a legitimate reset",
        );
    }

    #[test]
    fn an_unstamped_reading_is_never_judged_stale_low() {
        // The residual trust boundary, stated as a property: the guard fires only on POSITIVE evidence
        // that the window is unchanged. A reading the API did not stamp carries no such evidence, so it
        // is trusted as-is — the pre-#614 behaviour, deliberately retained rather than guessed at.
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        assert!(!is_stale_low(mark, &reading(0.40, None)));
        assert_eq!(plausible_session(mark, &reading(0.40, None)).session, 0.40);
    }

    #[test]
    fn plausible_session_raises_a_stale_low_reading_to_the_mark() {
        // Issue #614 AC 1, the correction the swap arms decide on: the stale-low session fraction is
        // raised to the window's retained high-water mark — a genuine LOWER bound on the truth — while
        // every other field passes through verbatim (only the SESSION dimension is corrected; `weekly`
        // has its own window).
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        let stale = reading(0.40, Some(WINDOW));
        let corrected = plausible_session(mark, &stale);
        assert_eq!(corrected.session, 0.88);
        assert_eq!(corrected.weekly, stale.weekly);
        assert_eq!(corrected.session_resets_at, stale.session_resets_at);
        assert_eq!(corrected.weekly_resets_at, stale.weekly_resets_at);
    }

    #[test]
    fn plausible_session_is_a_no_op_on_a_plausible_reading() {
        // The normal tick: a reading at or above the mark (and one in a rolled window) is returned
        // UNCHANGED, so the guard is inert outside the stale-reading case it exists for.
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        assert_eq!(
            plausible_session(mark, &reading(0.91, Some(WINDOW))),
            reading(0.91, Some(WINDOW))
        );
        assert_eq!(
            plausible_session(mark, &reading(0.05, Some(WINDOW_NEXT))),
            reading(0.05, Some(WINDOW_NEXT))
        );
        assert_eq!(
            plausible_session(None, &reading(0.40, Some(WINDOW))),
            reading(0.40, Some(WINDOW))
        );
    }

    #[test]
    fn a_stale_low_reading_does_not_cancel_an_otherwise_due_swap() {
        // Issue #614 AC 1, end to end through `decide` — the headline behaviour at the pure-decision
        // layer. The account is really at 0.88 (this window's mark) and the reactive threshold is 0.85,
        // so a swap is DUE; the cache-lagged 0.40 response would otherwise read as a Hold.
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        let stale = reading(0.40, Some(WINDOW));
        assert_eq!(
            decide(&stale, 0.85, 0.98),
            SwapDecision::Hold,
            "the raw stale reading cancels the due swap — the bug this guards",
        );
        assert_eq!(
            decide(&plausible_session(mark, &stale), 0.85, 0.98),
            SwapDecision::Swap,
            "decided on the plausible reading, the due swap still fires",
        );
    }

    #[test]
    fn plausible_anchor_session_raises_a_stale_low_anchor_to_the_mark() {
        // Issue #619, the correction the #452 blind gate decides on: the pre-blind anchor's stale-low
        // session is raised to the frozen window high-water mark — a genuine LOWER bound on the truth —
        // so a cache-lagged reading arriving just before the account went blind cannot write a
        // below-band anchor that then cancels the otherwise-due preemptive swap.
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        assert_eq!(plausible_anchor_session(mark, 0.40), 0.88);
    }

    #[test]
    fn plausible_anchor_session_is_a_no_op_on_a_plausible_or_unmarked_anchor() {
        // A plausible anchor (at or above the mark) passes through UNCHANGED — the guard is inert
        // outside the stale-low case — and a `None` mark (an account never polled with a parseable
        // `session_resets_at`, so `fold` never seeded one) leaves the anchor as-is: the pre-#619
        // value stands, never a fabricated raise off absent evidence.
        let mark = SessionHighWater::fold(None, &reading(0.88, Some(WINDOW)));
        assert_eq!(plausible_anchor_session(mark, 0.91), 0.91);
        assert_eq!(plausible_anchor_session(mark, 0.88), 0.88);
        assert_eq!(plausible_anchor_session(None, 0.40), 0.40);
    }

    // --- the swap engine (#6) ---

    const ACCT_A: &str = "Sessiometer/u-A";
    const ACCT_B: &str = "Sessiometer/u-B";

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

    /// A minimal `~/.claude.json` displaying `uuid`, at mode `mode`, plus unrelated
    /// fields the co-write must preserve. Returns the tempdir guard and the path.
    fn claude_json(uuid: &str, mode: u32) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let body = format!(
            r#"{{"numStartups":7,"oauthAccount":{{"accountUuid":"{uuid}","emailAddress":"{uuid}@x.com"}},"projects":{{"/a":1}}}}"#
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        (dir, path)
    }

    /// The `oauthAccount.accountUuid` currently displayed in a `~/.claude.json`.
    fn displayed_uuid(path: &Path) -> String {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        v["oauthAccount"]["accountUuid"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// A `FakeCredentialStore` seeded with `blob` as the active canonical item.
    async fn store_holding(blob: &[u8]) -> FakeCredentialStore {
        let store = FakeCredentialStore::empty();
        store.write(&cred(blob)).await.unwrap();
        store
    }

    /// A `FakeAccountStash` seeded with both accounts' stashes.
    async fn stash_with(a: StashedAccount, b: StashedAccount) -> FakeAccountStash {
        let stash = FakeAccountStash::empty();
        stash.write(ACCT_A, &a).await.unwrap();
        stash.write(ACCT_B, &b).await.unwrap();
        stash
    }

    #[tokio::test]
    async fn reroutes_the_token_and_co_writes_the_identity() {
        let store = store_holding(b"A-token").await;
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        let report = swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        // The canonical item now holds B's token, and the post-swap re-read confirmed it.
        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert!(report.canonical_confirmed);
        // The display identity now shows B.
        assert!(report.oauth_cowritten);
        assert_eq!(displayed_uuid(&json), "u-B");
    }

    #[tokio::test]
    async fn re_stashes_outgoing_with_its_fresh_token_and_preserved_identity() {
        // A's stash holds an OLD token; the canonical holds A's CURRENT (refreshed)
        // token — the drift the re-stash guards against. A's stashed `oauthAccount`
        // is the stable half that must be preserved (NUANCE 1).
        let store = store_holding(b"A-refreshed").await;
        let stash = stash_with(stashed(b"A-stale", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        let a = stash.read(ACCT_A).await.unwrap();
        // A was re-stashed with its FRESH canonical token, not the stale stashed one…
        assert_eq!(a.credential.expose(), b"A-refreshed");
        // …and its display-only `oauthAccount` was PRESERVED, not fabricated/changed.
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
        assert_eq!(a.oauth_account.raw_json(), oauth("u-A").raw_json());
        // The incoming write happened (after the re-stash): canonical is B's token.
        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn re_stashes_the_outgoing_account_before_writing_the_incoming() {
        // Recording seams sharing one ordered log, so the re-stash-before-incoming
        // ordering is observed directly across the two seams.
        type Log = Rc<RefCell<Vec<String>>>;

        struct RecStore {
            log: Log,
            slot: RefCell<Option<Credential>>,
        }
        impl CredentialStore for RecStore {
            async fn read(&self) -> Result<Credential> {
                self.log.borrow_mut().push("read-canonical".into());
                self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
            }
            async fn write(&self, credential: &Credential) -> Result<()> {
                self.log.borrow_mut().push("write-canonical".into());
                *self.slot.borrow_mut() = Some(credential.clone());
                Ok(())
            }
        }

        struct RecStash {
            log: Log,
            items: RefCell<HashMap<String, StashedAccount>>,
        }
        impl AccountStash for RecStash {
            async fn write(&self, service: &str, account: &StashedAccount) -> Result<()> {
                self.log.borrow_mut().push(format!("write-stash:{service}"));
                self.items
                    .borrow_mut()
                    .insert(service.to_owned(), account.clone());
                Ok(())
            }
            async fn read(&self, service: &str) -> Result<StashedAccount> {
                self.log.borrow_mut().push(format!("read-stash:{service}"));
                self.items
                    .borrow()
                    .get(service)
                    .cloned()
                    .ok_or(Error::StashIncomplete {
                        service: service.to_owned(),
                    })
            }
            async fn delete(&self, service: &str) -> Result<()> {
                self.log
                    .borrow_mut()
                    .push(format!("delete-stash:{service}"));
                self.items.borrow_mut().remove(service);
                Ok(())
            }
        }

        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let store = RecStore {
            log: log.clone(),
            slot: RefCell::new(Some(cred(b"A-token"))),
        };
        let stash = RecStash {
            log: log.clone(),
            items: RefCell::new(HashMap::new()),
        };
        stash
            .write(ACCT_A, &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write(ACCT_B, &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (_dir, json) = claude_json("u-A", 0o600);
        log.borrow_mut().clear(); // ignore the seeding writes

        swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        let log = log.borrow();
        let restash = log
            .iter()
            .position(|e| e == "write-stash:Sessiometer/u-A")
            .expect("the outgoing account was re-stashed");
        let write_incoming = log
            .iter()
            .position(|e| e == "write-canonical")
            .expect("the incoming token was written to the canonical item");
        assert!(
            restash < write_incoming,
            "re-stash of the outgoing account must precede the incoming canonical write; log = {log:?}"
        );
    }

    #[tokio::test]
    async fn writes_the_canonical_item_before_co_writing_the_identity() {
        // A store that snapshots the displayed `~/.claude.json` uuid AT THE MOMENT
        // it writes the canonical item — proving canonical-then-oauth ordering: at
        // canonical-write time, the co-write (step 4) has not run, so the file
        // still shows the pre-swap account.
        struct SnapshotStore {
            slot: RefCell<Option<Credential>>,
            claude_json: PathBuf,
            uuid_at_write: RefCell<Option<String>>,
        }
        impl CredentialStore for SnapshotStore {
            async fn read(&self) -> Result<Credential> {
                self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
            }
            async fn write(&self, credential: &Credential) -> Result<()> {
                let snap = std::fs::read(&self.claude_json)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .and_then(|v| v["oauthAccount"]["accountUuid"].as_str().map(str::to_owned));
                *self.uuid_at_write.borrow_mut() = snap;
                *self.slot.borrow_mut() = Some(credential.clone());
                Ok(())
            }
        }

        let (_dir, json) = claude_json("u-A", 0o600);
        let store = SnapshotStore {
            slot: RefCell::new(Some(cred(b"A-token"))),
            claude_json: json.clone(),
            uuid_at_write: RefCell::new(None),
        };
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;

        swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        // At the instant the canonical item was written, the co-write had NOT run,
        // so the file still showed the pre-swap account…
        assert_eq!(store.uuid_at_write.borrow().as_deref(), Some("u-A"));
        // …and after the swap the co-write has landed: the file now shows B.
        assert_eq!(displayed_uuid(&json), "u-B");
    }

    #[tokio::test]
    async fn b_to_a_to_b_cycle_keeps_canonical_and_identity_consistent() {
        let store = store_holding(b"A-token").await;
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        for (from, to, token, uuid) in [
            (ACCT_A, ACCT_B, b"B-token".as_slice(), "u-B"),
            (ACCT_B, ACCT_A, b"A-token".as_slice(), "u-A"),
            (ACCT_A, ACCT_B, b"B-token".as_slice(), "u-B"),
        ] {
            let report = swap(&store, &stash, from, to, &json).await.unwrap();
            assert!(report.canonical_confirmed, "{from} -> {to} should confirm");
            assert!(
                store.read().await.unwrap().matches(&cred(token)),
                "{from} -> {to}: canonical should hold the incoming token"
            );
            assert_eq!(
                displayed_uuid(&json),
                uuid,
                "{from} -> {to}: identity should match"
            );
        }
    }

    #[tokio::test]
    async fn a_canonical_oauth_mismatch_reconciles_to_the_incoming_account() {
        // A deliberate pre-existing inconsistency: the canonical says A, but the
        // displayed identity is a THIRD account (e.g. a prior best-effort co-write
        // was clobbered). The swap must reconcile BOTH halves to the incoming account.
        let store = store_holding(b"A-token").await;
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-STALE", 0o600);

        let report = swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        assert!(report.canonical_confirmed);
        assert!(report.oauth_cowritten);
        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json), "u-B");
    }

    #[tokio::test]
    async fn a_third_writer_between_write_and_re_read_leaves_the_swap_unconfirmed() {
        // The confirmation re-read (the 2nd read) returns a different blob than was
        // written — a concurrent `/login` or refresh winning the race between the
        // write and the post-swap re-read.
        struct ThirdWriterStore {
            reads: RefCell<u32>,
            slot: RefCell<Option<Credential>>,
            third_writer: Credential,
        }
        impl CredentialStore for ThirdWriterStore {
            async fn read(&self) -> Result<Credential> {
                let mut n = self.reads.borrow_mut();
                *n += 1;
                if *n == 1 {
                    // Step 1: the outgoing account's current blob.
                    self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
                } else {
                    // Step 5: a concurrent writer has since changed the item.
                    Ok(self.third_writer.clone())
                }
            }
            async fn write(&self, credential: &Credential) -> Result<()> {
                *self.slot.borrow_mut() = Some(credential.clone());
                Ok(())
            }
        }

        let store = ThirdWriterStore {
            reads: RefCell::new(0),
            slot: RefCell::new(Some(cred(b"A-token"))),
            third_writer: cred(b"C-from-a-concurrent-login"),
        };
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        let report = swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        // The token reroute still happened (B was written); the re-read just could
        // not confirm it, because a third writer won the race.
        assert!(!report.canonical_confirmed);
        // The co-write to ~/.claude.json still succeeded (best-effort display).
        assert!(report.oauth_cowritten);
    }

    #[tokio::test]
    async fn an_absent_outgoing_stash_aborts_before_overwriting_the_canonical() {
        // Only B is stashed; A's stash is absent, so the REQUIRED re-stash of the
        // outgoing account cannot run — the swap must abort before the canonical
        // item is touched (no half-swap).
        let store = store_holding(b"A-token").await;
        let stash = FakeAccountStash::empty();
        stash
            .write(ACCT_B, &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = swap(&store, &stash, ACCT_A, ACCT_B, &json).await;

        assert!(matches!(result, Err(Error::StashIncomplete { .. })));
        // The canonical item is untouched — still A's token.
        assert!(store.read().await.unwrap().matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn an_absent_incoming_stash_aborts_before_re_stashing_the_outgoing() {
        // Only A is stashed; B's stash is absent. Because every input is read before
        // any mutation, the swap aborts as a true no-op: the outgoing account's
        // stash is NOT rewritten and the canonical item is untouched.
        let store = store_holding(b"A-token").await;
        let stash = FakeAccountStash::empty();
        // A's stashed token deliberately DIFFERS from the canonical, so a re-stash
        // (which would copy the canonical token in) is detectable if it wrongly ran.
        stash
            .write(ACCT_A, &stashed(b"A-stash-token", "u-A"))
            .await
            .unwrap();
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = swap(&store, &stash, ACCT_A, ACCT_B, &json).await;

        assert!(matches!(result, Err(Error::StashIncomplete { .. })));
        // A's stash was NOT rewritten (still its original token, not the canonical
        // one) — the abort touched nothing.
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(a.credential.expose(), b"A-stash-token");
        // The canonical item is likewise untouched.
        assert!(store.read().await.unwrap().matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn a_stale_claude_json_cannot_corrupt_another_accounts_stash() {
        // Issue #211 (AC2): the wrong-identity re-stash guard. `~/.claude.json` is
        // stale (shows A) while the LIVE canonical is actually B's token, so a caller
        // (`use`) mis-resolves outgoing = A and asks to swap A → B. The live canonical
        // (B's token) belongs to the INCOMING account, so re-stashing it under A's key
        // would corrupt A's stash. The engine must REFUSE with ZERO writes.
        let store = store_holding(b"B-token").await; // canonical is really B's
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600); // display lies: shows A

        let result = swap(&store, &stash, ACCT_A, ACCT_B, &json).await;

        // AC1: refused on the token↔key mismatch — no wrong-identity staple.
        assert!(
            matches!(result, Err(Error::SwapWrongIdentityRestash)),
            "the swap must refuse a wrong-identity re-stash"
        );
        // AC2: A's stash is UNCORRUPTED — still A's own token + identity, never B's.
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "A's stash must be untouched"
        );
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
        // AC3: ZERO writes — the canonical item and B's stash are both untouched.
        assert!(
            store.read().await.unwrap().matches(&cred(b"B-token")),
            "the canonical item must be untouched (ZERO writes)"
        );
        let b = stash.read(ACCT_B).await.unwrap();
        assert_eq!(
            b.credential.expose(),
            b"B-token",
            "B's stash must be untouched"
        );
    }

    // --- adopt-target recovery (#212) --------------------------------------

    #[tokio::test]
    async fn adopt_target_installs_the_target_when_the_canonical_is_absent() {
        // AC #1 (engine half): the canonical is GONE (scrubbed) — `store.read()` is
        // `CredentialNotFound`. Adopt-target installs B's token → canonical and
        // co-writes B's identity, WITHOUT reading or re-stashing any outgoing account.
        let store = FakeCredentialStore::empty();
        store.set_not_found(true); // the scrubbed / absent canonical
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-STALE", 0o600); // display is stale/cleared

        let report = adopt_target(&store, &stash, ACCT_B, &json).await.unwrap();

        // The canonical now holds B's token (the write created the absent item), and
        // the post-write re-read confirmed it.
        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert!(report.canonical_confirmed);
        // The display identity was co-written to B.
        assert!(report.oauth_cowritten);
        assert_eq!(displayed_uuid(&json), "u-B");
        // AC #3: NOTHING was re-stashed — A's stash is byte-for-byte untouched, so no
        // credential could be stapled under a wrong identity (the departing token was
        // never required).
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "A's stash must be untouched"
        );
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
    }

    #[tokio::test]
    async fn adopt_target_installs_the_target_when_the_canonical_is_rotated() {
        // AC #1 (engine half), rotated variant: the canonical holds a ROTATED token that
        // matches no stash (a forced logout replaced it). Adopt-target overwrites it with
        // B's token regardless — it does not re-stash the orphan under any identity.
        let store = store_holding(b"ORPHAN-rotated-token").await;
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-STALE", 0o600);

        let report = adopt_target(&store, &stash, ACCT_B, &json).await.unwrap();

        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert!(report.canonical_confirmed);
        assert!(report.oauth_cowritten);
        assert_eq!(displayed_uuid(&json), "u-B");
        // The rotated orphan was NOT stashed anywhere — A's stash is untouched.
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "A's stash must be untouched"
        );
    }

    #[tokio::test]
    async fn adopt_target_aborts_with_zero_writes_when_the_keychain_is_locked() {
        // AC #2: "locked ≠ gone." A LOCKED keychain is transient (retry when unlocked),
        // NOT a scrubbed credential — so adopt-target must ABORT with ZERO writes rather
        // than clobber a canonical it cannot even read. The probe catches the lock before
        // any write.
        let store = store_holding(b"A-token").await;
        store.set_locked(true);
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = adopt_target(&store, &stash, ACCT_B, &json).await;

        assert!(
            matches!(result, Err(Error::KeychainLocked { .. })),
            "a locked keychain must abort the adopt (locked ≠ gone)"
        );
        // ZERO writes: unlock and confirm the canonical is untouched, the display is
        // untouched, and NOTHING was stashed.
        store.set_locked(false);
        assert!(
            store.read().await.unwrap().matches(&cred(b"A-token")),
            "the canonical must be untouched (ZERO writes)"
        );
        assert_eq!(
            displayed_uuid(&json),
            "u-A",
            "the display must be untouched"
        );
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(a.credential.expose(), b"A-token");
    }

    #[tokio::test]
    async fn adopt_target_aborts_with_zero_writes_when_the_canonical_is_present_but_unreadable() {
        // "Could not read ≠ gone." A canonical that is PRESENT but whose secret cannot be
        // read (a non-lock, non-not-found `security` error — an ACL / auth-deny) is NOT a
        // scrubbed credential: adopt-target must ABORT with ZERO writes rather than clobber
        // a present token without re-stashing it. The probe treats this exactly as a lock
        // (only a CONFIRMED-absent or readable canonical proceeds), matching the normal
        // swap's step-1 read, which `?`-aborts on any error.
        let store = store_holding(b"A-token").await;
        store.set_unreadable(true);
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = adopt_target(&store, &stash, ACCT_B, &json).await;

        assert!(
            matches!(result, Err(Error::Keychain { .. })),
            "a present-but-unreadable canonical must abort the adopt (could not read ≠ gone), got {result:?}"
        );
        // ZERO writes: clear the read fault and confirm the canonical is untouched, the
        // display is untouched, and NOTHING was stashed.
        store.set_unreadable(false);
        assert!(
            store.read().await.unwrap().matches(&cred(b"A-token")),
            "the canonical must be untouched (ZERO writes)"
        );
        assert_eq!(
            displayed_uuid(&json),
            "u-A",
            "the display must be untouched"
        );
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(a.credential.expose(), b"A-token");
    }

    #[tokio::test]
    async fn adopt_target_aborts_before_any_write_when_the_incoming_stash_is_absent() {
        // Read-everything-before-mutate: the incoming stash is the essential input; an
        // absent one aborts as a true no-op (ZERO writes) — the canonical stays absent.
        let store = FakeCredentialStore::empty();
        store.set_not_found(true);
        let stash = FakeAccountStash::empty(); // B is NOT stashed
        stash
            .write(ACCT_A, &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = adopt_target(&store, &stash, ACCT_B, &json).await;

        assert!(matches!(result, Err(Error::StashIncomplete { .. })));
        // ZERO writes: the canonical is still absent (never created).
        assert!(matches!(store.read().await, Err(Error::CredentialNotFound)));
        // The display is untouched.
        assert_eq!(displayed_uuid(&json), "u-A");
    }

    #[tokio::test]
    async fn adopt_target_locked_installs_the_target_through_the_lock() {
        // The lock-wrapped adopt (the production path): an uncontended lock acquires
        // instantly and the adopt runs, proving `adopt_target_locked` drives the same
        // recovery as the bare engine through the single-writer lock (#64).
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let store = FakeCredentialStore::empty();
        store.set_not_found(true);
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_jdir, json) = claude_json("u-STALE", 0o600);

        let report = adopt_target_locked(
            Some((lock.as_path(), SWAP_LOCK_MAX_WAIT)),
            &store,
            &stash,
            ACCT_B,
            &json,
        )
        .await
        .unwrap();

        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert!(report.canonical_confirmed);
        assert_eq!(displayed_uuid(&json), "u-B");
    }

    // --- the single-writer swap lock (#64) ---------------------------------

    #[tokio::test]
    async fn the_swap_lock_serializes_two_writers_with_no_overlap() {
        // The lock's core property (issue #64 acceptance): two writers contending on
        // one lock never occupy the critical section at once — the second BLOCKS
        // until the first releases. Each worker, while holding the lock, marks the
        // section occupied and yields TWICE, so the other worker is polled and WOULD
        // observe an overlap if the lock did not serialize them.
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let occupancy = Rc::new(Cell::new(0u32));
        let max_seen = Rc::new(Cell::new(0u32));

        let worker = |occupancy: Rc<Cell<u32>>, max_seen: Rc<Cell<u32>>, lock: PathBuf| async move {
            let _guard = SwapLock::acquire(&lock, SWAP_LOCK_MAX_WAIT).await.unwrap();
            let now = occupancy.get() + 1;
            occupancy.set(now);
            max_seen.set(max_seen.get().max(now));
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            occupancy.set(occupancy.get() - 1);
        };

        tokio::join!(
            worker(occupancy.clone(), max_seen.clone(), lock.clone()),
            worker(occupancy.clone(), max_seen.clone(), lock.clone()),
        );

        assert_eq!(
            max_seen.get(),
            1,
            "the swap lock must serialize writers — the second blocks until the first releases"
        );
    }

    #[tokio::test]
    async fn the_swap_lock_fails_closed_while_held_then_recovers_on_release() {
        // FAIL-CLOSED (the boundary-conformance refinement): a contended acquire that
        // exhausts its bounded wait returns `SwapLockBusy` (the caller then aborts
        // with ZERO writes) rather than proceeding without the lock. Once the holder
        // releases, a fresh acquire succeeds — the lock is per-swap, not sticky.
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");

        let held = SwapLock::acquire(&lock, SWAP_LOCK_MAX_WAIT).await.unwrap();
        // A second, SEPARATE open of the same file contends even within one process
        // (flock locks the open file description) — so the bounded wait elapses.
        let busy = SwapLock::acquire(&lock, Duration::from_millis(120))
            .await
            .unwrap_err();
        assert!(
            matches!(busy, Error::SwapLockBusy),
            "a held lock must fail closed, got {busy:?}"
        );
        assert_eq!(
            busy.exit_code(),
            4,
            "fail-closed shares the locked-keychain code"
        );

        drop(held);
        // Released → the next swap acquires it (no stale-lock reaping needed).
        let _recovered = SwapLock::acquire(&lock, Duration::from_millis(500))
            .await
            .expect("the lock is free once the holder drops");
    }

    #[tokio::test]
    async fn two_real_swap_writers_on_one_item_never_leave_a_split_pair() {
        // The acceptance integration: two REAL swap engines (steps 1–5) contend on
        // one keychain item + one `~/.claude.json`, serialized only by the lock. The
        // shared store YIELDS inside its canonical write, widening the exact window a
        // split would open (canonical written by one writer, json co-written by the
        // other). With the lock, the writers serialize, so the final pair is
        // CONSISTENT — canonical token and displayed identity name the SAME account —
        // and reflects the writer that ran last (fresh state), never a torn mix.
        type Slot = Rc<RefCell<Option<Credential>>>;

        struct YieldingStore {
            slot: Slot,
        }
        impl CredentialStore for YieldingStore {
            async fn read(&self) -> Result<Credential> {
                self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
            }
            async fn write(&self, credential: &Credential) -> Result<()> {
                // Yield mid-write: without the lock the OTHER swap would interleave
                // here, between this canonical write and its own json co-write.
                tokio::task::yield_now().await;
                *self.slot.borrow_mut() = Some(credential.clone());
                tokio::task::yield_now().await;
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let (_jdir, json) = claude_json("u-O", 0o600);

        // One shared canonical item, seeded with the origin account O.
        let slot: Slot = Rc::new(RefCell::new(Some(cred(b"O-token"))));
        let store_x = YieldingStore { slot: slot.clone() };
        let store_y = YieldingStore { slot: slot.clone() };
        // Two stashes per writer: the shared origin (outgoing) and that writer's
        // distinct incoming target. Distinct stash instances stand in for the one
        // keychain — both writers re-stash O and write the canonical, the contended
        // surface the lock protects.
        let stash_x = stash_with(stashed(b"O-token", "u-O"), stashed(b"X-token", "u-X")).await;
        let stash_y = stash_with(stashed(b"O-token", "u-O"), stashed(b"Y-token", "u-Y")).await;

        let lw = (lock.as_path(), SWAP_LOCK_MAX_WAIT);
        let (rx, ry) = tokio::join!(
            swap_locked(Some(lw), &store_x, &stash_x, ACCT_A, ACCT_B, &json),
            swap_locked(Some(lw), &store_y, &stash_y, ACCT_A, ACCT_B, &json),
        );
        rx.unwrap();
        ry.unwrap();

        // The final pair is CONSISTENT — not a split. The canonical token and the
        // displayed identity name the SAME account (both X or both Y), proving no
        // interleave left canonical from one writer beside json from the other.
        let canonical = slot.borrow().clone().unwrap();
        let displayed = displayed_uuid(&json);
        let consistent = (canonical.matches(&cred(b"X-token")) && displayed == "u-X")
            || (canonical.matches(&cred(b"Y-token")) && displayed == "u-Y");
        assert!(
            consistent,
            "split write: canonical and ~/.claude.json disagree (displayed={displayed})"
        );
    }

    #[tokio::test]
    async fn the_fallback_adopt_fails_closed_while_a_daemon_write_holds_the_lock() {
        // Issue #167 / #212 fallback safety: when the daemon is UP, `use <spare> --force` adopt-
        // recovery falls back to the STANDALONE adopt (`adopt_target_locked`), which takes the SAME
        // cross-process swap lock (`paths::swap_lock`) that EVERY daemon canonical write holds — the
        // auto / emergency / socket swaps via `swap_locked`, and the #282 `promote_canonical`. So
        // while a daemon canonical write is IN FLIGHT (lock held), the fallback adopt CANNOT also
        // write: a bounded acquire fails CLOSED (`SwapLockBusy`) with ZERO writes — exactly one
        // writer ever touches the canonical, never a double / torn write. Once the daemon releases,
        // the fallback acquires and installs the target in one clean write. (The daemon holds the
        // full `SWAP_LOCK_MAX_WAIT`; the fallback is given a short bounded wait so the contention
        // resolves fast — the same technique the sibling fail-closed test uses.)
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let (_jdir, json) = claude_json("u-O", 0o600);
        let store = store_holding(b"O-token").await;
        let stash = stash_with(stashed(b"O-token", "u-O"), stashed(b"R-token", "u-R")).await;

        // A daemon canonical write is in flight: it holds the single-writer swap lock.
        let held = SwapLock::acquire(&lock, SWAP_LOCK_MAX_WAIT).await.unwrap();

        // The fallback adopt, given a bounded wait, fails CLOSED — no second writer, ZERO writes.
        let busy = adopt_target_locked(
            Some((lock.as_path(), Duration::from_millis(120))),
            &store,
            &stash,
            ACCT_B,
            &json,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(busy, Error::SwapLockBusy),
            "the fallback adopt must fail closed while the daemon holds the lock, got {busy:?}",
        );
        // ZERO writes: the canonical still holds O and the display still shows O — no double write.
        assert!(store.read().await.unwrap().matches(&cred(b"O-token")));
        assert_eq!(displayed_uuid(&json), "u-O");

        // Once the daemon releases, the fallback adopt acquires and installs R — one clean write.
        drop(held);
        adopt_target_locked(
            Some((lock.as_path(), Duration::from_millis(500))),
            &store,
            &stash,
            ACCT_B,
            &json,
        )
        .await
        .expect("the fallback adopt acquires once the daemon releases the lock");
        assert!(store.read().await.unwrap().matches(&cred(b"R-token")));
        assert_eq!(displayed_uuid(&json), "u-R");
    }

    /// The mid-turn swap-correctness oracle (issue #12), driven end-to-end against
    /// the real `/usr/bin/security` CLI on a throwaway keychain — never the login
    /// keychain. macOS-only: the property rests on `security -U`'s atomic in-place
    /// update (`build/version-compat.md`), so the real CLI is the system under
    /// test (the same reason [`crate::keychain`]'s round-trip lives behind this cfg).
    ///
    /// Models the scenario the issue names: the target app (Claude Code) re-reads
    /// the canonical credential **per request**, so a swap that lands mid-turn must
    /// present a clean cut — a concurrent reader sees the outgoing account, then the
    /// incoming account, and never a torn / empty / half-written blob in between.
    /// The fully-live tail (the in-flight request's at-most-one transparently-retried
    /// 401, which is the *target's* retry, not ours) needs a live Claude token and
    /// stays a deferred manual oracle — see the module docs.
    #[cfg(target_os = "macos")]
    mod mid_turn_live {
        use super::*;

        use std::process::Command as StdCommand;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use crate::keychain::RealCredentialStore;
        use crate::stash::RealAccountStash;

        /// Claude Code's well-known generic-password service for the canonical
        /// credential (mirrors the private `keychain::SERVICE`; hard-coded here
        /// because the test seeds the item the way `/login` would).
        const CANONICAL_SERVICE: &str = "Claude Code-credentials";
        /// A canonical `acct` deliberately unlike `$USER`, so the store resolves the
        /// STORED acct rather than guessing — the same point `keychain`'s round-trip
        /// test makes.
        const CANONICAL_ACCT: &str = "sessiometer-midturn-acct";

        /// Make + unlock a throwaway keychain; the returned tempdir guard keeps it
        /// alive. Mirrors `keychain::tests::real_cli::fresh_keychain`.
        fn fresh_keychain() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let kc = dir.path().join("test.keychain-db");
            assert!(StdCommand::new("/usr/bin/security")
                .args(["create-keychain", "-p", ""])
                .arg(&kc)
                .status()
                .expect("spawn create-keychain")
                .success());
            assert!(StdCommand::new("/usr/bin/security")
                .args(["unlock-keychain", "-p", ""])
                .arg(&kc)
                .status()
                .expect("spawn unlock-keychain")
                .success());
            (dir, kc)
        }

        /// Seed the canonical `Claude Code-credentials` item, simulating `/login`.
        fn seed_canonical(kc: &Path, secret: &str) {
            assert!(StdCommand::new("/usr/bin/security")
                .args([
                    "add-generic-password",
                    "-U",
                    "-s",
                    CANONICAL_SERVICE,
                    "-a",
                    CANONICAL_ACCT,
                    "-w",
                    secret,
                ])
                .arg(kc)
                .status()
                .expect("spawn add-generic-password")
                .success());
        }

        fn delete_keychain(kc: &Path) {
            let _ = StdCommand::new("/usr/bin/security")
                .arg("delete-keychain")
                .arg(kc)
                .status();
        }

        /// How many independent swap scenarios to run when discriminating a real
        /// non-atomic window from the #457 flake. A genuine delete-then-add opens an
        /// absent window on EVERY swap (the write path itself is broken), so it
        /// reproduces across essentially every scenario; the #457 flake is instead a
        /// rare securityd cross-process `errSecItemNotFound` under a concurrent
        /// `security` modify — a Heisenberg that appears in a small MINORITY of runs
        /// (20/20 clean locally; 1× in CI). Declaring the item "reproducibly absent"
        /// only on a MAJORITY of scenarios sits in the wide valley between the two
        /// rates: a deterministic regression clears the majority every time, a lone
        /// transient never does. (Immediate-re-read "persistence" does NOT work — a
        /// realistic bare delete-then-add window is ~one `add` process, which self-heals
        /// before a few back-to-back re-reads finish, so the window must be caught by
        /// RECURRENCE across scenarios, not persistence within one — issue #457.)
        const ABSENCE_REPRO_ATTEMPTS: u32 = 5;

        /// AC (#12): a scripted long-running request + a forced mid-request swap →
        /// the request completes AND the next request reports the new account.
        ///
        /// The "long-running request" is a reader re-reading the canonical item in a
        /// tight loop (the target's per-request read); the "forced mid-request swap"
        /// is the real [`swap`] rotating A → B underneath it. The reader runs as its
        /// own task so its `security` reads genuinely race the swap's `security`
        /// write on the shared keychain.
        ///
        /// Runs [`run_mid_turn_swap_scenario`] up to `ABSENCE_REPRO_ATTEMPTS` times and
        /// decides by MAJORITY: a real delete-then-add is absent in (essentially) every
        /// scenario → a majority-absent verdict fails; the rare cross-process transient
        /// is absent in a minority → a majority-clean verdict passes. Short-circuits as
        /// soon as a majority lands either way, so a healthy run pays for a bare
        /// majority of clean scenarios.
        #[tokio::test]
        async fn a_long_running_request_completes_and_the_next_request_reports_the_new_account() {
            let majority = ABSENCE_REPRO_ATTEMPTS / 2 + 1;
            let mut absent_scenarios: u32 = 0;
            let mut clean_scenarios: u32 = 0;
            let mut last_absent_reads: u32 = 0;
            for _ in 0..ABSENCE_REPRO_ATTEMPTS {
                let absent_reads = run_mid_turn_swap_scenario().await;
                if absent_reads == 0 {
                    clean_scenarios += 1;
                } else {
                    absent_scenarios += 1;
                    last_absent_reads = absent_reads;
                }
                // Decide as soon as a majority lands: a real (reproducible) window
                // reaches the `absent` majority; a healthy write reaches the `clean` one.
                if absent_scenarios >= majority || clean_scenarios >= majority {
                    break;
                }
            }
            // A MAJORITY of independent scenarios found the canonical item absent mid-
            // swap: the absence is REPRODUCIBLE, which the atomic `-U` write can never
            // be — a genuine delete-then-add gap a per-request reader falls through. A
            // lone transient (minority) never reaches here (issue #457).
            assert!(
                clean_scenarios >= majority,
                "the canonical item was absent mid-swap in a majority of scenarios \
                 ({absent_scenarios}/{ABSENCE_REPRO_ATTEMPTS}, last {last_absent_reads}× reads) \
                 — the write was not atomic (a reproducible delete-then-add gap)"
            );
        }

        /// One forced-mid-turn-swap scenario against a throwaway keychain; returns the
        /// count of reads that found the canonical item ABSENT while the swap raced
        /// underneath — the only flaky signal, handed to the caller for majority
        /// (reproducibility) adjudication. Every OTHER guarantee (never-torn,
        /// spans-the-swap, ends-on-B, one-way cut, reroute landed, outgoing preserved)
        /// is DETERMINISTIC and asserted here directly: those never flake, so a
        /// violation fails the scenario on the spot rather than being retried.
        async fn run_mid_turn_swap_scenario() -> u32 {
            // Seed the canonical item to A and stash both A and B — the state capture
            // (#4) plus a prior `/login` would leave behind.
            let (_dir, kc) = fresh_keychain();
            seed_canonical(&kc, "A-token");
            let stash = RealAccountStash::for_keychain(kc.clone());
            stash
                .write(ACCT_A, &stashed(b"A-token", "u-A"))
                .await
                .unwrap();
            stash
                .write(ACCT_B, &stashed(b"B-token", "u-B"))
                .await
                .unwrap();
            let (_json_dir, json) = claude_json("u-A", 0o600);

            // `saw_a` gates the swap until the request has actually read the OUTGOING
            // account at least once (so the record spans the cut, not just lands on
            // B); `swap_done` lets the reader stop once it observes the new account
            // after the swap has returned.
            let saw_a = Arc::new(AtomicBool::new(false));
            let swap_done = Arc::new(AtomicBool::new(false));

            let reader = {
                let kc = kc.clone();
                let saw_a = Arc::clone(&saw_a);
                let swap_done = Arc::clone(&swap_done);
                tokio::spawn(async move {
                    let store = RealCredentialStore::for_keychain(kc);
                    let mut seen: Vec<Vec<u8>> = Vec::new();
                    // Reads that found the canonical item ABSENT (errSecItemNotFound,
                    // code 44 → `CredentialNotFound`). The atomic `-U` write keeps the
                    // item present at every instant, so a genuine delete-then-add would
                    // surface here on EVERY swap — but a lone hit can also be the rare
                    // securityd cross-process transient, so the caller adjudicates by
                    // MAJORITY across scenarios (§ ABSENCE_REPRO_ATTEMPTS) rather than
                    // failing on a single scenario's count. Capturing it (rather than
                    // discarding every error) is what keeps "never torn / never absent"
                    // falsifiable in CI, not merely observed.
                    let mut absent_reads: u32 = 0;
                    // A wall-clock backstop so a regression that never cuts over
                    // FAILS the assertions below rather than hanging CI.
                    let deadline = Instant::now() + Duration::from_secs(30);
                    while Instant::now() < deadline {
                        match store.read().await {
                            Ok(c) => {
                                let blob = c.expose().to_vec();
                                if blob.as_slice() == b"A-token" {
                                    saw_a.store(true, Ordering::SeqCst);
                                }
                                let is_b = blob.as_slice() == b"B-token";
                                seen.push(blob);
                                if is_b && swap_done.load(Ordering::SeqCst) {
                                    break;
                                }
                            }
                            // The item was absent — a CANDIDATE non-atomic window,
                            // confirmed real by the caller only if it REPRODUCES across
                            // a majority of scenarios (a lone hit is the transient).
                            Err(Error::CredentialNotFound) => absent_reads += 1,
                            // Any other error is benign contention under the concurrent
                            // write (a locked / busy keychain); the target would
                            // transparently retry, so the loop retries too.
                            Err(_) => {}
                        }
                        tokio::task::yield_now().await;
                    }
                    (seen, absent_reads)
                })
            };

            // Hold the swap until the in-flight request has read A at least once.
            let gate = Instant::now() + Duration::from_secs(10);
            while !saw_a.load(Ordering::SeqCst) {
                assert!(
                    Instant::now() < gate,
                    "the in-flight request never read the pre-swap account A"
                );
                tokio::task::yield_now().await;
            }

            // The forced mid-request swap.
            let swap_store = RealCredentialStore::for_keychain(kc.clone());
            let report = swap(&swap_store, &stash, ACCT_A, ACCT_B, &json)
                .await
                .unwrap();
            swap_done.store(true, Ordering::SeqCst);

            let (seen, absent_reads) = reader.await.expect("the reader task panicked");

            // The request completed: every observation is a COMPLETE, valid
            // credential — exactly the outgoing or the incoming token, never empty /
            // half-written / garbage. This is the atomic-`-U` guarantee in action.
            for (i, blob) in seen.iter().enumerate() {
                assert!(
                    blob.as_slice() == b"A-token" || blob.as_slice() == b"B-token",
                    "read #{i} saw a torn credential ({} bytes) — the swap was not atomic",
                    blob.len()
                );
            }
            // It genuinely spanned the swap: it read the outgoing account…
            assert!(
                seen.iter().any(|b| b.as_slice() == b"A-token"),
                "never observed the pre-swap account A"
            );
            // …and the next request reports the new account.
            assert!(
                seen.last().is_some_and(|b| b.as_slice() == b"B-token"),
                "the request did not end on the post-swap account B"
            );
            // The cut is clean and one-way: once B appears, A never returns.
            let first_b = seen
                .iter()
                .position(|b| b.as_slice() == b"B-token")
                .expect("never observed the post-swap account B");
            assert!(
                seen[first_b..].iter().all(|b| b.as_slice() == b"B-token"),
                "the active credential flapped back to A after the cutover"
            );

            // An independent fresh read confirms the canonical reroute landed…
            assert!(swap_store.read().await.unwrap().matches(&cred(b"B-token")));
            assert!(report.canonical_confirmed);
            // …and the OUTGOING account is unaffected: A's credential is preserved,
            // intact and recoverable, in its own stash — the in-flight request that
            // already read A can still complete against it.
            let a = stash.read(ACCT_A).await.unwrap();
            assert_eq!(a.credential.expose(), b"A-token");

            delete_keychain(&kc);
            absent_reads
        }
    }
}
