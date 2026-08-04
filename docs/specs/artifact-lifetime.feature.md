<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: the operator can destroy a consumed migration artifact

Tracked as **issues #1049, #1048**. Requirements: PRD R-12, R-10b. Design § 4.9, RSK-9.

**Rule under test**: `import` reads the artifact and leaves it, while the plaintext warning advises
deleting it with no mechanism. Under scope selection the *typical* artifact becomes roster-only — a
pure credential file — so the gap widens rather than narrows.

## Scenario: a successful import can destroy its source  · Cap-9.1

    Given an artifact that imports successfully
    When the operator ran `import --shred`
    Then the source artifact no longer exists on disk

## Scenario: a failed import does not destroy its source  · Cap-9.1

    Given an artifact whose import fails partway
    When `--shred` was requested
    Then the source artifact still exists

    # The artifact may be the only copy, and a failed import is exactly when it is needed again.

## Scenario: shred does not claim more than it delivers  · Cap-9.2

    Given the shred feature has shipped
    When its help text and documentation are read
    Then neither claims secure or forensic erasure

    # On APFS, overwrite-in-place does not reliably destroy the prior extent. Claiming erasure we do
    # not deliver is the same false-assurance failure AD-2 declines for staleness.

## Scenario: the plaintext warning matches what the tool can do  · Cap-7.8

    Given `--no-secrets` has been removed, so every artifact carries credentials
    When PLAINTEXT_WARNING is read
    Then its advice corresponds to a mechanism the tool actually provides
