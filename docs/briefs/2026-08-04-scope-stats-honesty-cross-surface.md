---
type: scope-brief
date: 2026-08-04
workflow: /scope
source: session /investigate — stats roster aggregate and fleet runway
prd: docs/requirements/stats-honesty-cross-surface.md
design: docs/design/stats-honesty-cross-surface-solution-design.md
items: 11 (#1028–#1038)
status: final
---

# Scope Brief: Stats Honesty

## What started this

Two observations, both correct, both about the same week:

> *"why there are 0s and 0 episodes of all accounts >=95% in the stats while it's literally 1 hour
> till any account is usable — wtf"*

> *"Accounts lasts ~648427 — that's wrong, we've been waiting for a few days of full downtime."*

Every roster-level figure was wrong on the week it mattered most, and **each was wrong in the
direction of calm**. That direction is the finding. A metric that fails toward alarm gets fixed the
day it ships; one that fails toward *nothing is happening* is invisible precisely when it is lying.

## What was actually wrong

Three independent defects, not one:

1. **The panel fabricates a zero.** `all_high_covered_secs` was `0` — the *denominator*. The census
   was never once able to observe the whole roster. The CLI gates on this correctly; the Swift
   `StatsRoster` does not decode it, so the panel renders `0 episodes (0s)` for a measurement that
   never happened. Identical in shape to #804 → #805.

2. **The runway saturates.** `(total_rate > 0.0).then(|| (total_headroom / total_rate).round() as i64)`
   — a guard testing for *exactly* zero against an EMA that decays toward zero and never arrives, feeding
   a cast that has been *saturating* since Rust 1.45. Live wire: `runway_secs: 9223372036854775807`.
   Reproduced at 1e-11 → 787,037 days; at 1e-300 → `i64::MAX`.

3. **The census is structurally blind — and this is the one that resized the work.** The census
   intersects every account's validity window, anchored to the *poll cadence*. A saturated peer is
   polled on the widened exhausted cadence (3600s) while staleness tracks `poll_secs` (300s), so it
   counts as covered ~8% of the time. Intersect five such peers and joint coverage collapses to zero.
   **The census goes blind exactly when the roster is saturated — the only time it has anything to
   report.** The `0 episodes` was not a render bug at all; the render bug was hiding it.

## What the scope did not assume

The PRD began as free-standing and was **reframed to remediation** when grounding found
**REQ-STA-B-008** already ratified as a `[keystone]`: *"UNKNOWN, never zero, never exhausted, never
healthy."* These are not new requirements — they are a **conformance gap against rules the project
already agreed to**. The producer's own code says so imperatively: *"Surfaces MUST consult the
denominator and render UNKNOWN rather than a bare `0`."*

The runway is the opposite case: **`fleet-runway` has no parent requirement at all** (0 hits in the
HQ PRD). So the `648427 days` figure is not merely inaccurate — it breaches the
**REQ-STA-B-006 / SUR-001 no-forecast keystones**. A compliance breach, and its severity rose accordingly.

The fix for defect 3 was already ratified *for the sibling metric*: REQ-STA-B-010 anchors a blocked
reading to its own carried expiry, *"which is what makes the metric measurable at all"*. The
implementation precedent is in-repo — `blocked_windows()` does it; `validity_windows()` does not.

## The 11 items

| Issue | Item | Requirements |
|---|---|---|
| #1028 | Runway degeneracy — meaningful-rate floor, no saturating cast, plausibility refusal | R-3, R-4, R-7, R-20 |
| #1029 | Panel census coverage gate + de-jargon | R-1, R-2, R-8, R-21 |
| #1030 | Census validity-window anchoring *(the structural fix)* | R-18, R-19 |
| #1031 | Capacity holds on the panel | R-2, R-9 |
| #1032 | Fleet runway on the panel | R-2, R-5, R-17, R-20 |
| #1033 | SPIKE — is the runway reportable at all under a representativeness guard? | gates R-6 |
| #1034 | Runway representativeness | R-5, R-6 |
| #1035 | Cross-surface parity contract — pin the roster axis | R-10, R-11, R-12 |
| #1036 | Runway fault recording | R-15 |
| #1037 | Mock gap-state frames | R-13 |
| #1038 | Regression tests | R-16 |

**R-14 has no item because it was discharged during scoping**: issue #866's body asserted
`StatsRoster` already carried `allHighCoveredSecs`. It does not. A comment preserves the record and
the body was surgically corrected — a false premise in an open issue that scopes adjacent work.

**Ordering that matters** (prose in-body, matching the repo's convention — native GitHub dependencies
are unused here):

```
#1028 ──┬──> #1032        runway must stop emitting i64::MAX before it reaches a 2nd surface
        └──> #1036        a fault can only be recorded once the fault is defined
#1033 ─────> #1034        half 2 only; half 1 (state the counted set) is unblocked
#1031, #1032 ──> #1037    frames depict what the panel will actually render
#1028, #1029, #1031, #1032 ──> #1035
                          parity pins CORRECTED behaviour — pinning first freezes the defect
#1038 runs ALONGSIDE the fixes. #1029, #1030 are roots.
```

## Your two interventions, and what they changed

Both were rejections of an options menu, and both were right to be.

- **"0 covered — covered WHAT?"** → **R-21**. The word was the field name `all_high_covered_secs`
  leaking into a user-facing string. Worth knowing: **the CLI has the same defect today**, rendering
  `, 64% covered`. Both surfaces are in scope for it.

- **"for CLI this line needs to be printed"** → **R-20**, and this one was load-bearing. `fleet_line`
  emits the runway *only* under `runway_secs: Some(_)` (doc: *"Rendered ONLY when the pool has a finite
  runway"*). So the meaningful-rate floor, shipped alone, would have made the line **vanish more
  often** — the corrective work would have made the surface *quieter*, not more honest. That is
  premortem P2 landing in reality before it shipped.

After the second rejection I stopped asking and decided the copy under the two constraints you gave,
recorded as explicitly correctable. It is in the design brief's copy table — correct it freely; it is
copy, not architecture.

## Gates

**Stage 3.7 — PASS-WITH-FINDINGS.** Forward coverage: all 21 requirements traced. Backward: no
phantom items. Three **design-reference carry-forward gaps** were found and fixed — D-STA-9 was
missing from both census items, D-STA-5's render rule from #1029, and the `CLI-Goldens-Rebaselined:`
trailer obligation from #1028. The gate was re-run green after the fixes rather than assumed. The
single remaining finding is R-14, which has no item because it is *done*.

**Stage 4 — 11/11 READY.** Two conditional notes, both stated in-band so no executor is blocked:
- **#1034 half 2** does not start until #1033 reports, with the decision rule (≥ ~20% of windows
  report → ship; below → descope to honesty-only) fixed **in advance**, so it cannot be renegotiated
  against a disappointing number.
- **#1031** is blocked-on #848 (an open `question`: the capacity-holds cause split has no stated
  assignment rule, two defensible rules disagreeing 6/1 vs 2/5 on the same corpus) — with an explicit
  narrowed path: *render the total without the split until it is settled*.

