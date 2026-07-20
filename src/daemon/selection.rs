// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Swap-TARGET selection: the pure `pick_target*` ranking family (issues #37, #393, #612).
//!
//! Given the roster's readings plus the cycle's viability bounds, these free functions answer
//! "which account should the daemon swap ONTO" — the viability filter (issues #10/#11/#36 plus the
//! always-on session anti-thrash gate), the dominant soonest-weekly-reset axis (issue #37), the
//! rationale the wire carries (issue #393), and the velocity + per-daemon-jitter tie-breaks (issue
//! #612). Every one is `&self`-free and reads only its arguments, so the selection policy is
//! unit-tested without a `Daemon`, a clock, or any I/O.
//!
//! Extracted verbatim from `daemon` per the God-module decomposition (issue #637 step 1, issue
//! #656) — a behavior-preserving move, re-exported under `crate::daemon::*` so every existing call
//! site resolves unchanged. The state machine that CONSUMES a target (the swap decision, the
//! refresh exclusions, `Daemon::next_swap`) stays in `daemon`.

use super::*;

/// Pick the viable swap target whose weekly window resets SOONEST (issue #37):
/// among accounts other than `active` that are enabled (issue #36), whose reading
/// is available, that are NOT session-saturated (session usage below
/// `session_ceiling`) and NOT weekly-exhausted (weekly usage below `weekly_ceiling`,
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
/// dimension: [`swap::decide`] swaps away on `session >= session_ceiling` OR
/// `weekly >= weekly_ceiling`, so a target at/above EITHER trigger re-trips next
/// cycle. Guarding only weekly left a session-saturated but weekly-viable account
/// eligible, and the soonest-reset rule — anti-correlated with session headroom,
/// since the account nearest its weekly reset is the most-cycled one — would pick
/// exactly such a target, producing an indefinite session ping-pong between the two
/// soonest-reset accounts. The `session < session_ceiling` filter closes that: the
/// acquire predicate is now at least as strict as the negation of the release
/// predicate on BOTH dimensions. It is unconditional, distinct from `floor` — a
/// STRICTER reserve layered on top (effective ceiling `min(session_ceiling, floor)`)
/// which the PROACTIVE caller passes (default 80, #398) and the EMERGENCY caller
/// drops (`None`) so a dead active always escapes. The disabled exclusion (#36): a parked account
/// is never a destination even with ample headroom, and — being excluded here
/// rather than relying on its (skipped) poll — it can never hold the daemon out of
/// the #11 terminal state.
pub(crate) fn pick_target(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    floor: Option<f64>,
    session_ceiling: f64,
    weekly_ceiling: f64,
) -> Option<usize> {
    // The index-only projection for the callers that need no rationale (the swap decision, the
    // refresh exclusions). [`pick_target_with_reason`] is the single source of selection truth;
    // this drops the reason so those call sites (and their tests) stay unchanged.
    pick_target_with_reason(
        active,
        readings,
        enabled,
        floor,
        session_ceiling,
        weekly_ceiling,
    )
    .map(|(i, _)| i)
}

/// The issue-#612 enhanced-selection inputs threaded into [`pick_target_ranked`] /
/// [`pick_target_with_reason_ranked`] beyond the base viability + #37 arguments. Bundled into one
/// struct so the ranked selectors stay within the repo's 7-argument clippy bound (this repo never
/// `#[allow]`s `too_many_arguments`).
///
/// The fields are `pub(super)`, not `pub(crate)`: only `daemon` itself builds one
/// ([`Daemon::selection_tiebreak`]), and [`VelocityEma`] is private to `daemon`, so widening them
/// crate-wide would leak a more-private type through a more-public field (`private_interfaces`).
#[derive(Clone, Copy)]
pub(crate) struct SelectionTiebreak<'a> {
    /// Per-account retained session-velocity EMA (issue #539), indexed in lockstep with `readings`;
    /// `&[]` — or any absent slot — reads as "no observed climb" (see [`velocity_rate`]).
    pub(super) velocity: &'a [Option<VelocityEma>],
    /// The per-daemon selection seed (see [`Daemon::tiebreak_seed`]). `Some` activates BOTH enhanced
    /// axes (velocity preference, then per-daemon jitter); `None` degrades selection to exactly the
    /// pre-#612 soonest-reset + roster-index order (velocity ignored, un-jittered).
    pub(super) seed: Option<u64>,
}

