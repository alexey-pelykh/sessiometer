---
type: requirements-brief
date: 2026-07-30
source: docs/requirements/menubar-accessibility-reachability.md
workflow: /capture-requirements
status: final
---

# Requirements Brief: Menu-bar Accessibility Reachability

## Problem Being Solved

The app's accessibility affordances are verified only **downstream of their own injection point** —
every gate supplies its own in-process stimulus, so the OS→app delivery path sits outside all of
them. That makes one state invisible to the entire apparatus: **built, gated, and unreachable**.
`PanelTypeScale` is in exactly that state today, with the full suite green.

## Key Requirements

1. **R-1** — Measure the platform delivery path as a **2×2**: (injected-env vs OS-setting) ×
   (`.accessory` vs `.regular`). Activation policy is the candidate mechanism reconciling #756 and
   #845, and #756 only ever measured one cell.
2. **R-1a** — Do **not** condition that probe on an `LSUIElement` Info.plist key. This app declares
   none; it calls `setActivationPolicy(.accessory)` at runtime. A key-hunting probe records a
   **false negative**.
3. **R-3** — If no OS path exists, record the finding **and adjudicate** the in-app preference on its
   merits. Supersedes #817's AC-3, which pre-committed to shipping it.
4. **R-5** — A **reachability gate** that fails when the driver is absent, proven falsifiable by
   mutation (CONSTRAINT-A). Nothing today catches deletion: `PanelDynamicTypeLintTests` exempts
   `StatusItemController.swift` by design.
5. **R-6** — Overlap gated on **content-sized** elements only; pure frame arithmetic is `k·(default)`
   at every class and cannot fail.
6. **R-7** — Verify the panel *fits a display* at `.accessibility3`. Never rendered there, because
   never reachable; labels alone want 454 pt against a 380 pt panel.
7. **R-9a/R-9b** — #868's two renderings need **operator ratification before implementation**, and
   its verification tier is **manual-only** — the SwiftUI a11y keys are get-only, a compile error.

## Key Decisions

1. **Framing reframed to a verification-tier gap** — the narrow "nothing drives Dynamic Type" is a
   symptom. Ratified; it makes the reachability gate *prevention* rather than an add-on.
2. **#817's AC-3 superseded** — a pre-commitment to the answer the spike exists to inform, made
   worse by the false-negative risk. #817's body is amended at Stage 3 so the two agree.
3. **R-7 admitted** from the Phase 4 category sweep, though no issue asks for it — same
   newly-reachable pattern as #845/#896.
4. **#845 is premise-gated, not scheduled** — its "fonts DO grow" is an *inference*, and #756's
   measurement contradicts it. If the premise is false, #845 is latent and its framing needs
   correcting rather than implementing.
5. **The spike sequences first** — it confirms-or-voids #845, selects the driver, and arms the
   circuit-breaker.

## Assumptions & Risks

🔴 **A-1** — "macOS doesn't deliver a text-size setting here" is **issue-inferred, never measured**.
🔴 **A-2** — "Settings' fonts DO grow" is inferred from category membership. A-1 and A-2 cannot both
be true as stated; R-1 resolves them.
🔴 **A-5** — Nobody has established the operator *wants* an in-app text-size control. The § 1b
circuit-breaker exists for this: "ship no driver" is a live outcome.
🟡 **A-4** — A producer-side gate may not be feasible from a bundle that excludes
`StatusItemController.swift`. Stage 2 owes a feasibility verdict.
🟡 **A-6** — The panel may not fit at the ceiling.
🟡 **A-7** — `design-menubar.md` § D-UX-SETTINGS is `RATIFICATION-PENDING`, and #845 sits in its own
conformance backlog — #845's oracle is not settled ground.

## Stats

- Objects: 8 | Requirements: 18 (12 numbered + 6 sub-clauses) | Acceptance Criteria: 12
- Assumptions: 0 green / 4 yellow / 3 red
- Feature completeness: 5 COMPLETE / 3 NEAR-COMPLETE / 2 INCOMPLETE
- DoR: **PASS-WITH-FINDINGS**

## Full PRD

See [menubar-accessibility-reachability.md](../requirements/menubar-accessibility-reachability.md)
