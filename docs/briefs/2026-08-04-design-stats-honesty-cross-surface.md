---
type: design-brief
date: 2026-08-04
source: docs/design/stats-honesty-cross-surface-solution-design.md
workflow: /design-solution
status: final   # locked — A-3 resolved by derivation, A-7 reclassified to SPIKE-1
---

# Design Brief: Stats Honesty

## Problem

Every roster-level figure was wrong on the week it mattered, each in the direction of *calm*. The
design makes each figure's precondition honestly computable, consulted at every render, and pinned by
a contract neither surface can move alone.

## Key Decisions

1. **The runway refuses on a *result-side* plausibility bound, never a clamp** — because an input-side
   epsilon on the burn rate is arbitrary and unit-coupled, while the result bound is derivable: weekly
   quotas reset, so any runway beyond **one weekly window** is a statement the metric cannot make. A
   clamped figure would be a *credible* lie, which is worse than an obvious one.
2. **The census anchors validity to each reading's carried expiry** — mirroring what REQ-STA-B-010
   already ratified for capacity holds. This is the fix for the blindness; the pattern already exists
   in-repo as `blocked_windows()`. Rejected the cheaper "widen `stale_after` globally", because it
   would make a genuinely dead daemon read as covered — destroying the thing REQ-STA-B-008 protects.
3. **The parity manifest PINs the roster axis, with at least one case expecting UNKNOWN** — declaring
   it uncovered would be honest but leaves the root cause untouched, and parity without an UNKNOWN
   case would pass happily with *both* surfaces printing `0`.
4. **The panel's roster block becomes a stacked list, not the CLI's joined line** — three facts now
   carry variable-length qualifiers, which is exactly what forces wrapping. Same STATE, different
   layout; `design-menubar` R-2 permits precisely this.
5. **The unrepresentative-subset predicate reuses the daemon's own viability boundary** — never a
   second, independently-chosen water. That drift is what REQ-STA-B-010 exists to prevent.
6. **Parity lands last**, after the render fixes, so the manifest pins corrected behaviour rather than
   freezing the defect.

## What your two interventions changed

> **Editorial note added 2026-08-11 (issue #1105).** This is a dated record and its claims are
> **left as written** — they were true on 2026-08-04 and correcting them would falsify the record.
> Two exceptions, both of which were *never* true rather than merely overtaken: the CLI render was
> called `fleet_line`, which has never been a Rust item (the band is `render_summary`; the runway's
> own line is `fleet_runway_line`), and the accompanying line citation is dropped rather than
> re-pinned. Both claims below have since been delivered — the runway line by issue #1028, the
> `, 64% covered` wording by issue #1029.

- **"0 covered — covered WHAT?"** → **R-21**. You caught implementation vocabulary leaking into a
  user-facing string. It's a field name (`all_high_covered_secs`). Worth knowing: **the CLI has the
  same defect today** — it renders `, 64% covered`. Both surfaces are now in scope for it.
- **"for CLI this line needs to be printed"** → **R-20**, and this one was load-bearing.
  `render_summary` emits the runway *only* under `runway_secs: Some(_)` (doc: *"Rendered ONLY when
  the pool has a finite runway"*). So the meaningful-rate floor would have made the line **vanish
  more often** — the corrective work would have made the surface quieter, not more honest.

I also had a factual error in the PRD that this check caught: I'd claimed the CLI leaves the counted
set unstated. It doesn't — it already prints `(1 of 6 counted)`. The open surface is the panel only.

## Copy I chose rather than asked again

You declined the menu twice, which told me the framing was wrong — copy is my job once you've given
the constraints, and you'd given two. These satisfy both; correct them freely, they're not architecture:

| State | Both surfaces |
|---|---|
| Census unmeasurable | `not measurable — never saw all 6 at once` |
| Census partially seen | `3 episodes (1h40m) — all 6 in view 64% of the week` |
| Census quiet | `0 episodes (0s)` |
| Runway, no burn | `accounts last: unknown — no measurable combined burn (1 of 6 counted)` |
| Runway, implausible | `accounts last: unknown — implausible result, recorded as a fault (1 of 6 counted)` |

No forecast verbs (REQ-STA-B-006), no field names (R-21), identical on both surfaces so nothing depends
on hover — which matters, because #950 already found `.help()` unreliable on a disabled control.

## Open Questions — both resolved, design is locked

- **A-3: the plausibility bound — RESOLVED, and it corrected the design.** `weekly_headroom` is a usage
  *fraction* and `weekly_rate` is fraction-per-second, so the quotient is seconds. But the computation
  ignores replenishment: every account's weekly quota resets on its own ~7-day cycle, so **any runway
  beyond one weekly window asserts the fleet drains with no reset intervening — impossible.** The bound
  is **one weekly window**, independent of roster size. My first draft said `roster × window` (42 d);
  that was too loose by a factor of the roster size, because pooling head-room across accounts does not
  push the horizon past the first reset — the resets are what refill the pool.

- **A-7: does R-6's predicate leave the runway reportable? — RECLASSIFIED as SPIKE-1.** It is a
  feasibility unknown, and those belong in a time-boxed spike, not as a question blocking a design
  lock. One evening: replay the on-disk history through the candidate predicate over rolling weekly
  windows and measure the reporting rate. **Decision rule fixed in advance** — ≥ ~20 % of windows
  report → ship R-6 as designed; below that, DG-2 fires and R-6 descopes to "honesty shipped,
  correctness not reachable in appetite". It cannot be settled analytically: the predicate depends on
  the empirical joint distribution of staleness and ceiling-proximity, which is exactly what surprised
  us once already.

## Design Tracks

| Track | Approach | Key trade-off |
|---|---|---|
| Technical Architecture | Precondition honesty at three layers: computable → consulted → pinned | R-18 is algorithmic, not a render fix — it's what re-sized the appetite |
| UX/IA | Shared STATE vocabulary, per-medium layout | Stacked panel list diverges visually from the CLI line, deliberately |
| UI/Visual | Mock gains a frame per reachable degraded state | Rebaselines panel goldens |
| Testing | Property + per-state unit + parity contract; semantic assertions only | Every test must fail against `cb3eaca` |

## Not answered here

**#865** (should the census refuse to report under roster fallback) is **not** closed by this design.
R-18 changes the measurability landscape #865 was raised against — it must be re-read afterwards,
not resolved by implication.

## Full Design

See [stats-honesty-cross-surface-solution-design.md](../design/stats-honesty-cross-surface-solution-design.md)
