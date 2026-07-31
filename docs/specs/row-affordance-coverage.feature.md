<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are XCTest (apps/menubar/Tests/) and the
scripts/check-*.sh shell gates. These scenarios exist to pin each acceptance criterion in scenario form
and bind it to an ACC capability from the Master Test Plan
(docs/design/panel-presentation-reference-coverage-solution-design.md § 11), so a test author has a 1:1
target. Do not read a written scenario as a passing test.
-->

# Feature: every row state gives hover feedback

Tracked as **issue #955**. Requirements: PRD R-4, R-11.
**Blocked**: issue #950 must answer whether .help() surfaces on a disabled Button.

## Scenario Outline: each row state has a hover response and a tooltip  · Cap-3.1

    Given a panel row in state <state>
    When the row is hovered
    Then it has a hover response, or a recorded decision that it deliberately has none
    And it has a tooltip, or a recorded decision that it deliberately has none

    Examples:
      | state                          |
      | resting switch-target          |
      | armed                          |
      | active account, no target      |
      | blocked by blockReason         |
      | swap pending                   |
      | dropped connection / degraded  |
      | blind / DEGRADED               |
      | credential fault in horizon    |

## Scenario: the auth glyph has a treatment of its own  · Cap-3.1

    Given the auth glyph currently only rides the row's background wash
    And its measured ink-mass drops 12.6% under hover as a result
    When the row is hovered
    Then the glyph has a treatment of its own, or its inertness is a recorded decision

## Scenario: the glyph's meaning reaches assistive technology  · Cap-3.1

    Given all three auth-glyph variants render with accessibilityHidden(true)
    When the accessibility exposure of the glyph is decided
    Then the decision is recorded either way
    But not left hidden merely because that is the current default

## Scenario: a blocked row explains why it cannot be switched to  · Cap-3.1

    Given a row disabled by a blockReason
    When issue #950 has established whether .help() surfaces on a disabled Button
    Then the blocked-reason copy is reachable by whatever mechanism that answer implies
