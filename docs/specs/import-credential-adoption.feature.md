<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite (`src/cli.rs` unit
tests, integration tests) and the scripts/check-*.sh shell gates. These scenarios exist to pin each
acceptance criterion in scenario form and bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11), so a test author has a 1:1
target. Do not read a written scenario as a passing test.
-->

# Feature: an imported credential is adopted, or the operator is told it was not

Tracked as **issue #1001**. Requirements: PRD R-2, R-2a. Design § 4.1, AD-1.

**Rule under test**: `import` stages credentials into per-account stashes. The canonical
`Claude Code-credentials` item — the only one Claude Code reads — is written **exclusively** by the
swap engine under the #64 lock. So import must never silently leave the active account staged and
report plain success.

## Scenario: importing the target's active account reports non-adoption  · Cap-1.1

    Given a target machine whose active account is A
    And a migration artifact containing a credential for account A
    When import runs
    Then the report states that A's credential is staged but not adopted
    And it names the command that completes the adoption
    But not by reporting plain success as though A were usable

## Scenario: import adds no second writer to the canonical item  · Cap-1.2

    Given a target machine with a readable canonical Claude Code-credentials item
    When import runs to completion
    Then the canonical item is byte-identical to what it was before the import
    And no swap lock was acquired by the import path

## Scenario: parked accounts are staged without any canonical interaction  · Cap-1.2

    Given a migration artifact containing credentials for two parked accounts
    When import runs
    Then each account's Sessiometer/<account_uuid> stash holds its imported credential
    And the canonical item is untouched

## Scenario: adoption through the sanctioned path leaves the canonical correct  · Cap-1.1

    Given an import has staged a credential for account A
    When the operator runs the command the report named
    Then the swap engine performs the transition under the #64 lock
    And the canonical item reflects A's imported credential
    But not by a writer introduced in the import path
