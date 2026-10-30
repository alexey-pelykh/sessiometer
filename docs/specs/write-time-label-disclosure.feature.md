# Feature: a write that makes a label name a second account, or renames an account, says so at the write

Issue #1556 · PRD REQ-ROS-B-001 / B-002 / Q-001, REQ-IDN-B-001, REQ-NTC-C-001 / C-002, AC-ROS-001 / 002, AC-IDN-001 / 002, AC-NTC-002 · design D-1, D-2, D-3, § 8.5, § 11.5

Example Mapping: 🟦 7 rules · 🟩 43 examples · 🟥 2 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests
> on is unratified and its problem framing is unvalidated. Each scenario waits to be bound in #1556's
> execution; none is a test yet. The stub maps the item as filed, CLI half only. The socket `capture`
> and `config-set` acknowledgements are #1559 and #1558, the menu-bar surfaces are #1561 and #1560,
> and shared-credential detection is #1557. An example is a scenario or an outline row.
>
> The Blue Card Gate trips: the item carries more rules than one item should. This stub does not
> split it. The rules are numbered so that a split has somewhere to cut.
>
> A scenario marked 🟥 hangs on a question that has no answer yet. It names the question's handle
> and writes no outcome as decided. The item's own default sits in the comment beside it. The 🟩
> count is the examples that carry a decided outcome; the 🟥 ones are not in it. Two of the
> questions are operator decisions: `duplicate-label-policy` (warn or refuse when a write makes a
> label name a second account) and `login-mismatch-policy` (what a write does when it gives an
> already-rostered account a different label). A third, `notice-parity-facts`, was a conflict
> inside the item; #1556's Build Reference settled it on 2026-10-27 (Rule 4). The two open ones
> are tabled at the end.
>
> The traps in this feature:
> - A notice that fires where nothing was created trains the operator to dismiss the one that
>   matters. Rule 1's quiet rows and Rule 4's quiet table exist to fail an implementation that
>   reports on every write. Their fixtures are the ones import already pins.
> - The rule has two clauses and neither is redundant. Rule 1 names the fixture that goes red when
>   each one is removed. The PRD lists a swap of two labels first among its fixtures
>   (REQ-ROS-Q-001), and a swap detects neither removal.
> - A scan that finds writers from a hand-kept list passes over the writer nobody listed. Rule 7 pins
>   the writer count and carries a canary.
>
> Words. A *bearer* of a label is a roster entry that carries it. `A:work` is an entry whose uuid is A
> and whose label is `work`; A, B and C stand for whole uuids. *The rule* is `label_collisions`, the
> pure function this item adds beside `Account`. *The written entry* is the one entry a `capture` or
> `login` creates or relabels. A *notice* is the line a write prints about a collision. `activate` is
> `login`'s verdict from `should_activate` (src/capture.rs:911): true when the login is the account
> already signed in, or when none is. Every roster here holds enabled entries only, so "in rotation"
> reads the same whether it counts the roster or its enabled entries (DQ-10 is not this item's).

## Rule 1 — one rule counts a collision exactly, and import behaves as it did

> REQ-ROS-B-002: a write creates a collision on a label when the label has more than one bearer after
> the write and more bearers than it had before. Today the rule lives inside `apply_import`.
> Its count is `label_bearers` (src/cli.rs:5245) and its test is `created_ambiguity` (src/cli.rs:5425).
> The item moves it beside `Account` (src/config.rs:360), as one pure function over two roster slices.
> The function takes the label comparison key as a parameter and returns each collided label with its
> bearers' uuids in roster order.

```gherkin
Scenario Outline: the rule on <write>
  Given a roster <before>
   When a write leaves the roster <after>
   Then the rule reports <collisions>

  Examples:
    | write                                              | before          | after                  | collisions    |
    | a new uuid under a label another entry bears       | A:work          | A:work, B:work         | work: A, B    |
    | a relabel onto another entry's label               | A:work, B:spare | A:work, B:work         | work: A, B    |
    | a third bearer joining a duplicate                 | A:work, B:work  | A:work, B:work, C:work | work: A, B, C |
    | an ordinary new account under a fresh label        | A:work          | A:work, B:personal     | none          |
    | a swap of two labels between two entries           | A:a, B:b        | A:b, B:a               | none          |
    | a refresh of an entry whose label has two bearers  | A:work, B:work  | A:work, B:work         | none          |
    | a relabel to the label the entry already carries   | A:work          | A:work                 | none          |
    | a relabel that ends a duplicate                    | A:work, B:work  | A:work, B:personal     | none          |
    # Covers AC `collision-rule-calibration`. The first three rows are the item's loud fixtures and the
    # next four its quiet ones. The last row is the path the notice's own remedy takes (Rule 5). Import
    # pins the same cases through `apply_import` (src/cli.rs:5277), and the rule must agree with each.

Scenario: the bearers come back in roster order, not in the order the write touched them
  Given a roster A:work, B:spare
   When B is relabelled work
   Then the rule reports work: A, B
    # B is the written entry, so a "written first" list would read B, A. The rule does not know which
    # entry was written. Rule 4 is where the written entry moves to the front.
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
    # hook and takes no side on the policy. A key applied to the after-roster alone counts two against
    # an unkeyed one and reports a collision for a roster no write changed. Every caller passes the
    # identity today, so no scenario here asserts that Work and work stay distinct through capture or
    # login. Covers the item's key parameter (Specification, "The collision rule").

Scenario Outline: removing a clause turns one fixture red
  Given the rule with the clause <clause> removed
   When the calibration fixtures above run
   Then the fixture <fixture> reports a collision, and the test fails

  Examples:
    | clause                     | fixture                                           |
    | more than one bearer after | an ordinary new account under a fresh label       |
    | more bearers than before   | a refresh of an entry whose label has two bearers |
    # The swap and the same-label relabel are quiet under either clause alone, so neither detects either
    # removal. The swap does other work: a writer that checked between two relabels would see a
    # transient duplicate, and import's swap scenario pins the finished-roster reading there.
    # Covers the mutation pass the item asks the pull request to record.

Scenario: import calls the shared rule and behaves exactly as before
  Given the duplicate-label fixtures that `apply_import` already has
   When import runs through the shared rule
   Then each fixture gives the outcome it gives today
    And the text `import_report` renders is byte-identical for every existing fixture
    # Covers AC `import-unchanged`. `import_report` is at src/cli.rs:5650 and its notice is
    # `duplicate_label_notice` (src/cli.rs:5628); neither changes. The fixtures are the tests
    # `apply_import_warns_when_it_creates_a_same_label_different_uuid_entry`,
    # `apply_import_stays_quiet_on_the_ordinary_same_label_same_uuid_case`,
    # `apply_import_warns_when_one_artifact_carries_its_own_duplicate_on_a_fresh_target`,
    # `apply_import_stays_quiet_on_an_ordinary_import_of_distinct_labels`,
    # `apply_import_stays_quiet_when_an_import_swaps_two_labels_between_accounts`,
    # `apply_import_stays_quiet_when_a_duplicate_the_target_already_had_is_overwritten`,
    # `apply_import_warns_when_it_deepens_a_duplicate_the_target_already_had` and
    # `apply_import_warns_when_an_overwrite_relabels_onto_an_existing_label`. They pass with no edit.
```

## Rule 2 — the plan decides before any write, and carries what the reply needs

> `plan_capture` (src/capture.rs:635) is the one place that matches a harvested uuid to an entry. It
> runs before the stash write in `run_capture` and in `run_login`. Today it returns the stash name and
> the outcome, and it overwrites a label in place, so the previous label is lost. The item makes it
> return a plan instead: the written entry's uuid, the outcome, the previous label when the write
> changed an existing entry's label, and the collisions the rule computes from the roster before and
> after the plan's own in-memory edit. It refuses nothing.

```gherkin
Scenario: a plan for a harvested uuid the roster does not hold
  Given a roster A:work
   When the plan is made for uuid B under the label work
   Then the plan names B as the written entry and the outcome as a new account
    And it carries no previous label
    And it carries the collision work: A, B
    # Covers AC `plan-returns-facts-before-any-write`. The plan is made from the roster and the
    # harvested uuid alone. It takes no stash and does no I/O, as `plan_capture` does not today.

Scenario: a plan that relabels an account the roster holds
  Given a roster A:work, B:spare
   When the plan is made for uuid B under the label work
   Then the plan names B as the written entry and the outcome as a refresh
    And it carries spare as the previous label
    And it carries the collision work: A, B
    # Today this branch overwrites the label in place, so the previous label is lost:
    # `existing.label = l.to_owned();` at src/capture.rs:662.
    # Covers AC `plan-returns-facts-before-any-write`.

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
    # In `run_capture` the plan call is `plan_capture` at src/capture.rs:577.
    # In `run_capture` the stash write is `stash.write` at src/capture.rs:585.
    # In `run_login` the plan call is `plan_capture` at src/capture.rs:825.
    # In `run_login` the stash write is `stash.write` at src/capture.rs:830.
    # The order holds today, so this scenario passes before the change and after it. It pins the order
    # against a refactor that moves the plan. The idiom is the source-order scans in this file.
    # `the_roster_save_is_reached_only_after_the_swap_lock_has_been_released` (src/capture.rs:2184) is one.
    # Covers AC `plan-returns-facts-before-any-write`, "runs before stash.write".
```

## Rule 3 — a write that changes an existing account's label names the label it replaced

> REQ-IDN-B-001. The confirmation line keeps its verb, its label and its count. Only a refresh or a
> revive that changed a label gains text. The cases where nothing changed are Rule 4's quiet table,
> which asserts no previous label as well as no notice.

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

🟥 Blocked on `login-mismatch-policy`. The scenarios below wait for the operator's answer.

```gherkin
Scenario: 🟥 a capture that gives the signed-in account a fresh label
  Given a roster A:work, B:spare
    And B is the account signed in to Claude Code
   When `capture personal` is run
   Then (pending `login-mismatch-policy`: whether the write completes, and whether the reply names spare as the label B carried)
    And no collision notice is printed
    # The item's default is to keep the rename and disclose it:
    # `Refreshed "personal", previously "spare" (still 2 in rotation).` The label personal has one
    # bearer, so the rule reports nothing. Design § 8.5 says the refuse option of this policy "would
    # close that path", the `capture <new-label>` rename that this test pins:
    # `recapture_updates_the_label_when_a_new_one_is_given` (src/capture.rs:1198).
    # Covers the Specification's first reply case, the `Refreshed … previously …` line, and
    # REQ-IDN-B-001 for `capture`, the same-class addition the PRD makes. No acceptance criterion of
    # the item names a `capture` oracle for it.

Scenario: 🟥 a login that lands on a rostered account and gives it a fresh label
  Given a roster A:work, B:spare
   When `login personal` lands on uuid B
   Then (pending `login-mismatch-policy`: whether the write completes, and whether the reply names spare as the label B carried)
    And no collision notice is printed
    # The item's default: `Revived "personal", previously "spare" (still 2 in rotation).`
    # Covers AC `oracle-login-names-the-label-it-had`; the item suggests the test name
    # `a_login_that_lands_on_another_account_names_the_label_it_had`. PRD § 8.1 and design § 12.2 say
    # the disclosure holds under every option. Design § 14.6 item 4 says the policy decides whether a
    # renaming revive is reported, refused or compared. Under refuse no rename completes, so there is
    # nothing to disclose, and the example waits rather than choosing between the two readings.
```

## Rule 4 — a write that creates a collision says so once, and a write that creates none says nothing

> REQ-ROS-B-001 and the precision half of REQ-ROS-Q-001. The report follows the unchanged confirmation
> line. The form of the report is `duplicate-label-policy`'s, so the loud scenarios wait and the quiet
> ones do not: nothing is reported, refused or warned about when no collision was created.

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
    # `confirmation_lines_match_the_acceptance_criteria` (src/capture.rs:1280) and
    # `login_confirmation_lines_name_the_account_by_label` (src/capture.rs:1294) pass with no edit.

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

🟥 Blocked on `duplicate-label-policy`. The scenarios below wait for the operator's answer.

```gherkin
Scenario: 🟥 a capture of a new uuid under a label another account bears
  Given a roster A:work
   When `capture work` harvests uuid B
   Then the operator learns, at this write, that work now names two accounts
    And the report names the label work
    And (pending `duplicate-label-policy`: the form of the report, a notice on the completed write or a refusal before any write)
    # The item's default is warn. The confirmation line stays `Captured "work" (now 2 in rotation).`
    # One notice line follows it on stdout. It names work, names both bearers by uuid with the
    # written entry first and marked "(this one)", and says what the duplicate costs and how to reach
    # an account. Under refuse the write stops before the stash is written, so nothing is stashed and
    # nothing is rostered. A refusal after the stash write would leave an orphan stash (AC-ROS-001,
    # BUT NOT). Refuse also needs AC-6 of the migration PRD re-ratified ("BUT NOT by enforcing label
    # uniqueness"), a `label-in-use` rejection and its Swift case.
    # Covers AC `oracle-new-uuid-under-a-borne-label`; the item names the test
    # `onboarding_a_new_uuid_under_a_label_another_account_bears_is_not_silent`.

Scenario: 🟥 a login that onboards a new account under a label another account bears
  Given a roster A:work
   When `login work` lands on uuid B, which the roster does not hold
   Then the operator learns, at this write, that work now names two accounts
    And the report names the label work
    And (pending `duplicate-label-policy`: the form of the report)
    # The item's default: `Onboarded "work" (now 2 in rotation).` then the notice. The confirmation
    # line carries no previous label, because a new account has none (AC-IDN-002, second half).

Scenario: 🟥 a login that lands on a rostered account and gives it another account's label
  Given a roster A:work, B:spare
   When `login work` lands on uuid B
   Then the operator learns, at this write, that work now names two accounts
    And (pending `duplicate-label-policy`: the form of the report)
    And (pending `login-mismatch-policy`: whether the write completes, and whether the reply names spare as the label B carried)
    # The item's default: `Revived "work", previously "spare" (still 2 in rotation).` then the notice.
    # Covers AC `oracle-revive-relabels-onto-a-borne-label`; the item names the test
    # `a_revive_that_relabels_onto_another_accounts_label_is_not_silent`.

Scenario: 🟥 a third account joins a label two accounts already bear
  Given a roster A:work, B:work
   When `capture work` harvests uuid C
   Then the operator learns, at this write, that work now names three accounts
    And (pending `duplicate-label-policy`: the form of the report)
    # The rule reports it (Rule 1, third row). A report that fired only for a first duplicate would
    # swallow a worse one the operator has just made, which is import's "deepening" scenario.
    # Covers AC `collision-rule-calibration` at the reply level, and REQ-ROS-B-001.

Scenario: 🟥 the notice lists the written entry first and the other bearers in roster order
  Given a roster A:work, B:spare, C:work
    And B is the account signed in to Claude Code
   When `capture work` is run
   Then the collision on work has the bearers A, B, C in roster order
    And (pending `duplicate-label-policy`: if the form is a notice, it lists B first and marks B, then A, then C)
    # The item shows its default notice for two bearers only:
    # `"work" now names 2 accounts: <uuid-B> (this one) and <uuid-A>. Commands that take a label refuse
    # "work" until one is renamed; `sessiometer use <uuid>` reaches either, and `sessiometer capture
    # <new-label>` renames the one you are signed in to.` For three bearers the list punctuation is the
    # pull request's. The item calls the wording a draft that copy review may refine, and the facts
    # binding. The notice is one line per collided label, and a single capture or login can collide
    # on one label only. Covers the Specification's second reply case, the marker and the order. No
    # acceptance criterion of the item pins the order.

Scenario: 🟥 the capture notice states the facts import's notice states
  Given the roster A:work, B:work
   When import's notice and the capture notice are rendered for the label work
   Then both name the label work
    And both say it now names more than one account
    And both say that commands taking a label refuse it
    And both say how a uuid reaches an account
    And both say that nothing was refused
    And the capture notice says that nothing was renamed only where its write renamed no entry
    # `notice-parity-facts`, settled in #1556's Build Reference on 2026-10-27: import's notice can
    # always say it renamed nothing, because import never renames. A capture or login that renamed
    # an entry states the rename in its confirmation line, and its notice does not deny it.
    And (pending `duplicate-label-policy`: the capture notice exists only if the form is a notice)
    # Import's notice is `duplicate_label_notice` at src/cli.rs:5628. Every site that resolves a label
    # to one account refuses an ambiguous one: `resolve_target` at src/use_account.rs:486 returns
    # `UseTargetAmbiguous` (src/error.rs:721), which exits with code 6 (src/error.rs:1220).
    # A uuid in place of the label reaches either account. Covers the item's conformance criterion.
```

## Rule 5 — the notice offers `capture <new-label>` only where that command would rename the written entry

> The item decides when the clause is true: after a capture always, and after a login only when
> `activate` is true. When another account is signed in, `capture <new-label>` would rename that other
> account. The negative half holds under every answer to the two decisions, because a notice that is
> not printed offers nothing. The positive half waits. The clause exists only if a notice exists, which
> is `duplicate-label-policy`'s to decide. And design § 8.5 says the refuse option of
> `login-mismatch-policy` "would close that path", the rename path the clause points at.

```gherkin
Scenario: a notice printed after a login that did not become the signed-in account omits the capture rename
  Given a collision made by `login work` that lands on uuid B
    And `activate` is false, because another account, not B, is signed in to Claude Code
   When the collision notice is printed
   Then it does not offer `sessiometer capture <new-label>`
    # That command renames the signed-in account, which is not the written entry here, so the offer
    # would send the operator to rename the wrong account. The verdict `activate` comes from
    # `should_activate` (src/capture.rs:911) and reaches the reply through the login report, so the
    # reply does not recompute it. The outcome is the same under every answer to the two decisions,
    # and an implementation that prints no notice at all passes it. The scenarios marked 🟥 below are
    # the ones that fail that implementation. Covers AC `remedy-clause-is-true`.
```

🟥 Blocked on `duplicate-label-policy` and `login-mismatch-policy`. The scenarios below wait for the operator's answer.

```gherkin
Scenario: 🟥 after a capture the notice offers the rename
  Given a collision made by `capture work` on uuid B, the account signed in to Claude Code
   Then (pending `duplicate-label-policy`: if the form is a notice) the notice offers `sessiometer capture <new-label>`
    And (pending `login-mismatch-policy`: whether that rename path stays open)
    # It renames the signed-in account, which is B. Covers AC `remedy-clause-is-true`.

Scenario: 🟥 after a login that became the signed-in account the notice offers the rename
  Given a collision made by `login work` that lands on uuid B
    And `activate` is true, so B is, or becomes, the account signed in to Claude Code
   Then (pending `duplicate-label-policy`: if the form is a notice) the notice offers `sessiometer capture <new-label>`
    And (pending `login-mismatch-policy`: whether that rename path stays open)
    # `activate` is true when the login is the account already signed in, or when none is signed in.
    # Covers AC `remedy-clause-is-true`.

Scenario: 🟥 following the remedy ends the duplicate and prints no collision notice
  Given the collision of the scenarios above on work, between A and B, with B signed in
   When `capture personal` is run
   Then the label work has one bearer
    And no collision notice is printed
    And (pending `login-mismatch-policy`: whether the reply names work as the label B carried)
    # This is the last row of Rule 1's calibration table. Covers AC `remedy-clause-is-true`: the offer
    # it makes has to work.
```

## Rule 6 — every new string is a scanned template, and none names an email, a token or a digest

> REQ-NTC-C-001 and REQ-NTC-C-002. Every operator-facing string this item adds is a named template in
> one new module, not an inline literal. The module registers its templates with the framing scan
> under the advisory audience. No new function in `src/cli.rs` spells operator prose, so the gap that
> #1167 tracks does not widen. If #1557 lands first it creates the module and this item adds its
> templates; if this item lands first, #1557 adds its own.

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
```

## Rule 7 — every function that can add a bearer to a label calls the rule

> The premortem's concern (PM-19) is a new writer shipping without the rule. The model is
> `every_reconcile_roster_call_site_reaches_the_one_shared_floor` (src/daemon/commands.rs:4723). It
> writes the comparison once, enumerates each call site by signature, and pins the count. The writers
> today are `plan_capture` (reached by `run_capture` and `run_login`) and `apply_import`.
> `Config::apply_settings` (src/config/settings.rs:37) is the config-set writer, and #1558 owns it.

```gherkin
Scenario: the comparison is written once, inside the rule
  Given the production source of the crate
   When the occurrences of the collision comparison are counted
   Then there is exactly one, and it is inside `label_collisions`
    # `apply_import` no longer carries a copy. Two copies drift, and the second is invisible even when
    # it is correct. Covers AC `every-writer-calls-the-rule`, first bullet.

Scenario: each roster writer is enumerated by signature and calls the rule
  Given the writers `plan_capture` and `apply_import`, each named by its signature
   When each body is read
   Then each calls `label_collisions`
    # `run_capture` and `run_login` reach the rule through `plan_capture`. A writer that moved to
    # another function fails rather than matching a coincidental substring elsewhere.
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
  Given `apply_settings` listed as pending
   When the scan runs
   Then it passes while the call is absent
    And it fails when a writer neither calls the rule nor is listed as pending
    # #1558 removes the pending mark when it adds the call. Covers AC `every-writer-calls-the-rule`.
```

## Red cards — the questions that no scenario above can close

| Blocked on | Question | Hangs on it | The item's own default |
|---|---|---|---|
| `duplicate-label-policy` | When a write makes a label name a second account, is the report a notice on the completed write (warn) or a refusal before any write (refuse)? Refuse needs AC-6 of the migration PRD re-ratified, a `label-in-use` rejection and its Swift case | Rule 4, the 🟥 scenarios; Rule 5, the 🟥 scenarios | warn, the only form that needs no re-ratification |
| `login-mismatch-policy` | When a login, or as design § 8.5 reads it a capture with a label, gives an already-rostered account a different label, does the write complete with the old label named, complete with a warning, refuse, or compare an email-shaped label with the captured address? | Rule 3, both 🟥 scenarios; Rule 4, the relabel-onto scenario; Rule 5, the last scenario and whether the clause is true | keep the rename and disclose it |

`notice-parity-facts` (which facts the capture and login notice must state to match import's) is
no longer open: on 2026-10-27 #1556's Build Reference was amended to say that nothing was refused, and
that nothing was renamed unless the write itself renamed the entry.

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from something the
item does not spell out.

- The notice's bytes beyond the facts the item calls binding, and its list punctuation for three or
  more bearers. The item calls the notice a draft that copy review may refine.
- The exact bytes of the previous-label line. The item shows `Refreshed "work", previously "spare"
  (still 2 in rotation).` and design § 14.6 calls the draft wording of every notice not load-bearing,
  and neither says whether this line counts as one. The facts are binding: the verb, the new label,
  the previous label and the count.
- The exit status of a write that printed a notice. No criterion states it.
- The name and path of the new notices module, and the names of its templates.
- The order of this item's notice against #1557's shared-credential notice, for a write that prints both.
- Whether Work and work are one label through `capture` or `login`. Every caller passes the identity
  key today, and `label-normalization-policy` decides whether that changes.
- The meaning and wording of the "in rotation" count (DQ-10, REQ-NTC-B-001). It is out of scope.
- The names of the tests for Rules 1, 2, 6 and 7. The item names only its oracles and the existing
  tests.

## Acceptance criteria that no scenario expresses

- *Mutation pass*: remove each clause of the rule and show a test fail, recorded in the pull request.
  Rule 1's clause-removal outline names the fixture that fails for each.
- *Existing tests*: they pass with no edit. They are the confirmation tests named in Rule 4, the
  `apply_import_*` tests named in Rule 1, and
  `recapture_updates_the_label_when_a_new_one_is_given` (src/capture.rs:1198). `cargo test` checks it.
- *Local gates*: the pre-push gates in `CLAUDE.md`, plus `msrv` and both `deny` checks, because the
  change is under `src/**`.
- *Oracles*: the item names RED oracles, with their failures recorded on the issue. Each asserts a
  form that one of the two decisions fixes, so they are authored once the operator decides.

## Where each group is asserted

Taken from the acceptance criteria, for whoever binds the scenarios.

- Rule 1: one pure test over the rule, and the `apply_import_*` tests through `apply_import`.
- Rule 2: a pure test over the plan's return value, and a source-order scan for the last scenario.
- Rules 3 and 4: `run_capture` and `run_login` over the in-memory stash, then the pure reply function
  that renders the report. The stash is `FakeAccountStash` (src/stash.rs:424). It cannot fail a write,
  which these scenarios do not need. `activate` reaches `run_login` inside `HarvestedLogin`, so a test
  sets it directly.
- Rule 5: the pure reply function, with `activate` as an input.
- Rule 6: the new module's own tests, the framing scan, and the redaction meter.
- Rule 7: a source scan modelled on
  `every_reconcile_roster_call_site_reaches_the_one_shared_floor` (src/daemon/commands.rs:4723).
- No function returns a capture or login's whole reply today. `confirmation` (src/capture.rs:688)
  and `login_confirmation` (src/capture.rs:508) return the confirmation line only.
  `CaptureReport` (src/capture.rs:81) and `LoginReport` (src/capture.rs:737) carry no collision and no
  previous label. The item's acceptance criteria name "the pure reply function" as the seam of Rules
  3 to 5, and it does not exist yet. A test written before the change cannot call it. A test of
  `confirmation` cannot turn green either: its three arguments carry no collision, and the item keeps
  `confirmation_lines_match_the_acceptance_criteria` passing without an edit. So whoever pre-authors
  an oracle for these rules must land a stub of the reply function, and the item pins neither its
  name nor its signature.
