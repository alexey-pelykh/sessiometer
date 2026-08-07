<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.

Each scenario below now NAMES the Rust test that binds it. That name is the executable gate; this
file is the statement of intent it satisfies. A scenario with no named test is not covered.
-->

# Feature: import does not silently create a duplicate-label roster

Tracked as **issue #1005**. Requirements: PRD R-6, R-6a. Design § 4.3, OQ-1.

**Rule under test**: duplicate labels are a *documented, accepted* state — `Config::validate`
(`src/config/validate.rs`) checks an empty `account_uuid`, an empty `label` and a duplicate
`account_uuid`, and deliberately has no duplicate-label arm. The rule is therefore **not**
that they must be forbidden. It is that import must not create one silently, and that once one exists
every command must agree on whether it resolves.

> **OQ-1 is DECIDED (2026-08-06): refuse on ambiguity, everywhere.** `enable`, `disable` and `remove`
> route through `use_account::resolve_target`, joining `use`, `poke` and the daemon's control-socket
> swap. All six sites now share one resolver, one refusal (`Error::UseTargetAmbiguous`) and one
> not-found error (`Error::UseTargetNotFound`). This is the design lean, option (i), chosen on
> `remove`'s irreversibility. Option (ii) — "`use` takes first like `enable`" — was a *regression*
> dressed as a symmetric alternative and is not implemented.

> **Test-construction constraint, load-bearing.** The pre-existing
> `the_migration_conflict_policy_default_drives_import_behaviour` builds its target as
> `src_config.clone()`, so every `account_uuid` matches **by construction** and this whole branch is
> unreachable in it. Every import scenario below requires a target roster that is **not** a clone of
> the source config.

## Scenario: a same-label, different-uuid import warns  · Cap-3.1

    Given a target roster carrying label L under account_uuid X
    And a migration artifact carrying label L under account_uuid Y
    When import runs
    Then the operator is warned that a duplicate label was created
    But not by refusing the import
    But not by enforcing label uniqueness

*Binds*: `apply_import_warns_when_it_creates_a_same_label_different_uuid_entry` (`src/cli.rs`).
The warning is an orthogonal flag on the per-account report row — the same shape `staged_not_adopted`
uses — so the four-way imported/skipped/overwritten/failed tally is unchanged and the import still
succeeds.

## Scenario: a same-label, same-uuid import does not warn  · Cap-3.1

    Given a target roster carrying label L under account_uuid X
    And a migration artifact carrying label L under the same account_uuid X
    When import runs
    Then no duplicate-label warning is emitted

    # This is the ordinary cross-machine case: account_uuid is the Claude account uuid and is stable
    # across machines, so the common path must stay quiet or the warning trains dismissal.

*Binds*: `apply_import_stays_quiet_on_the_ordinary_same_label_same_uuid_case` (`src/cli.rs`), which
asserts silence under **both** conflict policies — `skip` returns before the roster is touched, and
`overwrite` replaces the entry in place, so neither creates a second bearer of the label. Its
companion `apply_import_stays_quiet_on_an_ordinary_import_of_distinct_labels` covers the even more
common case of a brand-new label (0 → 1 bearers), which is what gates the `after > 1` half of the
rule: without it, every ordinary import would warn.

## Scenario: an artifact carrying its own duplicate warns on a fresh target  · Cap-3.1

    Given a target with NO existing roster
    And an artifact carrying label L twice, under account_uuid X and account_uuid Y
    When import runs
    Then the operator is warned that a duplicate label was created
    But not by comparing the incoming labels only against the target's roster
    But not by refusing the import

*Binds*: `apply_import_warns_when_one_artifact_carries_its_own_duplicate_on_a_fresh_target`
(`src/cli.rs`).

> **The collision can arrive inside one artifact, and on a fresh target that is the only way it can.**
> An implementer who reads R-6's "already exists **on the target**" literally checks each incoming
> label against `local`'s roster. On a fresh target `apply_import` starts from an **empty** roster,
> `local` is `None`, the check is skipped, and both entries are appended — creating in one shot
> precisely the state R-6 exists to prevent, with every other scenario green.
>
> **How the implementation defeats it**: the check reads the FINISHED roster, not `local` and not
> the roster mid-loop. `apply_import` counts each label's bearers before the merge and again after,
> and warns where a label ends with more than one bearer AND more than it started with. On a fresh
> target the before-count is zero, so a collision arriving inside one artifact is caught with no
> special case for it.
>
> The before/after comparison is not incidental precision — a per-write check gets the next two
> scenarios wrong.

