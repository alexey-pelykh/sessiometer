<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are XCTest (apps/menubar/Tests/) and the
scripts/check-*.sh shell gates. These scenarios exist to pin each acceptance criterion in scenario form
and bind it to an ACC capability from the Master Test Plan
(docs/design/panel-presentation-reference-coverage-solution-design.md § 11), so a test author has a 1:1
target. Do not read a written scenario as a passing test.
-->

# Feature: the expiry value shares the right-hand value gutter

Tracked as **issue #951** — ✅ **DELIVERED 2026-07-30**, commit `c5f851d` (PR #993); issue CLOSED.
Requirements: PRD R-1, R-10.

> Retained as the acceptance record, not as open work. The scenarios below state what was required and
> remain the description of correct behaviour — do **not** read them as a to-do list.

**IA rule under test**: all fact-tier values right-align in the shared value gutter, whether or not the
fact has a bar.

## Scenario: the expiry value aligns with the reset duration  · Cap-1.1

    Given a panel rendered for an account whose credential has an expiry
    And the account's session and weekly meter lines are visible above it
    When the right edge of the expiry value's ink is measured on a 1:1 capture
    Then it is horizontally consistent with the right edge of the reset-duration ink
    But not achieved by inheriting a percent cell that expiry never populates

## Scenario: the within-horizon bracket is not clipped  · Cap-1.2

    Given an account whose expiry falls inside the warning horizon
    When the expiry line renders
    Then the value appears in its bracketed form
    And both bracket characters are fully visible
    And the value still right-aligns to the gutter

## Scenario: a long duration does not overflow the cell  · Cap-1.2

    Given an account whose expiry is far in the future, producing the longest duration string
    And that account is also inside the warning horizon, so the string is bracketed
    When the expiry line renders
    Then no character is clipped or ellipsised

## Scenario: CLI and panel agree on where expiry belongs  · Cap-1.3

    Given the CLI status table renders columns ACCOUNT SESSION% RESET WEEKLY% RESET EXPIRY AUTH
    When the panel's expiry placement is compared against it
    Then expiry sits to the right of the reset column in both surfaces