/// Like [`pick_target`], but velocity-aware and per-daemon jittered (issue #612): the index-only
/// projection of [`pick_target_with_reason_ranked`]. A `None` [`SelectionTiebreak::seed`] degrades
/// to exactly [`pick_target`].
pub(crate) fn pick_target_ranked(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    floor: Option<f64>,
    session_ceiling: f64,
    weekly_ceiling: f64,
    sel: SelectionTiebreak,
) -> Option<usize> {
    pick_target_with_reason_ranked(
        active,
        readings,
        enabled,
        floor,
        session_ceiling,
        weekly_ceiling,
        sel,
    )
    .map(|(i, _)| i)
}

/// The retained session-velocity EMA rate (issue #612) for roster account `idx`, or `0.0` when there
/// is no TRUSTED signal — the slot is absent (`None`, or an out-of-range / empty slice, as the
/// un-jittered projection passes `&[]`), OR its EMA is not yet SUSTAINED
/// ([`samples`](VelocityEma::samples) `< MIN_VELOCITY_SAMPLES`). Gating on sustained-ness mirrors the
/// #539 projective trigger: a single-sample spike is noise, so it reads as "no observed climb"
/// rather than deprioritising a target on one unstable interval. A missing/untrusted signal thus
/// treats an un-warmed or just-reset account as the safest (lowest-velocity) landing rather than
/// penalising it for lack of data. See [`VelocityEma`].
fn velocity_rate(velocity: &[Option<VelocityEma>], idx: usize) -> f64 {
    velocity
        .get(idx)
        .copied()
        .flatten()
        .filter(|v| v.samples >= MIN_VELOCITY_SAMPLES)
        .map_or(0.0, |v| v.rate)
}

/// A per-(daemon seed, roster index) tie-break key (issue #612): decorrelate the index into the
/// seed with the [`SplitMix64`] golden-ratio odd increment, then avalanche it. Stable for a fixed
/// seed (so a daemon's selection never flaps across ticks) and well-distributed across seeds (so two
/// daemons over the same roster disperse instead of herding onto one co-selected target). Reuses the
/// existing jitter PRNG — no new dependency, so `cargo deny` stays green.
fn selection_tiebreak_key(seed: u64, index: usize) -> u64 {
    SplitMix64::new(seed.wrapping_add((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)))
        .next_u64()
}

/// Like [`pick_target`], but also returns WHY the winner was chosen ([`NextSwapReason`], issue
/// #393) — the rationale [`Daemon::next_swap`] carries on the wire so the panel + `sessiometer
/// status` render the ONE reason the daemon actually used. The un-jittered, velocity-blind
/// projection: exactly [`pick_target_with_reason_ranked`] with no velocity signal and no per-daemon
/// seed, so it keeps the pre-#612 soonest-reset + roster-index order — the standing measurement the
/// blind-gate SLI (via [`pick_target`]) and the unit tests want. The `status` preview is NO LONGER a
/// consumer: since #612 `next_swap` runs the enhanced selection, so that what it surfaces is what the
/// daemon would promote. Kept as the shared core (rather than re-deriving the reason in
/// `next_swap`) so the filter set can never drift between the selection and its stated reason.
pub(crate) fn pick_target_with_reason(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    floor: Option<f64>,
    session_ceiling: f64,
    weekly_ceiling: f64,
) -> Option<(usize, NextSwapReason)> {
    pick_target_with_reason_ranked(
        active,
        readings,
        enabled,
        floor,
        session_ceiling,
        weekly_ceiling,
        SelectionTiebreak {
            velocity: &[],
            seed: None,
        },
    )
}

