---
type: architecture-decision-record
number: 24
title: "The reactive swap arm looks ahead over the measured re-observation gap (max-window coverage)"
date: 2026-07-18
status: accepted
amends: 23
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0024: The reactive swap arm looks ahead over the measured re-observation gap (max-window coverage)

## Status

**Accepted** — 2026-07-18. **Amends [ADR-0023](0023-session-trigger-ceiling-semantics.md)**
(**#609**): it implements ADR-0023 § Alternatives 6 (the deferred gap-percentile `poll_gap`)
and reframes ADR-0023's *strict-early-fire* invariant to **max-window coverage**, and it adds
the downward-only ceiling jitter deferred in **#605**. ADR-0023's **ceiling-semantics
decision** — `session_trigger` is a settled ceiling both swap arms derive their fire point
*backward* from, one predicate on two estimators, the tail margin, the reserve — is
**preserved**; what this ADR amends is *how the reactive arm derives its fire point* and *how
the two arms compose*, not the ceiling meaning. (Same shape as ADR-0023 amending ADR-0022: the
decision is carried forward; a specific *record of meaning* is superseded.)

The decision was reached by a two-lens council (an SRE/reliability lens and a Rust
API/invariant lens) that converged 2/2 on this design for the *strategic* reason — keeping the
#539 projection arm inside its **#538**-validated horizon envelope (0 over-fire at H ≤ 150 s) —
not for low code-churn. The council flagged and cleared the tactical-convergence trap explicitly.

## Context

**ADR-0023 left one margin thin, by its own account.** ADR-0023 set the default ceiling **95**,
deliberately *below* the `P100 < 99` swap-out-overshoot SLO (**#455**), as a second,
gap-independent margin. The reason it needed that second margin: the reactive arm's re-observation
lookahead (`poll_gap`) modeled only the *theoretical* near-limit round-trip `2 ×
near_limit_poll_secs` (~120 s at the default cadence), while the **measured** active-account
re-observation gap — swap-decision to next reading, over the #366 staggered interleave
([ADR-0012](0012-active-reobservation-via-schedule-interleave.md)) — runs **p50 112 s / p90 313 s
/ max 972 s**. On that tail, usage climbs unseen ~200 s past `effective_ceiling` before the next
poll, and the post-swap committed tail (**#595**) then lands it over the SLO. ADR-0023 § Alternatives
6 named the *principled* fix — widen `poll_gap` to a measured gap percentile — and **deferred** it
as a calibration follow-up needing its own decision: *which* percentile, and *how it composes with
the per-cycle ceiling jitter*.

**The strict-early-fire invariant made the naive fix a no-op.** ADR-0023's reactive threshold was
`max(effective_ceiling − velocity × poll_gap, effective_ceiling − velocity × H) = effective_ceiling
− velocity × min(poll_gap, H)`. The `max`-clamp guaranteed the projection arm (fires at
`effective_ceiling − velocity × H`) never fired *later* than the reactive arm — but it also **capped
the reactive lookahead at `H`** (default 120 s). So simply passing a wider `poll_gap` (313 s) with
`H` left at 120 s changes nothing: `min(313, 120) = 120`. A falsifier unit test documented exactly
this clamp. Widening the reactive lookahead therefore *required* a decision about the reactive/
projection relationship — it was never a one-constant swap.

**The ceiling jitter could erode the sub-SLO margin.** The per-cycle ceiling draw
(`trigger_strategy`) used symmetric jitter (`base ± spread`, clamped to `50..=99`). With the ceiling
now a *not-cross line set below the SLO for margin*, a configured upward jitter draw would raise the
ceiling *above* the operator-set value and spend that headroom — the opposite of what the sub-SLO
default intends. Constraining it to downward-only was deferred in **#605**.

## Decision

**1. The reactive arm looks ahead over the measured re-observation gap, decoupled from the
horizon.** `swap::reactive_session_threshold` **drops its `horizon_secs` parameter and the
`max`-clamp**; it is now simply

```text
effective_ceiling − velocity × poll_gap
```

with `poll_gap = swap::reactive_poll_gap_secs(near_limit_poll_secs)` when the near-limit fast-poll is
on, or `0` when it is disabled (`near_limit_poll_secs == 0`) — the latter preserving the **#584** path
where the #539 projection is the sole velocity-aware estimator. The reactive arm is now *structurally*
independent of `H` (`H` is not in its signature).

`reactive_poll_gap_secs` is `max(2 × near_limit_poll_secs, swap::REACTIVE_REOBSERVATION_GAP_SECS)`: the
measured **p90 313 s** (a calibrated constant beside `TAIL_MARGIN`) is the gap **floor**, and a slower
configured poll widens the lookahead past it via the cadence-scaled `2 × near_limit_poll_secs` term (the
pre-#609 model, kept as the scaling term rather than discarded). The `max` is load-bearing, not
cosmetic: it keeps the substitution an **unconditional widening** of the pre-#609 lookahead `min(2 ×
near_limit_poll_secs, H)` — `max(2c, floor) ≥ 2c ≥ min(2c, H)` for every cadence `c` and horizon `H` —
so the reactive `poll_gap` never *shrinks* versus before under any config. A bare `poll_gap = 313`
would instead NARROW it for a slower poll (`2 × near_limit_poll_secs > 313`), a latent SLO regression
(§ Alternatives 6).

**2. Max-window coverage replaces strict-early-fire.** Each arm covers a DISTINCT unseen window: the
reactive arm the **re-observation gap** (`poll_gap` — how long until the account is next *seen*), the
projection arm the **velocity horizon** `H` (`session_velocity_horizon_secs` — how far the EMA
reaches). The daemon composes them (reactive checked first in `swap::decide`, projection consulted on
`Hold`), so the swap fires at the EARLIER threshold:

```text
min(effective_ceiling − velocity × poll_gap,  effective_ceiling − velocity × H)
  = effective_ceiling − velocity × max(poll_gap, H)
```

— early enough to cover the LARGER unseen window. The composed fire point is **monotone
non-increasing in both `poll_gap` and `H`**, so widening either window can only LOWER the fire point,
hence lower the landing point: **the `P100 < 99` SLO stays reachable by construction** (widening never
worsens overshoot; its only cost is earlier swaps — the cheap error under ADR-0023's asymmetry). Which
arm leads is config-dependent and both stay live: reactive leads when `poll_gap > H` (the default:
313 s vs 120 s — the gap-tail specialist); projection leads when `H > poll_gap` or `poll_gap == 0`
(the horizon / fast-poll-off fallback). This preserves ADR-0023's one-predicate/two-estimators
decision (both derive from the single ceiling and share the reserve); it supersedes only the *record*
that the projection is a strict early-fire of the reactive arm — under the real gap it is the reactive
arm that leads.

**3. p90, not max.** `max` (972 s) over-fits to the worst gap ever seen (a backoff outlier) yet would
apply every cycle: at the observed peak velocity (~6.95 %/min ≈ 0.00116 frac/s) a 972 s lookahead
pulls the fire point down ~1.13 — to/below 0, a swap at any usage — while even the median account
fires ~10 pp early. p90 keeps the median's early-fire to ~3 pp while still pulling the fast tail in;
the gap beyond p90 is absorbed by the defence-in-depth already present (`TAIL_MARGIN` and, until an
operator raises it, the sub-SLO ceiling). The error asymmetry favours p90 (an early swap is cheap —
`all_exhausted` fired 2× in 17 days).

**4. A calibrated CONSTANT, not runtime-adaptive.** Same discipline as `TAIL_MARGIN`: the
re-observation gap is a slow-moving property of the polling architecture, not a fast signal. A runtime
percentile would close an adaptive loop with its own failure modes — a transient backoff incident
inflating the observed gap would make every account swap far too early exactly when capacity is
tightest. Re-verify against the **#595** landing SLI if the gap distribution is suspected to have
shifted (the #538/#540 replay recipe).

**5. The default ceiling stays 95.** The widening MAKES 99 reachable — the margin is now earned by the
lookahead rather than the sub-SLO ceiling — but the default stays 95 as the conservative operator
lever. Raising it toward 99 is an operator decision, verified against the live #595 landing SLI; this
change does not bundle it (bundling two levers against one SLI would confound attribution). The
`session_trigger` doc and the README are re-pointed to say "99 now reachable."

**6. Downward-only ceiling jitter.** `timing::Strategy::draw_downward` folds a configured jitter
below the operator-set `base` — `Uniform` → `[base − spread, base]`, `Normal` → a half-normal
`base − |z|·stddev`, `None` → `base` — then clamps to `[lo, hi]`. The session-ceiling draw uses it;
`weekly_trigger` (still a fire-AT trigger, where symmetric jitter is correct) keeps `draw`. The fold
is a proper one-sided distribution (no point-mass at `base`, unlike clamping a symmetric draw's upper
bound to `base`, which would pile the upper half onto `base` and halve the decorrelation). It consumes
the **same** `rng`-sample count as `draw` per jitter mode (`None`: 0, `Uniform`: 1, `Normal`: 2), so
swapping the call site never shifts the daemon's per-cycle draw stream.

**Composition (ADR-0023 § Alt 6's open question).** `effective_ceiling` is derived from the
**downward-jittered** ceiling (≤ the operator base), and the reactive fire is `effective_ceiling −
velocity × poll_gap`. Both levers — the wider gap lookahead AND the downward jitter — pull the fire
point *earlier*, never later: they compose ADDITIVELY in the safe direction, consistent with the
cheap-early-swap asymmetry.

## Alternatives considered

1. **Widen BOTH `poll_gap` and the default horizon `H` to the gap percentile** (so `min(gap, H) = gap`
   and the formula/invariant are untouched) — **rejected**. It drags `H` to 313 s, **2× outside the
   #538-validated `H ≤ 150 s` envelope** (0 over-fire), re-opening the #539/#538 projection calibration
   and overloading an operator knob with a second meaning. Its only merit is low churn — the
   tactical-convergence trap the council named.

2. **Runtime-measured adaptive gap percentile** (the daemon tracks its own re-observation gaps) —
   **deferred**. Real adaptive-loop failure modes (cold start; a transient incident inflating the gap →
   fleet-wide over-early swaps when capacity is tightest). The `TAIL_MARGIN`-style constant is the
   established, verifiable discipline; adaptivity is a separately scoped change if ever warranted.

3. **Add a reactive observed-usage floor now** (mirroring the projection arm's
   `session_velocity_min_project_above`) — **deferred**. The reactive arm's unbounded-below is the
   *intended* early-protective mechanism (it covers the full re-observation gap and can legitimately
   cross from a lower reading); a floor would re-censor exactly the fast-climbing accounts the widening
   exists to catch, at the cost of the landing math. Ratification-pending per ADR-0023 § Consequences;
   the config-load bound is the `v_peak` validator, now shipped by #608 (ADR-0023 § Alternatives 3) —
   it rejects only the *unsatisfiable* stack, leaving this intended runtime unbounded-below intact.

4. **Also raise the default ceiling 95 → 99 in this change** — **rejected**. The widening *enables* it,
   the operator *chooses* it (on live-SLI evidence). Bundling two independent levers against one SLI
   would make a post-change breach un-attributable.

5. **Clamp the ceiling jitter's upper bound at `base`** (instead of folding) — **rejected**. Clamping a
   symmetric draw at `base` piles the entire upper half onto `base` (a point-mass), giving ~25%
   two-daemon lockstep-at-ceiling for a `Normal` jitter and halving the effective spread — a real
   decorrelation loss on the most-consequential value. The fold has no point-mass.

6. **A bare `poll_gap = REACTIVE_REOBSERVATION_GAP_SECS` (313 s), no cadence floor** — **rejected**. It
   is the obvious literal of "replace `2 × near_limit_poll_secs` with the p90," and it is correct for
   the default and any tight cadence — but it NARROWS the lookahead (a latent, unguarded SLO regression)
   for any config with `2 × near_limit_poll_secs > 313`, i.e. `near_limit_poll_secs > 156`: there the
   pre-#609 lookahead `min(2 × near_limit_poll_secs, H)` can exceed 313 (when `H > 313` too — both knobs
   are in range), so the bare constant would RAISE the fire point and could push a landing over the SLO
   the old form avoided — breaking the "reachable by construction" guarantee for exactly that corner
   (surfaced by the #609 validate pass). `max(2 × near_limit_poll_secs, 313)` (Decision 1) keeps 313 as
   a floor and the guarantee unconditional, at one extra `max`, and folds the cadence scaling back in
   for free. The full runtime-adaptive gap tracker stays deferred (Alternative 2).

## Consequences

### Positive

- **`P100 < 99` is reachable by construction, for every config** — the composed fire point is monotone
  non-increasing in both windows AND the reactive `poll_gap` is a `max`-floor that never shrinks the
  pre-#609 lookahead (§ Decision 1), so the #609 widening can only lower the landing — not merely at the
  default cadence but for all `near_limit_poll_secs` / `H`. The ceiling can return to the SLO line (an
  operator lever), the margin now earned by an honest lookahead rather than a sub-SLO default.
- **Zero new operator knobs.** The gap percentile is an internal calibrated constant; the downward
  jitter is a consumer-side draw mode, not a new `Jitter` variant (no config/serde/template surface).
- **The projection arm stays inside its validated envelope.** `H` is untouched, so #538/#539's
  0-over-fire-at-`H ≤ 150 s` result still holds — the reason this design was chosen over widening `H`.

### Negative / trade-offs

- **The projection arm is redundant in the *default* config** (reactive leads when `poll_gap > H`). It
  stays live and independently tested via the `near_limit_poll_secs == 0` path (#584) and any
  long-horizon config. This extends — not introduces — the "projection redundant at the default
  cadence" property ADR-0023 already disclosed.
- **The reactive unbounded-below is sharpened.** A wider gap deepens the early-fire reach on the
  retained-EMA staleness tail (a just-ended burst can fire a swap at moderate usage for a few decay
  ticks). Disclosed and unfloored by design (Alternative 3); the SLI surfaces it after the fact.
- **The gap constant is not runtime-adaptive.** A shift in the re-observation-gap distribution needs a
  code change plus re-verification against the #595 landing SLI. The SLI *surfaces* such a shift; it
  does not auto-retune the constant.

## Related

- Issues: **#609** (this ADR — the gap-percentile lookahead, max-window coverage, downward jitter).
  **#597** ([ADR-0023](0023-session-trigger-ceiling-semantics.md) — the ceiling redesign this amends).
  **#455** (the `P100 < 99` SLO). **#595** (the landing-point SLI — the calibration basis and the
  instrument that verifies the SLO stays reachable on live data). **#539**
  ([ADR-0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md) — the projection arm,
  whose horizon is kept inside its #538-validated envelope by this design). **#540 / #538** (near-limit
  poll coverage and the source of the p50 112 / p90 313 / max 972 s gap measurement).
  [ADR-0012](0012-active-reobservation-via-schedule-interleave.md) (the interleave the gap is measured
  over). **#605** (where the downward-only ceiling jitter was deferred). **#608** (the shipped `v_peak`
  coupling validator + observed-peak SLI — ADR-0023 § Alternatives 3). Follow-up still open: the
  `session_trigger → session_ceiling` rename (ADR-0023 § Alternatives 4).
- Code: `swap::REACTIVE_REOBSERVATION_GAP_SECS`, `swap::reactive_poll_gap_secs` (the `max`-floor
  poll_gap derivation), and the reworked `swap::reactive_session_threshold` (`src/swap.rs`) — the
  gap-percentile constant and the max-window (`H`-independent) reactive threshold, with the
  max-window-coverage grid test, the monotonicity test, the regression-lock falsifier, and the
  `poll_gap`-never-narrows-the-pre-#609-lookahead regression-lock. The reactive call, and the reframed
  projection-arm doc-comments (`src/daemon.rs`). `timing::Strategy::draw_downward` (`src/timing.rs`) — the downward-only fold with
  RNG-sample-count parity. The `session_trigger` field doc-comment (`src/config.rs`), re-pointed to
  ceiling-plus-max-window semantics.
- ADRs: [ADR-0023](0023-session-trigger-ceiling-semantics.md) (**amended** — ceiling decision
  preserved; the reactive derivation and the strict-early-fire invariant reframed here).
  [ADR-0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md) (the projection arm).
  [ADR-0012](0012-active-reobservation-via-schedule-interleave.md) (the re-observation gap this arm
  now looks ahead over).
