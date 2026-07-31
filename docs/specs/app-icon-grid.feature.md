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

> Retained as the acceptance record, not as open work. **One thing here is still live**: the fix shipped
> without a gate, so nothing catches a regression — that is issue **#991**, still OPEN. The scenarios
> below are the specification that gate should assert.

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

## Scenario: the baked corner radius is gone from the app-icon path  · Cap-4.1

    Given macOS applies its own mask to the app icon
    When the emitted app-icon raster is inspected
    Then it carries no additional baked corner radius of its own

## Scenario: the other three icon.svg consumers are untouched  · Cap-4.2

    Given brand/src/icon.svg feeds four consumers
    And only the AppIcon raster set wants the inset
    When the inset stage is added
    Then apple-touch-icon.png is still full-bleed, per Apple's touch-icon convention
    And the four derived status-colour variants are byte-identical to before
    But not achieved by insetting inside brand/src/icon.svg itself