## Scenario: an import that swaps two labels between accounts does not warn  · Cap-3.1

    Given a target roster carrying label a under account_uuid X and label b under account_uuid Y
    And an artifact carrying label b under account_uuid X and label a under account_uuid Y
    When import runs under the overwrite policy
    Then no duplicate-label warning is emitted
    But not by checking the roster as it stands part-way through the merge

*Binds*: `apply_import_stays_quiet_when_an_import_swaps_two_labels_between_accounts` (`src/cli.rs`).

> **This is what makes the check read the FINISHED roster rather than each write.** Replacing the
> first entry leaves the roster transiently carrying `b` twice; the second replacement resolves it.
> A check that reads the roster at each write warns here — naming a label that resolves perfectly
> well by the time the operator sees the report, and telling them to go substitute an account-uuid
> for a handle that works. That is precisely the dismissal-training failure R-6's quiet common path
> exists to avoid (PRD § P5), arrived at from the other direction.
>
> Note *which* clause does the work, because it is not the intuitive one: the swap is suppressed by
> `after > 1` evaluated on the finished roster (each label ends at one bearer), **not** by the
> before-comparison. The scenario below is the one the before-comparison uniquely covers. Deleting
> `&& after > before` leaves this scenario green.

## Scenario: overwriting a duplicate the target already had does not warn  · Cap-3.1

    Given a target roster already carrying label L under two different account_uuids
    And an artifact carrying the same two accounts
    When import runs under the overwrite policy
    Then no duplicate-label warning is emitted

    # R-6 warns when import would CREATE a duplicate. Here the count goes 2 to 2: the operator was
    # warned when it was actually made, and repeating it on every later import trains dismissal.

*Binds*: `apply_import_stays_quiet_when_a_duplicate_the_target_already_had_is_overwritten`
(`src/cli.rs`).

## Scenario: deepening a duplicate the target already had DOES warn  · Cap-3.1

    Given a target roster already carrying label L under two different account_uuids
    And an artifact carrying a third account under label L
    When import runs
    Then the operator is warned

*Binds*: `apply_import_warns_when_it_deepens_a_duplicate_the_target_already_had` (`src/cli.rs`).

> The mirror of the scenario above, and the reason the rule is *more bearers than before* rather than
> *the duplicate is new*. Without it, "don't re-warn about a pre-existing duplicate" would silently
> swallow an import that made one worse.
>
> **Known limit, deliberate.** The rule reasons about a COUNT, so a count-preserving substitution of
> bearers is invisible to it: target `[dup/A, dup/B, solo/C]` overwritten by `[solo/A, dup/C]` ends as
> `[solo/A, dup/B, dup/C]`, where `dup/C` is a genuinely new same-label/different-uuid entry and
> `count(dup)` is 2 either side — so nothing is said. Accepted rather than fixed: `dup` was
> unresolvable before the import and is unresolvable after, so the operator's actionable state is
> unchanged, and warning would re-open § P5 to tell them what they already knew.

## Scenario: duplicate-label resolution is consistent across commands  · Cap-3.2

    Given a roster containing two accounts both labelled L under different account_uuids
    When the operator invokes use with L
    And the operator invokes enable with L
    And the operator invokes disable with L
    And the operator invokes remove with L
    Then all four refuse with UseTargetAmbiguous and change nothing
    But not by asserting only the three non-destructive commands

*Binds*: `apply_enabled_refuses_a_duplicate_label_without_touching_the_roster` and
`apply_remove_refuses_a_duplicate_label_without_touching_the_roster` (`src/cli.rs`); `use`'s half is
the pre-existing `resolve_target_reports_ambiguous_for_a_duplicated_label_and_never_guesses`
(`src/use_account.rs`).

> `remove` is the load-bearing one and the easiest to drop. It is the only one of the four that is
> **irreversible**: `remove_account` deletes the resolved account's keychain stash. A test that asserts
> the other three agree passes with `remove` still silently deleting the wrong account's credentials,
> which is the case OQ-1 said should drive the policy — and did.

