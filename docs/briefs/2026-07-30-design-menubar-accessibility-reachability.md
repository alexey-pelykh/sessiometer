---
type: design-brief
date: 2026-07-30
source: docs/design/menubar-accessibility-reachability-solution-design.md
workflow: /design-solution
status: draft
---

# Design Brief: Menu-bar Accessibility Reachability

> `status: draft` — **not a locked design.** Four load-bearing open questions remain (below); the
> Open-Questions Lock Gate forbids `final` while any stands. Marking it `final` would be a false lock.

## Problem

Every existing gate supplies its own in-process stimulus, so the OS→app delivery path lies outside
all of them. That makes one state invisible: **built, gated, and unreachable**. `PanelTypeScale` is
in it today, suite green.

## Key Decisions

1. **Measure before choosing** — R-1's 2×2 gates the driver fork *and* confirms-or-voids #845.
   Nothing that depends on the driver's identity is designed to a presumed answer.
2. **The reachability gate ships under every outcome — one item, two shapes.** Positive gate if a
   driver ships; **defect pin** if none does (green while unreachable, red when fixed). Both are
   mutation-falsifiable; both already have in-tree precedent. This makes the design robust to its own
   central unknown, and it is why "ship no driver" costs almost nothing.
3. **A-4 resolved: the producer-side gate IS feasible.** The concern was that
   `StatusItemController.swift` is excluded from the test bundle. That bars a *compiled* gate, not a
   *source-as-data* one — `PanelDynamicTypeLintTests` reads that exact file today (it must, to exempt
   it). Same mechanism, same file, not an analogy.
4. **T2 as a new verification tier** — source-as-data lint, the only tier that can see the producer.
   T1 (in-process) structurally cannot; T3 (manual) was never run. Extends ADR-0031. *Net-new,
   surfaced rather than absorbed.*
5. **Storage: client-local (S-B), author-chosen** — on one argument, not preference: under daemon
   storage a display preference becomes unreadable *exactly when the panel is already degraded*, so
   the accessibility setting evaporates during a fault. An affordance that fails with its transport
   is not an accessibility affordance. Also needs no wire change at all.

## Design Tracks

| Track | Approach | Key trade-off |
|---|---|---|
| Technical Arch | Live option set for the driver; single injection site preserved | Defers the decision; buys correctness |
| Testing Arch *(central)* | Three tiers T1/T2/T3; CONSTRAINT-A on every new gate | T2 is new surface to maintain |
| UI/Visual | Author the two missing oracles; ratify #868's renderings first | Blocks #868 on the operator |
| UX/IA | Settings control **only** under the in-app-preference option | Would be #946's third unreferenced surface |
| Data/API | **No wire change under S-B** | S-A's cost is the argument against it |

Security: N/A (no new surface). Performance: N/A (one panel, one user). Infra/Integration: N/A.

## Gates

Feasibility **PASS** (no INFEASIBLE must-haves) · Risk **PASS** (no unmitigated HIGH) ·
Forward coverage **PASS** · Backward coverage **PASS** · Design Lock **NOT LOCKED**.

Highest risk (9/9): *the probe measures the wrong variable and records a false negative.* Mitigated by
R-1a (the app has no `LSUIElement` plist key to find — it's a runtime call) plus the 2×2's explicit
cell enumeration.

## Open Questions

- **Which driver?** Context: needs SPIKE-1's 2×2. Impact if deferred: driver, gate polarity, storage,
  and Settings UX all stay open. *Measurable.*
- **Do Settings' fonts actually grow?** Context: #756 measured inert, #845 inferred they scale.
  Impact: #845 may be latent, not live — implementing it would build on a false premise. *Measurable.*
- **What replaces vibrancy under Reduce Transparency, and `Switching…` under Reduce Motion?**
  Context: amends a ratified aesthetic; no reference authors it. Impact: blocks all #868 work.
  ***Needs you — not measurable.***
- **Ratify client-local storage?** Context: author-chosen on the daemon-down argument. Impact: only
  material if the in-app preference ships. ***Needs you.***

## Full Design

See [menubar-accessibility-reachability-solution-design.md](../design/menubar-accessibility-reachability-solution-design.md)
