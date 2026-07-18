---
type: architecture-decision-record
number: 25
title: "weekly_trigger is a settled ceiling too, with an independently calibrated tail margin"
date: 2026-07-18
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0025: `weekly_trigger` is a settled ceiling too, with an independently calibrated tail margin

## Status

**Accepted** — 2026-07-18 (**#607**). Extends
[ADR-0023](0023-session-trigger-ceiling-semantics.md) to the second trigger dimension.
Supersedes nothing: ADR-0023's session decision is unchanged, and this ADR amends its
§ Related note that `#41 weekly_trigger` is "the *genuinely* separate second knob… untouched"
— it is no longer untouched, though it remains genuinely separate.

## Context

**The weekly dimension had the blind spot the session dimension lost in #597.** ADR-0023 gave
`session_trigger` ceiling semantics: both swap arms fire *backward* from it through a
`TAIL_MARGIN`, so a swap *lands* the outgoing account below the ceiling after its post-swap
committed tail. `weekly_trigger` (**#41**) was left a bare fire-*at* trigger — swap when
`weekly >= weekly_trigger`.

The tail is not session-specific. It is one fixed quantity of already-committed in-flight work
that keeps billing the parked account after a swap redirects only *new* requests (**#595**
measured it; **#596** returned **GO** confirming it is real drain, not a `/oauth/usage` cache
artifact). That work bills the **weekly** window too — which is exactly why `weekly` co-moved
during the post-swap window in **8/13** of the #596 tail episodes. So a *correct* weekly swap
firing exactly at 98 could still tail past 98, and no fire-*at* value could reach it: the same
finding that motivated #597 for session.

## Decision

**`weekly_trigger` becomes a settled *ceiling*, and the weekly fire point is derived backward
from it through its OWN calibrated margin:**

```text
weekly_effective_ceiling = max(weekly_ceiling − WEEKLY_TAIL_MARGIN, 0)
```

(`swap::weekly_effective_ceiling`, `swap::WEEKLY_TAIL_MARGIN` — `src/swap.rs`.) At the default
ceiling 98 the weekly fire point is **97**.

**One weekly line, used by BOTH the release and acquire predicates.** This is load-bearing, not
incidental. `pick_target`'s anti-thrash invariant is that *"the acquire predicate is at least as
strict as the negation of the release predicate on BOTH dimensions"*. Lowering only the release
side would open a `[ceiling − margin, ceiling)` band in which an account is simultaneously
fire-eligible and target-eligible — swap onto it and it re-trips `swap::decide`'s weekly
dimension on the very next tick, a ping-pong bounded only by cooldown. The **session** dimension
closes this gap with the `target_max_session_usage` reserve (default 80, far below its ceiling);
the weekly dimension has **no such reserve**, so it closes it by moving both predicates together.
Concretely, every rotation decision keys off `Daemon::weekly_rotation_line` (the deterministic
counterpart) or the per-cycle derived draw. All three swap-firing arms — the reactive
`swap::decide` release plus its target selection, the #539 velocity-projection preemptive swap, and
the #452 bounded-blindness (blind-preempt) swap — and every operator-facing or scheduling verdict:
the all-exhausted relief hint, refresh exclusions, the `next_swap` preview, the snapshot
`weekly_exhausted` verdict, the out-of-rotation slow-poll (both its entry condition and its
reset-aware window), the `use` pre-swap gate, and the bounded-blindness gate's viable-target SLI
(the #595 landing evidence). Each is covered by a test that fails if that call site is reverted to
the raw ceiling.

**Two deliberate exemptions, both liveness-first.** `emergency_swap` (dead active) and
`recover_scrubbed_canonical` (fleet-wide lockout) keep the **raw** ceiling *and* symmetric jitter,
both of which *widen* the admissible target set. There the weekly value is a target-viability
filter rather than a not-cross line for the active account, and a dead or unauthenticated fleet is
strictly worse than a target that must rotate again shortly — the same trade these paths already
make by dropping the session reserve (`None`).

**Downward-only jitter, as ADR-0024 established for session.** A ceiling is a not-cross line, so
a configured jitter may only ever pull it *lower* (more margin), never above the operator-set
value. `draw_downward` consumes the same per-mode RNG count as `draw`, so the per-cycle draw order
stays deterministic (the emergency paths above keep symmetric `draw` by design).

**The dimensions stay independent (#41).** Each derives from its own ceiling through its own
margin; a swap fires when EITHER crosses; neither subsumes the other; and per-dimension
strict-early-fire holds (the fire point is strictly below its ceiling for every operator-settable
`50..=99`).

### Calibration — and the assumption it rests on

`WEEKLY_TAIL_MARGIN = 0.01` (**1 pp**), **not** session's 6 pp.

The weekly tail has **not been measured directly** — no weekly landing SLI exists yet. The value is
*scaled* from the #595 session measurement, and the scaling assumption is recorded here rather than
presented as a finding. The committed tail is one quantity `Δ` against two denominators:

```text
weekly_tail = session_tail × (session_quota / weekly_quota)
```

Writing `k = weekly_quota / session_quota` (how many full session windows the weekly budget is
worth), and taking the #595 session **max +5 pp**, the worst-case weekly tail is `5 pp / k`. A 1 pp
margin covers it exactly when:

```text
k >= 5     — the weekly budget is worth at least five full session windows
```

**That inequality is the assumption to re-check.** Note what it deliberately does *not* assume:
that `k` equals the *window-duration* ratio (168 h / 5 h ≈ 33.6). That ratio is an **upper** bound
on `k`, not a safe one — it describes a weekly budget large enough to run every session window
back-to-back, i.e. a weekly limit that never binds. This tool exists because the weekly limit *does*
bind (`weekly_exhausted` is a reachable, surfaced state), so the real `k` is materially below 33.6,
and a margin justified by the duration ratio would be justified in the wrong direction.

`k >= 5` is nonetheless a weak requirement — it fails only if a whole week's budget is worth under
five 5-hour sessions, which would make the weekly limit bind inside a single heavy day. Three
properties bound the cost of being wrong: there is **no weekly SLO** analogous to session's
`P100 < 99` (an under-margin degrades runway; it breaches no committed target), the default ceiling
98 leaves 2 pp of slack to the real 100 wall beneath the margin, and the error asymmetry is
ADR-0023's (`all_exhausted` fired 2× in 17 days — capacity is not binding, so an early swap is the
cheap error).

**Why not copy session's 6 pp.** It would fire the weekly arm at 92 under the default ceiling,
surrendering 6 pp of a 7-day window to guard a tail of `5 pp / k` for a `k` well above 1. The two
dimensions measure different quantities against different denominators (#41); independently
calibrated margins are the point.

## Alternatives considered

1. **Leave `weekly_trigger` a fire-*at* trigger** — **rejected**. It is unreachable by any value:
   the tail lands *after* a correct swap, which is precisely the #595/#596 finding that motivated
   #597 for session. The weekly window is simply a slower instance of the same mechanism.

2. **Copy `TAIL_MARGIN` (6 pp) to the weekly dimension** — **rejected**, and explicitly out of
   scope per #607. It spends ~40× the scaled tail, and it would silently couple two dimensions
   whose independence (#41) is a settled decision.

3. **Move only the release predicate, leaving target viability at the raw ceiling** — **rejected**.
   It re-opens the `[ceiling − margin, ceiling)` ping-pong band that `pick_target`'s own doc says
   the weekly-exhaustion exclusion exists to prevent. Both predicates move together, or neither.

4. **Give the weekly arm a `velocity × lookahead` term as well** (the #609 session treatment) —
   **not done**. The reactive lookahead corrects for gap-staleness on a fast-moving signal; weekly
   moves ~2 orders of magnitude slower, has no projection peer (`velocity_swap` projects session
   only), and so has a single fire arm. Revisit only if a weekly landing measurement shows
   gap-crossing weekly overshoot.

5. **Rename `weekly_trigger` → `weekly_ceiling` in this change** — **deferred to the naming
   follow-up (#606)**, for exactly the reason ADR-0023 § Alternatives 4 deferred the session
   rename: the token spans ~168 Rust sites, the `config.toml` key, and the **menubar wire contract**
   (`ConfigWire.swift`, `SettingsModel.swift`, and golden `Fixtures.swift`), which is too broad to
   land CI-green alongside a semantic change and is mechanically separable. #597 itself shipped as a
   *reframe* keeping the `session_trigger` name. Renaming weekly alone would also leave a **worse**
   half-renamed surface (`session_trigger` beside `weekly_ceiling`) that #606 would have to undo, so
   both dimensions rename together there. #606 must also reckon with a name collision this change
   introduces: `decide_action` now has both a `session_ceiling` and a `weekly_ceiling` local — the
   same collision ADR-0023 § Alternatives 4 already flagged for `session_ceiling`.

## Consequences

### Positive

- **A weekly swap now lands below its ceiling.** The dimension gains the property #597 gave
  session, closing the blind spot the #596 weekly co-movement evidence exposed.
- **The anti-thrash invariant is preserved end-to-end**, and is now stated once
  (`Daemon::weekly_rotation_line`) rather than implied at each call site.
- **No new operator knob**, no schema/wire change (the menubar carries a boolean verdict plus the
  raw config number; no derived threshold crosses the wire), and no session-dimension change.

### Negative / trade-offs

- **`WEEKLY_TAIL_MARGIN` rests on a stated assumption (`k >= 5`), not a measurement.** This is the
  weakest part of the change and is recorded as such. The follow-up is a **weekly landing SLI** —
  the peak `weekly_pct` a parked account reaches within `reliability::LANDING_WINDOW_SECS` of a
  swap, the weekly analogue of #595 — after which the constant should be re-calibrated against an
  observed distribution rather than a scaling argument.
- **"Weekly exhausted" now means 1 pp earlier.** The snapshot verdict, `use` gate, and slow-poll
  cadence all key off the rotation line, so an account at 97.5 (default ceiling) reports exhausted
  and is slow-polled. This is deliberate: it is the honest operational meaning ("the daemon will not
  rotate onto this"), and the alternative leaves a band where the UI reports an account usable while
  the daemon refuses it — including a `use` command that silently undoes itself.
- **`all_exhausted` / `NoViableTarget` can fire ~1 pp earlier.** Bounded and directionally
  consistent with ADR-0023's error asymmetry.
- **Name/semantics mismatch until #606.** `weekly_trigger` *means* ceiling; a reader must trust the
  reframed doc-comments until the rename lands — the same interim state ADR-0023 accepted for
  `session_trigger`.

## Related

- Issues: **#607** (this ADR). **#41** (the separate weekly dimension this reframes). **#597** /
  **#595** / **#596** (the session ceiling redesign, the landing SLI it calibrated against, and the
  spike confirming the tail is real drain — the weekly co-movement evidence is #596's).
  **#11**/**#37** (weekly exhaustion + soonest-reset target selection, whose exclusion this keeps
  coherent). **#398** (the `target_max_session_usage` reserve — the session dimension's equivalent
  anti-thrash closure, which weekly lacks). Follow-ups: the weekly landing SLI (calibration), and
  the paired `session_trigger`/`weekly_trigger` → `*_ceiling` rename (**#606**).
- Code: `swap::weekly_effective_ceiling`, `swap::WEEKLY_TAIL_MARGIN` and its compile-time guards
  (`src/swap.rs`); `Daemon::weekly_rotation_line` and the derived per-cycle draw in
  `Daemon::decide_action` (`src/daemon.rs`); the `use` pre-swap gate (`src/use_account.rs`); the
  reframed `weekly_trigger` field doc, `DEFAULT_WEEKLY_TRIGGER`, and hand-emitted `config.toml`
  comment (`src/config.rs`, per [ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md)).
- ADRs: [ADR-0023](0023-session-trigger-ceiling-semantics.md) (the session ceiling redesign this
  extends — unchanged, its § Related "untouched" note amended).
  [ADR-0024](0024-reactive-lookahead-gap-percentile-max-window-coverage.md) (downward-only ceiling
  jitter, adopted here for weekly).
  [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md) (the reserve whose absence
  on the weekly dimension forces the both-predicates-move-together closure).
