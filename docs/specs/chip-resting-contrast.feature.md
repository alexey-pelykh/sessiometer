<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are XCTest (apps/menubar/Tests/) and the
scripts/check-*.sh shell gates. These scenarios exist to pin each acceptance criterion in scenario form
and bind it to an ACC capability from the Master Test Plan
(docs/design/panel-presentation-reference-coverage-solution-design.md § 11), so a test author has a 1:1
target. Do not read a written scenario as a passing test.
-->

# Feature: the resting affordance meets the non-text contrast floor

Tracked as **issue #956** — ✅ **DELIVERED 2026-07-30**, commit `0ab82fc` (PR #995); issue CLOSED.
Requirements: PRD R-3a, R-3b.

> ~~**Blocked**: issue #949 must measure the as-shipped resting contrast first.~~ — **DISCHARGED**.
> #949 measured it on a built panel: **1.91:1 light / 2.70:1 dark**, 0 of 243 light and 0 of 433 dark
> chip pixels clearing the 3:1 floor; armed already passed at 4.73:1. #956 then shipped design § 4.4
> **option (d)** — a four-variant `SwapChipResting` colour set (#828282 light / #808080 dark plus the
> Increase-Contrast pair) at **3.34:1 in both appearances**. `.armed` deliberately did not move; the
> shipped rest→armed step is 1.42×. Retained as the acceptance record, not as open work.

## Scenario: the shipped resting contrast is known  · Cap-2.1

    Given the widely-quoted 2.10:1 is the design mock's token, hand-composited over a flat background
    And the Swift renders SwiftUI's .tertiary over the panel's vibrancy
    And the codebase claims only an approximate relation between them
    When the resting chip's contrast is measured on a real render of the app
    Then a recorded number exists for the shipped surface
    But not substituted by the mock's token value

## Scenario: the resting affordance clears the WCAG 1.4.11 floor  · Cap-2.1

    Given the resting chip is the only at-rest indication that a row is actionable
    When its contrast against the composited row background is asserted
    Then it is at least 3.0:1
    Or a knowing deviation is explicitly recorded, together with the fact that the
      capability then becomes permanently unassertable

## Scenario: a perceptible rest-to-armed step survives the fix  · Cap-2.2

    Given the armed token measures 4.53:1
    And raising resting toward 3.0 compresses the very delta the existing gate measures
    When resting is raised
    Then armed is raised with it
    And a step of at least 1.3x remains between them
    But not a fix that clears 3.0 by flattening the hover response

## Scenario: the assertion is absolute, not relational  · Cap-2.1

    Given the existing gate asserts armedInk > restingInk and a magnitude floor of 0.001 at 4/255
    And a below-floor resting state passes that gate green
    When the new assertion is written
    Then it asserts an absolute contrast ratio
    But not merely that armed exceeds resting

## Scenario: pinned tokens survive system appearance adaptation  · Cap-2.1

    Given option (a) replaces SwiftUI's system tints with pinned numeric tokens
    And system tints adapt to Increase Contrast and appearance changes while pinned ones do not
    When pinned tokens are used
    Then the chip is verified legible under Increase Contrast and in both appearances
    Or option (b)'s non-colour channel is chosen instead, which keeps system adaptation