/// The single source of selection truth (issue #612): [`pick_target_with_reason`] extended with the
/// two enhanced-selection axes, applied ONLY when [`sel.seed`](SelectionTiebreak::seed) is `Some`
/// (a live per-daemon seed).
///
/// The viability FILTER and the dominant #37 axis are UNCHANGED — a known weekly reset sorts ahead
/// of an unknown one, then by soonest reset epoch. Two axes refine a tie AMONG equally-soon targets,
/// in order:
///
/// 1. **Velocity**: prefer the target with the LOWER retained session-velocity EMA (`velocity`,
///    indexed in lockstep with `readings`) — a steeply-climbing peer would re-trip
///    [`swap::decide`]'s session dimension soon after a swap TO it (a wasted swap + near-term
///    re-swap), so a calmer peer with the same time-to-reset is the better landing. An absent signal
///    reads as `0.0` (see [`velocity_rate`]).
/// 2. **Per-daemon jitter**: break a remaining tie by a per-daemon-stable hashed order
///    ([`selection_tiebreak_key`]) instead of roster index, so two daemons over the same roster —
///    which see identical server-side `weekly_resets_at` and would otherwise deterministically
///    co-select and hammer ONE target — disperse. Stable within a daemon (fixed seed), so the jitter
///    itself never flaps across ticks (the velocity axis above legitimately tracks its EMAs).
///
/// Dispersal is BOUNDED to the tie case by design: where one account holds a strictly-soonest reset,
/// every daemon still co-selects it — #37 dominance is preserved above. That is exactly the herd the
/// issue describes (independent daemons reading the same server-side `weekly_resets_at`), so the
/// jitter engages precisely where the collision arises and never at the cost of the #37 axis. It
/// lowers the PROBABILITY of co-selection; it does not fix the 2×-billing case where two machines are
/// already on one account (that is the shared-signal velocity mitigation, tracked separately).
///
/// The final fallback stays the earliest roster index, so selection is always total + deterministic.
/// With [`sel.seed`](SelectionTiebreak::seed) `None` BOTH axes are skipped and this is byte-identical
/// to the pre-#612 behaviour
/// — the contract the un-jittered projection and every existing selection test rely on. Distinct
/// from the downward-only swap-CEILING jitter (issue #609): that perturbs the fire THRESHOLD; this
/// perturbs the target CHOICE among tied candidates.
///
/// The reason is computed from the WINNER exactly as before: velocity / jitter are sub-tie-breaks
/// WITHIN a reset-axis class, never a new axis, so the wire [`NextSwapReason`] set is unchanged (a
/// velocity/jitter-broken reset tie is still `SoonestReset`; a positional choice among unknown-reset
/// targets is still `RosterOrder`, the per-daemon key just refining it before roster index).
pub(crate) fn pick_target_with_reason_ranked(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    floor: Option<f64>,
    session_ceiling: f64,
    weekly_ceiling: f64,
    sel: SelectionTiebreak,
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
        .filter(|&(_, usage)| usage.weekly < weekly_ceiling)
        // Always-on session anti-thrash gate: exclude a target at/above the session
        // trigger — it would immediately re-trip [`swap::decide`]'s session dimension
        // and thrash (the exact mirror of the weekly filter above). Distinct from the
        // `floor` below, which tightens this ceiling further when the caller passes it.
        .filter(|&(_, usage)| usage.session < session_ceiling)
        .filter(|&(_, usage)| floor.is_none_or(|f| usage.session < f))
        .collect();
    let candidate_count = viable.len();
    // Soonest weekly reset (issue #37) is the DOMINANT axis: a known reset sorts ahead of an
    // unknown one (`false` < `true`), then by the reset epoch ascending. Issue #612 refines a tie
    // AMONG equally-soon targets — first by velocity (lower retained session-velocity EMA), then by
    // a per-daemon jitter key — with the earliest roster index the final fallback (matching
    // [`soonest_weekly_reset`]'s tie-break, #11, when the enhanced axes also tie or are inactive).
    // Both #612 axes are inert unless `sel.seed` is `Some`, so the un-jittered projection keeps the
    // exact pre-#612 order. `min_by` keeps the first of equal-comparing elements, but the roster-
    // index step is a strict order over distinct indices, so the winner is always unique.
    let (idx, usage) = viable.into_iter().min_by(|&(a_idx, a), &(b_idx, b)| {
        let a_reset = a.weekly_resets_at.map_or((true, i64::MAX), |r| (false, r));
        let b_reset = b.weekly_resets_at.map_or((true, i64::MAX), |r| (false, r));
        a_reset
            .cmp(&b_reset)
            // Both #612 axes, gated ONCE on the per-daemon seed: velocity first (a lower retained
            // session-velocity EMA wins a reset tie — a steep climber would re-trip the session
            // dimension soon after a swap TO it), then the jitter key to disperse the cross-machine
            // co-selection herd on a remaining tie (a fixed per-daemon seed keeps it stable across
            // ticks). `None` is the legacy path: both inert, straight to the roster index below.
            .then_with(|| match sel.seed {
                Some(seed) => velocity_rate(sel.velocity, a_idx)
                    .total_cmp(&velocity_rate(sel.velocity, b_idx))
                    .then_with(|| {
                        selection_tiebreak_key(seed, a_idx)
                            .cmp(&selection_tiebreak_key(seed, b_idx))
                    }),
                None => std::cmp::Ordering::Equal,
            })
            // The earliest roster index — the pre-#612 tie-break, and the only one legacy uses.
            .then_with(|| a_idx.cmp(&b_idx))
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
        // NONE did. No reset-time tiebreak existed; a positional tie-break won (roster index, or —
        // under enhanced selection — the per-daemon jitter key ahead of it: still positional, not a
        // reset axis). Reporting `OnlyCandidate` here would assert "only viable target" while other
        // targets were viable; `SoonestReset` would fabricate an epoch none of them carried.
        NextSwapReason::RosterOrder
    };
    Some((idx, reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- pick_target (pure) ------------------------------------------------

    // A weekly trigger well above every reading in the pick_target tests below, so
    // the weekly-exhaustion exclusion (#11) is a no-op for the ones that pin the
    // floor / selection behavior; the #11 tests use readings at/above it.
    const WK: f64 = 0.98;

    // A session trigger matching the default (`DEFAULT_SESSION_CEILING`), for the
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
        // The opt-in target_max_session_usage (#10) is a STRICTER reserve layered on the always-on
        // session gate: with the floor OFF a target need only clear the gate
        // (session < trigger); an enabled floor also excludes accounts that pass the
        // gate but sit at/above the floor. Effective ceiling = min(session_ceiling, floor).
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
        // with the target-max-session-usage OFF and ample session headroom — swapping there
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
    fn pick_target_excludes_the_weekly_tail_margin_band_so_the_arms_cannot_thrash() {
        // Issue #607, the anti-thrash lockstep. The weekly RELEASE predicate now fires at the
        // effective weekly ceiling (`ceiling − WEEKLY_TAIL_MARGIN`), so the ACQUIRE predicate must
        // move with it or the `[effective, raw)` band becomes a ping-pong: an account there is
        // simultaneously fire-eligible and target-eligible, so a swap TO it re-trips `decide`'s
        // weekly dimension on the very next cycle. `decide_action` closes this by passing the SAME
        // derived line to `pick_target` that it releases on — the invariant this function's doc
        // states as "the acquire predicate is at least as strict as the negation of the release
        // predicate on BOTH dimensions". The session dimension closes the same gap with the
        // `target_max_session_usage` reserve (#398); weekly has no such reserve, hence this.
        let weekly_threshold = crate::swap::weekly_effective_ceiling(WK); // 0.97 at the 0.98 default
        let readings = vec![
            Some(Usage {
                session: 0.50,
                weekly: 0.99,
                weekly_resets_at: None,
                session_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.975, // INSIDE the tail-margin band: below the raw ceiling, at/above the
                // fire point. Viable under the pre-#607 raw line; must NOT be now.
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.20, // genuinely open
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        // Against the derived line the band account is excluded and selection falls to the open one.
        assert_eq!(
            pick_target(
                0,
                &readings,
                &all_on(&readings),
                None,
                SESS,
                weekly_threshold
            ),
            Some(2),
            "an account inside the weekly tail-margin band is not a viable target",
        );
        // Falsifier: against the RAW ceiling that same account IS eligible — which is precisely the
        // thrash this lockstep prevents. If a future change reverts `decide_action` to passing the
        // raw ceiling here, this documents what breaks.
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, SESS, WK),
            Some(1),
            "against the raw ceiling the band account would be selected — the thrash case",
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

    // --- pick_target_ranked: velocity-aware + per-daemon jitter (pure, #612) ----------

    /// A reading builder for the #612 selection tests — `session`, `weekly`, and the weekly reset;
    /// the session reset is always absent (these tests never key off it).
    fn ranked_reading(session: f64, weekly: f64, weekly_resets_at: Option<i64>) -> Option<Usage> {
        Some(Usage {
            session,
            weekly,
            weekly_resets_at,
            session_resets_at: None,
        })
    }

    /// A retained velocity EMA for the #612 selection tests. `samples` is explicit because it is
    /// load-bearing: at or above [`MIN_VELOCITY_SAMPLES`] the rate is TRUSTED and the velocity axis
    /// acts on it; below, `velocity_rate` reads it as no signal.
    fn ema(rate: f64, samples: u32) -> Option<VelocityEma> {
        Some(VelocityEma { rate, samples })
    }

    /// [`pick_target_ranked`] with the frame every #612 case shares held fixed — account 0 active,
    /// the 0.80 reserve floor (ADR-0013), the base triggers — so each call shows only its
    /// discriminator: the readings, the velocity EMAs, and the per-daemon seed (`None` = the legacy,
    /// velocity-blind, un-jittered path).
    fn pick_ranked(
        readings: &[Option<Usage>],
        velocity: &[Option<VelocityEma>],
        seed: Option<u64>,
    ) -> Option<usize> {
        pick_target_ranked(
            0,
            readings,
            &all_on(readings),
            Some(0.80),
            SESS,
            WK,
            SelectionTiebreak { velocity, seed },
        )
    }

    /// Two viable targets tied on their weekly reset ("headroom is close") but climbing at different
    /// session velocities: the enhanced #612 selection prefers the CALMER (lower-velocity) one, since
    /// a steeply-climbing peer would re-trip the session dimension soon after a swap TO it. Velocity
    /// outranks the jitter, so the choice is seed-independent — and the un-jittered projection is
    /// velocity-blind (keeps the pre-#612 roster-order winner).
    #[test]
    fn pick_target_prefers_lower_velocity_when_headroom_is_close() {
        // "Headroom is close" (the issue's framing, and this test's AC-pinned name) is modelled
        // literally: indices 1 and 2 carry IDENTICAL headroom (0.30 session / 0.20 weekly) and reset
        // at the same instant (200), so every axis above velocity ties and velocity is what
        // mechanically discriminates. Roster order — and the legacy path — would take index 1.
        let readings = vec![
            ranked_reading(0.97, 0.10, Some(100)), // active (excluded)
            ranked_reading(0.30, 0.20, Some(200)), // tied reset, steep climber
            ranked_reading(0.30, 0.20, Some(200)), // tied reset, gentle climber → better landing
        ];
        let velocity = [None, ema(0.010, 3), ema(0.001, 3)];
        // Velocity outranks the jitter, so the calmer index 2 wins for EVERY seed.
        for seed in [1_u64, 7, 4242, u64::MAX] {
            assert_eq!(
                pick_ranked(&readings, &velocity, Some(seed)),
                Some(2),
                "lower velocity wins the reset tie, independent of the daemon seed {seed}",
            );
        }
        // Legacy (no seed) is velocity-blind → the pre-#612 roster-index winner (index 1).
        assert_eq!(pick_ranked(&readings, &velocity, None), Some(1));
        // The winner still won on the (tied) soonest reset — velocity is a sub-tie-break WITHIN that
        // axis, so the wire reason is unchanged.
        assert_eq!(
            pick_target_with_reason_ranked(
                0,
                &readings,
                &all_on(&readings),
                Some(0.80),
                SESS,
                WK,
                SelectionTiebreak {
                    velocity: &velocity,
                    seed: Some(9),
                },
            ),
            Some((2, NextSwapReason::SoonestReset { resets_at: 200 })),
        );
    }

    /// Two viable targets tied on BOTH reset and velocity: only the per-daemon jitter can break the
    /// tie, so across a range of daemon seeds the winner is NOT always the same account — the herd
    /// disperses (two daemons over the same roster no longer converge on one co-selected target). The
    /// un-jittered legacy path, by contrast, is the deterministic roster-order pick.
    #[test]
    fn pick_target_jitter_disperses_the_co_selection_herd_across_daemon_seeds() {
        let readings = vec![
            ranked_reading(0.97, 0.10, Some(100)), // active (excluded)
            ranked_reading(0.20, 0.20, Some(200)),
            ranked_reading(0.20, 0.20, Some(200)),
        ];
        // No velocity signal anywhere (`&[]`) → the velocity axis ties (0.0 == 0.0), so ONLY the
        // per-daemon jitter can break the reset tie.
        let winners: std::collections::BTreeSet<usize> = (0..24_u64)
            .map(|s| pick_ranked(&readings, &[], Some(s)).expect("a viable target exists"))
            .collect();
        assert!(
            winners.len() > 1,
            "per-daemon jitter must disperse the herd: over 24 seeds the winner was always {winners:?}",
        );
        assert!(
            winners.iter().all(|&i| i == 1 || i == 2),
            "only the two tied candidates can win: {winners:?}",
        );
        // Legacy (no seed) never disperses — the deterministic roster-order pick.
        assert_eq!(pick_ranked(&readings, &[], None), Some(1));
    }

    /// A daemon's tie-break is STABLE: for a fixed seed the same tied set yields the same winner on
    /// every call (successive ticks), and an unrelated reading drift elsewhere never moves it — so
    /// the jitter disperses ACROSS daemons without flapping WITHIN one.
    #[test]
    fn pick_target_jitter_is_stable_within_a_daemon_across_ticks() {
        // The tied winner set (index 1, 2) plus a fourth, later-reset viable account whose reading
        // drifts across "ticks" but can never enter the tied set.
        let make = |drift: f64| {
            vec![
                ranked_reading(0.97, 0.10, Some(100)), // active (excluded)
                ranked_reading(0.20, 0.20, Some(200)),
                ranked_reading(0.20, 0.20, Some(200)),
                ranked_reading(drift, 0.20, Some(500)),
            ]
        };
        let seed = Some(0x00C0_FFEE_u64);
        let pick = |readings: &[Option<Usage>]| pick_ranked(readings, &[], seed);
        let first = pick(&make(0.10));
        assert!(matches!(first, Some(1) | Some(2)));
        // Repeated ticks with the same seed + same tied set → identical winner (no flapping)…
        for _ in 0..5 {
            assert_eq!(
                pick(&make(0.10)),
                first,
                "same daemon seed must not flap the winner across ticks",
            );
        }
        // …and an unrelated reading drift elsewhere never moves the tie-break winner.
        assert_eq!(
            pick(&make(0.55)),
            first,
            "an unrelated reading change must not move the per-daemon tie-break winner",
        );
    }

    /// Neither #612 axis disturbs the dominant #37 soonest-reset rule: a strictly-sooner reset wins
    /// even when it is the STEEPER-climbing account, and across every daemon seed — velocity and
    /// jitter only ever break a reset TIE, never re-order distinct resets.
    #[test]
    fn pick_target_enhanced_axes_never_override_a_strictly_sooner_reset() {
        let readings = vec![
            ranked_reading(0.97, 0.10, Some(100)), // active (excluded)
            ranked_reading(0.30, 0.20, Some(200)), // soonest viable — but steep
            ranked_reading(0.30, 0.20, Some(500)), // flat, but resets LATER
        ];
        let velocity = [
            None,
            ema(0.050, 4), // index 1 climbing FAST…
            ema(0.000, 4), // …index 2 flat, but resets later
        ];
        for seed in [0_u64, 3, 88, u64::MAX] {
            assert_eq!(
                pick_ranked(&readings, &velocity, Some(seed)),
                Some(1),
                "the strictly-soonest reset wins regardless of velocity or the seed {seed}",
            );
        }
    }

    /// An UN-sustained velocity — a single-sample spike (`samples < MIN_VELOCITY_SAMPLES`) — reads as
    /// no signal (0.0), mirroring the #539 gate, so it does NOT deprioritise a target on one unstable
    /// interval: a reset tie against a zero-velocity sibling falls through to the jitter and
    /// disperses, exactly as if the spike were absent (a trusted spike would instead force the
    /// zero-velocity sibling to win for every seed).
    #[test]
    fn pick_target_ignores_an_unsustained_velocity_spike() {
        let readings = vec![
            ranked_reading(0.97, 0.10, Some(100)), // active (excluded)
            ranked_reading(0.30, 0.20, Some(200)), // steep, but only ONE sample → untrusted
            ranked_reading(0.30, 0.20, Some(200)), // no signal
        ];
        // One sample only — BELOW `MIN_VELOCITY_SAMPLES`, so the rate is untrusted.
        let spike = [None, ema(0.050, 1), None];
        let winners: std::collections::BTreeSet<usize> = (0..24_u64)
            .map(|s| pick_ranked(&readings, &spike, Some(s)).expect("a viable target exists"))
            .collect();
        assert!(
            winners.contains(&1),
            "an unsustained spike must not force index 1 out of the winning set: {winners:?}",
        );
        assert!(
            winners.len() > 1,
            "with the spike ignored the tie disperses via jitter: {winners:?}",
        );
    }
}
