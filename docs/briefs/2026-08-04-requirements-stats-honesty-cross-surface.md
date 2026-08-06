---
type: requirements-brief
date: 2026-08-04
source: docs/requirements/stats-honesty-cross-surface.md
workflow: /capture-requirements
status: final
---

# Requirements Brief: Stats Honesty — Coverage Gating, Runway Degeneracy, Cross-Surface Parity

## Problem Being Solved

On a week when the entire six-account fleet was saturated and no account was usable for days, every
roster-level figure `sessiometer` reports was wrong — and each was wrong in the direction of *calm*.
The panel printed `0 episodes (0s)` for a census that was **unmeasurable**; the CLI printed a runway
of `~648427` days from a burn rate that had decayed to nearly nothing; and the one metric that actually
explained the downtime (capacity holds — 6 of them, ≥10h22m) is CLI-only. The affected user is the sole
operator, who is also the sole developer.

## Key Requirements

1. **A measurement with a zero coverage denominator reports UNKNOWN on every surface** — never a
   number (R-1). This is not new: it is the already-ratified `[keystone]` **REQ-STA-B-008**, which
   the CLI obeys and the panel never received the field to obey.
2. **UNKNOWN must be distinguishable from a measured zero** and from a genuinely quiet week (R-2).
3. **A burn rate not meaningfully distinguishable from zero yields UNKNOWN, stated — not omitted**
   (R-3), and no runway figure may come from a saturating cast (R-4).
4. **The runway must stay a past-rate descriptor, never a forecast** (R-7) — this makes the
   `648427 days` figure a breach of two keystones (REQ-STA-B-006, REQ-STA-SUR-001), not just an
   inaccuracy.
5. **Where the runway counts a subset of the roster, say so; where the subset excludes every
   ceiling-bound account, refuse** (R-5, R-6).
6. **The panel reports capacity holds**, gated on their own denominator (R-9).
7. **The cross-surface parity manifest either pins the roster/stats axis or declares it uncovered**
   (R-11) — this is the prevention, and the reason the item list is not just "fix two renders".

## Key Decisions

1. **This is a remediation PRD in the code repo, not a product PRD** — because the parent already
   exists. `hq/strategy/prd-stats.md` owns 21 `REQ-STA-*` requirements including the governing
   keystone. Re-deriving "gap honesty" as a new requirement would have duplicated a ratified one.
   Every requirement here traces *up*, with the parent's claim quoted in-band so the document stays
   actionable by anyone who cannot reach the private HQ.
2. **The goal is honesty, not accuracy.** Making the runway's counted set *representative* means
   changing which accounts the daemon polls — out of scope. Making it *honest* is cheap. If the
   representative computation can't be reached inside appetite, the descope target is stated up front
   (DG-2): ship the honesty half and stop.
3. **`fleet-runway` has no parent requirement at all** — zero occurrences in `prd-stats.md`. It exists
   only in the design doc, which extends gap honesty to it but only for the *no-samples* case. That
   crack is why nobody specified its degenerate-rate behaviour. Adding the parent requirement belongs
   to the HQ's lifecycle and is explicitly out of scope here.
4. **No schema or wire change is needed.** Both `all_high_covered_secs` and `capacity_hold_covered_secs`
   were verified already serialized. This corrected my own initial hypothesis, which had the census
   field missing from the wire — the gap is Swift-consumer-side only.
5. **The panel is faithful to its build reference.** `menubar-preview.html` depicts only the happy
   path, so this is also a **Reference Defect** routing to the design owner — the *implementation*
   conformed to a reference that never covered the degraded case.
6. **Parity ordering matters.** R-11/R-12 should land *after* the render fixes, so the manifest pins
   corrected behaviour rather than freezing the defect (premortem P3).

## Assumptions & Risks

- **A-7 (HIGH, untested)** — R-6's refusal may disable the runway *permanently*: idle accounts are
  always stale under one-account-per-tick polling, so a naive predicate might never report again. The
  hedge is mandatory: replay a week of real history before shipping it.
