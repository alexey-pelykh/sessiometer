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

## Scenario: a live-but-unresponsive daemon still warns  · Cap-10.1

    Given the control socket does not answer but the single-instance lock is held
    When the operator runs `export`
    Then the command warns, exactly as it does for a responsive daemon
    But not by reporting the daemon as not running

> The middle state of `daemon_liveness()`'s tri-state (`src/cli.rs:1870-1878`), and the one an
> implementer must not invent an answer for. A daemon that is starting up or wedged **holds the lock
> and will still refresh**, so it invalidates the artifact just as a responsive one does — fail
> **closed** and warn. The variant's own doc comment says it is *"Reported honestly, NOT as 'not
> running'"*. A two-state test that maps this to the quiet branch ships silence at the moment the
> warning matters.

## Scenario: a probe failure does not fail the export  · Cap-10.1

    Given the liveness probe cannot reach a conclusive answer
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
