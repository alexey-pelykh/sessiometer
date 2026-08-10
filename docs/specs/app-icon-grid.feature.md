<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are XCTest (apps/menubar/Tests/) and the
scripts/check-*.sh shell gates. These scenarios exist to pin each acceptance criterion in scenario form
and bind it to an ACC capability from the Master Test Plan
(docs/design/panel-presentation-reference-coverage-solution-design.md § 11), so a test author has a 1:1
target. Do not read a written scenario as a passing test.
-->

# Feature: the app icon conforms to the macOS icon grid

Tracked as **issue #952** — ✅ **DELIVERED 2026-07-30**, commit `12ee1c4` (PR #992); issue CLOSED.
Requirements: PRD R-2, R-2a.

> Retained as the acceptance record, not as open work. The gate these scenarios specify is now
> **shipped** — issue **#991** CLOSED 2026-08-10, commit `2388c26` (PR #1142): `AppIconGridTests`
> measures the emitted rasters. The scenarios below are the specification it asserts against, and the
> note under the corner-radius scenario records how far that assertion actually reaches.

## Scenario: the grid value is grounded before it is applied  · Cap-4.1

    Given the ~81-83% figure was inferred from three peer applications
    And no Apple-published source has been read
    When an inset value is chosen
    Then it comes from Apple's published app-icon grid, not from peer measurement
    But if that source is not located within the circuit-breaker window
    Then the item converts to a spike rather than shipping a peer-derived guess

## Scenario: every emitted size conforms  · Cap-4.1

    Given the AppIcon.appiconset emitted by brand/generate.sh
    When the opaque-content bounding box is measured as a fraction of canvas at each size from 16 to 1024
    Then every size conforms to the grounded grid
    And the measurement is taken on the emitted raster, not on the SVG source

## Scenario: the baked corner radius survives into the app-icon raster  · Cap-4.1

    Given macOS applies no mask of its own — it is not iOS
    And the rounding therefore lives in the artwork, as it does in every peer app measured
    When the emitted app-icon raster is inspected at each declared size
    Then no corner of its own body box is fully opaque
    And where 256 divides the canvas, that box's fill stays at or below the rounded-vs-square boundary
    But not by pinning the declared 22.36 % radius

> **INVERTED 2026-08-10 (#1141).** Until this date the scenario above asked for the opposite — *"the
> baked corner radius is gone from the app-icon path"*, on the premise that *"macOS applies its own mask
> to the app icon"* — and both halves are wrong. Every peer app measured reads **alpha 0** at its own
> body-box corners: if the system masked, a peer could ship square artwork, and none does. So dropping
> `icon.svg`'s `rx="229"` ships a hard-cornered **square**, which has the *same* bounding box as the
> shipped tile and is therefore invisible to the *every emitted size conforms* scenario above. The
> radius was never the defect — it rides `APPICON_SCALE` from 229 down to **184.3** on an 824 body,
> 0.6 % under the template's 185.4; the icon read over-rounded because it was over-**sized**. #952
> shipped the correction deliberately: `brand/README.md` § "The baked `rx` stays — macOS is not iOS"
> carries the peer measurement, `brand/generate.sh` carries it at `APPICON_INSET`, and `AppIconGrid` is
> the executable form. The old text is **not** retained in scenario shape — a Gherkin block here is an
> instruction to a test author, not a record, and #991 names this file as one of its three build
> references. That risk is not hypothetical: writing #991's gate, the first corner predicate was drafted
> from the old framing and had to be inverted once the rasters were measured.
>
> **What the shipped gate actually reaches**, so this scenario is not read as more:
> `AppIconGridTests.testEveryEmittedSizeKeptItsBakedCornerRadius` runs the corner half
> (`AppIconGrid.cornerAlphas`) on all ten declared rasters and the aggregate half
> (`AppIconGrid.squareFillThreshold`) on the five whose canvas 256 divides. Both key on opacity
> **magnitude**, so a uniformly translucent square satisfies them — they hold in the one regime the
> producer generates, an opaque `rsvg-convert` pass per size (**#1148**). And the radius set they accept
> is much wider than the declared 22.36 % (**#1149**). Both are tracked; neither is corrected here.

## Scenario: the other three icon.svg consumers are untouched  · Cap-4.2

    Given brand/src/icon.svg feeds four consumers
    And only the AppIcon raster set wants the inset
    When the inset stage is added
    Then apple-touch-icon.png is still full-bleed, per Apple's touch-icon convention
    And the four derived status-colour variants are byte-identical to before
    But not achieved by insetting inside brand/src/icon.svg itself
