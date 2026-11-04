# Feature: a write that would make a label name a second account, or rename an account, is refused at the write unless the operator consents

Issue #1556 · PRD § 8.1 (the operator's rulings of 2026-11-03), REQ-ROS-B-001 / B-002 / Q-001 / C-002, REQ-IDN-B-001 / C-001 / C-002, REQ-NTC-C-001 / C-002, AC-ROS-001 / 002, AC-IDN-001 / 002 / 003, AC-NTC-002 · design D-1, D-2, D-3, § 8.2, § 8.5, § 11.5, § 12.2, § 12.3

Example Mapping: 🟦 10 rules · 🟩 103 examples · 🟥 0 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests
> on is unratified and its problem framing is unvalidated. Each scenario waits to be bound in #1556's
> execution; none is a test yet. The stub maps the item as re-authored on 2026-11-03 to the operator's
> rulings, CLI half only. The socket `capture` and `config-set` refusals and acknowledgements are #1559
> and #1558, the menu-bar surfaces are #1561 and #1560, the `<label>` sentence of `login --help` is
> #1570, and shared-credential detection is #1557. An example is a scenario or an outline row.
>
> The Blue Card Gate trips: the item carries more rules than one item should. This stub does not
> split it. The rules are numbered so that a split has somewhere to cut.
>
> No scenario here waits on an open question. The two operator decisions this stub used to wait on
> were ruled by the operator on 2026-11-03 (PRD § 8.1, design § 12.2). Under `duplicate-label-policy`,
> a write that would make a label name a second account is refused unless `--overwrite` is given.
> Under `login-mismatch-policy`, a write that would rename the account it lands on is refused unless
> `--rename` is given. The two flag names are the operator's. What `--overwrite` does, that `--force`
> is not also rename consent, the free label a new account derives, and the shape of a refusal are
> readings the `/scope` orchestrator settled in design § 12.3; the exit codes and the audit outcome
> values are #1556's own calls. A scenario that rests on one of these says so in its comment, and each
> is ratification-pending. `notice-parity-facts`, settled on 2026-10-27 by #1556's Build Reference,
> governs no line this item now prints (see Red cards).
>
> The traps in this feature:
> - A notice that fires where nothing was created trains the operator to dismiss the one that
>   matters. Rule 1's quiet rows and Rule 4's quiet table exist to fail an implementation that
>   reports on every write. Their fixtures are the ones import already pins.
> - The rule has two clauses and neither is redundant, and it is not the count rule `import` keeps.
>   Rule 1 names the fixture that goes red when each clause is removed, and the one that goes red
>   when the count rule stands in for the rule. The PRD lists a swap of two labels first among its
>   fixtures (REQ-ROS-Q-001), and a swap detects only the first removal.
> - A scan that finds writers from a hand-kept list passes over the writer nobody listed. Rule 7 pins
>   the writer count and carries a canary.
> - A refusal reached after the stash write leaves an orphan stash behind, and a refused `login` has
>   already spent the operator's interactive login (AC-ROS-001, BUT NOT). Rule 9's no-write outline
>   fails an implementation that refuses late.
> - A flag that acts where nothing needs it gets passed out of habit, and `--overwrite` deletes a
>   stored login. Rule 10 pins that a surplus flag changes nothing, and Rule 8 that a replacement takes
>   exactly one holder, in its own place, and deletes its login only once the roster no longer names it.
>
> Words. A *bearer* of a label is a roster entry that carries it. `A:work` is an entry whose uuid is A
> and whose label is `work`; A, B and C stand for whole uuids. *The rule* is `label_collisions`, the
> pure function this item adds beside `Account`. *The landed account* is the account a write lands on,
> found by the uuid of the identity it harvested. A *holder* is another account that carries the label
> before the write. *The written entry* is the one entry a `capture` or `login` creates, relabels or
> puts in a holder's place. *Consent* is `--rename` or `--overwrite`. A *derived label* is the label a
> new account takes when no label is given: its uuid, or, where another account already carries that
> string, its uuid followed by the first of `-2`, `-3` and so on that no account carries (Rule 5). A
> *notice* is a line a completed write prints about what it did; a *refusal* is the message a refused
> write prints instead of writing. `activate`
> is `login`'s verdict from `should_activate` (src/capture.rs): true when the login is the account
> already signed in, or when none is. Every roster here holds enabled entries only unless a scenario
> says an entry is parked, so "in rotation" reads the same whether it counts the roster or its enabled
> entries (DQ-10 is not this item's).

## Rule 1 — one shared rule finds a collision per account, and import keeps its own rule and behaves as it did

> REQ-ROS-B-002, as refined on 2026-11-03 (design § 12.3, Terms): a write creates a collision on a
> label when it gives an account that label, which the account did not carry before, and another
> account carries the label after the write. The count rule REQ-ROS-B-002 first stated, more than one
> bearer after the write and more bearers than before, is the rule `apply_import` has today: its count
> is `label_bearers` (src/cli.rs:5245) and its test is `created_ambiguity` (src/cli.rs:5425). A save
> that moves one account onto a label while another account leaves it keeps that count where it was,
> so the count rule misses it. The item adds the per-account rule beside `Account` (src/config.rs:360),
> as one pure function over two roster slices, for the four writes the rulings govern, and
> `apply_import` keeps its own rule unchanged until the operator rules on `import` (design § 12.3,
> C-1). The function takes the label comparison key as a parameter and returns each collided label
> with its bearers' uuids in roster order.

```gherkin
Scenario Outline: the rule on <write>
  Given a roster <before>
   When a write leaves the roster <after>
   Then the rule reports <collisions>

  Examples:
    | write                                              | before              | after                  | collisions    |
    | a new uuid under a label another entry bears       | A:work              | A:work, B:work         | work: A, B    |
    | a relabel onto another entry's label               | A:work, B:spare     | A:work, B:work         | work: A, B    |
    | a third bearer joining a duplicate                 | A:work, B:work      | A:work, B:work, C:work | work: A, B, C |
    | an entry joining a label as another leaves it      | A:work, B:work, C:c | A:work, B:c, C:work    | work: A, C    |
    | an ordinary new account under a fresh label        | A:work              | A:work, B:personal     | none          |
    | a swap of two labels between two entries           | A:a, B:b            | A:b, B:a               | none          |
    | a refresh of an entry whose label has two bearers  | A:work, B:work      | A:work, B:work         | none          |
    | a relabel to the label the entry already carries   | A:work              | A:work                 | none          |
    | a relabel that ends a duplicate                    | A:work, B:work      | A:work, B:personal     | none          |
    # Covers AC `collision-rule-calibration`. The first four rows are the item's loud fixtures and the
    # next four its quiet ones. The fourth is the write the count rule misses: work has two bearers
    # before and after, but C did not carry work before and A carries it after. No `capture` or `login`
    # can make it, since each writes one entry; a `config-set` save can, and #1558 refuses that save
    # through this rule (design § 12.3, Terms, C-7). The last row is how the CLI frees a duplicated
    # label: sign in to one of its holders and run `capture <other-label> --rename` (Rule 9). Import pins
    # the other cases through `apply_import` (src/cli.rs:5277), under its own rule, and this rule must
    # agree with it on each.

Scenario: the bearers come back in roster order, not in the order the write touched them
  Given a roster A:work, B:spare
   When B is relabelled work
   Then the rule reports work: A, B
    # B is the written entry, so a "written first" list would read B, A. The rule does not know which
    # entry was written. Rule 9 is where the refusal names the landed account first.
    # Covers AC `collision-rule-calibration` ("bearers in roster order").

Scenario Outline: the comparison key is applied to both rosters
  Given a key, supplied by the test, that folds case
    And the roster before is A:Work, B:work
   When the roster after is <after>
   Then the rule reports <collisions>

  Examples:
    | after                  | collisions                      |
    | A:Work, B:work         | none                            |
    | A:Work, B:work, C:WORK | one collision, bearers A, B, C  |
    # The key is the hook that `label-normalization-policy` would plug into. This scenario tests the
    # hook and takes no side on the policy. A key applied to the after-roster alone reads A as carrying
    # a label it did not carry before, and reports a collision for a roster no write changed. Every
    # caller passes the identity today, so no scenario here asserts that Work and work stay distinct
    # through capture or login. Covers the item's key parameter (Specification, "The collision rule").

Scenario Outline: removing a clause turns one fixture red
  Given the rule with the clause <clause> removed
   When the calibration fixtures above run
   Then the fixture <fixture> reports a collision, and the test fails

  Examples:
    | clause                                     | fixture                                           |
    | another account carries the label after    | an ordinary new account under a fresh label       |
    | the account did not carry the label before | a refresh of an entry whose label has two bearers |
    # The same-label relabel is quiet under either clause alone, so it detects neither removal; the swap
    # detects the first, as the ordinary new account does. The swap does other work too: a writer that
    # checked between two relabels would see a transient duplicate, and import's swap scenario pins the
    # finished-roster reading there. Covers the mutation pass the item asks the pull request to record.

Scenario: the count rule in place of the per-account test misses one fixture
  Given the rule with REQ-ROS-B-002's count rule, more than one bearer after and more than before, in place of its per-account test
   When the calibration fixtures above run
   Then the fixture of an entry joining a label as another leaves it reports no collision, and the test fails
    # Every other fixture above gives the same verdict under both rules, so this row alone keeps the
    # count rule out of the shared function. Covers the mutation pass the item asks the pull request to
    # record.

Scenario: import keeps its own rule and behaves exactly as before
  Given the duplicate-label fixtures that `apply_import` already has
   When import runs, still through `label_bearers` and `created_ambiguity`
   Then each fixture gives the outcome it gives today
    And the text `import_report` renders is byte-identical for every existing fixture
    And on each fixture the shared rule finds a collision on exactly the labels whose written rows import marks as duplicates
    # Covers AC `import-unchanged` and the item's conformance criterion. `apply_import` keeps the count
    # rule until the operator rules on `import` (design § 12.3, Terms, C-1), and Rule 7's writer scan
    # lists it as the one writer with its own rule. `import_report` is at src/cli.rs:5650 and its notice
    # is `duplicate_label_notice` (src/cli.rs:5628); neither changes. The fixtures are the tests
    # `apply_import_warns_when_it_creates_a_same_label_different_uuid_entry`,
    # `apply_import_stays_quiet_on_the_ordinary_same_label_same_uuid_case`,
    # `apply_import_warns_when_one_artifact_carries_its_own_duplicate_on_a_fresh_target`,
    # `apply_import_stays_quiet_on_an_ordinary_import_of_distinct_labels`,
    # `apply_import_stays_quiet_when_an_import_swaps_two_labels_between_accounts`,
    # `apply_import_stays_quiet_when_a_duplicate_the_target_already_had_is_overwritten`,
    # `apply_import_warns_when_it_deepens_a_duplicate_the_target_already_had` and
    # `apply_import_warns_when_an_overwrite_relabels_onto_an_existing_label`. They pass with no edit.
```

## Rule 2 — the plan decides before any write: a refresh, a new entry, a replacement or a refusal

> `plan_capture` (src/capture.rs:635) is the one place that matches a harvested uuid to an entry. It
> runs before the stash write in `run_capture` and in `run_login`, and the socket `capture` reaches it
> through `capture_locked`. Today it returns the stash name and the outcome, overwrites a label in
> place, and refuses nothing. The item puts one decision in front of the stash write. Given the label
> and the consent the operator gave, the plan runs the rule over the roster before and after its own
> in-memory edit, and returns either a refusal or a plan. A plan carries the written entry's uuid, its
> label, the outcome, the previous label when it renamed an entry, and the holder it replaced, if any.
> It carries no collision: a write that would create one is refused, and a derived label is made free
> before the rule runs (Rule 5). A refusal carries its reason, the landed account and the holders, and
> leaves the roster as it found it. `plan_capture` keeps its two arms, update-in-place and push (I-1 of
> docs/requirements/roster-loss-prevention.md): a replacement takes the holder out by its uuid, as
> `apply_remove` does, runs the new-account arm, and puts the new entry at the index the holder held.

```gherkin
Scenario: a plan for a new uuid under a label one other account carries, given no consent, is a refusal
  Given a roster A:work, C:spare
   When the plan is made for uuid B under the label work, with no consent
   Then the plan is a refusal for the reason label-in-use
    And it names B as the landed account, not on the roster
    And it names A as the one holder
    And the roster is A:work, C:spare, as it was
    # The plan is made from the roster, the harvested uuid, the label and the consent alone. It takes
    # no stash and does no I/O, as `plan_capture` does not today.
    # Covers AC `plan-returns-facts-before-any-write` and AC `oracle-new-uuid-under-a-borne-label`.

Scenario: the same plan, given overwrite consent, puts B in A's place
  Given a roster A:work, C:spare
   When the plan is made for uuid B under the label work, with `--overwrite`
   Then the plan names B as the written entry, the outcome as a new account, and A as the holder it replaced
    And the roster is B:work, C:spare, with B at index 0, where A was, and enabled
    And the plan carries no previous label and no collision
    # Two entries before and two after: a replacement never lowers the count, which is why its reload
    # stays append-only (Rule 8). The orchestrator's reading of the operator's word "overwrite"
    # (design § 12.3, C-5): exactly one holder, replaced in its own place.
    # Covers AC `plan-returns-facts-before-any-write` and AC `overwrite-replaces-one-holder-in-place`.

Scenario Outline: a plan that would rename an account onto a label another account carries is a refusal whatever consent is given
  Given a roster A:work, B:spare
   When the plan is made for uuid B under the label work, with <consent>
   Then the plan is a refusal for the reason label-in-use
    And it names B as the landed account, on the roster as spare
    And it carries both conditions: the write would rename spare to work, and A carries work
    And the roster is A:work, B:spare, as it was

  Examples:
    | consent                      |
    | no consent                   |
    | `--rename`                   |
    | `--overwrite`                |
    | `--rename` and `--overwrite` |
    # Overwrite consent applies only to a new account, so no flag makes this one write: the
    # orchestrator's reading in design § 12.3, C-4. Today this branch overwrites the label in place, so
    # the previous label is lost: `existing.label = l.to_owned();` at src/capture.rs:662. Rule 4 holds
    # the same case at the reply. Covers AC `plan-returns-facts-before-any-write` and AC
    # `oracle-revive-relabels-onto-a-borne-label`.

Scenario Outline: a plan that changes no label has no previous label
  Given a roster A:work
   When the plan is made for uuid A with <label argument>
   Then the plan carries no previous label
    And the roster is as it was

  Examples:
    | label argument                       |
    | no label                             |
    | a blank label, empty or spaces only  |
    | the label work                       |
    | the label work with spaces around it |
    # `plan_capture` trims the argument and drops a blank one before it uses it:
    # `label.map(str::trim)` at src/capture.rs:656. An implementation that compared the raw argument
    # would print a previous label for the last row, where nothing changed.
    # Covers AC `plan-returns-facts-before-any-write` and the item's invariant that a write which
    # creates no collision and changes no label reports nothing.

Scenario: a relabelling plan never changes which uuids the roster holds
  Given a set of small rosters, some with duplicate labels
   When a plan relabels one of their entries
   Then the uuids in the roster after the plan are the uuids before it, in the same order
    # Covers the item's invariant that a label never changes an entry's `account_uuid`. The item says
    # "generated rosters"; design § 11.5 adds no property-testing crate, so the set is written out.

Scenario: the plan is made before the stash is written, in both writers
  Given the source of `run_capture` and of `run_login`
   When the plan call and the stash write are located in each
   Then the plan call comes first
    # `run_capture` and `run_login` are in src/capture.rs. In each, the plan call is `plan_capture`
    # and the stash write is `stash.write`.
    # The order holds today, so this scenario passes before the change and after it. It pins the order
    # against a refactor that moves the plan. The idiom is the source-order scans in src/capture.rs.
    # `the_roster_save_is_reached_only_after_the_swap_lock_has_been_released` (src/capture.rs) is one.
    # Covers AC `plan-returns-facts-before-any-write`, "runs before stash.write".
```

## Rule 3 — a label that would rename an account on the roster needs `--rename`, and with it the reply names the label it replaced

> REQ-IDN-B-001 and REQ-IDN-C-002, under the operator's ruling on `login-mismatch-policy`. A write that
> gives an account the roster already holds a label other than the one it carries is refused before
> any write unless `--rename` is given. With the flag the write completes, and its confirmation line
> keeps its verb, its new label and its count and names the label the account carried. A write that
> gives no label, or the label the account already carries, is a plain refresh, so REQ-LGN-B-004's "a
> bare `login` for an existing label refreshes it (idempotent)" holds. The cases where nothing changed
> are Rule 4's quiet table, which asserts no previous label as well as no notice. And the #447 prompt is
> offered only for an account the roster does not hold, so pressing Enter at it can never rename one.

```gherkin
Scenario Outline: a new account given no label has no previous label
  Given a roster A:work
   When <command> harvests uuid B, which the roster does not hold, with no label argument
   Then the reply is <reply>
    And it carries no previous label

  Examples:
    | command | reply                              |
    | capture | Captured "B" (now 2 in rotation).  |
    | login   | Onboarded "B" (now 2 in rotation). |
    # A new account with no label is named by its uuid: `derive_label` at src/capture.rs:621. The edge
    # this guards: an implementation that reads "no label before, a label now" as a relabel prints
    # `previously ""`. "No label argument" means the label reaches the plan as absent. At a terminal
    # `capture` first offers the email as an editable default (#447); a piped run, a missing email or
    # end-of-input at the prompt leaves the label absent.
    # Covers AC `no-previous-label-for-a-new-account` (AC-IDN-002, first half).
```

```gherkin
Scenario Outline: a write that would rename the account it lands on is refused unless `--rename` is given
  Given a roster A:work, B:spare
    And B is the account `<command>` lands on
   When `<command> personal` runs with <consent>
   Then <result>

  Examples:
    | command | consent    | result                                                                                         |
    | capture | no consent | it is refused for the reason relabels-another-account before any write, and B keeps spare      |
    | capture | `--rename` | the reply is `Refreshed "personal", previously "spare" (still 2 in rotation).`                  |
    | login   | no consent | it is refused for the reason relabels-another-account before any write, and B keeps spare      |
    | login   | `--rename` | the reply is `Revived "personal", previously "spare" (still 2 in rotation).`                    |
    # The label personal has one bearer after the rename, so the rule reports nothing and no collision
    # line follows. The refusal names spare, the label B carries, which is REQ-IDN-C-001 in the refuse
    # form; the item suggests the test name `a_login_that_lands_on_another_account_names_the_label_it_had`
    # for the login rows. `recapture_updates_the_label_when_a_new_one_is_given` (src/capture.rs)
    # keeps pinning the rename at the plan, given rename consent, and a sibling pins the refusal without
    # it. Covers AC `rename-needs-consent` and AC `oracle-login-names-the-label-it-had`, and
    # REQ-IDN-B-001 for `capture`, the same-class addition the PRD makes.

Scenario Outline: the label prompt is offered only for an account the roster does not hold
  Given a roster A:work, B:spare
    And `capture` runs at a terminal with no label argument
    And the account signed in to Claude Code is <account>
   When the operator answers the prompt, if one is offered, with third
   Then <prompt>
    And the reply is <reply>

  Examples:
    | account                         | prompt                                       | reply                                       |
    | B, which the roster holds       | no label is offered                          | `Refreshed "spare" (still 2 in rotation).`  |
    | C, which the roster does not    | the account's email is offered as the label  | `Captured "third" (now 3 in rotation).`     |
    # Today `capture` offers the email before it knows whether the account is on the roster:
    # `prefill_label_from_identity` (src/capture.rs:166) runs before `capture_locked`, so Enter at the
    # prompt gives a rostered account its email as a new label. The prompt reads the identity unlocked
    # and the locked read decides; if the two disagree, Rule 3's refusal still stands between a typed
    # label and a rename. The orchestrator's reading, design § 12.3, C-3. Covers AC
    # `rename-needs-consent`.
```

## Rule 4 — a label another account carries refuses the write before anything is written, and a write that creates no collision refuses nothing and says nothing

> REQ-ROS-B-001 under the operator's ruling on `duplicate-label-policy`, and the precision half of
> REQ-ROS-Q-001. A collision is what Rule 1 counts. A write that would create one is refused before any
> write, unless the landed account is new, exactly one holder carries the label, and `--overwrite` is
> given (Rule 8). Where the label already names two or more accounts, or where the write would also
> rename the landed account, no flag lets it through. No completed write creates a collision: a new
> account given no label takes a derived label that is made free first (Rule 5), because REQ-LGN-B-003
> forbids that path to hard-error. Nothing is reported, refused or warned about when no collision would
> be created.

```gherkin
Scenario Outline: a write that creates no collision and changes no label prints exactly today's line
  Given a roster <roster>
   When <command> harvests <harvest> with <label argument>
   Then the whole reply is <reply>
    And it carries no notice and no previous label

  Examples:
    | command | roster         | harvest      | label argument | reply                                      |
    | capture | A:work, B:work | B            | none           | Refreshed "work" (still 2 in rotation).    |
    | capture | A:work         | A            | work           | Refreshed "work" (still 1 in rotation).    |
    | capture | A:work         | a new uuid B | personal       | Captured "personal" (now 2 in rotation).   |
    | login   | A:work, B:work | B            | none           | Revived "work" (still 2 in rotation).      |
    | login   | A:work         | A            | work           | Revived "work" (still 1 in rotation).      |
    | login   | A:work         | a new uuid B | personal       | Onboarded "personal" (now 2 in rotation).  |
    # Covers AC `quiet-writes-stay-quiet` and REQ-ROS-Q-001's CLI fixtures. The first row of each
    # command refreshes an entry whose label already had two bearers: it adds none, so the count stays
    # at two. The second row harvests A itself under the label A already carries; AC-ROS-001 names a
    # report there as a BUT NOT. The third row is an ordinary new account under a fresh label.
    # In src/capture.rs, `confirmation_lines_match_the_acceptance_criteria` and
    # `login_confirmation_lines_name_the_account_by_label` pass with no edit.

Scenario: every quiet scenario in the import stub has a capture or login counterpart that stays silent
  Given the quiet scenarios of docs/specs/import-duplicate-label.feature.md
   When each is restated as a write by `capture` or `login`
   Then each prints exactly today's line
    # Covers the item's conformance criterion, checked by concretize-and-compare rather than by reading.
    #   same label, same uuid ...................... a refresh with the label the entry already carries
    #   an ordinary import of distinct labels ...... a new account under a fresh label
    #   overwriting a duplicate already there ...... a refresh of an entry whose label has two bearers
    #   a swap of two labels ....................... none: one capture or login touches one entry,
    #                                                and Rule 1's swap row covers the rule
```

```gherkin
Scenario: a capture of a new uuid under a label another account carries is refused before any write
  Given a roster A:work
   When `capture work` harvests uuid B
   Then the capture is refused for the reason label-in-use
    And the refusal names B as not on the roster, by its uuid, names work, and names A by its uuid
    And it offers `--overwrite`, which would replace A with B and delete A's stored login
    And nothing is stashed, and the roster is A:work, as it was
  But a capture that harvests A itself, under the label work, refreshes A and is not refused
    # The item names the test `onboarding_a_new_uuid_under_a_label_another_account_bears_is_not_silent`,
    # one of REQ-ROS-C-002's oracles, which now asserts the refusal (PRD § 8.1). A refusal after the
    # stash write would leave an orphan stash (AC-ROS-001, BUT NOT). AC-6 of the migration PRD is
    # narrowed by its 2026-11-03 amendment to load time and to `import`, which still warns and never
    # refuses. Covers AC `oracle-new-uuid-under-a-borne-label`.

Scenario: a login that would onboard a new account under a label another account carries is refused
  Given a roster A:work
   When `login work` lands on uuid B, which the roster does not hold
   Then the login is refused for the reason label-in-use, as the capture above is
    And the refusal also says that nothing from this login was saved
    And nothing is stashed, the canonical credential and `~/.claude.json` are not written, and the roster is A:work
    # A refused login has spent the operator's interactive login: the harvest comes first, and the
    # refusal comes before the stash write (PRD § 8.1). AC-IDN-002's "plus REQ-ROS-B-001's report if
    # another entry already carries work" becomes this refusal. Covers AC
    # `oracle-new-uuid-under-a-borne-label` for `login`, and AC `refusal-precedes-every-write`.

Scenario Outline: a login that lands on a rostered account and would give it another account's label is refused whatever the flags
  Given a roster A:work, B:spare
   When `login work` lands on uuid B, with <consent>
   Then the login is refused for the reason label-in-use
    And the refusal names B as the landed account, on the roster as spare
    And it says the login would rename spare to work, and that A carries work
    And it names the way through in two steps: free work first, then run the login again with `--rename`
    And it names no flag as enough on its own
    And nothing is stashed, and the roster is A:work, B:spare

  Examples:
    | consent                      |
    | no consent                   |
    | `--rename`                   |
    | `--overwrite`                |
    | `--rename` and `--overwrite` |
    # The item names the test `a_revive_that_relabels_onto_another_accounts_label_is_not_silent`, the
    # other CLI oracle of REQ-ROS-C-002, which now asserts this refusal. Overwrite consent applies only to
    # a new account, and a rename onto a held label is never one write, so no flag the refusal names can
    # lead to a second refusal for the same write: the orchestrator's reading in design § 12.3, C-4.
    # Freeing the label is Rule 9's list. AC-IDN-001's "And REQ-ROS-B-001's report accompanies
    # it" becomes this refusal. Covers AC `oracle-revive-relabels-onto-a-borne-label`.

Scenario Outline: a write under a label two or more accounts already carry is refused, and no flag helps
  Given a roster A:work, B:work
   When `capture work` harvests uuid C, with <consent>
   Then the capture is refused for the reason label-in-use
    And the refusal says work already names 2 accounts, and names A and B by their uuids
    And it offers no flag
    And nothing is stashed, and the roster is A:work, B:work

  Examples:
    | consent       |
    | no consent    |
    | `--overwrite` |
    # The rule counts it (Rule 1, third row). A refusal that fired only for a first duplicate would let
    # the operator deepen one that already exists, which is import's "deepening" scenario. Two holders
    # cannot be replaced one for one, so the write is refused as ambiguous, the way `remove` refuses a
    # duplicated label (`UseTargetAmbiguous`, issue #1005); the operator frees the label of all but one
    # holder by uuid first, or picks another label. The orchestrator's reading, design § 12.3, C-5.
    # Covers AC `two-or-more-holders-refuse-whatever-the-flags` and REQ-ROS-B-001.

Scenario: a socket capture shares the plan, so it refuses the same writes before anything is written
  Given the daemon's roster A:work
   When a socket `capture` with the label work captures the signed-in account B, which the roster does not hold
   Then the acknowledgement is `rejected`
    And nothing is stashed, and the roster is A:work on disk and in the daemon
    # The socket `capture` reaches the same plan through `capture_locked` (src/capture.rs:234) and
    # carries no consent, so the decision this item adds refuses there too, from this item's merge. Its
    # reason tags are #1559's to add (`label-in-use`, `relabels-another-account`); until #1559 lands,
    # `classify_capture_failure` (src/daemon/classify.rs:167) reports the refusal as the existing
    # `failed`. This scenario asserts only what holds on both sides of #1559's merge. Covers AC
    # `socket-capture-refuses-through-the-plan`.
```

## Rule 5 — a derived label is made free before the write, so it is never refused and never completes a collision

> A new account captured or logged in with no label takes its uuid as its label
> (`derive_label`, src/capture.rs:621), and REQ-LGN-B-003 and D-LGN-6 forbid that path to hard-error
> or prompt. Where
> another account already carries that exact string, which a deliberate relabel, an import, a restore or a hand edit can produce (design § 12.3, C-2), the
> derivation appends `-2`, `-3` and so on, and takes the first label no account carries. The write is
> never refused for a derived label and never completes a collision, and its reply prints the label it
> chose, as every capture and login reply already does. This is the orchestrator's reading in design
> § 12.3, C-2; it replaces the warn form's one-line notice for this case. "Carries" is decided
> under the rule's comparison key (Rule 1). Only a label the plan derives is made free: a label the
> operator typed, or confirmed at the #447 prompt, is the operator's, and Rule 4 refuses it where
> another account carries it. "No label argument" means the label reaches the plan as absent: a piped
> run, a missing email, or end-of-input at the prompt.

```gherkin
Scenario Outline: a new account given no label takes its uuid, or its uuid with the first suffix no account carries
  Given a roster <roster>
   When `capture` with no label argument harvests uuid B, which the roster does not hold
   Then the capture is not refused, and B is rostered under the label <label>
    And the whole reply is `Captured "<label>" (now <count> in rotation).`
    And the rule reports no collision

  Examples:
    | roster     | label | count |
    | A:work     | B     | 2     |
    | A:B        | B-2   | 2     |
    | A:B, C:B-2 | B-3   | 3     |
    | A:B-2      | B     | 2     |
    # `A:B` is an entry whose uuid is A and whose label is B's uuid. Only a deliberate relabel produces
    # one: a `capture <label> --rename`, a Settings rename or a hand edit. The search takes the first
    # label no account carries, and the last row shows that it starts from the bare uuid, so a free uuid
    # is never suffixed. Nothing follows the confirmation line: the warn form's line for a derived label
    # that named two accounts does not ship. Covers AC `derived-label-is-made-free`.

Scenario: a login that onboards a new account with no label derives the same free label
  Given a roster A:B
   When `login` with no label argument lands on uuid B, which the roster does not hold
   Then the login is not refused, and B is rostered under the label B-2
    And the whole reply is `Onboarded "B-2" (now 2 in rotation).`
    # Both commands derive through `plan_capture` (src/capture.rs:635), so they cannot disagree. Covers
    # AC `derived-label-is-made-free`.

Scenario: a label the operator gives is never suffixed
  Given a roster A:B
   When `capture B` harvests uuid B, which the roster does not hold
   Then the capture is refused for the reason label-in-use, before any write
    # Typing B's uuid as the label is the operator's choice, so Rule 4 holds it like any other label;
    # only a label the plan derives is made free. A label confirmed at the #447 prompt is the operator's
    # too, so Enter at a prompt that offers a label another account carries leads to this refusal, not to
    # a suffix. Covers AC `derived-label-is-made-free` and AC `oracle-new-uuid-under-a-borne-label`.

Scenario: a rostered account given no label keeps its label and is never derived again
  Given a roster A:work, B:B-2
   When `capture` with no label argument harvests uuid B
   Then the whole reply is `Refreshed "B-2" (still 2 in rotation).`, and B keeps the label B-2
    # Only a new account derives a label. A refresh keeps what the account carries, as Rule 3 and Rule
    # 4's quiet table pin for other labels, even when the bare uuid has since become free. Covers AC
    # `derived-label-is-made-free`.
```

## Rule 6 — every new string is a scanned template, and none names an email, a token or a digest

> REQ-NTC-C-001 and REQ-NTC-C-002. Every operator-facing string this item adds is a named template, not
> an inline literal. The notices are templates in one new module, which registers them with the framing
> scan under the advisory audience. The refusal is an `Error`, whose authored prose the #1139 guard
> scans against the whole central vocabulary with per-variant carve-outs (design § 8.2). No new function
> in `src/cli.rs` spells operator prose, so the gap that #1167 tracks does not widen. This item creates
> the module; #1557, which lands after it (`Blocked by #1556`), adds its own templates.

```gherkin
Scenario: every new template passes the advisory framing scan
  Given the templates in the new module
   When each is scanned with `scan_advisory_banned` (src/framing_vocabulary.rs:456)
   Then none contains a banned token
    # Covers AC `notices-pass-the-scans`. The advisory audience excuses only `enable`:
    # `ADVISORY_EXEMPT_TOKENS` at src/framing_vocabulary.rs:296.

Scenario: a template that is declared and not registered fails the module's tripwire
  Given a template added to the module with no registration for the scan
   When the module's completeness tripwire runs
   Then it fails, and it names the template
    # The tripwire in `src/cli.rs` covers that file only. Its register is
    # `INLINE_PROSE_REGISTER` (src/cli.rs:14321). The new module needs its own.
    # Covers AC `notices-pass-the-scans`.

Scenario: an injected banned token is caught
  Given a registered template with a banned token appended
   When the scan runs over it
   Then the scan reports the token
    # The scan has to bite, or a green run proves nothing. The precedent injects a token into an
    # advisory: `injected` at src/cli.rs:14747. Covers AC `notices-pass-the-scans`.

Scenario: no new function in src/cli.rs spells operator prose
  Given the change is applied
   When `every_function_spelling_inline_prose_is_dispositioned` runs
   Then it passes with no edit to its pinned counts
    And `duplicate_label_notice` and `import_report` stay `Unscanned`
    # `every_function_spelling_inline_prose_is_dispositioned` is at src/cli.rs:14420.
    # `("import_report", Unscanned, ...)` is the register entry at src/cli.rs:14371, and
    # `duplicate_label_notice` has the entry on the next line. This item does not widen that gap and
    # does not close it.
    # Covers AC `notices-pass-the-scans`.

Scenario: no new reply line names a harvested email, a token or a digest of a credential
  Given a fixture whose harvested email, token and credential digest the meter knows
    And the operator's own label passed as the authored-label allow-set
   When each new reply line is rendered for it
   Then `assert_clean` passes over every line
    # The meter is the issue #15 redaction meter: `assert_clean` at src/redaction.rs:256, with
    # `meter_fixture` at src/redaction.rs:122. It is test-only. Covers AC `no-email-no-token-no-digest`
    # and AC-NTC-002.

Scenario: a label the operator authored in the shape of an email passes, and an unauthored email fails
  Given a label the operator authored, shaped like an email address
   When a reply line names it
   Then the meter passes it through the authored-label allow-set (issue #444)
  But a harvested address that is not in the allow-set fails the meter
    # `scan` at src/redaction.rs:190 takes the allow-set as a separate input and never seeds it from the
    # credential-read email. Covers AC `no-email-no-token-no-digest`.

Scenario: the refusal's prose is scanned, and the one banned word it spends is a command name
  Given the refusal's authored prose, for every reason and for both commands
   When the #1139 guard scans it against the whole central vocabulary
   Then the only banned token it finds is remove, in `sessiometer remove <uuid>`
    And that token is carved out, with its reason, for each refusal variant that spells it, in `ERROR_PROSE_LEDGER`
    And the pinned list in `every_ledger_entry_is_earned_reasoned_and_pinned` grows by those entries and by nothing else
    # A refusal that names how to free a label has to spell the `remove` command, and `remove` is in
    # `BANNED_TOKENS` (src/framing_vocabulary.rs:182). The advisory audience excuses only `enable`, and
    # the #1139 audience has no exemption set by design, so the carve-out is per (variant, token), like
    # the three entries `ERROR_PROSE_LEDGER` (src/error.rs:1742) holds; it is the command-name class
    # `USAGE_EXEMPT_TOKENS` (src/framing_vocabulary.rs:310) records for `Error::CliUsage`. Growing the
    # pinned list in `every_ledger_entry_is_earned_reasoned_and_pinned` (src/error.rs:2685) reddens that
    # test on purpose, and the pull request records the decision. The wording avoids `add`, which is
    # banned too: it says "run it again with `--overwrite`", never "add `--overwrite`".
    # Covers AC `notices-pass-the-scans`.

Scenario: no refusal names a harvested email, a token or a digest
  Given a fixture whose harvested email, token and credential digest the meter knows
   When the refusal is rendered for every reason, for `capture` and for `login`
   Then `assert_clean` passes over every rendering
    # A refusal names an account by its label, as not on the roster with its uuid, or by uuid where a
    # label is ambiguous (REQ-NTC-C-001), and never by an email or a token (issue #15). Covers AC
    # `no-email-no-token-no-digest`.
```

## Rule 7 — every function that can add a bearer to a label calls the shared rule, except `import`, which keeps its own

> The premortem's concern (PM-19) is a new writer shipping without the rule. The model is
> `every_reconcile_roster_call_site_reaches_the_one_shared_floor` (src/daemon/commands.rs:4723). It
> writes the comparison once, enumerates each call site by signature, and pins the count. The writers
> that reach the rule are `capture`, `login` and the socket `capture`, through `plan_capture`.
> `apply_import` is the one writer that keeps its own rule, the count rule, until the operator rules
> on `import` (design § 12.3, Terms, C-1), and the scan lists it so.
> The config-set writer is `perform_config_set`, which saves what
> `Config::apply_settings` (src/config/settings.rs:37) returns, and #1558 owns its call to the rule.

```gherkin
Scenario: the shared comparison is written once, inside the rule, and import's own stays in import
  Given the production source of the crate
   When the occurrences of the shared collision comparison and of import's `created_ambiguity` are counted
   Then there is exactly one shared comparison, and it is inside `label_collisions`
    And `created_ambiguity` is computed once, inside `apply_import`
    # A copy of the shared comparison in a writer drifts from the rule, and is invisible even when it is
    # correct. Import's count rule is the one deliberate second rule (design § 12.3, Terms), and the
    # scan pins it to `apply_import`, so it cannot spread to another writer. Covers AC
    # `every-writer-calls-the-rule`, first bullet.

Scenario: each roster writer is enumerated by signature and calls the rule, or is listed with its disposition
  Given the roster writers, each named by its signature with its disposition
   When each body is read
   Then each writer listed as calling the rule reaches `label_collisions` through `plan_capture`
    And `apply_import`, listed as keeping its own rule, does not call `label_collisions`
    # `capture` and the socket `capture` reach the rule through `capture_locked` and `run_capture`, and
    # `login` through `run_login_locked` and `run_login`; all three meet it in `plan_capture`. A writer
    # that moved to another function fails rather than matching a coincidental substring elsewhere.
    # Covers AC `every-writer-calls-the-rule`, second bullet.

Scenario: a writer that is not in the enumeration fails the count
  Given a new function that adds a bearer to a label and is not enumerated
   When the scan runs
   Then it fails on the writer count
    # The socket `login` that #1021 would add is the next such writer. The scan fails until that
    # writer calls the rule. Covers AC `every-writer-calls-the-rule`, third bullet.

Scenario: the scan finds what it claims
  Given a copy of the source with the call removed from one enumerated writer
   When the scan runs over the copy
   Then it fails, and it names that writer
    # The canary runs through the same predicate the real assertion uses, over a deliberately broken
    # subject. A scan that matched nothing would return the same pass as a clean tree. Covers AC
    # `every-writer-calls-the-rule`, fourth bullet, and its demand that the scan show it can fail.

Scenario: config-set's writer is pending until #1558 removes the mark
  Given `perform_config_set`, which saves what `apply_settings` returns, listed as pending
   When the scan runs
   Then it passes while the call is absent
    And it fails when a writer neither calls the rule nor is listed with a disposition
    # #1558 removes the pending mark when it adds the call. Covers AC `every-writer-calls-the-rule`.
```

## Rule 8 — `--overwrite` replaces exactly one holder in its own place, and deletes its stored login once the roster no longer names it

> The operator's ruling on `duplicate-label-policy` names the flag; what it does is the reading the
> `/scope` orchestrator settled in design § 12.3, C-5. It applies only when the landed account
> is new and exactly one holder carries the label. Then the new account takes the holder's place: the
> holder's entry is replaced at its own index, the new entry joins enabled like every freshly captured
> account (issue #36), and the account count does not change. Because the count does not change, the
> reload stays append-only, and the daemon's never-shrink floor in
> `reconcile_roster` (src/daemon/commands.rs:781) still protects the roster: it refuses an
> append-only roster that has
> fewer accounts, never one that has as many. The order is: stash the new account under the swap lock;
> save the roster once, after that lock is released; notify the daemon, append-only; then delete the
> holder's stash under the swap lock, taken again. A delete that fails leaves an orphan stash, which the
> reply names by uuid; the replacement itself has completed, and `orphan-stash-policy` stays open. The
> reply names the replaced account by its uuid, since its label is the label just given.

```gherkin
Scenario Outline: a capture with `--overwrite` puts the new account in the holder's place
  Given a roster A:work, C:spare, where A is <holder state>
   When `capture work --overwrite` harvests uuid B
   Then the roster is B:work, C:spare, with B at index 0 and enabled
    And B is stashed, and A's stash is deleted after the roster is saved and the daemon is notified
    And the reply is `Captured "work" (now 2 in rotation).` followed by a line that names A's uuid as the account B replaced, and says A's stored login was deleted

  Examples:
    | holder state |
    | enabled      |
    | parked       |
    # A parked holder is replaced too, and B still joins enabled: a replacement takes nothing from the
    # holder but its place. The confirmation line keeps today's count, the roster's length (DQ-10 is not
    # this item's). A draft of the second line, whose facts are binding: `It replaced <uuid-A>, which
    # carried "work"; the stored login of <uuid-A> was deleted.` Covers AC
    # `overwrite-replaces-one-holder-in-place`.

Scenario: a replacement keeps the order stash, save, notify, delete, and still declares an append-only reload
  Given the source of `capture` and of `reconcile_login`
   When the stash write, the roster save, the daemon notify and the holder's stash delete are located in each
   Then they come in that order
    And the notify still declares `ReloadIntent::AppendOnly`
    And the roster save still comes after the swap lock is released
    # In src/capture.rs, `the_roster_save_is_reached_only_after_the_swap_lock_has_been_released`
    # pins the save outside the swap lock, and
    # `every_roster_verb_declares_the_intent_its_verb_implies` pins both commands
    # to `AppendOnly`; a flip of either to `Mutating` is the edit
    # that reinstates the 2026-08-27 roster collapse. Both stay green unedited. Deleting after the
    # notify means a running daemon has dropped the holder before its stash is gone, so it does not swap
    # to an account whose stash is missing; `remove_account` (src/cli.rs:5990) deletes before it
    # notifies. The delete takes the swap lock again, so no swap reads the holder's stash while it is
    # deleted. Covers AC `overwrite-replaces-one-holder-in-place`.

Scenario: a holder's stash that cannot be deleted is named, and the replacement stands
  Given a roster A:work, C:spare
    And the keychain refuses to delete A's stash
   When `capture work --overwrite` harvests uuid B
   Then the roster is B:work, C:spare
    And the reply says A's stored login could not be deleted and is still in the keychain, and names A's uuid
    And the capture exits 0
    # The replacement completed when the roster was saved; the stash left behind is an orphan, as a
    # crash between a stash write and a roster save leaves one today (PRD § 11.1, Partial). Nothing reaps
    # or reports an orphan yet (`orphan-stash-policy`), so the reply's uuid is the operator's only handle
    # on it. The exit status is #1556's call, ratification-pending: the write the operator asked for
    # completed, as a failed reload notify and a failed `~/.claude.json` co-write never fail the command
    # either. `FakeAccountStash` (src/stash.rs:424) cannot fail a delete, so the test wraps it, as
    # `FailingWriteStash` (src/cli.rs:11971) is written to fail a write. Covers AC
    # `overwrite-replaces-one-holder-in-place`.

Scenario: a login with `--overwrite` that replaces the signed-in account does not make the new one active
  Given a roster A:work, C:spare
    And A is the account signed in to Claude Code
   When `login work --overwrite` lands on uuid B, which the roster does not hold
   Then the roster is B:work, C:spare, and A's stash is deleted
    And the canonical credential and `~/.claude.json` still hold A, and no rostered account is active
    And the reply says the account it replaced was the signed-in one, that no rostered account is active now, and names `sessiometer use work`
    # #274's rule stays: a login becomes active only when it is the account already signed in, or when
    # none is (`should_activate`, src/capture.rs). Here A is signed in, so `activate` is false. The
    # daemon's reconcile drops A and re-resolves the active account by uuid, to none. The orchestrator's
    # reading, design § 12.3, C-5. Covers AC `overwrite-replaces-one-holder-in-place`.
```

## Rule 9 — a refusal says which account the write landed on, why, which flag would let it through, and the other ways through; it exits 6 or 7 and leaves one audit line

> The shape is the orchestrator's reading in design § 12.3, C-9; the exit codes and the
> audit outcome values are #1556's calls, ratification-pending. A refusal is one message, printed
> instead of any write. It names, in this order: the landed account, by its current label (with its
> uuid where another account carries that label too), or as not on the roster, by its uuid; the reason;
> each flag that would let the write proceed, only where one would; and the other ways through the CLI
> can run. Those are: pick another label; free the label by removing its holder
> (`sessiometer remove <uuid>`) or by signing in to the holder and running
> `capture <other-label> --rename`; and, on macOS, renaming it in Settings. A refused `login` also says
> that nothing from the login was saved, and that if the landed account is not the one meant, the
> operator signs in to the intended account instead. The CLI exits 6 when the label already names two
> or more accounts, following `UseTargetAmbiguous` (src/error.rs:721), the code `use` and `remove` exit
> with for a duplicated label; and 7 for every other refusal, following the gate refusals `use` exits 7
> with when it refuses a named target without `--force` (`exit_code`, src/error.rs:1208). The audit
> line carries the reason as the event log spells it, `label_in_use` or `relabels_another_account`, the
> values the socket `capture`'s line takes once #1559 maps its rejections onto them.

```gherkin
Scenario Outline: each refusal names its landed account, its reason, the flag that helps and the ways through, in that order, and exits with its code
  Given a roster <roster>
   When <write> lands on <landed>
   Then the refusal names <landed account>, then <reason>, then <flag>, then the ways through
    And the command exits <code>

  Examples:
    | roster                  | write              | landed               | landed account                  | reason                                                                                     | flag                                                         | code |
    | A:work                  | `capture work`     | uuid B, not rostered | not on the roster, and B's uuid | work already names another account, A's uuid                                               | `--overwrite`, which replaces A and deletes its stored login | 7    |
    | A:work, B:spare         | `capture personal` | uuid B               | spare                           | the write would rename spare to personal                                                   | `--rename`                                                   | 7    |
    | A:work, B:spare         | `login work`       | uuid B               | spare                           | the login would rename spare to work, and A carries work                                   | none                                                         | 7    |
    | A:work, B:work          | `capture work`     | uuid C, not rostered | not on the roster, and C's uuid | work already names 2 accounts, A's and B's uuids                                           | none                                                         | 6    |
    | A:work, B:spare, C:work | `login work`       | uuid B               | spare                           | the login would rename spare to work, and work already names 2 accounts, A's and C's uuids | none                                                         | 6    |
    # The ways through differ by reason. A label in use: pick another label, or free it by removing the
    # holder by uuid, by signing in to it and running `capture <other-label> --rename`, or by renaming it
    # in Settings on macOS. A rename: run it again with no label, which keeps spare, or rename the
    # account in Settings on macOS. A rename onto a held label: free the label first, then run it again
    # with `--rename`. Two or more holders: free the label of all but one of them, or pick another label.
    # The message's bytes are a draft; its parts and their order are binding. Covers AC
    # `refusal-names-the-way-through` and AC `refusal-exit-codes`.

Scenario: a refused login also says nothing from the login was saved, and where to sign in instead
  Given a roster A:work
   When `login work` lands on uuid B, which the roster does not hold
   Then the refusal says that nothing from this login was saved
    And it says that if B is not the account meant, the operator signs in to the intended account and runs the login again
    # The interactive login is spent: the harvest happened before the plan could refuse (PRD § 8.1).
    # Covers AC `refusal-names-the-way-through`.

Scenario Outline: the Settings way through is named only where the Settings window exists
  Given a refusal for a label another account carries
   When it is rendered where the Settings window <window>
   Then the ways through <settings>

  Examples:
    | window                | settings                        |
    | exists, on macOS      | include renaming it in Settings |
    | does not exist        | do not mention Settings         |
    # The menu-bar app is macOS-only (ADR-0029). The renderer takes whether the window exists as an
    # input, so both rows run in the `test` job on macOS; the caller passes `cfg!(target_os = "macos")`.
    # Covers AC `refusal-names-the-way-through`.

Scenario Outline: a refusal is taken before every write
  Given a roster A:work, B:spare
   When `<command> work` lands on uuid B and is refused
   Then no stash is written, the roster is not saved, and the daemon is not notified
    And neither the canonical credential nor `~/.claude.json` is written

  Examples:
    | command |
    | capture |
    | login   |
    # REQ-IDN-C-002 and AC-IDN-003: the stash and the roster are left as they were, and the operator is
    # told why, BUT NOT by a refusal after the stash write. `run_login` (src/capture.rs) re-points the
    # canonical item and co-writes `~/.claude.json` only when `activate` is true; a refusal takes neither
    # step whatever `activate` is. `capture` never writes either. Covers AC `refusal-precedes-every-write`.

Scenario Outline: a refused write leaves one audit line that carries its reason
  Given a roster A:work, B:spare
   When <write> is refused
   Then the event log gains exactly one line for it, <line>
    And the line names the landed account by its uuid, as `login`'s post-harvest `failed` line does

  Examples:
    | write                           | line                                                            |
    | `capture work` lands on new C   | `event=capture account=<uuid-C> outcome=label_in_use`           |
    | `login personal` lands on B     | `event=login account=<uuid-B> outcome=relabels_another_account` |
    # `login` already writes one `Event::Login` line per run (`emit_login_event`, src/capture.rs:359), and
    # a refusal gives that line the reason as its outcome. CLI `capture` writes no event line today; a
    # refusal writes one `Event::Capture` line, the line the socket `capture` writes, with the same two
    # outcome values, onto which #1559 then maps the socket's rejections. The event log spells reasons in
    # snake case, as `prior_configuration` spells the socket's `prior-configuration`. A completed write's
    # line is unchanged. Covers AC `refusals-leave-an-audit-line`.
```

## Rule 10 — the two flags parse on both commands, change nothing where they are surplus, and the help says what each does

> The flag names are the operator's. #1556 owns both commands' flag lists and `CAPTURE_USAGE`'s
> `<label>` entry; `LOGIN_USAGE`'s `<label>` entry is #1570's (`Blocked by #1556`). Today both commands
> parse through `parse_positional` (src/cli.rs:337), which rejects every flag but `--help`, so
> `capture work --rename` is a usage error. The flags parse as `import --overwrite`
> (`parse_import`, src/cli.rs:767) and `use --force` (`parse_use`, src/cli.rs:560) parse: boolean,
> with no value and no
> short form, in any order around the label. Surplus consent changes nothing: the same write, the same
> reply and the same event as the command without the flag. `use --force` records its swap as forced
> whether or not a gate would have refused it (`SwapReason::Forced`, src/use_account.rs:775); these
> flags do not copy that.

```gherkin
Scenario Outline: both flags parse on both commands, in any order around the label
  Given the command line <argv>
   When it is parsed
   Then <result>

  Examples:
    | argv                              | result                                                                     |
    | `capture work --rename`           | a capture with the label work and rename consent                           |
    | `capture --overwrite work`        | a capture with the label work and overwrite consent                        |
    | `login --rename --overwrite work` | a login with the label work and both consents                              |
    | `login --rename`                  | a login with no label and rename consent                                   |
    | `capture work --force`            | a usage error naming the flag, as any unknown flag is today                |
    | `remove work --rename`            | a usage error: `disable`, `enable`, `remove` and `poke` take no new flag   |
    # `parse_positional` stays for the four commands it serves; `capture` and `login` move to a parser
    # of their own that accepts the two boolean flags. A value attached to a flag (`--rename=x`) is
    # rejected the way lexopt rejects one on `--force` today. The operator's first ruling offered
    # "`--rename` or `--force`"; that `--force` is not also rename consent is the orchestrator's reading
    # in design § 12.3, C-3, so that one flag means one thing. Covers AC `flags-parse-on-both-commands`.

Scenario Outline: a flag where nothing needs it changes nothing
  Given a roster <roster>
   When <write with the flag> runs, and the same write without the flag runs on a copy of the roster
   Then both give the same roster, the same stash writes, the same reply and the same event line

  Examples:
    | roster | write with the flag                                        |
    | A:work | `capture personal --overwrite`, harvesting new uuid B      |
    | A:work | `login personal --rename`, landing on new uuid B           |
    | A:work | `capture work --rename`, harvesting A                      |
    | A:B    | `capture --overwrite` with no label, harvesting new uuid B |
    # Overwrite consent acts only where exactly one holder carries the label the write would give. The
    # last row's label is derived, and Rule 5 makes it free before the plan decides, so no holder carries
    # it and `--overwrite` replaces nothing: B is rostered as B-2 with or without the flag, and A keeps
    # its stored login. Covers AC `surplus-consent-changes-nothing`.

Scenario Outline: a flag that cannot let the write through leaves its refusal as it is
  Given a roster <roster>
   When <write> lands on <landed>
   Then it is refused for the reason <reason> before any write, and the refusal names <flag> as the flag that would let it through
    And the refusal, the exit code and the audit line are those the same write gives without the flag

  Examples:
    | roster          | write                        | landed               | reason                   | flag          |
    | A:work          | `capture work --rename`      | uuid B, not rostered | label-in-use             | `--overwrite` |
    | A:work, B:spare | `login personal --overwrite` | uuid B               | relabels-another-account | `--rename`    |
    # Rename consent acts only on a rename, and B is new to the roster, so the first write renames
    # nothing; overwrite consent acts only on a new account, and B is on the roster, so the second
    # replaces nothing (design § 12.3, C-5). Each flag given is surplus, and surplus consent changes
    # nothing, so each refusal is the one the write gets without the flag, and names the flag that
    # would let it through (Rule 9). Covers AC `surplus-consent-changes-nothing` and AC
    # `refusal-names-the-way-through`.

Scenario: the help for each command lists both flags and says what each does
  Given `sessiometer capture --help` and `sessiometer login --help`
   When each is read
   Then each lists `--rename` and `--overwrite`, and its usage line shows both
    And `--rename` says it lets a label rename the account the write lands on
    And `--overwrite` says it lets a label one other account carries replace that account, and that the replaced account's stored login is deleted
    And `capture`'s `<label>` entry says that a label that would rename the signed-in account is refused unless `--rename` is given; that, for an account new to the rotation, a label one other account carries is refused unless `--overwrite` is given, which replaces that account and deletes its stored login; that a label two or more accounts carry, or one that would both rename the signed-in account and take another account's label, is refused whatever the flags; that the email is offered at a terminal only for an account new to the rotation; and that an omitted label derives from the account-uuid, with a suffix where another account carries it
    # `CAPTURE_USAGE` is at src/cli.rs:997 and `LOGIN_USAGE` at src/cli.rs:1010. `LOGIN_USAGE`'s
    # `<label>` entry, which says a login label names "the new account", is #1570's to re-key; this item
    # changes only that constant's usage line and flag list. Help text is scanned by `scan_help_banned`,
    # which excuses `remove`. Covers AC `flags-parse-on-both-commands`.

Scenario: each command's help lists exactly the flags its parser accepts
  Given the flags `capture` and `login` each accept
   When they are compared with the flags each command's help lists
   Then the two lists are equal for each command
    # The model is `log_usage_lists_exactly_the_flags_the_parser_accepts` (src/cli.rs:16615). Covers AC
    # `flags-parse-on-both-commands`.
```

## Red cards — the questions that no scenario above can close

> Re-authored on 2026-11-03 to the operator's rulings: both cards below are closed, and the scenarios that waited on them are Green in the refuse form.

None is open. The two this stub carried are closed:

| Was blocked on | Question | The operator's answer, 2026-11-03 | Where its scenarios are now |
|---|---|---|---|
| `duplicate-label-policy` | When a write makes a label name a second account, is the report a notice on the completed write (warn) or a refusal before any write (refuse)? | Refuse, unless `--overwrite` is given | Rules 2, 4, 8 and 9 |
| `login-mismatch-policy` | When a login, or as design § 8.5 reads it a capture with a label, gives an already-rostered account a different label, does the write complete with the old label named, complete with a warning, refuse, or compare an email-shaped label with the captured address? | Refuse, unless `--rename` is given | Rules 2, 3, 4 and 9 |

`notice-parity-facts` (which facts the capture and login notice must state to match import's) is
no longer open: on 2026-10-27 #1556's Build Reference was amended to say that nothing was refused, and
that nothing was renamed unless the write itself renamed the entry. Since 2026-11-03 it governs no line
this item prints: under the rulings no completed `capture` or `login` creates a collision (Rules 4 and
5), so neither prints a collision notice.

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from something the
item does not spell out.

- The bytes of every new line beyond the facts the item calls binding: the replacement line, the
  orphan line, the line for a replaced signed-in account, and the refusal. The item calls them drafts
  that copy review may refine inside the framing scans; their facts, and the order of the refusal's
  parts, are binding.
- The exact bytes of the previous-label line. The item shows `Refreshed "work", previously "spare"
  (still 2 in rotation).` and design § 14.6 calls the draft wording of every notice not load-bearing,
  and neither says whether this line counts as one. The facts are binding: the verb, the new label,
  the previous label and the count.
- The stream a refusal is printed on, beyond what `main` does with any `Error` today.
- The names of the notices module, its templates, the plan and reply functions and the refusal's
  `Error` variants. The specification names none; the pre-authored oracle picks the seams its tests
  need, and #1556 implements to them.
- The order of this item's lines against #1557's shared-credential line, for a write that prints both.
- Whether Work and work are one label through `capture` or `login`, for the refusal and for the
  derived label's suffix search alike. Every caller passes the identity key today, and
  `label-normalization-policy` decides whether that changes.
- The meaning and wording of the "in rotation" count (DQ-10, REQ-NTC-B-001). It is out of scope.
- The rejection tag a socket `capture` refusal carries, which is #1559's. Rule 4's socket scenario
  asserts only `rejected`.

## Acceptance criteria that no scenario expresses

- *Mutation pass*: remove each clause of the rule, then put the count rule in its place, and show a
  test fail each time, recorded in the pull request. Rule 1's clause-removal outline and its
  count-rule scenario name the fixture that fails for each.
- *Existing tests*: they keep their assertions. The confirmation tests named in Rule 4, the
  `apply_import_*` tests named in Rule 1, and the source scans
  `the_roster_save_is_reached_only_after_the_swap_lock_has_been_released`,
  `both_append_only_verbs_consult_the_witness_before_they_can_write` and
  `every_roster_verb_declares_the_intent_its_verb_implies` (all in src/capture.rs) pass with no
  edit. A test that calls `plan_capture`, `run_capture`, `run_login` or `capture_locked` changes
  only by the consent argument the new input adds.
  `recapture_updates_the_label_when_a_new_one_is_given` (src/capture.rs) is the one whose call
  gains rename consent on purpose, and a sibling pins the
  refusal without it. `cargo test` checks them.
- *Local gates*: the pre-push gates in `CLAUDE.md`, plus `msrv` and both `deny` checks, because the
  change is under `src/**`.
- *Doc comments*: every comment in `src/capture.rs` that describes a rename on a refresh, or says
  what `plan_capture` does to the roster, describes the refuse form. They include the doc comments on
  `prefill_label_from_identity`, `derive_label`, `plan_capture` and `run_login`; the comment on the
  refresh arm, above `existing.label = l.to_owned();`; the comment in `run_login` above
  `let label = roster`; the append-only comment above
  `notify_daemon_roster_reload(ReloadIntent::AppendOnly)` in `capture`; and the one above
  `notify_daemon_roster_reload(ReloadIntent::AppendOnly)` in `reconcile_login`. The last two say no
  account is dropped. Review reads them; no test does.
- *Oracles*: a pre-authored RED oracle item, delivered by a party other than #1556's executor and filed
  after the planning pull request merges, carries the item's oracles, including the three the PRD and
  the item name (`onboarding_a_new_uuid_under_a_label_another_account_bears_is_not_silent`,
  `a_revive_that_relabels_onto_another_accounts_label_is_not_silent` and
  `a_login_that_lands_on_another_account_names_the_label_it_had`), each with its failure recorded on
  that item. #1556 removes their RED markers in the commit that makes them pass.

## Where each group is asserted

Taken from the acceptance criteria, for whoever binds the scenarios.

- Rule 1: one pure test over the rule, and the `apply_import_*` tests through `apply_import`.
- Rule 2: a pure test over the plan's return value, and a source-order scan for the last scenario.
- Rules 3, 4 and 5: `run_capture` and `run_login` over the in-memory stash, then the pure reply
  function that renders what the write printed. The stash is `FakeAccountStash` (src/stash.rs:424).
  `activate` reaches `run_login` inside `HarvestedLogin`, so a test sets it directly. The prompt outline
  of Rule 3 is a pure test over the predicate that decides whether to offer a label. Rule 4's socket
  scenario runs `perform_socket_capture` (src/daemon/commands.rs:280) over its fakes.
- Rule 6: the new module's own tests, the framing scans, the #1139 ledger test, and the redaction meter.
- Rule 7: a source scan modelled on
  `every_reconcile_roster_call_site_reaches_the_one_shared_floor` (src/daemon/commands.rs:4723).
- Rule 8: the plan, which is pure, for the placement; a source-order scan over `capture` and
  `reconcile_login` for the order; and the pure reply function, with the delete's result as an input.
  The delete runs in the I/O shell after the save, where no hermetic test reaches it, which is why the
  order is a scan.
- Rule 9: the pure refusal renderer, with the command, the refusal and whether the Settings window
  exists as inputs; `Error::exit_code` (src/error.rs:1208); `run_capture` and `run_login` over fakes for
  the no-write outline; and, for the audit outline, a pure test over the event a refusal produces and
  its `to_log_line` (src/observability.rs:2592), with a source scan that both commands emit it.
- Rule 10: `parse` (src/cli.rs:803), which is pure, and the usage constants.
- No function returns a capture or login's whole reply today. In src/capture.rs, `confirmation` and
  `login_confirmation` return the confirmation line only, and `CaptureReport` and `LoginReport`
  carry no collision and no
  previous label. Nothing renders a refusal. The item's acceptance criteria name "the pure reply
  function" and the refusal as seams of Rules 3 to 5, 8 and 9, and neither exists yet. A test written
  before the change cannot call them, and a test of `confirmation` cannot turn green either: its three
  arguments carry no collision, and the item keeps `confirmation_lines_match_the_acceptance_criteria`
  passing without an edit. So the pre-authored oracle lands compile-only stubs of the seams it needs,
  under names it picks, and #1556 implements to them.
