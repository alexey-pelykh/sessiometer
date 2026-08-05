<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: `status` makes the EXPIRY value's provenance legible

Tracked as **issue #1006**. Requirements: PRD R-7. Design § 4.5, AD-4.

**Rule under test**: `read_poll_clocks` (`src/daemon/snapshot_build.rs:45-53`) reads the **canonical**
item for the active account and the account's **stash** for parked ones. That asymmetry is *correct*
and stays. The defect is that both render identically, so a staged-but-unadopted import is
indistinguishable from a no-op.

## Scenario: the active account's value is marked as canonical-sourced  · Cap-5.1

    Given a status render with one active account and at least one parked account
    When EXPIRY is displayed
    Then an operator can tell which slot each value was read from

## Scenario: a stash/canonical disagreement is surfaced  · Cap-5.1

    Given an account whose stash and canonical item carry different expiry deadlines
    When status renders that account
    Then the disagreement is visible rather than silent

    # This is exactly the post-import staged-not-adopted state, so surfacing it converts the
    # 2026-07-31 symptom ("others updated and active did not") into a diagnostic.

## Scenario: authority is unchanged  · Cap-5.1

    Given the active account
    When its EXPIRY is resolved
    Then the canonical item remains the authoritative source
    But not changed to the stash by this feature

    # Which slot is authoritative belongs to issue #1001, not to a display change.
