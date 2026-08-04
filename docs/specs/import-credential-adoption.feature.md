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

## Scenario: importing the target's active account reports non-adoption  · Cap-1.1 · AC-2a

    Given a target machine whose active account is A
    And a migration artifact containing a credential for account A
    When import runs
    Then the report states that A's credential is staged but not adopted
    And the command it names carries the --force flag
    But not by naming `use <label>` unqualified
    But not by reporting plain success as though A were usable

> `--force` is the load-bearing token, so it is what this scenario asserts on. The unqualified form
> is a provable no-op for the active account — `SwapTarget::resolve` short-circuits on service-name
> equality (`src/use_account.rs:325-326`) and the committed test
> `already_active_without_force_is_a_noop_success_with_zero_writes`
> (`src/use_account.rs:2490-2502`) asserts `canonical == b"A-token"`, `calls == 0`. A test that
> accepts any "command that completes the adoption" passes while the guidance ships the no-op —
> reproducing the original failure through its own remediation.

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

## Scenario: adoption through the sanctioned path leaves the canonical correct  · Cap-1.1 · AC-2a

    Given a target machine whose currently-active account is A
    And an import has staged a credential for that same account A
    When the operator runs the command the report named
    Then the swap engine performs the transition under the #64 lock
    And the canonical item reflects A's imported credential
    But not by a writer introduced in the import path

> The `currently-active` precondition is not incidental — it is the whole scenario. Satisfied with a
> **parked** account this passes trivially (a parked account is not short-circuited, so any
> invocation works) while leaving the AC-2a defect completely untested. AC-2a is explicit: *Given the
> imported account is the target's currently-active one.*
