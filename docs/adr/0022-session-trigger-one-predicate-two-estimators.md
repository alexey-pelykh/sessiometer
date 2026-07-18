---
type: architecture-decision-record
number: 22
title: "session_trigger is one predicate on two estimators of the same quantity, not two knobs"
date: 2026-07-17
status: superseded
superseded_by: 23
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0022: `session_trigger` is one predicate on two estimators of the same quantity, not two knobs

## Status

**Superseded by [ADR-0023](0023-session-trigger-ceiling-semantics.md)** —
2026-07-17, when the #597 ceiling redesign landed and reframed `session_trigger`
from a fire-at trigger into a settled **ceiling** both arms derive their fire point
backward from. The one-predicate / two-estimators **decision** this ADR records is
**preserved** by that redesign (the strict-early-fire invariant is an explicit #597
acceptance criterion); what changed is the *derivation*, so the "reach-the-trigger"
doc-comment this ADR corrected is now itself superseded. Original status **Accepted**
— 2026-07-17.

Records the **#598** decision that `session_trigger`
is a single swap-away predicate evaluated by **two estimators of the same
quantity** — reactive `observed >= session_trigger` and the #539 projection
`observed + velocity × H >= session_trigger` — that share one trigger by
design, and must not be split into two separately-calibrated knobs. The
scope *started* from the instinct to split it; recording the decision is what
stops that instinct recurring — all three 2026-07-17 council lenses independently
re-derived the one-predicate design from scratch (unanimous **3/3** against the
split) because the point lived only in a code doc-comment and issue threads, not
in a discoverable record.

This ADR **supersedes nothing**. It *affirms and formalises*
[ADR-0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md) — the
#539 velocity-projection arm recorded there **is** the second estimator, already
documented in `velocity_swap`'s doc-comment as "a strict early-fire of the
reactive decision, not a differently-calibrated one" — and it *cross-references*
[ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md), whose
`target_max_session_usage <= session_trigger` reserve invariant binds the single
value and so is corroborating evidence that there is one trigger, not two.

**This ADR is superseded by [ADR-0023](0023-session-trigger-ceiling-semantics.md)
(#597 landed).** #597 reframed `session_trigger` from "swap when usage *reaches*
this" into a settled **ceiling** both arms derive their fire point *backward* from
(`ceiling − tail_margin − velocity × gap`). That reframe changed what the value
*means*, so the doc-comment this ADR corrected is a **point-in-time** truth of the
now-superseded "reach-the-trigger" semantics — recorded here as such so the record
and the reframe stay coherent. #597 **preserved** the one-predicate invariant (its
own acceptance criterion requires the projection never fire later than reactive); it
changed the *derivation*, not the "one predicate" **decision** — which is why this
ADR is superseded (its *record of the meaning*), not reversed (its *decision*).

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a
roster, and swaps the active account **away** before it exhausts its 5-hour
session window. The swap-away question is a single one — *"is the active account
about to cross the session trigger?"* — and the daemon answers it with **two
estimators of the same quantity**:

- **Reactive** (`swap::decide`): `observed >= session_trigger` — the account has
  already been *read* at or over the trigger.
- **Projection** (**#539**,
  [ADR-0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md)):
  `observed + rate_ema × H >= session_trigger` — a **fresh** reading plus the
  retained per-account EMA velocity is projected to cross the trigger within the
  horizon `H`. It is called from `decide_action` **exactly where the reactive path
  would Hold** (observed still below the trigger), so it fires *earlier* on the same
  climb.

