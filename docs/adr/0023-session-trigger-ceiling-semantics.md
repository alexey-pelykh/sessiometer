---
type: architecture-decision-record
number: 23
title: "session_trigger is a settled ceiling both swap arms derive their fire point backward from"
date: 2026-07-17
status: accepted
supersedes: 22
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0023: `session_trigger` is a settled ceiling both swap arms derive their fire point backward from

## Status

**Accepted** — 2026-07-17. **Supersedes
[ADR-0022](0022-session-trigger-one-predicate-two-estimators.md)** (**#597**).

ADR-0022 recorded `session_trigger` as **one swap-away predicate on two estimators
of the same quantity** and flagged two operator-misleading omissions — the post-swap
committed **tail** (the trigger is not the landing point) and the **coupling** of the
safe value to `poll_secs` / `session_velocity_horizon_secs`. It recorded the *current*
"swap when usage **reaches** this" semantics as a **point-in-time** truth and named
itself the supersession target for #597. #597 lands that redesign: `session_trigger`
becomes a settled **ceiling** both arms derive their fire point *backward* from. The
one-predicate / two-estimators **decision** ADR-0022 made is **preserved** (its
strict-early-fire invariant is an explicit #597 acceptance criterion); what this ADR
supersedes is ADR-0022's *record of the meaning*, not its decision.

## Context

**The SLO ADR-0022's tail makes unreachable.** The swap-out overshoot SLO is
`P100 < 99` (**#455**): across a measurement window the outgoing account's session
usage must never *land* at or above 99. ADR-0022 established two facts that make this
unreachable under the "reach-the-trigger" semantics at any single trigger value:

1. **The velocity spread defeats a magic number.** `session_pct_per_min` runs p50
   0.63, p90 1.86, **max 6.95** (~11×). A `session_trigger` safe at peak velocity is
   wasteful at median; one safe at median breaches at peak. No single fire-*at* value
   survives the spread.

2. **46% of breaches land *after* a correct swap.** A swap that fires *exactly* at the
   trigger still overshoots, because the **parked** account keeps billing already-
   committed in-flight work after the swap redirects only *new* requests. The
   landing-point SLI (**#595**, merged) measured this committed tail over
   2026-07-01…07-17: mean **+1.08 pp**, p90 **+2 pp**, **max +5 pp**, settling
   ~135–455 s post-swap. Three swaps fired at *exactly* 95 (zero decision-point
   overshoot) and the parked account still reached 100. This tail is **46%** of all
   ≥99 breaches, and it lands past the reach of *any* fire-*at*-trigger value.

**The design gates are satisfied.** The **#596** spike returned **GO**: the tail is
**real in-flight drain**, not a stale `/oauth/usage` cache artifact — so the fix must
*build a margin term*, not merely re-poll. The **#595** landing-point SLI is merged
and is the instrument that verifies `P100 < 99` becomes achievable.

**What "swap earlier" must not break.** ADR-0022's decision — one predicate, two
estimators, the projection (**#539** / [ADR-0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md))
a **strict early-fire** of the reactive decision, never a differently-calibrated one
— is load-bearing and was re-derived 3/3 by council. The reserve invariant
`target_max_session_usage <= session_trigger`
([ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md);
`Error::ConfigTargetMaxSessionAboveTrigger`) binds **one** value. Any reframe must
keep the single predicate, the single reserve referent, and the strict-early-fire
ordering.

## Decision

**`session_trigger` becomes a settled *ceiling* — the line the outgoing account must
not cross — not a fire-*at* trigger. Both swap arms derive their fire point *backward*
from it through a single `effective_ceiling`, so the account *lands* below the ceiling
after its committed tail. One config value, no arm split, no new operator knob.** The
council default is raised **95 → 99** (the ceiling *is* the SLO line now, not a
margin-below-it guess).

**The effective ceiling.** `effective_ceiling = max(ceiling − TAIL_MARGIN, 0)`, where
`ceiling = session_trigger / 100` and `TAIL_MARGIN = 0.06` (a usage fraction; §
Calibration). A swap that lands the outgoing account *at* the effective ceiling leaves
exactly the tail margin of headroom, so the post-swap committed tail lands below the
true ceiling. At the default ceiling 0.99 the effective ceiling is **0.93**.
(`swap::effective_ceiling`, `swap::TAIL_MARGIN` — `src/swap.rs`.)

**Both arms fire backward from `effective_ceiling`.** Each leaves headroom for the gap
over which usage keeps climbing unseen, at the retained EMA velocity:

- **Reactive** (`swap::reactive_session_threshold`, consumed by `swap::decide`): the
  observed session fraction at/above which to swap is

  ```text
  max(effective_ceiling − velocity × poll_gap_secs,
      effective_ceiling − velocity × horizon_secs)
    = effective_ceiling − velocity × min(poll_gap_secs, horizon_secs)
  ```

  so the account climbs to *exactly* the effective ceiling over one re-observation
  gap after the swap decision.

- **Projection** (`velocity_swap`, **#539** / ADR-0017): fires when
  `observed + velocity × horizon_secs >= effective_ceiling`, i.e. at observed
  `effective_ceiling − velocity × horizon_secs` — the **same** effective ceiling, the
  **same** per-cycle jittered ceiling draw, the **same** reserve.

**The strict-early-fire invariant is preserved *structurally* by the `max` form.** The
reactive threshold is `>=` the projection fire point (`effective_ceiling − velocity ×
horizon_secs`) for **every** `velocity >= 0` and **every** `(poll_gap, horizon)`
pairing — with no runtime branch — because the reactive `max` includes that exact term
as one of its two operands. So the projection never fires at a *higher* observed value
than the reactive arm, i.e. never *later*. Two regimes:
  - `horizon >= poll_gap` (normal, incl. the default where they are equal): the
    `poll_gap` term wins; reactive fires one poll gap early, projection earlier still.
  - `horizon < poll_gap` (a short-horizon misconfiguration): the `horizon` term wins,
    clamping the reactive threshold **up** to the projection fire point so the two
    *coincide* rather than invert. The **un-clamped** `effective_ceiling − velocity ×
    poll_gap` alone would invert here — a falsifier test asserts exactly that. An
    exhaustive grid test (ceilings × velocities × poll-gaps × horizons) asserts the
    invariant holds everywhere.

**`poll_gap = 2 × near_limit_poll_secs`** — the reactive re-observation round-trip (a
reading is up to one near-limit interval stale, plus one interval until the next poll;
~112 s measured at the default 60 s cadence). Two consequences fall out of this choice,
both intended:
  - **`near_limit_poll_secs == 0` (fast-poll disabled) → `poll_gap == 0`**, so the
    reactive threshold collapses to the bare `effective_ceiling` and the **#539
    projection is the sole velocity-aware estimator** — the pre-#597 division of labour,
    now landing-margined. This keeps the projection arm exercisable and its **#584**
    test coverage live.
  - **At the default cadence `poll_gap = 120 s ≈ horizon `H`= 120 s**, so the two
    estimators converge on `effective_ceiling − velocity × H`; the reactive arm, checked
    first, is the one that fires. The projection is the *distinct* earlier fire only
    where `H > poll_gap` (a longer horizon) or `poll_gap == 0`. In the default config the
    projection is therefore *redundant with* reactive by construction — documented, not a
    defect.

**`velocity == 0` collapses both arms to the bare effective ceiling.** The velocity is
the retained per-account EMA, gated `>= MIN_VELOCITY_SAMPLES` **identically in both
arms** (an unwarmed or window-reset EMA reads 0). An idle account fires *at* the
effective ceiling, never early — the margin term only ever pulls the fire *earlier*,
never later.

**Name kept, semantics re-pointed.** `session_trigger` retains its name (now meaning
*ceiling*); its struct-field doc, the `DEFAULT_SESSION_TRIGGER` constant, and the hand-
emitted `config.toml` template comment ([ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md))
are reframed to ceiling semantics and point here. A pure rename to `session_ceiling` is
deferred to a follow-up (§ Alternatives 4). The **#455** AC permits this — it asks for a
single "ceiling" value "(`session_trigger` or its successor)".

**The reserve validator is unchanged.** `target_max_session_usage <= session_trigger`
stays exactly as-is (`Config::validate`, `Error::ConfigTargetMaxSessionAboveTrigger`;
range `50..=99` unchanged). Under ceiling semantics the reserve now sits below the
*ceiling* — a correct, if loose, bound (a swapped-to target keeps runway below the line
the newly-active account is judged against). The tighter coupling
`target_max <= effective_ceiling − v_peak × poll_gap` needs a peak-velocity constant
that does not exist yet; introducing it is deferred (§ Alternatives 3).

### Calibration

`TAIL_MARGIN = 0.06` (6 pp) is calibrated from the **#595** landing-point SLI — the
canonical post-swap committed-tail measurement (window through 2026-07-17), whose
distribution (mean +1.08 pp / p90 +2 pp / **max +5 pp**) is re-confirmed at build
against the merged #595 record rather than copied from #597's issue body. 6 pp is
**strictly above** the measured **max** tail of +5 pp, so the strict `P100 < 99`
landing SLO keeps ~1 pp headroom even on the worst observed tail. It is deliberately
larger than the p90 (+2 pp) for two reasons: the SLO is defined by the **max**, not the
median; and the **error asymmetry** favours buying margin — `all_exhausted` fired only
**2× in 17 days**, so capacity is **not** the binding constraint, which makes an early
swap cheap while an overshoot is expensive. The value is a documented constant with
recorded provenance and a re-verification procedure (re-run the #595 landing-SLI if the
tail distribution is suspected to have shifted), **not** a magic number and **not**
copied unverified — the "re-verify at build, don't hardcode" discipline #597 requires.

## Alternatives considered

1. **Lower `session_trigger` to a single, more conservative fire-*at* value** —
   **rejected**. No single value survives the ~11× velocity spread (§ Context), and a
   lower value *still* cannot touch the 46% post-swap tail, which lands after a correct
   swap. This is the alternative ADR-0022 already recorded as "rejected — the finding
   that motivates #597."

2. **Split `session_trigger` into two separately-calibrated knobs** (`reactive_trigger`
   + `projection_trigger`) — **rejected**, already settled 3/3 by council in ADR-0022.
   Independent draws can land the projection trigger *above* the reactive one on a tick,
   inverting the strict-early-fire invariant; and the split leaves the reserve invariant
   without a referent. Ceiling semantics keep the single shared draw, so the invariant
   holds by construction.

3. **Add the peak-velocity coupling validator now**
   (`target_max_session_usage <= effective_ceiling − v_peak × poll_gap`) — **deferred to
   a follow-up**. The honest coupling needs an assumed `v_peak` constant that does not
   exist today (ADR-0022 § Consequences flagged exactly this). Introducing it, plus an
   observed-peak-exceeds-`v_peak` SLI to keep it honest, is its own scoped change;
   shipping it inside #597 would bundle an unrelated config-surface decision. The interim
   protection is the unchanged loose reserve bound plus the #595 landing SLI.

4. **Rename `session_trigger` → `session_ceiling` in this change** — **deferred to a
   follow-up**. A literal-token rename touches ~187 sites, including golden-pinned log
   fields, serde field names, the menubar wire contract, and the `config.toml` key —
   too broad to land CI-green alongside the semantic change in one step, and a rename is
   mechanically separable from the derivation redesign. The rename must also reckon with a
   name already taken: `session_ceiling` is currently a local in the all-exhausted relief
   path (`session_trigger.min(target_max_session_usage)` — the relief-viability gate), a
   *different* quantity from this change's `effective_ceiling`, so the follow-up needs a
   distinct field name or must rename that local too. The re-pointed doc-comments carry the
   "means ceiling" caveat until the rename lands.

5. **Ceiling semantics with the `max`-form reactive clamp and `poll_gap = 2 ×
   near_limit`** — **chosen**. Makes `P100 < 99` reachable by *landing-margining* both
   arms, preserves ADR-0022's one-predicate decision and the strict-early-fire invariant
   structurally (no runtime branch, proven by an exhaustive grid + a falsifier test),
   adds **zero** operator knobs, and gives ADR-0022 a clean supersession target.

## Consequences

### Positive

- **`P100 < 99` is reachable.** Both arms fire *backward* from `effective_ceiling`, so
  the outgoing account lands below the ceiling even after its committed tail — the SLO
  the #595 SLI verifies.
- **Zero new knobs; the invariant is preserved structurally.** One config value still,
  one predicate still; the strict-early-fire ordering holds for all `velocity >= 0` and
  all `(poll_gap, horizon)` with no runtime branch.
- **Clean supersession of ADR-0022.** The one-predicate/two-estimators decision is
  carried forward intact; only the *meaning* of the value changed, and it is recorded.

### Negative / trade-offs

- **`TAIL_MARGIN` is a calibrated constant, not runtime-adaptive.** A shift in the tail
  distribution needs a code change plus re-verification against the #595 SLI. The SLI
  *surfaces* such a shift after the fact; it does not auto-retune the margin.
- **The `target_max` coupling stays documented-not-enforced** until the `v_peak`
  follow-up (§ Alternatives 3). An operator can still set
  `poll_secs` / `session_velocity_horizon_secs` / `session_trigger` into a silently-loose
  combination; the loose reserve bound and the #595 landing SLI are the interim guard.
- **Name/semantics mismatch until the rename lands.** `session_trigger` *means* ceiling;
  a reader must trust the reframed doc-comment until the follow-up rename (§ Alternatives
  4) makes the name say it.
- **The projection is redundant in the *default* config.** Because `poll_gap ≈ H` at the
  default cadence, the reactive arm is the one that fires; the projection's distinct
  early-fire value appears only when `H > poll_gap` or fast-poll is disabled. This is a
  documented property of the default, not a regression — the projection arm remains
  independently tested (#584) via the `near_limit == 0` path.
- **The reactive velocity term is unbounded below — an intended arm divergence, disclosed.**
  `reactive_session_threshold` is not floored, so a large `velocity × min(poll_gap, H)` pulls
  the fire point below the projection arm's `session_velocity_min_project_above` band, and — at
  the extreme (a config stacking `near_limit_poll_secs` toward its 3600 s max with `horizon_secs`
  at its 600 s ceiling and sustained peak velocity) — to/below 0. This is the intended
  early-protective swap (the account is climbing too fast for its re-observation gap; landing it
  at the effective ceiling means firing now), the CHEAP error under this ADR's asymmetry, and the
  reason the two arms deliberately differ (the projection's small ≤~14 pp lookahead makes a
  low-usage projection spurious and so is floored; the reactive arm's full-re-observation-gap
  lookahead can legitimately cross from a lower reading and so is not). The residual cost is a
  retained-EMA staleness window (a just-ended burst can still fire a swap at moderate usage for a
  few EMA-decay ticks) — an accepted property of the council-chosen runtime EMA, bounded and
  self-correcting. A config-**load** bound on the absurd combinations is folded into the deferred
  `v_peak` coupling validator (§ Alternatives 3); until then it is documented in
  `reactive_session_threshold`'s doc-comment and asserted as intended by a unit test. Surfaced by
  the #597 validation pass as a robustness observation (ratification-pending — the operator may
  elect to add a reactive observed-usage floor, at the cost of the landing math the current design
  preserves).

## Related

- Issues: **#597** (this ADR — the ceiling redesign). **#455** (the `P100 < 99`
  swap-out-overshoot SLO this reframe makes reachable). **#595** (the merged landing-
  point SLI — the calibration basis for `TAIL_MARGIN` and the instrument that verifies
  the SLO). **#596** (the spike whose **GO** verdict confirmed the tail is real in-flight
  drain, mandating the margin term). **#539** (the velocity-projection arm — the second
  estimator, now deriving from `effective_ceiling`; ADR-0017). **#584** (the velocity-arm
  path kept exercisable by the `near_limit == 0` → projection-only design). **#363** (the
  reaction-latency umbrella whose residual the two estimators jointly cover). **#587**
  (the phantom `active_backoff_cap_secs` doc bug — the doc/reality-drift class the kept
  loose-coupling risks, mitigated here by honest doc-comments). **#41**
  (`weekly_trigger` — the *genuinely* separate second knob; it estimates a *different*
  quantity and is untouched). **#398/#417** (the `target_max_session_usage` reserve and
  its clamp to `session_trigger`). Follow-ups to file: the pure `session_trigger →
  session_ceiling` rename (§ Alternatives 4); the `v_peak` coupling validator plus an
  observed-peak SLI (§ Alternatives 3).
- Code: `swap::effective_ceiling`, `swap::reactive_session_threshold`, and
  `swap::TAIL_MARGIN` (`src/swap.rs`) — the ceiling derivation and the strict-early-fire
  `max` clamp, with the exhaustive-grid + falsifier unit tests. The reactive draw +
  threshold and the `velocity_swap` projection (`src/daemon.rs`) — both now derive from
  `effective_ceiling`; the swap-reason log compares against the derived `session_threshold`
  so a velocity-early session swap is not mis-logged as weekly. `session_trigger` (the
  struct field, its `50..=99` range check, the `DEFAULT_SESSION_TRIGGER = 99` constant,
  and the `config.toml` template comment) in `src/config.rs`, all reframed to ceiling
  semantics. The reserve validator in `Config::validate` (`src/config.rs`) and
  `Error::ConfigTargetMaxSessionAboveTrigger` (`src/error.rs`) — kept coherent, unchanged.
- ADRs: [ADR-0022](0022-session-trigger-one-predicate-two-estimators.md)
  (**superseded** — one predicate on two estimators; this ADR **preserves that decision**
  and supersedes only its record of the "reach-the-trigger" meaning).
  [ADR-0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md) (the #539
  velocity-projection arm — the second estimator, whose projection now crosses
  `effective_ceiling`). [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md)
  (`target_max_session_usage` reserve — the `<= session_trigger` invariant kept coherent
  under ceiling semantics). [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)
  (active re-observation via interleave — the re-observation gap the reactive arm's
  `poll_gap` term looks ahead over). [ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md)
  (config hand-emit — the reframed `config.toml` `session_trigger` comment follows it).
