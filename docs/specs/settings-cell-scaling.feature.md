# Feature: Settings field cells scale with their own fonts

Issue #845 · PRD R-8 / R-8a · tier **T1** · **PREMISE-GATED on #971**

Example Mapping: 🟦 3 rules · 🟩 4 examples · 🟥 1 open (the premise itself)

## Rule 0 — the premise gate (must clear FIRST)

```gherkin
Scenario: the premise is confirmed before anything is implemented
  Given issue #845 asserts Settings' system-text-style fonts grow with the OS setting
    And StatusPanelTypeScale.swift:13-14 measured relative text styles as inert at all twelve classes
   When issue #971's `.regular` column is measured
   Then the premise is either CONFIRMED or REFUTED

Scenario: a refuted premise voids the implementation
  Given #971 shows Settings' fonts do NOT grow
   When this issue is revisited
   Then it is reclassified as LATENT, not live
    And its "degrades as the accessibility setting increases" framing is corrected
    And no cell-scaling code is written
    # Implementing against a refuted premise would be building on a false input
```

## Rule 1 — if confirmed: scaled font, scaled cell

```gherkin
Scenario Outline: the cell grows with the text it holds
  Given the premise is CONFIRMED
    And the Settings window renders at <sizeClass>
   When tunableFieldWidth (96) and accountLabelFieldWidth (160) are measured
   Then each has scaled by the same factor its font did, or sizes to content
    And the value in the field is readable

Examples:
  | sizeClass       |
  | large           |
  | accessibility3  |
  # "A scaled font in a fixed cell is a clipping bug, not a fix" — issue #756's own AC-2
```

## Rule 2 — the existing defect pin must be retired correctly

```gherkin
Scenario: the pin is replaced, not deleted
  Given SettingsTextMetricsTests pins this defect (green while it stands, red when fixed)
   When the defect is fixed
   Then that pin is REPLACED by a scaling sweep modelled on PanelTextMetricsTests AC-3
    And the pin is not simply removed
    # Deleting it would drop the only assertion covering this surface
```

## 🟥 Open

- **The premise** — see Rule 0. Also: `hq design-menubar.md:228` dispositions this "reference wins →
  #845", but `design-menubar.md`'s **own** register R-10 (`:316` — not this PRD's R-10, which is
  TextSizePreference storage) marks § D-UX-SETTINGS `RATIFICATION-PENDING` with this issue in its own
  backlog. The oracle is not settled ground.
