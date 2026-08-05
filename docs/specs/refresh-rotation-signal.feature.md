<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: `rotated` is emitted only where it carries information

Tracked as **issue #1004**. Requirements: PRD R-5, R-5a. Design § 4.4, AD-3.

**Rule under test**: `classify()` computes `rotated` as `seeded_rt != after_rt`
(`src/refresh.rs:434-437`) *before* the outcome is known. A `dead` outcome sets
`after_rt = Some("")`, and an empty token never equals the seeded one — so `rotated=true` is
**guaranteed by construction** on every dead line. The fix is to make that state unrepresentable,
not merely unprinted.

## Scenario: a dead refresh emits no rotation claim  · Cap-4.1

    Given a refresh whose response clears the refresh token in place
    When the outcome is classified
    Then the outcome is dead
    And the emitted event carries no rotated value

    # The live line this comes from:
    #   outcome=dead expires_before=06:50:10Z expires_after=06:50:10Z rotated=true window_secs=0
    # window_secs=0 and expires_before == expires_after prove no exchange occurred, yet the line
    # claimed a rotation.

## Scenario: an errored refresh emits no rotation claim  · Cap-4.1

    Given a refresh that fails before a token is returned
    When the outcome is classified
    Then the emitted event carries no rotated value

## Scenario: a successful refresh still reports rotation  · Cap-4.1

    Given a refresh that returns a new refresh token differing from the seeded one
    When the outcome is classified
    Then the outcome is refreshed
    And the emitted event reports that the token rotated
    But not by removing the field from the path where it is meaningful

## Scenario: the meaningless combination is unrepresentable  · Cap-4.1

    Given the RefreshOutcome type
    When a non-refreshed outcome is constructed
    Then no rotated value can be attached to it

    # Type-level, not formatting-level: a formatting-layer suppression can be reintroduced by a
    # later emitter change; moving rotated inside the refreshed variant cannot.