Every item carries its Build Reference **in-band**, with parent-requirement claims quoted verbatim.
The HQ path is provenance only — an executor may sit behind a repo boundary it cannot cross, and a
pointer nobody can dereference is not a delivery path.

## Grounding refresh

The tree moved `cb3eaca` → `4acdf6e` mid-scope, touching two files this scope targets
(`StatusPanelFormat.swift`, `StatusPanelRoster.swift`). All three defects were **re-verified intact**
at the new HEAD; only line drift (`statsAggregateText` :2218 → :2245). `cb3eaca` stays pinned as the
named pre-fix baseline because every line reference here was grounded against it.

That window also landed `docs/requirements/gui-cli-capability-parity.md` — a PRD whose name collides
with #1035's. It is **capability** parity (can the GUI *do* what the CLI does); #1035 is **render
STATE** parity (do both surfaces *say* the same thing). Disjoint artifacts. A guard is in #1035 so
nobody merges them.

## Not answered here

**#865** — *should the census refuse to report under roster fallback* — is **not** closed by this
scope. R-18 changes the measurability landscape it was raised against, so it must be **re-read
afterwards**. It must not be closed by implication.

## One caveat worth stating plainly


The design's dual-lens Lock Gate normally dispatches independent `product-strategist` and
`ux-architect` agents. Agent dispatch was unavailable this session, so both lenses were applied
analytically by a single actor. That is recorded in the design doc as single-actor ratification rather
than claimed as independent review — the lock is honest about what it is.