## Scenario: every label-resolving site agrees on a duplicate  · Cap-3.2

    Given a roster carrying label L under two different account_uuids
    When each label-resolving site is given L
    Then use, poke and the daemon's control-socket swap all refuse identically
    And enable, disable and remove refuse identically too
    But not by asserting only the four commands R-6a used to name
    But not by treating this as one policy over one resolver — there were two mechanisms

*Binds*: `every_label_resolving_site_shares_one_resolver` (`src/cli.rs`), which asserts the property
that makes the six agree — that `enable`/`disable`/`remove` reach `resolve_target` — rather than
sampling call sites one at a time.

> **Two mechanisms, six call sites — and `remove` was the one that did damage.** Before this change:
>
> | Mechanism | Matches | On a duplicate | Call sites |
> |---|---|---|---|
> | `use_account::resolve_target` | `label` **or** `account_uuid` | `UseTargetAmbiguous` — refuses; its doc says it *"NEVER guesses"* | `use`, `poke`, daemon control-socket swap |
> | exact-label `.find()` / `.position()` | `label` only | **silently took the first match** | `enable` / `disable` (`apply_enabled`), `remove` (`apply_remove`) |
>
> "Make the four consistent" was not implementable as written: `enable`/`disable`/`remove` did not
> apply a *different policy* at the shared resolver, they never called it. Consistency meant routing
> them through `resolve_target` — a code change with its own blast radius, since `AccountLabelNotFound`
> and `UseTargetNotFound` were distinct errors with distinct exit codes. The two scenarios below pin
> that blast radius rather than leaving it implicit.

## Scenario: an account-uuid is the remedy for an ambiguity refusal  · Cap-3.2

    Given a roster carrying label L under account_uuid X and account_uuid Y
    When the operator passes account_uuid Y where they would have passed L
    Then enable, disable and remove each act on exactly that account

*Binds*: `the_label_resolving_verbs_accept_an_account_uuid_so_a_refusal_is_actionable` (`src/cli.rs`).

> This is not incidental. `resolve_target` matches `label` **or** `account_uuid`, so routing the
> label-only verbs through it necessarily widens what they accept — and that widening is the *only*
> way an operator can act on a refusal, because option (iii) (an explicit `--account-uuid` flag) was
> not chosen and no such flag exists. A refusal with no remedy would be a worse defect than the
> first-match-wins it replaces. The usage strings move from `<label>` to `<account>` accordingly,
> matching `use` and `poke`.

## Scenario: the label-resolving verbs share one exit-code taxonomy  · Cap-3.2

    Given a label that matches no account
    When enable, disable or remove is invoked with it
    Then the failure is UseTargetNotFound and the process exits 5
    And an ambiguous label exits 6

*Binds*: `apply_enabled_rejects_an_unknown_label_without_touching_the_roster` and
`apply_remove_rejects_an_unknown_label_without_touching_the_roster` for exit 5, and
`every_label_resolving_site_shares_one_resolver` for exit 6 — all in `src/cli.rs`, each asserting the
literal code off a real verb path.

> **Deliberately NOT bound in `src/error.rs`.** A test there restating `UseTargetNotFound => 5` and
> `UseTargetAmbiguous => 6` would pass unchanged against the pre-fix tree: this change did not touch
> `Error::exit_code`'s mapping, and both variants predate it. What changed is *which verbs reach that
> mapping*, so the gate has to sit where the verbs produce the errors. `src/error.rs` carries a
> comment at that site recording why the obvious test is absent.

> **A deliberate, observable behaviour change.** `Error::exit_code` mapped `UseTargetNotFound => 5` and
> `UseTargetAmbiguous => 6`, while `AccountLabelNotFound` appeared nowhere in that match and so fell
> through to the generic `_ => 1`. Routing therefore moves an unmatched `enable`/`disable`/`remove`
> from exit **1 → 5**, and introduces exit **6** where nothing previously failed at all. A script
> keying on these verbs' exit codes sees it.
>
> `AccountLabelNotFound` is **retired** rather than kept: after the routing its only remaining
> constructors were in tests, and a never-constructed variant fails `-D warnings` as dead code. The
> retirement is forced by the change, not a separate preference.
