<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: the source machine warns before it mints an artifact it will invalidate

Tracked as **issues #1050, #1051**. Requirements: PRD R-13, R-14, R-14a. Design § 4.9, RSK-10.

**Rule under test**: the design's position is that the staleness hazard is *not detectable at the
target — only preventable at the source*. There is currently zero source-side implementation of it.

## Scenario: export warns when this machine's daemon is live  · Cap-10.1

    Given the local daemon is running
    When the operator runs `export`
    Then the command warns that the daemon's next refresh will invalidate the artifact
    But does not block the export

## Scenario: export is quiet when the daemon is not running  · Cap-10.1

    Given the local daemon is not running
    When the operator runs `export`
    Then no liveness warning is emitted

    # Warning unconditionally trains dismissal — RSK-1's failure mode reproduced on a second
    # surface, and it would destroy the signal exactly where it matters.

## Scenario: a probe failure does not fail the export  · Cap-10.1

    Given the control socket is absent or unresponsive
    When the operator runs `export`
    Then the export still completes

## Scenario: an export and its import can be correlated  · Cap-10.2

    Given an artifact exported on one machine and imported on another
    When both events are read
    Then a common artifact digest identifies them as the same artifact
    And the import event carries the scope the operator requested
    But neither carries a label, token, or email
    But not by requiring a requested-scope field on the export event

    # The requested scope, never the artifact's claimed scope (AD-6).
    # Import-only, and deliberately: `export` takes no narrowing flag (R-9c, AD-5, Cap-7.5), so it has
    # no operator-requested scope to log. Asserting one there forces either an inert constant or an
    # export scope flag that violates R-9c in the same change. Export correlates on the digest alone.