---

## Appendix — Design Reference Register

Carried here because the scope working file (`.tmp/`, gitignored scratch) is reclaimed at completion.
The carry-forward into each item's `## Build Reference` was audited green at Stage 3.7 Phase 1.9b.

| Surface | Reference | Kind | Governs |
|---|---|---|---|
| Gap/coverage semantics, both surfaces | `hq/strategy/design-stats.md` § D-STA-9 | parity-surface | *"Missing sample = UNKNOWN (never 0/exhausted/healthy); per-period coverage = seen÷expected; low-coverage annotated."* Rationale is normative: *"a daemon-down week must not read as 'underused → cancel'."* |
| Roster block render | `hq/strategy/design-stats.md` § D-STA-5, :129 | visual-mock | Source of the `—` gap sentinel. Its `≥95%` is *"an illustration of the shape, not a pinned value"* — the menubar once hardcoded `≥90%` from an earlier version of that line: **an illustration read as a spec** |
| CLI ↔ panel state parity | `hq/strategy/design-menubar.md` § R-2 | parity-surface | *"a divergence between them is a bug"* — STATE-parity, not glyph-parity |
| Stats tab aggregate callout | `apps/menubar/design/menubar-preview.html` (`.agg`, :943/:1020) | visual-mock | The panel's build reference. **Depicts only the happy path** — no gap-state frame exists |
| Severity/parity contract | `build/fixtures/cross-surface-severity.json` + `src/cross_surface.rs` (ADR-0026 / #768) | parity-surface | Byte-pinned manifest, two independent conformers. **The roster/stats aggregate is in neither the pinned set nor the declared-`uncovered_axes` set** |
| Panel goldens | `apps/menubar/design/renders/panel-goldens/**` | render | Rebaseline needs `Panel-Goldens-Rebaselined:` |
| CLI goldens | `build/fixtures/cli-renders/**` | render | Rebaseline needs `CLI-Goldens-Rebaselined:` |
| Parent requirements | `hq/strategy/prd-stats.md` (private HQ) | spec | `REQ-STA-{B,C,CFG,I,Q,SUR}` family, 21 IDs. **`fleet-runway` absent entirely** (0 hits) — the second, independent gap. Quoted in-band throughout; the path is provenance, never a dereferenceable dependency |

**One adopt-time provenance flag, and it is the interesting one.** The absence of a gap-state frame in
`menubar-preview.html` **is not a ratified decision** — it was never depicted and never adjudicated.
Because the mock is the panel's build reference, its *silence* currently reads as "render the number
unconditionally". So the panel is faithful to a reference that never covered the degraded case: a
**Reference Defect**, not an implementation defect. #1037 is the corrective.

## Appendix — Stage 0.7 exemption

The Existence & Desirability Gate was **not applicable**: this is evidence-backed corrective work on
shipped surfaces of an in-use product, so the existence question was settled when `sessiometer stats`
and the menubar panel were built. Classification (`Audit findings`) was reached on soft signals only —
a severity table, finding IDs, evidence and recommendations in session context, with no structural
`# Audit Findings for Scoping` file — so the exemption was **decided rather than user-confirmed**, and
is recorded here as correctable rather than silently inherited.
