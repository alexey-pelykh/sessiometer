---
title: Panel & Brand Presentation Reference Coverage
scope: menubar-panel-presentation
created: 2026-07-30
status: draft
dor_status: passed-with-findings
source: /investigate 2026-07-30; findings were transient scratch, not retained
formulation:
  technical-architecture: complete
  ux-ia: complete
  ui-visual: complete
  testing-architecture: complete
features:
  expiry-placement: {stage: design, tracks: {ux-ia: complete, ui-visual: complete}}
  icon-grid: {stage: design, tracks: {technical-architecture: complete}}
  chip-affordance: {stage: delivered, tracks: {ui-visual: complete}}   # ADR-STUB-1 DECIDED → option (d); #956, 0ab82fc
  row-affordance-coverage: {stage: design, tracks: {ux-ia: complete}}
  axis-disposition-gate: {stage: design, tracks: {testing-architecture: complete}}
artifacts:
  design-doc: docs/design/panel-presentation-reference-coverage-solution-design.md
  # No design-brief or requirements-brief keys: this PRD's pipeline run never wrote them. The keys
  # previously here named files that have never existed anywhere, in this repo or on any disk —
  # a pointer with no referent is not provenance, it is a claim of provenance (issue #1063).
---

# PRD — Panel & Brand Presentation Reference Coverage

> **Provenance warning, read before acting.** This PRD was authored by an AI pipeline (`/scope` Stage 1)
> from an investigation the same session produced. Requirements R-5 … R-11 are **pipeline-authored and
> not yet user-ratified** — they were decided autonomously under the standing preference for deciding
> reversible calls, and every one of them is a reversible tracker-scoping call. Each carries
> `Origin` + `Ratification` so no reader mistakes an AI addition for a stakeholder ask. See § 12.

## 1. Problem Statement

**Current state.** Six presentation defects are live across two shipped surfaces — the menu-bar status
panel and the macOS app icon. All six were discovered by a human looking at a screenshot. None was caught
by any of the repo's nine CI jobs, and none is tracked.

**Affected users.** Every user of the menu-bar app. Two defects are reachable before the app is even
opened: the app icon renders visibly unlike its peers in the macOS Login Items pane (a surface users read
as a legitimacy signal for a background login item), and the panel's `EXPIRY` value sits in a different
column from every other value in the same grid.

