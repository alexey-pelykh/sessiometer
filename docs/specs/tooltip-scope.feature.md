<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are XCTest (apps/menubar/Tests/) and the
scripts/check-*.sh shell gates. These scenarios exist to pin each acceptance criterion in scenario form
and bind it to an ACC capability from the Master Test Plan
(docs/design/panel-presentation-reference-coverage-solution-design.md § 11), so a test author has a 1:1
target. Do not read a written scenario as a passing test.
-->

# Feature: a tooltip is scoped to the element it describes

Tracked as **issue #953**. Requirements: PRD R-5, R-11.

## Scenario: the switch tooltip belongs to the chip  · Cap-3.2

    Given the design mock scopes title="Switch to this account" to the chip
    When a switch-target row is hovered on the chip
    Then the switch tooltip appears

## Scenario: the health glyph does not answer with the switch copy  · Cap-3.2

    Given a switch-target row whose auth state is healthy
    When the health glyph is hovered
    Then the switch copy does not appear
    And either an auth-state tooltip appears, or the glyph's tooltip-less state is a recorded decision

## Scenario: the row body still explains itself  · Cap-3.1

    Given the switch tooltip has been narrowed from the row to the chip
    And the row body remains most of the hover target
    When the row body is hovered away from the chip and the glyph
    Then some tooltip appears, or the absence is a recorded decision
    But not silently nothing
