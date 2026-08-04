<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: import does not silently create a duplicate-label roster

Tracked as **issue #1005**. Requirements: PRD R-6, R-6a. Design § 4.3, OQ-1.

**Rule under test**: duplicate labels are a *documented, accepted* state (`src/cli.rs:5148-5149` —
"labels are operator handles; uniqueness is not enforced"). The rule is therefore **not** that they
must be forbidden. It is that import must not create one silently, and that once one exists every
command must agree on whether it resolves.

> **Test-construction constraint, load-bearing.** The existing
> `the_migration_conflict_policy_default_drives_import_behaviour` builds its target as
> `src_config.clone()` (`src/cli.rs:10741`), so every `account_uuid` matches **by construction** and
> this whole branch is unreachable in it. Every scenario below requires a target roster that is
> **not** a clone of the source config.

## Scenario: a same-label, different-uuid import warns  · Cap-3.1

    Given a target roster carrying label L under account_uuid X
    And a migration artifact carrying label L under account_uuid Y
    When import runs
    Then the operator is warned that a duplicate label was created
    But not by refusing the import
    But not by enforcing label uniqueness

## Scenario: a same-label, same-uuid import does not warn  · Cap-3.1

    Given a target roster carrying label L under account_uuid X
    And a migration artifact carrying label L under the same account_uuid X
    When import runs
    Then no duplicate-label warning is emitted

    # This is the ordinary cross-machine case: account_uuid is the Claude account uuid and is stable
    # across machines, so the common path must stay quiet or the warning trains dismissal.

## Scenario: duplicate-label resolution is consistent across commands  · Cap-3.2

    Given a roster containing two accounts both labelled L under different account_uuids
    When the operator invokes use with L
    And the operator invokes enable with L
    And the operator invokes disable with L
    And the operator invokes remove with L
    Then all four agree on whether L is resolvable
    But not by asserting only the three non-destructive commands

> `remove` is the load-bearing one and the easiest to drop. `apply_remove` resolves by
> `position(|account| account.label == label)` (`src/cli.rs:5219-5227`) — first-match-wins, like
> `enable`/`disable` — but it is the only one of the four that is **irreversible**: `remove_account`
> deletes the resolved account's keychain stash (`src/cli.rs:5195-5211`). A test that asserts the
> other three agree passes with `remove` still silently deleting the wrong account's credentials,
> which is the case OQ-1 says should drive the policy.

    # Today they do NOT: use refuses with UseTargetAmbiguous (exit 6, src/use_account.rs:453) while
    # apply_enabled silently takes the earliest entry (src/cli.rs:5150-5163). Which one is correct
    # is open question OQ-1 and is not settled by this spec.