Both arms compare against the **same** per-cycle jittered `session_trigger` draw —
the projection crosses the very trigger the reactive path just held below **this
tick** — and `pick_target` sees the **same** reserve. The invariant is already
stated in the daemon (`velocity_swap`'s doc-comment, `src/daemon.rs`): *the
projective peer is a strict early-fire of the reactive decision, **not a
differently-calibrated one***. There are exactly **two** call sites reading that
draw — the reactive `swap::decide` and the projection `velocity_swap`.

**The corroborating cross-field invariant.** The reserve rule
`target_max_session_usage <= session_trigger`
([ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md); enforced in
`Config::validate`, its own `Error::ConfigTargetMaxSessionAboveTrigger` variant in
`src/error.rs`) binds **one** value. It exists so a swapped-to target keeps runway
below the trigger the *newly-active* account will itself be judged against. Split
`session_trigger` into a reactive knob and a projection knob and this invariant has
no answer to *which* trigger the reserve must sit below — a structural tell that
there is one trigger, not two.

**Contrast — where the repo genuinely does have two knobs.** `session_trigger` and
`weekly_trigger` (**#41**) *are* two independent values with **no** cross-field
constraint, precisely because they estimate **different quantities** (the 5-hour
session window versus the harder 7-day weekly window); a swap fires when **either**
reaches its own trigger. Two *quantities* earn two knobs; two *estimators of one
quantity* do not. That is the line the split proposal crosses.

**Two facts about `session_trigger` the code doc-comment omits — true regardless
of the split question, and both misleading to an operator:**

1. **The post-swap committed tail — the trigger is not the landing point.** A swap
   that fires *correctly* at the trigger can still overshoot, because in-flight work
   keeps billing the **parked** account after the swap redirects only *new*
   requests. Measured over 2026-07-01…07-17 (**#595** landing-point SLI): mean
   **+1.08 pp**, p90 **+2 pp**, **max +5 pp**, settling ~135–455 s after the swap.
   Three swaps fired at *exactly* 95 — on-target, zero decision-point overshoot —
   and the parked account still reached 100. This tail is **46%** of all ≥99 SLO
   breaches, and it lands *after* a correct swap, past the reach of *any* trigger
   value. The **#596** spike confirmed it is **real in-flight drain, not** a stale
   `/oauth/usage` cache artifact (verdict **GO**): the decisive `weekly`-co-movement
   test — fresh post-swap billing keeps `weekly` rising — held in 8 of 13 tail
   episodes, with **0** showing the stale-cache signature.

2. **The coupling — the safe value is a function of `poll_secs` and
   `session_velocity_horizon_secs`, not a free constant.** Each arm must leave
   headroom for the *gap* over which usage keeps climbing unseen: the reactive arm
   for the active-account **re-observation gap** (set by `poll_secs` and the
   [ADR-0012](0012-active-reobservation-via-schedule-interleave.md) interleave), the
   projection arm for the horizon **`H`** (`session_velocity_horizon_secs`). Change
   either and the safe `session_trigger` shifts — and **nothing catches it**: a
   coupling validator would need an assumed peak-velocity constant (`v_peak`) to
   compute the required margin, and no such constant exists today. So the coupling
   is real but **documented-only, unenforced** — the same class of silent doc/reality
   drift as the phantom `active_backoff_cap_secs` comment (**#587**).

## Decision

**`session_trigger` stays one config value: one swap-away predicate, two estimators
of the same quantity. Do not split it into separately-calibrated reactive and
projection knobs.** The reactive and projection arms share the trigger by design;
the projection is a *strict early-fire* of the reactive decision (same per-cycle
jittered draw, same reserve), never a differently-calibrated one.

**Record the two omitted facts in the `session_trigger` doc-comment** (the Rust
struct-field doc, the hand-emitted `config.toml` template comment
([ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md)), and the default
constant), so an operator reading it learns that the trigger is **not** the landing
point (the tail) and that its safe value is **coupled** to `poll_secs` and
`session_velocity_horizon_secs` — pointing here for the full rationale.

This is a **recording** decision. It changes **no** behaviour, **no** config value,
and **no** trigger logic — it makes durable a design that was already shipped and
already correct, and corrects a doc-comment that under-described it.

## Alternatives considered

1. **Split `session_trigger` into two separately-calibrated knobs** (a
   `reactive_trigger` and a `projection_trigger`) — **rejected, 3/3 council
   unanimous**.
   - **Same quantity, one trigger.** Both arms estimate *the same thing* — the
     active account's session usage against the swap-away threshold. Two knobs
     assert two triggers where there is one; the projection exists to answer the
     reactive question *earlier*, not to answer a *different* question.
   - **A split can invert the early-fire invariant.** Under independent jitter
     draws, a separately-drawn projection trigger could land *above* the reactive
     one on a given tick, so the projection would fire **later** than reactive —
     the exact opposite of its purpose (it is the OBSERVED-overshoot *early* peer).
     Sharing the one draw is what guarantees "strict early-fire."
   - **It leaves the reserve invariant undefined.** `target_max_session_usage <=
     session_trigger` binds one value; a split gives it no referent (§ Context).
   - **It solves nothing the real problem needs.** The tail is **46%** of breaches
     and lands *after* a correct swap — no trigger value, and no split of the
     trigger, can reach it. Splitting adds a knob and an invariant hole while
     leaving the actual SLO gap untouched.

2. **Lower `session_trigger` to a single, more conservative value** — **rejected**
   (the finding that motivates #597, recorded here so "just lower it" is not
   re-proposed).
   - No single value survives the velocity spread: `session_pct_per_min` runs p50
     0.63, p90 1.86, **max 6.95** (~11×). A value safe at peak velocity is wasteful
     at median; one safe at median breaches at peak.
   - And a lower value *still* does not touch the 46% post-swap tail. The correct
     fix is **ceiling semantics** (#597), where both arms derive their fire point
     *backward* from a settled ceiling with a `tail_margin` term — not a lower magic
     number. That is a separate, design-gated item; this ADR neither implements nor
     pre-empts it.

3. **Leave the design in the code doc-comment and issue threads only** —
   **rejected**. It was *already* stated in `velocity_swap`'s doc-comment, and the
   split instinct still recurred and consumed a full council to re-settle. Rationale
   that lives only in a doc-comment is not discoverable *as a decision*; an ADR is
   (that is the directory's whole purpose).

4. **Record it — a new ADR plus the doc-comment fix** — **chosen**. Durably settles
   the no-split decision, corrects the operator-misleading omissions, and gives the
   ceiling redesign (#597) a clean supersession target.

## Consequences

### Positive

- **The "split it" instinct is durably settled.** The next contributor who reaches
  for two knobs finds the decision, the reasoning, and the falsifier (would the
  split ever help the 46% tail? no) without re-running a council.
- **The doc-comment no longer misleads.** An operator learns that a swap firing
  *at* the trigger is not the account's *landing* point (mean +1.08 pp, max +5 pp of
  committed tail), and that the safe trigger value is **coupled** to `poll_secs` and
  `session_velocity_horizon_secs` rather than a free number to nudge.
- **Clean supersession target for #597.** Recording the *current* "reach-the-trigger"
  semantics as a point-in-time truth lets the ceiling reframe supersede this ADR
  cleanly, rather than silently diverging from an un-recorded design.

### Negative / trade-offs

- **The doc-comment is a point-in-time truth.** If #597 lands, `session_trigger`
  gains **ceiling** semantics (a value both arms derive backward from) and this
  record is superseded. This is flagged in Status and in the doc-comment itself, so
  the reframe does not orphan a stale comment — but until #597 lands *or* is
  abandoned, a reader must hold "this is the reach-the-trigger meaning" in mind.
- **The coupling is documented, not enforced.** Recording it does not stop an
  operator from setting `poll_secs` / `session_velocity_horizon_secs` /
  `session_trigger` into a silently-wrong combination — the coupling validator stays
  unwritable until a `v_peak` constant exists (a candidate #597 introduces). The
  interim protection is the honest doc-comment plus the false-projection and
  landing-point SLIs (**#595**), which *surface* a mis-set combination after the
  fact rather than *rejecting* it at config-load.

## Related

- Issues: **#598** (this ADR). **#363** (the reaction-latency umbrella whose residual
  the two estimators jointly cover). **#539** (the shipped velocity-projection arm —
  *the second estimator*: `observed + rate_ema × H >= session_trigger` off a fresh
  reading, a strict early-fire of the reactive path; recorded in ADR-0017). **#595**
  (the landing-point SLI that measures the post-swap tail — mean +1.08 pp, max
  +5 pp; merged, the empirical basis for fact 1). **#596** (the spike whose **GO**
  verdict confirmed the tail is real in-flight drain, not a stale `/oauth/usage`
  cache artifact). **#597** (the ceiling redesign that **supersedes** this ADR —
  ADR-0023; reframes `session_trigger` as a settled ceiling both arms derive
  backward from, preserving the one-predicate invariant). **#587** (the phantom
  `active_backoff_cap_secs` doc bug — the same class of doc/reality drift the coupling
  omission is). **#41** (`weekly_trigger` — the *genuinely* separate second knob,
  because it estimates a *different* quantity: the contrast that draws the split line).
  **#455** (the swap-out-overshoot SLO `P100 < 99` the tail makes unreachable at 95 —
  #597's target). **#398/#417** (the `target_max_session_usage` reserve default and its
  clamp to `session_trigger`, the cross-field invariant in code).
- Code: `velocity_swap` in `src/daemon.rs` — the second estimator, whose doc-comment
  carries the "strict early-fire … not a differently-calibrated one" invariant this
  ADR formalises (cited by symbol; the historical `daemon.rs:4582-4585` line reference
  in #597's and this item's body has since drifted). `session_trigger` in
  `src/config.rs` — the struct-field doc, the `DEFAULT_SESSION_TRIGGER` constant, and
  the hand-emitted `config.toml` template comment, all corrected by this item. The
  cross-field validator in `Config::validate` (`src/config.rs`) plus
  `Error::ConfigTargetMaxSessionAboveTrigger` (`src/error.rs`) — the one-value reserve
  invariant.
- ADRs: [ADR-0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md)
  (the #539 velocity-projection swap arm — **affirmed and formalised**: it *is* the
  second estimator; this ADR records that it shares the reactive trigger by design).
  [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md)
  (`target_max_session_usage` reserve — **x-ref**: its `<= session_trigger` invariant
  binds the single value, corroborating one-trigger-not-two).
  [ADR-0012](0012-active-reobservation-via-schedule-interleave.md) (active
  re-observation via interleave — **x-ref**: the reactive arm's observation gap that
  the coupling leaves headroom for).
  [ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md) (config hand-emit — the
  emitted-`config.toml` `session_trigger` comment this item corrects follows it).
  [ADR-0023](0023-session-trigger-ceiling-semantics.md) (the #597 ceiling redesign —
  **supersedes this ADR**: reframes `session_trigger` from a fire-at trigger into a
  settled ceiling both arms derive backward from, preserving the one-predicate /
  two-estimators decision recorded here). **This ADR supersedes none** — it is itself
  superseded by ADR-0023.
