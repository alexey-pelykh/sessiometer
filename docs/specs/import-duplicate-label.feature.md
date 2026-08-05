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

    # Today they do NOT: use refuses with UseTargetAmbiguous (exit 6, src/use_account.rs:453) while
    # apply_enabled silently takes the earliest entry (src/cli.rs:5150-5163). Which one is correct
    # is open question OQ-1 and is not settled by this spec.

> `remove` is the load-bearing one and the easiest to drop. `apply_remove` resolves by
> `position(|account| account.label == label)` (`src/cli.rs:5219-5227`) — first-match-wins, like
> `enable`/`disable` — but it is the only one of the four that is **irreversible**: `remove_account`
> deletes the resolved account's keychain stash (`src/cli.rs:5195-5211`). A test that asserts the
> other three agree passes with `remove` still silently deleting the wrong account's credentials,
> which is the case OQ-1 says should drive the policy.

## Scenario: an artifact carrying its own duplicate warns on a fresh target  · Cap-3.1

    Given a target with NO existing roster
    And an artifact carrying label L twice, under account_uuid X and account_uuid Y
    When import runs
    Then the operator is warned that a duplicate label was created
    But not by comparing the incoming labels only against the target's roster
    But not by refusing the import

> **The collision can arrive inside one artifact, and on a fresh target that is the only way it
> can.** *Added 2026-08-05 (eleventh pass); both scenarios above put the collision between the target
> roster and the artifact, and Cap-3.1 pinned the same shape.* `Config::validate` rejects an empty
> label and a duplicate `account_uuid` but **never** a duplicate label
> (`src/config/validate.rs:281-293`), and `render` writes `label =` per account
> (`src/config/render.rs:808`) — so a roster already carrying the documented, accepted collision
> (`src/cli.rs:5148-5149`) mints an artifact carrying it internally.
>
> An implementer who reads R-6's "already exists **on the target**" literally checks each incoming
> label against `local`'s roster. On a fresh target `apply_import` starts from an empty roster
> (`src/cli.rs:4744-4750`), `local` is `None`, the check is skipped, and both entries are appended —
> creating in one shot precisely the state R-6 exists to prevent, with both scenarios above green.

## Scenario: every label-resolving site agrees on a duplicate  · Cap-3.2

    Given a roster carrying label L under two different account_uuids
    When each label-resolving site is given L
    Then `use`, `poke` and the daemon's control-socket swap all refuse identically
    And `enable`, `disable` and `remove` do not silently act on the first match
    But not by asserting only the four commands R-6a used to name
    But not by treating this as one policy over one resolver — there are two mechanisms

> **Two mechanisms, six call sites — and `remove` is the one that does damage.** *Added 2026-08-05
> (twelfth pass); every surface said "all four label-resolving commands" (`use`, `enable`, `disable`,
> `remove`), which is wrong on the count and on the substance.* Derived from source, not sampled —
> re-run `.tmp/enumerate.py`:
>
> | Mechanism | Matches | On a duplicate | Call sites |
> |---|---|---|---|
> | `use_account::resolve_target` (`src/use_account.rs:441-459`) | `label` **or** `account_uuid` | `UseTargetAmbiguous` — refuses; its doc says it *"NEVER guesses"* | `use` (`src/use_account.rs:607`), `poke` (`src/poke.rs:290`), daemon control-socket swap (`src/daemon/commands.rs:99`) |
> | exact-label `.find()` / `.position()` | `label` only | **silently takes the first match** | `enable` / `disable` (`src/cli.rs:5152`), `remove` (`src/cli.rs:5221`) |
>
> The harm is concrete and it is on `remove`: on the duplicate label the scenarios above say `import`
> can create, `remove L` deletes the **first** match's roster entry and then its **keychain stash**
> (`src/cli.rs:5195-5211`) with no ambiguity check anywhere in that path — while `use L` on the same
> roster refuses. An operator resolving the duplicate warning by removing "the extra one" can destroy
> the wrong account's credential.
>
> "Make the four consistent" is not implementable as written either: `enable`/`disable`/`remove` do
> not apply a *different policy* at the shared resolver, they never call it. Consistency means routing
> them through `resolve_target` or deliberately not — a code change with its own blast radius, since
> `AccountLabelNotFound` and `UseTargetNotFound` are distinct errors with distinct exit codes
> (`src/error.rs:954-955`). **That is the decision OQ-1 owes, and neither `poke` nor the daemon path
> appeared on any surface before this pass.**