- **A-1 (HIGH)** — the two coverage denominators are documented identically but were observed with
  wildly different values (**0** vs **386747 s**) over the same window and roster. Gate each
  measurement on its own denominator; do not unify into a shared helper until this resolves.
- **A-3 (MED)** — a *principled* plausibility bound for the runway (roster-size × weekly window) is
  plausible but unverified. Regardless, the requirement is **refusal, never a clamp** — premortem P7
  is that a clamped figure becomes a *credible* lie, which is worse.
- **A-5 (MED)** — that the operator wants the panel to *say* UNKNOWN rather than hide the row is
  inferred from the complaint's shape, never asked. Gated at DG-1.

## Decided rather than surfaced — R-12 and R-13

Both are pipeline-authored, and the reflex was to route them to you for ratification. That reflex was
tested against "what does the operator know that the pipeline does not?" and neither survived it:

- **R-12** (the parity manifest must pin at least one UNKNOWN case — otherwise parity passes with
  *both* surfaces printing `0`) is additive rigor inside R-11, which you already scoped. Reversible,
  fail-closed in the safe direction, justified by evidence already in the document.
- **R-13's requirement** (a build reference must depict the states its surface can reach) is the lesson
  the sibling `panel-presentation-reference-coverage` PRD already established.

Asking either would have transferred accountability without transferring capability. Both are recorded
as `pipeline-decided` with the reasoning in § 12, so no reader mistakes them for stakeholder asks.

**R-13's *depiction* is a different matter** and stays yours: what the panel actually shows when a
census is unmeasurable is carried into Stage 2 as a required design input, hedged meanwhile to "state
it visibly" (A-5) — the reversible choice.

## Scope amendment — a tenth item

Stage-1 grounding found the panel ships a **`fleetRunwayWarnSecs` tunable for a figure it never
displays**. That sat outside your bounded nine-item selection, so it was surfaced rather than absorbed —
and asked *before* Stage 2, because it changes the design surface rather than just the item count.

**You promoted it in.** The runway becomes a panel surface: **R-17**, feature `runway-on-panel`. The
setting stops being an orphan and the panel reaches three of the four roster-block facts
`design-stats.md:115` specifies. Two constraints follow:

1. **R-17 is ordered behind R-3/R-4.** Shipping the runway to a second surface while it can still emit
   `i64::MAX` would double the blast radius of the defect this work exists to close.
2. **R-9 and R-17 share one layout decision.** The panel's roster block goes from one fact to three;
   that is a single Stage-2 design call, not two taken in isolation.

## One correction worth flagging

The `UnknownStateFidelity` baseline was first written as "CLI 1.0 (5/5)". That conflated *the CLI obeys
the coverage rule* (true) with *the CLI has full unknown-state fidelity* (false — all four
runway-degeneracy cases are CLI-side defects). Corrected to **CLI 2/6**. The CLI is the reference
implementation for **coverage gating only**; on the runway it is as wrong as the panel. Worth holding
onto for Stage 2, where "match the CLI" is otherwise a tempting shortcut.

## Stats

- Objects: 5 · Requirements: **17** · Acceptance criteria: **10** (all with BUT NOT) · Quality attributes: 4
- Assumptions: 7 (2 decided / 4 test / 1 surface) · Premortem findings: 7 · State matrix: 11 rows
- Provenance: 8 `n/a` (operator's words or a ratified keystone) · 7 user-ratified via scope membership
  · 2 `pipeline-decided` with recorded rationale · **0 unratified-and-unaccounted**
- DoR: **PASS-WITH-FINDINGS** — carried by check 5 (two INCOMPLETE features:
  `runway-representativeness`, blocked on assumption A-7; `gap-state-reference`, blocked on your
  depiction call). Check 6 (provenance) is a clean **PASS**.

## Full PRD

See [stats-honesty-cross-surface.md](../requirements/stats-honesty-cross-surface.md)