**Why now.** The `EXPIRY` defect landed `23fecc4` (#884, PR #922) on **2026-07-29** — one day before it was
noticed. It is the newest instance of a pattern already acknowledged in-repo: `83a275d` (#763/#947) states
this same root cause verbatim for the Settings window. Three independent surfaces, one mechanism.

**The problem, stated as a mechanism rather than a list of bugs:**

> Presentation coverage is asserted **per element, against whatever the build reference happens to
> author** — never against the set of **axes** an element needs decided. So an axis nobody authored is an
> axis nobody decided, and an axis nobody decided is settled silently by whatever the layout code
> happened to do.

Two branches, both live:

- **(i) The oracle is silent or absent for the axis** — F-1, F-2, F-4. `apps/menubar/design/menubar-preview.html`
  is correctly scoped as "the oracle only for what it authors — silence is not authority" (repo
  `CLAUDE.md`), but nothing *enumerates* what it fails to author. The expiry row's author deliberately
  decided the label cell, deliberately dropped the bar, and never decided the value's column — which then
  fell into the bar's vacated slot. One surface over: no reference states the macOS app-icon grid, so a
  full-bleed canvas shipped against a platform contract that expects an inset body.
- **(ii) The oracle speaks, but the axis is unmeasurable to the gate** — F-3, F-5. This is sharper than
  "no gate exists". The repo already enforces absolute WCAG contrast: `Tests/StatusPanelFormatTests.swift`
  carries a WCAG 2.x relative-luminance/contrast helper and **13 call sites, asserting ≥ 3.0 (1.4.11) and
  ≥ 4.5 (AA)** against the palette (#445). The swap chip escapes that discipline not for want of tooling
  but because it deliberately renders through SwiftUI's **system** hierarchical tints — `.resting` →
  `.tertiary`, `.armed` → `.secondary` (`StatusPanelFormat.swift:482-483`) — which have **no numeric value
  anywhere in the codebase** for the existing helper to read. The axis is unrepresentable, so it is
  unassertable.

**Framing challenged, two alternatives rejected:**

- *"Six unrelated bugs."* Rejected — F-1/F-2 and F-3/F-5 form two clean mechanism-pairs, and #763 is a
  third independent instance. Pattern, not coincidence.
- *"Just finish the mock."* Rejected as insufficient — F-3's defect is **inside** the mock (its own
  `--text-3` token composites to 2.10:1), so completing the mock cannot fix it; and F-2 is on a surface a
  panel mock will never cover.

**Prevention over solution.** Because branch (i) is *invisible by construction*, fixing six instances
without changing how axes get declared predicts recurrence (see premortem P2, § 8). The prevention answer
— an authoring-time axis declaration — is therefore **in scope** (R-8), not deferred. This is a direct
result of the framing pass; the source findings file recorded it as a question.

## 1b. Boundaries

### Appetite

**1 week (small batch)** for everything not decision-gated: R-1, R-2, R-5, R-6, R-8, R-9.
The decision-first items (R-3, R-4, R-7) are **not sized** — their appetite cannot be set before their
decision is made, and sizing them now would fabricate precision.

**Circuit breakers** — hit one, the item converts to a spike rather than expanding:
- R-2: if the macOS icon grid is not grounded against an Apple-published source within 2 hours, stop and
  spike. An inset derived only from three peer measurements is a guess wearing a number.
- R-3: if pinning chip tokens turns out to break Increase-Contrast / Dark-Mode adaptation, stop — that is
  the trade-off surfacing, and it belongs to the decision, not the implementation.

### Out of Scope

- **Any code, config, or asset change.** This PRD is a planning artifact. Implementation is `/do`'s.
- **Dynamic Type and Reduce Transparency.** Outside the panel mock entirely (repo `CLAUDE.md`), and
  outside this scope. Named here so their absence is a boundary, not an oversight.
- **The Settings window.** `83a275d` (#763/#947) already owns the same root cause there. Generalize, do
  not re-file.
- **The meter tint contrasts.** Owned by open siblings #830 / #831. The chip is not a duplicate of those.
- **Internationalization.** Verified: the app ships **no** `.lproj`, `.xcstrings`, or `NSLocalizedString`
  call. Fixed-width value cells therefore carry no localized-overflow risk today. If localization is ever
  added, R-1's cell widths become a live constraint — recorded, not scoped.
- **Panel telemetry.** We have no signal for whether users find the swap chip at all. Naming the gap; not
  proposing to close it here.
- **`apps/menubar/spikes/**`.** Outside the build graph; not live code.

## 2. ORCA Object Model

| Object | Definition | CTAs |
|---|---|---|
| **PanelElement** | A rendered unit of the status panel — account row, meter line, expiry line, swap chip, auth glyph. The unit whose axes require decisions. | `Render`, `DeclareAxes`, `ConformToReference` |
| **PresentationAxis** | One decidable dimension of a PanelElement: **column placement · resting contrast · hover response · tooltip scope · accessibility exposure**. | `Decide`, `Measure` |
| **BuildReference** | The oracle authoring an axis's target. Instances: the panel mock; the CLI `render_status` column order (parity); the macOS icon grid (external). Carries a **coverage set** — which `(element, axis)` pairs it authors. | `Author`, `Amend`, `Rebaseline` |
| **PresentationGate** | An automated check asserting an axis conforms. Carries a **strength**: *relational* (armed > resting) or *absolute* (≥ 3.0:1). | `Assert` |
| **AppIconAsset** | The brand icon across its emitted forms: `brand/src/icon.svg` → PNG set → `Assets.car` → optional Icon Composer `.icon`. Subject to a platform grid contract. | `Emit`, `Inset` |

**The load-bearing relationship**: `BuildReference.coverage ⊇ PanelElement.axes` is the invariant the root
cause violates. Nothing computes either side today, which is why the violation is silent.

## 3. Requirements (EARS)

### PanelElement

**R-1** — *When* the status panel renders an account's expiry line, the system **shall** place the expiry
value in a pinned value cell horizontally consistent with the reset-duration cell of the meter lines above
it. `Origin: user-stated` ("row labels are misaligned"). `Ratification: n/a`.

**R-4** — *When* the status panel renders any account row, the system **shall** provide that row with at
least one hover response and one tooltip, **including** rows that offer no switch target (the active
account, a dropped connection). `Origin: user-stated` ("icons don't have their hover elements properly
visible"). `Ratification: n/a`.

**R-5** — *Where* the panel mock scopes a tooltip to a specific element, the system **shall** scope the
Swift tooltip to that same element rather than to an enclosing container.
`Origin: AI-inferred-expansion`. `Ratification: pending-user`.

**R-11** — *When* R-5's tooltip scope narrows from row to chip, the system **shall not** leave the row
body without any tooltip. `Origin: AI-inferred-expansion (premortem P5)`. `Ratification: pending-user`.

### PresentationAxis

**R-3a** — The system **shall** measure the *as-shipped* resting contrast of the swap chip before any
remediation is designed. **The 2.10:1 figure in the source findings is the mock's `--text-3` token, not a
measurement of SwiftUI's `.tertiary` as rendered on the panel's vibrancy** — the codebase itself only
claims "≈" between them (`StatusPanelFormat.swift:482-483`). The shipped value is **unknown**.
`Origin: AI-inferred-expansion`. `Ratification: pending-user`.

**R-3b** — *Where* an axis carries the sole at-rest indication that an element is actionable, the system
**shall** either (a) render it through a numerically pinned token whose contrast is asserted ≥ 3.0:1
(WCAG 1.4.11), **or** (b) record an explicit, rationale-bearing decision to retain the system tint and
accept it as unassertable. `Origin: user-stated symptom, AI-inferred threshold`. `Ratification: pending-user`.

> **RETRACTED — this constraint was wrong three ways** (2026-07-30, § 13.3). It read: raising resting
> toward 3.0 compresses the rest→armed delta, so both ends must move. In fact (i) `--text-2` is a **text**
> token tuned for AA 4.5:1, not a non-text ceiling — the chip is a 1.4.11 non-text component and armed has
> headroom to full ink; (ii) resting raised to 3.0 against armed 4.53 already gives a **1.51× step**,
> clearing the ≥1.3× floor, so armed need not move and the mock edit touches **one** token; (iii) the hover
> response has a second, independently-gated channel — `RowSwitchButtonStyle.wash` at ΔL\* **+3.21** vs
> +0.00 on a control — so the affordance cannot "disappear entirely" from a chip-token change.
>
> **What replaces it.** The real constraint is *representational*: `.tertiary` is a
> `HierarchicalShapeStyle` whose `Resolved == Never`, so it can never be read numerically on any macOS
> version, and the deployment target (macOS 13.0) has no `resolve` API at all. Option (a) was therefore
> never "instrument the same visual" — any pinning is a redesign. Use `PanelTint.asset(String)`
> (`StatusPanelFormat.swift:671`), which keeps OS-driven light/dark **and** declared high-contrast variants
> while being numerically readable in tests via `assetRGB`.

**R-6** — The system **shall** determine whether SwiftUI surfaces `.help()` on a `.disabled()` `Button`.
If it does not, `switchBlockedText` is unreachable on every blocked row and R-4 grows a blocked-row branch.
This is a platform fact settled by a live probe, not by reading code. `Origin: AI-inferred-expansion`.
`Ratification: pending-user`.

### AppIconAsset

**R-2** — *When* the icon pipeline emits app-icon raster assets, the system **shall** inset the icon body
within its canvas to the macOS app-icon grid, leaving transparent margin, rather than emitting a
full-bleed canvas with a baked corner radius. `Origin: user-stated` ("our icon is different").
`Ratification: n/a`.

**R-2a** — *Before* R-2 fixes an inset value, the system **shall** ground the grid against an
Apple-published source. The ~81–83 % figure is inferred from three peer apps (Docker 81.2, Calculator 82.8,
Notes 82.8) — sufficient to establish *that* ours is wrong at 100.0 %, **insufficient** to establish what
right is. `Origin: AI-inferred-expansion (premortem P3)`. `Ratification: pending-user`.

### BuildReference

**R-7** — *Where* a PanelElement has an axis no BuildReference authors, the system **shall** either author
that axis into the reference or record the axis as deliberately undecided.
`Origin: enrichment-expanded`. `Ratification: pending-user`.

**R-8** — *When* a new PanelElement is authored, the system **shall** require its five axes to be
explicitly dispositioned (decided, or deliberately deferred with rationale) before it ships. The mechanism
**shall attach to an already-enforced moment** in the existing pipeline — not ship as a standalone
document. `Origin: enrichment-expanded, promoted by Phase 0 prevention framing`. `Ratification: pending-user`.

**R-9** — *When* the panel mock is amended, the system **shall** treat the amendment as a fleet-wide
render-parity re-baseline. `design/build-comparison.py` slices the mock **live** — there is no stored
golden — so amending one token moves every panel frame's baseline simultaneously, and an unrelated
regression can ride in under the same re-baseline. `Origin: AI-inferred-expansion (premortem P1)`.
`Ratification: pending-user`.

**R-10** — *When* R-1 places the expiry value, the placement **shall** be a design decision, not a
mechanical copy of `UsageMeter`'s four-cell grid. Two facts make the copy wrong: expiry has **no
percentage**, so inheriting the 40 pt percent cell leaves a permanent empty gap; and `expiryLineCell`
returns a **variable-width bracketed** form — `[cell]` when the expiry is within the warning horizon
(#934/#935) — so a fixed right-aligned cell must accommodate the bracket without clipping the semantic
mark. `Origin: AI-inferred-expansion (premortem P4)`. `Ratification: pending-user`.

## 4. Acceptance Criteria (GWT + BUT NOT)

**AC-1 (R-1, R-10)** — *Given* a rendered panel with ≥ 1 account whose credential has an expiry,
*When* the expiry line and the meter lines are measured on the same capture, *Then* the expiry value's
cell edge is horizontally consistent with the reset-duration cell edge.
**BUT NOT** by inheriting a percent cell expiry does not populate; **BUT NOT** by clipping or hiding the
`[…]` within-horizon bracket; **BUT NOT** by breaking the CLI's `ACCOUNT SESSION% RESET WEEKLY% RESET
EXPIRY AUTH` column parity (R-2 of the CLI/panel parity contract).

**AC-2 (R-2, R-2a)** — *Given* the emitted `AppIcon.appiconset` at any size, *When* the opaque-content
bounding box is measured as a fraction of canvas, *Then* it matches the Apple-published grid within
tolerance. **BUT NOT** with the value sourced only from peer-app measurement; **BUT NOT** by retaining the
baked 22.4 % corner radius on top of the system's own mask; **BUT NOT** at only some sizes — all of
16→1024 conform or the change is incomplete.

**AC-3 (R-3a, R-3b)** — *Given* the swap chip in its resting state, *When* its as-shipped contrast against
the composited row background is measured, *Then* a recorded number exists and either clears 3.0:1 or is
covered by a written accept-decision. **BUT NOT** satisfied by the mock's token value standing in for the
shipped render; **BUT NOT** by a relational assertion (`armed > resting`) — that is the gate which already
passes; **BUT NOT** by raising resting so far that the rest→armed step becomes imperceptible.

**AC-4 (R-4, R-6, R-11)** — *Given* each of the ten row states in § 7, *When* the row is hovered,
*Then* the state's matrix cell for hover-response and tooltip is non-empty.
**BUT NOT** leaving the row body tooltip-less after narrowing scope to the chip; **BUT NOT** assuming
`.help()` surfaces on a disabled Button without R-6's probe.

**AC-5 (R-8)** — *Given* a newly authored PanelElement, *When* it reaches the moment R-8's mechanism
attaches to, *Then* an un-dispositioned axis is surfaced.
**BUT NOT** as a document with no enforcement point — a checklist that gates nothing is ceremony and P2
predicts it fails silently; **BUT NOT** by adding a new CI job when an existing enforced moment will carry it.

**AC-6 (R-9)** — *Given* a commit amending `menubar-preview.html`, *When* it lands, *Then* the panel-frame
re-baseline it causes is acknowledged. **BUT NOT** bundled with an unrelated panel change in the same commit.

## 5. Quality Attributes (Planguage)

```
TAG:     RestingAffordanceContrast
SCALE:   WCAG 2.x contrast ratio, resting swap-chip glyph vs its composited row background, as shipped
METER:   absolute assertion in the menubar test target, reusing the existing #445 contrast helper
         (Tests/StatusPanelFormatTests.swift) via its raw-sRGB init for composited results
MUST:    >= 3.0   (WCAG 1.4.11, non-text UI component)
GOAL:    >= 3.0
STRETCH: >= 3.5 while the armed state retains a >= 1.3x further step
PAST:    unknown as shipped; mock token composites to 2.10 (proxy only — see R-3a)
```

```
TAG:     AxisCoverage
SCALE:   fraction of (PanelElement, PresentationAxis) pairs that are BOTH decided by a BuildReference
         AND asserted by a PresentationGate
METER:   enumeration at R-8's attachment point
MUST:    every NEW element at 5/5 dispositioned
GOAL:    100% for elements in the panel mock's authored set
PAST:    uncomputed — nothing computes either side of the relation today
```

```
TAG:     IconGridConformance
SCALE:   opaque-content bounding box as a percentage of canvas, per emitted raster size
METER:   pixel measurement across all AppIcon.appiconset sizes
MUST:    within tolerance of the Apple-published grid at every size
PAST:    100.0% at every size (full-bleed); peers measure 81.2 / 82.8 / 82.8
```

## 5b. Feature Completeness

| Feature | Verdict | Gap |
|---|---|---|
| Expiry column placement (R-1, R-10) | ✅ **DELIVERED** 2026-07-30 | ~~Target placement undecided~~ — decided and shipped as issue #951, commit `c5f851d`. |
| App-icon grid (R-2, R-2a) | ✅ **DELIVERED** 2026-07-30 | ~~Inset value ungrounded (R-2a)~~ — grounded and shipped as issue #952, commit `12ee1c4`. **Still open**: the grid has no *gate* (issue #991), and whether an Icon Composer `.icon` asset ships alongside (D5). |
| Resting chip contrast (R-3a, R-3b) | ✅ **DELIVERED** 2026-07-30 | ~~As-shipped value unmeasured~~ — issue #949 measured it on a built panel (1.91:1 light / 2.70:1 dark, 0 of 243 / 433 chip pixels clearing 3:1); issue #956 then shipped a four-variant `SwapChipResting` colour set at **3.34:1 both appearances**, commit `0ab82fc`. The product decision resolved to design § 4.4 option (d). |
| Row affordance coverage (R-4, R-11) | **NEAR-COMPLETE** ⟵ *amended* | **Was INCOMPLETE on a false premise** — see § 13.2. The mock **does** author the active row (28 `.acct active`) and the blocked copy **verbatim** ×4; the auth-glyph omission is a deliberate 42-vs-0 `title=` pattern, not silence. Remaining gap is **one** state (blocked) and a *treatment* for the health glyph. |
| Tooltip scope (R-5) | **COMPLETE** | Reference is present and unambiguous (`menubar-preview.html:742`). Conform, subject to R-11. |
| Disabled-Button `.help()` (R-6) | **COMPLETE** as a spike | Answerable by one live probe. |
| Reference authoring (R-7) | **INCOMPLETE** | Depends on R-1/R-4 decisions — you cannot author a frame for an undecided target. |
| Axis disposition mechanism (R-8) | **INCOMPLETE** | Attachment point not yet identified. This is the gating unknown; without it R-8 is a document. |
| Re-baseline discipline (R-9) | **NEAR-COMPLETE** | A `Panel-Goldens-Rebaselined:` trailer gate already exists for the golden PNGs; whether it covers a *mock* edit is unverified. |

## 6. Success Criteria

**North Star** — share of panel `(element, axis)` pairs that are both **decided** and **asserted**.

**Leading indicators**
- An absolute resting-contrast assertion exists for the chip (binary). GOAL: present.
- Count of `(element, axis)` pairs with an authored reference. GOAL: rising; MUST: new elements at 5/5.
- R-8's mechanism has a named, already-enforced attachment point. GOAL: named, not "documented".

**Lagging indicator**
- Presentation defects in the covered axis set discovered by human screenshot inspection.
  PAST: 6 in one sitting. GOAL: 0.

**Decision gate** — if a **7th** instance of this root cause surfaces on a surface R-8 was supposed to
cover, R-8's mechanism has failed as designed; escalate it from advisory to an enforced gate rather than
re-writing the checklist.

## 7. State Matrix — Account Row Affordance Coverage

The matrix that makes branch (i) visible. `?` = not established by evidence; that is the finding.

| # | Row state | Resting indicator ≥ 3:1 | Hover response | Tooltip |
|---|---|---|---|---|
| 1 | Resting, switch-target | **?** (R-3a) | ✅ measured +57.9 % chip, −7.50 lum wash | ✅ row-scoped (diverges from mock — R-5) |
| 2 | Hover, switch-target | n/a | ✅ | ✅ |
| 3 | Armed / pressed | mock token 4.53:1 | ✅ | ✅ |
| 4 | Active account (no target) | — none | ❌ none | ❌ **none** (R-4) |
| 5 | Blocked (`blockReason != nil`) | — | ❌ disabled | **?** (R-6) |
| 6 | Swap pending (`phase.isPending`) | — | ❌ disabled | **?** (R-6) |
| 7 | Dropped connection / degraded | — | ❌ none | ❌ none (R-4) |
| 8 | Blind / DEGRADED (#485) | eye.slash + dashed meter | ❌ none | ❌ none |
| 9 | Credential fault (expiry horizon) | `[…]` bracket mark | ❌ none | ❌ none |
| 10 | Empty / loading roster | — | n/a | n/a |

Auth glyph, all states: `.accessibilityHidden(true)`, no `.help()`, no hover treatment of its own — its
ink-mass *drops* 12.6 % under hover because it merely rides the row wash.

> **`.accessibilityHidden(true)` is CORRECT — do not read this row as a defect** (amended 2026-07-30,
> § 13.2). The row collapses to **one** accessibility element (`StatusPanelRoster.swift:283-287`:
> `.accessibilityElement(children: .ignore)` + `.accessibilityLabel`), and `authSpoken`
> (`StatusPanelFormat.swift:1864-1959`) speaks every auth verdict **plus its remedy** in words. Hiding the
> glyph is required to avoid double-reading. A VoiceOver user currently gets **more** than a sighted one.
>
> **Amended dispositions.** Of the ten states, exactly **one** — state 5, blocked — carries a load-bearing
> reason no resting render conveys, and its remedy is **persistent inline text**, not a tooltip (a
> load-bearing fact must not be hover-only). States 3, 6, 7, 8, 9 are **correctly** empty (transient, or
> already carried by a shape/text cue); state 4's silence is correct because "active" is carried positively
> by the filled-vs-ring dot and the accent row fill. The `?`/`❌` marks below record *absence*, not *debt*.
> State 4's real gap is **#839** (non-interactive rows publish `AXUnknown`, so the active row is missing
> from every VoiceOver rotor) — strictly larger than any tooltip, and already tracked.

## 8. Assumption Registry

| ID | Assumption | Importance | Evidence | Verdict | Cheapest test | Hedge while open |
|---|---|---|---|---|---|---|
| A1 | The macOS icon grid is a platform contract at ~81–83 % | HIGH | 3 peer apps measured; **no Apple doc read** | **test** | Read Apple's published app-icon grid | Assert only that 100 % is wrong; do not pin a number |
| A2 | `.help()` does not surface on a disabled Button | MED | none — unverified | **test** | One live hover probe (R-6) | Treat blocked-row copy as possibly unreachable |
| A3 | Amending `--text-3` re-baselines every panel frame | MED | `build-comparison.py` slices mock live (auto-memory) | **decide** | — | Isolate mock edits in their own commit |
| ~~A4~~ | ~~The shipped `.tertiary` resting contrast ≈ the mock's 2.10:1~~ | **HIGH** | **RESOLVED — REFUTED** | ~~test~~ | **#949 measured it: 1.91:1 light / 2.70:1 dark on a built panel** | The mock's 2.10:1 was never the shipped value — it is a ceiling no pixel reached. Assumption discharged 2026-07-30 |
| ~~A5~~ | ~~Pinning chip tokens is safe for Increase Contrast / Dark Mode~~ | HIGH | **RESOLVED — CONFIRMED** | ~~test~~ | **#956 shipped a four-variant colour set** (light/dark + both Increase-Contrast) at 3.34:1 | Safe *as an asset colour set*, which is why option (d) beat (a): a raw pinned `Color` would have lost the Increase-Contrast escalation (#832) |
| A6 | An axis checklist changes behaviour | HIGH | P2 says no, if unenforced | **decide** | Identify an enforced attachment point first | Do not ship as a standalone doc |
| A7 | Six findings are one root cause | **HIGH** ⟵ *amended* | 2 mechanism-pairs + #763 third instance — **medium evidence, no test** | **test** ⟵ *was `decided`* | Run the proposed detector against the **corpse**: a diagnosis is validated when the detector would have **FAILED** the actual defective artifacts, not when new work passes it | Treat branch (ii) as a **separate** mechanism (§ 13.1); do not let one prevention item claim both |

### Premortem (Phase 0, de-anchored — findings the ISO sweep cannot enumerate)

*Six months out, all six findings fixed, and the same defect class recurred. What happened?*

- **P1** → R-9. The F-3 fix amended the mock; because the harness slices it live, every frame re-baselined
  at once and an unrelated regression rode in unnoticed.
- **P2** → R-8, AC-5. The checklist shipped as a document nobody reads; a new element shipped with
  un-decided axes anyway.
- **P3** → R-2a. The icon was inset to a peer-derived 81 %, Apple's actual grid differs, and the icon is
  now wrong in a *new* way that is harder to argue about.
- **P4** → R-10. The expiry row inherited `UsageMeter`'s grid, gained a permanent empty percent gap, and
  the "fix" read as more broken than the defect.
- **P5** → R-11. The tooltip moved to the chip, conforming to the mock — and the row body, which is most
  of the hover target, lost its tooltip entirely.
- **P6** → out of scope, recorded. Nothing was re-shot, so `menubar-hero.png` (already stale) keeps
  misrepresenting the panel in the README.

## 9. Cross-Cutting & Non-Functional Concerns

**9.1 Security** — N/A. No requirement touches credentials, the AF_UNIX transport, peer auth, or the
keychain. R-1 renders an expiry *duration*, never token material.

**9.2 Compliance & Regulatory** — **In scope and load-bearing.** WCAG 2.x **1.4.11** (non-text UI component
contrast, 3:1) is R-3b's normative basis; **AA 4.5:1** applies to the panel's text and is already asserted
for the palette (#445, 13 call sites). No legal obligation is claimed — this is a self-adopted standard the
repo already enforces elsewhere, which is precisely why the chip's exemption is a gap rather than a choice.

**9.3 Reliability & Observability** — The **gates are** the observability here, and their weakness is the
finding: `PanelInteractionStateTests` asserts a relation (`armed > resting`) and a magnitude floor
(`chipOnlyFloor = 0.001` at `deltaChannelThreshold = 4/255`), never an absolute value — so a
below-floor resting state passes green. `panel-goldens` is additionally a deliberately **soft** gate (every
step `continue-on-error`), so it can never report drift. No runtime telemetry exists for panel
presentation, and none is proposed (§ 1b).

**9.4 Performance & Scalability** — N/A. All requirements are static presentation values (column widths,
color tokens, icon insets, tooltip attachment). None affects render cost, poll cadence, or the daemon.

**9.5 Operational** — N/A for the running system. One build-pipeline consequence: R-2 changes
`brand/generate.sh` output, which feeds `AppIcon.appiconset` → `Assets.car` via `actool`. Per
`symbolset-fills-never-strokes`, `actool` reinterpretation is only catchable on-device — so R-2's
verification must measure the **emitted** raster, not the SVG source.

**9.6 Lifecycle** — R-8 is the lifecycle requirement: it governs the authoring moment for every *future*
PanelElement, which is what distinguishes this scope from instance-patching. R-7 governs reference upkeep.
R-9 governs amendment discipline. These three are the durable half; R-1…R-6 are the six live instances.

> **Instance status as of 2026-07-31** (this PRD was authored 2026-07-30). Three of the six have
> shipped: **R-1/R-10** (#951, `c5f851d`), **R-2/R-2a** (#952, `12ee1c4`), **R-3a/R-3b** (#949 measured
> 1.91:1 light / 2.70:1 dark; #956 shipped a `SwapChipResting` colour set at 3.34:1, `0ab82fc`). Still
> live: **R-4/R-11** (#955), **R-5** (#953), **R-6** (#950). **The durable half — R-7 (#957) and R-8
> (#954) — has not been touched, and that is the load-bearing gap.** Three instances closing is exactly
> the outcome this PRD argues is insufficient: § 1's whole claim is that instance-patching predicts a
> fourth surface. Do not read the shipped fixes as progress against the mechanism.

## 10. Source Traceability

| Requirement | Source | Reliability |
|---|---|---|
| R-1, R-10 | Measured: expiry ink x≈82–121 vs reset x≈331–367, live capture at `83a275d`; `StatusPanelRoster.swift:592-606`; `expiryLineCell` bracket form | A — self-verifying |
| R-2, R-2a | Measured: 4 apps' opaque bbox at 128×128; `brand/src/icon.svg:9`; `generate.sh:186-203`; `find . -name "*.icon"` → empty on macOS 26.5.2 | A — self-verifying (ours); C — inferred (the grid) |
| R-3a, R-3b | `menubar-preview.html:154-156,253-255`; `StatusPanelFormat.swift:482-483,667`; `StatusPanelFormatTests.swift:2265-2300` + 13 call sites; `PanelInteractionStateTests.swift:135-147,319-353` | A for the tokens and gate; **premise unverified** for the shipped render |
| R-4, R-11 | `StatusPanelRoster.swift:283-287,477-526`; live capture: auth glyph ink −12.6 % under hover | A — self-verifying |
| R-5 | `menubar-preview.html:742` vs `StatusPanelRoster.swift:276` | A — self-verifying |
| R-6 | `StatusPanelRoster.swift:275-276` | **unverified** — platform behaviour, needs a probe |
| R-7, R-8 | Synthesis across F-1/F-2/F-4 + `83a275d` (#763/#947) precedent | C — authored analysis |
| R-9 | `design/build-comparison.py` live-slice behaviour; auto-memory `menubar-render-parity-harness` | B — prior verified session |
| Localization N/A | `find` → no `.lproj`/`.xcstrings`; grep → no `NSLocalizedString` | A — self-verifying |

**Full evidence record**: `.tmp/investigate-panel-visual-defects-roadmap.md`.
**Captures**: `.tmp/panelcaps/live-panel-HEAD.png`, `.tmp/panelcaps/live-panel-HOVER-op.png` (390×884, 1:1,
Release build at `83a275d`, real 6-account roster).

> ⚠️ **Both paths are local scratch — `.tmp/` is gitignored and these do NOT resolve in a fresh clone
> or an isolated worktree.** They are provenance, not a dereferenceable source. Consequence, stated
> plainly: the figures in this section are **not independently re-verifiable from the repo alone** —
> to re-check them you must re-capture (build Release at the cited commit and re-run the panel render),
> not open a file. The numbers that *have* since been re-established on committed ground are the
> chip-contrast ones, via issue #949 and the gate that shipped with #956 (`0ab82fc`) — prefer those.
> This is a known gap, not an oversight: it is the same dangling-reference class this PRD's own R-7
> exists to prevent, one level up, and it applies to the `source:` field in the frontmatter too.

## 11. Related Work — Generalize, Do Not Duplicate

| Issue | Relationship |
|---|---|
| **#903** | "The mock authors an armed chip rule it never instantiates" — ratifies a relation, never an appearance. R-3b is the absolute-contrast half of the same gap. **Decide together.** |
| **#571** | No visual oracle for the panel — the parent of R-1's and R-4's absent-reference rows; R-7 is its concrete instance. |
| **#901** | Panel accessibility sibling. |
| **#830 / #831** | Meter-tint contrast. Neither covers the chip; R-3 is **not** a duplicate. |
| **#763 / #947** (`83a275d`) | States this root cause verbatim for the Settings window — the in-repo precedent that makes this a pattern. |
| **#884 / PR #922** (`23fecc4`) | Introduced the F-1 defect, 2026-07-29. |
| **#934 / #935** | The `[…]` within-horizon expiry bracket that R-10 must not clip. |

## 12. Definition-of-Ready Verdict

**`dor_status: passed-with-findings`**

| Check | Result |
|---|---|
| 1. Validated problem statement | **PASS** — § 1, traced to the Phase 0 framing pass; two alternative frames explicitly rejected. |
| 2. Explicit out-of-scope | **PASS** — § 1b, 7 declarations + appetite + 2 circuit breakers. |
| 3. Success & telemetry metrics | **PASS** — § 6: 3 leading, 1 lagging, 1 decision gate. |
| 4. Cross-cutting & NFR | **PASS** — § 9.1–9.6 all present; three are `N/A` with rationale. |
| 5. Feature completeness verdict | **PASS** — § 5b, all 9 features verdicted; 4 INCOMPLETE with gaps stated. |
| 6. Requirement provenance | **FINDING** — see below. |

**The finding, stated plainly.** Of 13 requirements, 4 trace to the user's literal question (R-1, R-2, R-4,
and R-3b's symptom); **9 are pipeline-authored and not user-ratified** (R-3a, R-5, R-6, R-7, R-8, R-9, R-10,
R-11, R-2a). Check 6 nominally fails on unratified pipeline-authored requirements. I have recorded
`passed-with-findings` rather than `failed` because **seven** of the nine are reversible tracker-scoping
calls, and the standing preference is for me to decide reversible calls rather than block on them.
**CORRECTION (2026-07-30): the "every one" claim was false for exactly two.** R-8 proposes a **CI gate**
and R-9 a **commit-trailer obligation** — a gate every future contributor must satisfy and a trailer every
future mock edit must carry are the *least* reversible items in this scope, not reversible ones. Both were
put to the maintainer: **R-8 was reshaped** (the CI gate is cut; a scope-time DoR question replaces it) and
**R-9 was CUT** (`check-panel-golden-rebaseline.sh` already meets that hazard). The remaining seven stand as
reversible. I have
**not** represented them as ratified: each carries `Ratification: pending-user`, and Stage 3 must carry that
tag into the issue bodies so no executor reads an AI addition as a stakeholder requirement.

Three of the nine deserve explicit attention on review, because they are the ones a reasonable stakeholder
might cut: **R-8** (a process change), **R-7** (authoring reference frames), and **R-2a** (grounding the
icon grid before fixing it — which slows R-2 down).

---

## 13. Post-Council Amendments (2026-07-30)

A `/council` pass ran after this PRD was written. It **corrected five load-bearing claims** made above.
Each correction is inlined at its own site; this section states them once, with the evidence.

### 13.1 The root cause is partly overfitted — branch (ii) is a different mechanism

§ 1 frames all six findings under one mechanism ("coverage asserted per element against whatever the
reference authors"). Branch **(i)** — *the oracle is silent for the axis* — holds: F-1, F-2 and F-4 all
fail because no reference authors the axis, and #763's Settings-window precedent is the same shape.

Branch **(ii)** does not. F-3 (chip contrast) fails because the value is **unrepresentable** — `.tertiary`
is a `HierarchicalShapeStyle` with `Resolved == Never`, so no gate *could* read it however well the axis
was enumerated. F-5 (tooltip scope) fails because the reference **was** authored and the implementation
**diverged** from it.

**The tell is remedy-disjointness.** A shared root cause implies a shared remedy. Enumerating axes fixes
(i) and does nothing for (ii): you cannot enumerate your way to a readable `.tertiary`, and F-5 needed
conformance to an oracle that already existed. Two mechanisms wearing one label.

**Consequence.** A7 moves from `decided` to `test`, and its importance from MED to HIGH — it is the
assumption R-7 and R-8 both rest on. The validating test is a **necropsy**: run the proposed detector
against the *corpse*. A detector that the actual defective artifacts **pass** falsifies the diagnosis; only
one that would have **failed** them validates it. This was not run before R-8 was filed, and running it
afterwards is what cut R-8's original CI-gate design (§ 13.5).

### 13.2 "The mock authors neither" was wrong on both halves

§ 5b and the Design Reference Register both recorded that the mock authors neither the auth-glyph treatment
nor the active-row state. Measured against `apps/menubar/design/menubar-preview.html`:

| Claim | Measured |
|---|---|
| mock does not author the active row | **28** `.acct active` occurrences; its `.id-trail` is *deliberately* chip-free |
| mock does not author blocked-row copy | authored **verbatim**: `title="No viable swap target — weekly-exhausted"` ×4 (`:2433, 2445, 2496, 2508`) |
| mock is silent on the auth glyph | `title=` on `.rowact` **42×**, on `.health` **0×** — a consistent pattern is a decision, not silence |

There is materially **more** oracle here than this PRD granted. The only genuinely unauthored axis is a
hover *treatment* for the health glyph.

Separately: `.accessibilityHidden(true)` on the auth glyphs is **correct**, not a gap — see the § 7
blockquote. The earlier framing had it inverted.

### 13.3 The coupled-contrast constraint was wrong three ways

Retracted in full at R-3b. Summary: `--text-2` is a *text* token at the *text* threshold and does not cap a
non-text component; resting alone at 3.0 against armed 4.53 clears the step floor at 1.51×; and the hover
response has a second independently-gated channel (`RowSwitchButtonStyle.wash`, ΔL\* +3.21). The real
constraint is representational, not perceptual.

**New measurement** (live 1:1 capture): resting chip **1.93:1** (rows 4, 5) to **2.06:1** (row 1); **0 of
70 chip pixels clear 3:1**. The mock's nominal 2.10:1 is a ceiling no pixel reaches. This partly answers
R-3a — but on one wallpaper, in light appearance only. ~~Dark is unmeasured.~~

> **SUPERSEDED 2026-07-30 by issue #949**, which completed exactly the gap this paragraph names.
> Full built-panel measurement: **1.91:1 light / 2.70:1 dark**, with **0 of 243 light and 0 of 433 dark**
> chip pixels clearing 3:1; armed measured **4.73:1** and needed no change. R-3a is fully answered, and
> R-3b shipped in #956 (`0ab82fc`). The narrower figures above are retained as the earlier partial
> reading — note the dark appearance came in *higher* (2.70) than the light (1.91), so the light-only
> capture was not merely incomplete, it was the pessimistic half.

### 13.4 Usage context the PRD never had — and its effect on priority

The maintainer uses the per-row swap chip **rarely** and the panel-footer Swap **never**. The real swap
path is `sessiometer use <account>` on the CLI. R-3a/R-3b therefore describe a **low-priority** defect:
worth a cheap fix, not worth a token migration or a design deliberation of its own.

Two consequences that were not in scope when this PRD was written:

- A **new defect** outranks it: the resting swap affordance and its own negation are near-interchangeable
  (`arrow.left.arrow.right` vs `nosign` — same slot, same tint, same ~13 pt cap-height, both ring-shaped).
  Contrast cannot fix that. Filed separately.
- A **feature request**: advance to the next account in the swap chain from the CLI without naming it —
  *"It would be nice to have a same feature in CLI 'to advance to next in swap chain'"*. Filed separately.

### 13.5 R-8 reshaped, R-9 cut

**R-8** — the *requirement* stands (a new element's axes must be dispositioned before it ships). Its
originally-designed **mechanism** — a CI trailer gate triggered by a new `struct … : View` — was cut by
necropsy: it catches **1 of 5** defects across **1 of 3** surfaces, and misses `authView`
(`StatusPanelRoster.swift:477`), a computed property carrying 2 of the 6 defects. Declaration shapes in
that file run **40 computed vars to 32 structs**, so the trigger keys on the *minority* idiom. Replaced,
at the maintainer's direction, with a **scope-time DoR question** — earlier, cheaper, and it reaches all
three surfaces because it is asked of the work item, not of the diff.

**R-9** — **CUT.** `scripts/check-panel-golden-rebaseline.sh` already enforces the re-baseline hazard for
committed goldens (#754). The mock-edit case it worried about is covered by the same discipline.

### 13.6 Ratification status after the council

Of the nine pipeline-authored requirements: **R-8 reshaped, R-9 cut** by maintainer decision; the other
seven remain `Ratification: pending-user` and reversible. `dor_status` stays `passed-with-findings`.
