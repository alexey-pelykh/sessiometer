# Feature: a menu-bar capture refuses to duplicate a label or rename an account, and its acknowledgement says which account it touched and which credential it shares

Issue #1559 · PRD REQ-ROS-B-001 / REQ-ROS-I-001 / REQ-ROS-C-001 / REQ-IDN-C-002 / REQ-STSH-B-002 / REQ-NTC-C-001, AC-ROS-001 / AC-ROS-003 / AC-IDN-003 / AC-STSH-001, the rulings of § 8.1 · design D-2, D-3, D-4, D-10, § 8.1, § 8.2, § 11.5, § 12.2, § 12.3

Example Mapping: 🟦 9 rules · 🟩 57 examples · 🟥 0 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests
> on is unratified and its problem framing is unvalidated. The field names `account_uuid` and
> `shared_with`, and the element shape of `shared_with`, are pipeline defaults that wait for
> ratification too. Each scenario waits to be bound in #1559's execution; none is a test yet. The
> stub maps the item as re-authored to the operator's refuse rulings: the socket `capture`
> acknowledgement, its two new refusals, and the Swift decoder of both. The CLI half is #1556,
> shared-credential detection is #1557, the `config-set` acknowledgement is #1558, and the menu-bar
> presentation is #1561 and #1560. An example is a scenario or an outline row.
>
> The Blue Card Gate trips: the item carries more rules than one item should. This stub does not
> split it. The rules are numbered so that a split has somewhere to cut. Rules 1 to 6 are what the
> daemon sends. Rule 7 is the Rust-side byte pin. Rules 8 and 9 are the Swift decoder and the gate
> between the two sides.
>
> Both operator decisions this stub once waited on are ruled (PRD § 8.1, 2026-11-03): a write that
> would make a label name a second account is refused unless `--overwrite` is given, and a write that
> would rename an account is refused unless `--rename` is given. Three readings of those rulings are
> the orchestrator's, recorded beside them and confirmed at ratification, and the scenarios below
> build on each. The socket `capture` carries neither flag, so it refuses both writes, with
> `label-in-use` and `relabels-another-account`, and accepts no consent. A write that meets both
> carries `label-in-use`. A new account captured with no label takes its uuid as its label, and when
> another account already carries that string, the shared capture plan (#1556) gives it the first
> free suffix, `-2`, `-3` and so on. That write is never refused and never completes a collision,
> because REQ-LGN-B-003 forbids that path to hard-error. No completed capture therefore carries a
> collision, and the acknowledgement gains no `collisions` key. The two tag names follow design
> § 8.2. No scenario is marked 🟥: every outcome below is decided under the rulings and those
> readings, and the 🟩 count is every example in the file.
>
> One question the PRD lists is not marked 🟥 here, and that is a call, not a decision. The PRD's
> DQ-6 asks which channel carries the shared-credential report of a daemon-side write. The design
> answers "both channels" (§ 14.8, ratification-pending), and Rule 4 builds on that answer. Two
> other sources already say it. REQ-STSH-B-002 (Must) reports the pair "on the surface that
> performed the write and as one durable event-log line". And the operator's parity ruling of
> 2026-07-31 says an operator on the GUI must reach the same capabilities as an operator on the CLI,
> which tells the operator what its write did. If the operator answers "event log only", Rule 4 and
> every `shared_with` row below go. The calls are tabled at the end.
>
> Words. `A:work` is a roster entry whose uuid is A and whose label is `work`; A, B, C and D stand
> for whole uuids, `A:uuid(C)` is an entry A whose label is C's uuid, written out, and `uuid(C)-2`
> is that string with `-2` appended. *The signed-in account* is the account the daemon harvests: the
> identity in `~/.claude.json` and the credential in the canonical keychain item. A *menu-bar
> capture* is the socket `capture` command; it *asks for work* when it carries that label and *asks
> for no label* when it carries none. K names a credential; no scenario quotes one. *The written
> entry* is the one roster entry a capture creates or refreshes. A *bearer* of a label is a roster
> entry that carries it. A *rename* is a capture that gives a rostered account a label other than
> the one it carries. *Nothing is written* means nothing is stashed, `config.toml` keeps its bytes
> and the live roster is unchanged. *Quiet* means the acknowledgement carries `account_uuid` and no
> `shared_with`.
>
> The traps in this feature:
> - A decode that does not throw proves nothing. The Swift decoder drops an unknown sibling key
>   without a sound, and a key it reads under the wrong spelling decodes as absent, which it
>   tolerates. A field added in Rust alone reaches nobody and fails no test. Rules 8 and 9 assert
>   values and key sets for that reason.
> - A gate must live where it can fail. `panel-goldens` is soft: every step is `continue-on-error`,
>   so it always reports pass. Rule 9 puts the gate in the `test` job.
> - A refusal test that checks only for `rejected` passes over a refusal that reached the wire as
>   `failed`: `classify_capture_failure` ends in a wildcard arm (src/daemon/classify.rs:167), and
>   `failed` invites a retry that refuses the same way. Rules 2 and 3 assert the tag.
> - A refusal taken after the stash write leaves an orphan stash (AC-ROS-001, BUT NOT). Rules 2 and 3
>   assert that nothing was written.
> - A refusal gate that also fired on a derived label would hard-error the path REQ-LGN-B-003
>   forbids to. Rule 2's derived-label outline asserts that the write completes under a free label.
> - A refusal of a write that created nothing blocks the operator for nothing and trains them to
>   route around the one that matters. Rule 5's quiet rows are import's own fixtures.
> - Rust sends `count`; the Swift body reads `accounts`; the app never gets the count. That predates
>   this item, and a whole-body comparison would fail on it today. Rules 8 and 9 compare this work's
>   fields only.

## Rule 1 — the acknowledgement names the entry the write touched

> Design § 8.1: `account_uuid` rides `captured` and `refreshed`, always, so that a notice can mark
> the entry the write touched. A label is a handle the operator can change. The uuid is the roster
> key, and no label edit moves it. Today the acknowledgement carries the label and the count only.

```gherkin
Scenario Outline: the acknowledgement carries the uuid the roster holds for the entry
  Given a roster <roster>
    And the signed-in account is <signed_in>
   When a menu-bar capture <ask>
   Then the acknowledgement is <result>
    And it carries account_uuid <uuid>, the label <label> and the count <count>
    And the roster on disk holds <uuid> under the label <label>

  Examples:
    | roster  | signed_in            | ask               | result    | uuid | label | count |
    | B:spare | A, not in the roster | asks for work     | captured  | A    | work  | 2     |
    | A:work  | A                    | asks for no label | refreshed | A    | work  | 1     |
    # Covers the first half of AC `quiet-writes-stay-quiet` ("`account_uuid` is present in each") and
    # the invariant "a label never changes an entry's `account_uuid`" for a new account and a
    # refresh. The rows are the fixtures of two existing tests, which change only by the added field
    # (AC `existing-tests-change-mechanically`):
    # `perform_socket_capture_captures_the_active_account_and_reconciles_the_roster` (src/daemon/commands.rs:1991)
    # `perform_socket_capture_refreshes_an_already_rostered_account_without_a_duplicate_row` (src/daemon/commands.rs:2074)
    # `count` keeps its meaning, the length of the live roster (`self.roster.len()`, src/daemon/commands.rs:416).
```

```gherkin
Scenario: a refused relabel leaves the uuid and the label where they were
  Given a roster B:spare
    And the signed-in account is B
   When a menu-bar capture asks for work
   Then the acknowledgement is rejected with the reason relabels-another-account
    And the roster on disk holds B under the label spare, and holds no other uuid
    And the live roster holds B under the label spare
    # Covers the invariant "a label never changes an entry's `account_uuid`", its third case, "a
    # relabel", which on the socket is now a refusal: the relabel never completes, so neither the uuid
    # nor the label moves. Capturing the signed-in account again under a new label is the path
    # `recapture_updates_the_label_when_a_new_one_is_given` pins; under the ruling that test gains
    # rename consent (#1556), and the socket carries none. Rule 3 asserts the refusal itself.
```

## Rule 2 — a write that would make a label name a second account is refused, and a derived label is made free instead

> REQ-ROS-B-001, REQ-ROS-B-002, REQ-ROS-I-001, AC-ROS-001, design D-1, D-2, § 8.1 and § 8.2, and
> the operator's ruling on `duplicate-label-policy` (PRD § 8.1). A write creates a collision when it
> would give an account a label that account did not carry before, and another account would then
> carry that label as well (the Terms of design § 12.3). #1556 adds that rule beside `Account` as the
> shared `label_collisions`, which the four write paths use, and gates the capture plan on it before
> the stash write. Import keeps its own count rule, which REQ-ROS-B-002 states: more than one bearer
> after the write and more bearers than before (`created_ambiguity`, src/cli.rs:5425, over the
> counts of `label_bearers`, src/cli.rs:5245). One capture writes one entry, so on every write it
> can make the two rules give the same verdicts. A menu-bar capture that gives a label and would
> create a collision is refused with `label-in-use`, nothing is written, and no key on the request
> lifts the refusal. A new account captured with no label takes its uuid as its label
> (`derive_label`, src/capture.rs:621), and REQ-LGN-B-003 forbids that path to hard-error. When
> another account already carries that string, which a relabel, an import, a restore or a hand edit can produce, #1556's
> plan appends `-2`, `-3` and so on until the label is free. That write is never refused and never
> completes a collision, and the acknowledgement's `label` names the label the plan chose, as it does
> for every capture. No completed capture carries a collision, so the capture acknowledgement gains
> no `collisions` key. The `config-set` refusal carries one (#1558).

```gherkin
Scenario: the account that already bears the label is not a collision
  Given a roster A:work
    And the signed-in account is A
   When a menu-bar capture asks for work
   Then the acknowledgement is refreshed with account_uuid A
    And the acknowledgement is quiet
    # Covers AC `oracle-socket-capture-refuses-a-collision`, its sentence "It must not refuse when the
    # captured uuid is A itself". One bearer before and one after, so the count did not rise, and the
    # label given is the one A carries, so this is no rename either. AC-ROS-001 names a report here as
    # a BUT NOT. This is also AC `quiet-writes-stay-quiet`'s "relabel to the label the entry already
    # carries", so Rule 5's table repeats it only for a two-bearer roster (its third row).

Scenario: a new account under a label another account bears is refused before any write
  Given a roster A:work
    And the signed-in account is B, which the roster does not hold
   When a menu-bar capture asks for work
   Then the acknowledgement is rejected with the reason label-in-use
    And nothing is written: nothing is stashed under B, and the roster holds A alone, on disk and live
    And the capture's audit line names the label work and the outcome label_in_use
    # Covers AC `oracle-socket-capture-refuses-a-collision`; the item suggests the name
    # `a_socket_capture_that_would_make_a_label_name_a_second_account_is_refused`. This is the socket
    # `capture` oracle REQ-ROS-C-002 names, asserting the refusal the ruling selects. The gate sits in
    # the plan, before `stash.write` (design D-2), so no orphan stash is left (AC-ROS-001, BUT NOT).
    # The audit line is the one `capture_failure` (src/daemon/commands.rs:437) builds for every
    # refusal: the label the request carried and the reason's token.

Scenario: a relabel onto another account's label carries label-in-use, the reason that wins when both apply
  Given a roster A:work, B:spare
    And the signed-in account is B
   When a menu-bar capture asks for work
   Then the acknowledgement is rejected with the reason label-in-use
    And the roster on disk still holds A under work and B under spare
    And the stash of B is not written
    # Covers AC `oracle-socket-capture-refuses-a-rename`, its second case. The write is a rename and a
    # collision at once, and it carries `label-in-use`. On the CLI no flag lets this one write through
    # (the label must be freed first, then the rename repeated); on the socket no flag exists at all.

Scenario: a third account joining a label two accounts already bear is refused the same way
  Given a roster A:work, B:work
    And the signed-in account is C, which the roster does not hold
   When a menu-bar capture asks for work
   Then the acknowledgement is rejected with the reason label-in-use
    And nothing is written: nothing is stashed under C, and the roster keeps A and B, on disk and live
    # A rule that refused only a first duplicate would let a worse one through. This is import's
    # "deepening" scenario (docs/specs/import-duplicate-label.feature.md), refused here. Covers AC
    # `oracle-socket-capture-refuses-a-collision`.

Scenario Outline: a new account captured with no label, whose uuid another account carries as its label, completes under the first free label
  Given a roster <roster>
    And the signed-in account is C, which the roster does not hold
   When a menu-bar capture asks for no label
   Then the acknowledgement is captured with account_uuid C and the label <label>
    And the roster on disk holds C under <label>, and every other entry under the label it carried
    And no label in the roster names more accounts than it named before the write

  Examples:
    | roster                 | label     |
    | A:uuid(C)              | uuid(C)-2 |
    | A:uuid(C), D:uuid(C)-2 | uuid(C)-3 |
    # Covers AC `derived-label-takes-a-free-label`. The suffix is the shared capture plan's, and #1556
    # adds it, so this outline passes once #1556 lands and owes no RED here. It pins two things at the
    # socket: the menu-bar path takes the same plan, and the refusal this item maps never reaches a
    # derived label. Row 2 shows the suffix itself taken. A write given no label is never refused
    # (REQ-LGN-B-003), and it never collides either, so no completed capture carries a collision.

Scenario: no key on the request lifts either refusal
  Given a capture request line that carries the label work and the keys overwrite, rename and force, each true
   When the daemon parses it
   Then the capture command it yields is the one the line without those keys yields: the label work, and nothing else
    # Covers AC `no-override-on-the-socket`. The socket carries no consent, so this rule's refusal and
    # Rule 3's stand whatever the request says. Today the shared request parse ignores an unknown key
    # and reads `force` for `swap` alone (`ControlRequest`, src/daemon/socket.rs:414), and
    # `serve_control` (src/daemon/socket.rs:904) builds `CaptureCommand` from the label only. That
    # holds before and after the change; the scenario pins it, so that a consent field cannot arrive
    # unasked. The panel's ways through are Settings, to rename, and #1019's routed `remove`, to
    # replace; the CLI's one-action replace is a parity gap recorded against #1019, not built here.

Scenario: each write import's duplicate-label notice warns about, restated as one capture, is refused
  Given the scenarios of docs/specs/import-duplicate-label.feature.md in which import warns
   When each is restated as a menu-bar capture that gives the label
   Then each is refused with label-in-use
    #   a same-label, different-uuid import ........ this rule's second scenario
    #   deepening a duplicate the target had ....... this rule's fourth scenario
    #   an artifact carrying its own duplicate ..... none. One capture writes one entry, and that
    #                                                scenario writes two at once
    # Covers the item's second conformance criterion, its refused half, by concretize-and-compare;
    # Rule 5 holds the quiet half. Import's rule is `created_ambiguity` (src/cli.rs:5425) over the
    # counts of `label_bearers` (src/cli.rs:5245), and its notice is
    # `duplicate_label_notice` (src/cli.rs:5628). Import still warns and never refuses (AC-6 of
    # docs/requirements/migration-credential-portability.md). The socket refuses the very writes
    # import warns about: on a write one capture can make, the shared rule and import's count give
    # the same verdicts (this rule's preamble), and the refusal carries no label or count (Rule 5).
```

## Rule 3 — a capture never renames an account, so no acknowledgement names a previous label

> REQ-IDN-C-002, AC-IDN-003, design D-2 and § 8.2, and the operator's ruling on
> `login-mismatch-policy` (PRD § 8.1). Today `plan_capture` (src/capture.rs:635) overwrites the
> label of an already-rostered entry in place, with `existing.label = l.to_owned()` (src/capture.rs:662),
> so a menu-bar capture that gives the signed-in account a new label renames it silently. Under the
> ruling a rename is refused before the stash write unless rename consent is given, and the socket
> carries none, so a menu-bar capture never completes a rename. It is refused with
> `relabels-another-account`, or with `label-in-use` when the new label is also another account's
> (Rule 2). No acknowledgement therefore carries a previous label, and REQ-IDN-I-001 and AC-IDN-004,
> which asked for one on `refreshed`, are re-keyed with the rulings. A capture that gives no label, or
> the label the account already carries, is a plain refresh, as Rule 1's outline and Rule 2's first
> scenario show. The card names no label the account carries now, because the panel cannot know for
> certain which account a capture landed on (design § 12.3, C-6); the rest of the card is #1561's. The Swift decoder cannot read `refreshed` at all today. That is #1474, which this item
> waits for, and which has not chosen the Swift shape of the outcome.

```gherkin
Scenario: a new account has no previous label
  Given a roster B:spare
    And the signed-in account is A, which the roster does not hold
   When a menu-bar capture asks for work
   Then the acknowledgement is captured with account_uuid A
    And the acknowledgement is quiet
    # AC-IDN-002's second half: "BUT NOT a previous-label disclosure, because a new account has
    # none". Since the rulings no acknowledgement carries a previous label at all (this rule's
    # preamble), so the scenario holds for the new account and for every other write.
```

```gherkin
Scenario: a capture that gives a rostered account a new label is refused before any write
  Given a roster B:spare
    And the signed-in account is B
   When a menu-bar capture asks for work
   Then the acknowledgement is rejected with the reason relabels-another-account
    And the roster on disk still holds B under the label spare
    And the stash of B is not written
    And the capture's audit line names the label work and the outcome relabels_another_account
    # Covers AC `oracle-socket-capture-refuses-a-rename`, its first case, and AC-IDN-003: the stash and
    # the roster are left as they were, BUT NOT a refusal that takes effect after the stash write.
    # REQ-IDN-C-002 puts the refusal before the stash is written. The relabel adds no `result` value
    # (Rule 8): a refusal is a `rejected` with a new reason, never a fourth result. The tag carries no
    # label, and the panel cannot tell for certain which account the capture landed on, so nothing on
    # the card names spare (design § 12.3, C-6).
```

## Rule 4 — a credential another entry already holds is named in the acknowledgement, and the event log says the same

> REQ-STSH-B-002, REQ-STSH-C-001, design D-4 and § 8.1, #1557. #1557 compares the credential just
> written, in memory and after the roster is saved, with each other entry's stash, and computes the
> pairs as one value in this handler. This item places that value in the acknowledgement as
> `shared_with`: one `{label, account_uuid}` per other entry, in roster order. The written entry's
> own uuid is the acknowledgement's `account_uuid`. The same value builds the `credential_shared`
> events, one per pair, after the capture's own event. Detection itself, its read count and its
> parked entries, is #1557's and is asserted there. This rule asserts what the acknowledgement does
> with the value. The call that DQ-6 is closed is in the header.

```gherkin
Scenario: a write of a credential another entry holds names that entry
  Given a roster A:work, whose stash holds the credential K
    And the signed-in account is B, which the roster does not hold, and its credential is K
   When a menu-bar capture asks for spare
   Then the acknowledgement is captured with account_uuid B
    And shared_with holds one element: the label work and the account_uuid A
    And one credential_shared event names the same pair: the written entry spare and the entry work
    # Covers AC `oracle-socket-capture-reports-a-shared-credential`. The pair is created at this
    # write, which is the only moment F3 can see it: the first refresh of either entry rotates its
    # refresh token and splits the bytes (design D-4).

Scenario: the event line names a uuid only beside an ambiguous label, and the acknowledgement always does
  Given a roster A:work, B:work
    And the stash of B holds the credential K
    And the signed-in account is C, which the roster does not hold, and its credential is K
   When a menu-bar capture asks for spare
   Then shared_with holds one element: the label work and the account_uuid B
    And the credential_shared event names the entry work with its uuid B
    And the event names no uuid for spare, whose label has one bearer
    # Covers AC `oracle-socket-capture-reports-a-shared-credential` ("naming A (label and uuid)") and
    # REQ-STSH-B-002's rule for the event line.
    # The fixture is CAP-07's (design § 11.5): `work` A, `work` B, then `spare` holding B's credential.
    # The event line follows D-3's rule, a uuid only for a side whose label has more than one bearer
    # (design § 8.4). The acknowledgement is a machine contract on a socket that already carries every
    # uuid in `config-get` (`view`, src/config/settings.rs:65), so it carries the uuid whenever it
    # carries a label, and the app applies the naming rule when it renders (design D-3).

Scenario: several holders are listed in roster order, one event per pair
  Given a roster A:work, B:spare, whose stashes both hold the credential K
    And the signed-in account is C, which the roster does not hold, and its credential is K
   When a menu-bar capture asks for third
   Then shared_with holds work A, then spare B
    And two credential_shared events follow the capture's own event, the pair with work first
    # Covers AC `oracle-socket-capture-reports-a-shared-credential` and the Specification's
    # `shared_with` row ("in roster order").

Scenario: the acknowledgement and the events name the same pairs
  Given the fixtures of the three scenarios above
   When the handler returns its acknowledgement and its events
   Then the pairs shared_with names equal the pairs the credential_shared events name, in the same order
    # Covers AC `ack-and-event-agree`. They agree because both are built from the one value #1557's
    # handler computes. A handler that ran the comparison twice, or built the events from a second
    # source, could disagree. The check reads the other entry's label, and its uuid where the event
    # carries one.

Scenario: an unreadable stash of another entry does not turn a landed capture into a failure
  Given a roster A:work, B:spare
    And the stash of A cannot be read
    And the stash of B holds the credential K
    And the signed-in account is C, which the roster does not hold, and its credential is K
   When a menu-bar capture asks for third
   Then the acknowledgement is captured with account_uuid C
    And shared_with holds one element, spare B
    And neither the acknowledgement nor any event mentions the stash that could not be read
    # Covers AC `oracle-socket-capture-reports-a-shared-credential` at the acknowledgement seam. The
    # rule is #1557's: a failed read skips that one comparison and prints nothing, because the absence of a
    # report is never a claim that no pair exists. The trap here is the handler: a read error
    # propagated with `?` into `capture_failure` would turn a capture whose stash and roster row are
    # both on disk into a `rejected` one. The write is never refused because of a pair, or the lack of one.
```

## Rule 5 — a write that created nothing says nothing, and a refusal carries its reason alone

> REQ-ROS-Q-001 (`CollisionReportPrecision`, goal 0) and the invariant "a write that creates no
> collision reports nothing". Quiet holds under the rulings as it held under every option: nothing is
> reported or refused when nothing was created, renamed or shared. The fixtures are import's quiet
> ones (docs/specs/import-duplicate-label.feature.md), because a socket capture touches one entry. A
> `rejected` acknowledgement keeps its shape: a redacted machine code and no uuid, label or count
> (Rule 2 of docs/specs/capture-rejection-parity.feature.md), for the five tags it has today and the
> two this item adds.

```gherkin
Scenario Outline: a write that created nothing sends account_uuid and nothing else new
  Given a roster <roster>
    And the signed-in account is <signed_in>, whose credential no other entry holds
   When a menu-bar capture <ask>
   Then the acknowledgement is <result> with account_uuid <uuid>
    And the acknowledgement is quiet

  Examples:
    | roster         | signed_in            | ask               | result    | uuid |
    | A:work         | B, not in the roster | asks for personal | captured  | B    |
    | A:work, B:work | B                    | asks for no label | refreshed | B    |
    | A:work, B:work | B                    | asks for work     | refreshed | B    |
    # Covers AC `quiet-writes-stay-quiet` and REQ-ROS-Q-001's fixtures that one capture can reach.
    # Row 1 is an ordinary new account under a fresh label: no other account carries `personal`, which
    # is what keeps it from being refused. Rows 2 and 3 refresh an entry whose label already had two
    # bearers, without and with the label given: B takes no label it did not carry, which is what keeps
    # them from being refused. Both halves of the shared rule carry weight (Rule 2's preamble), and
    # import's count gives the same verdicts, `after > 1` failing on row 1 and `after > before` on rows
    # 2 and 3, as import's stub shows. The third fixture the AC names, a relabel to
    # the label the entry already carries, is Rule 2's first scenario.

Scenario: every quiet scenario of the import stub that one capture can reach has a counterpart that completes and stays quiet
  Given the quiet scenarios of docs/specs/import-duplicate-label.feature.md
   When each is restated as a menu-bar capture
   Then each completes without a refusal, and is quiet
    # Covers the item's second conformance criterion, its quiet half, by concretize-and-compare rather
    # than by reading; Rule 2 holds the refused half.
    #   same label, same uuid ...................... Rule 2's first scenario
    #   an ordinary import of distinct labels ...... row 1 above
    #   overwriting a duplicate already there ...... rows 2 and 3 above
    #   a swap of two labels ....................... none. One capture writes one entry, and a swap
    #                                                moves two. Its counterpart is `config-set` (#1558)
    # The item words this criterion as "every quiet scenario in that file". That cannot be met: the
    # swap has no counterpart to give. Read it as every quiet scenario a socket capture can reach, as
    # #1558 words the same clause ("that can arise through `config-set`").

Scenario: a refusal carries its reason and nothing else
  Given a menu-bar capture that the daemon refuses, for each reason it can send
   Then the acknowledgement line carries the result rejected and that reason's tag, and no other key
    And it carries no account_uuid, no label, no count and none of the new keys
    # Covers the item's "Quiet writes and refusals" paragraph and AC `refusal-vocabulary-moves-as-one`.
    # The reasons are the seven tags of the enum `CaptureRejection` (src/daemon/socket.rs:672): the five
    # it has today, no-active-account, keychain-locked, swap-lock-busy, failed and prior-configuration,
    # whose lines stay byte-identical, and the two this item adds, label-in-use and
    # relabels-another-account. One of the five is already pinned whole by
    # `the_prior_configuration_tag_is_redacted` (src/daemon/socket.rs:2035), and Rule 7 pins each new
    # one whole by its golden. The same line is the Swift side's input in Rule 8.

Scenario: a roster save that fails after the stash landed names nothing
  Given a menu-bar capture whose credential was stashed under the uuid B
    And the roster save then fails
   Then the acknowledgement is rejected with the reason failed, and carries no uuid, no label and no count
    And no credential_shared event is written for the capture
    # Covers the item's "Quiet writes and refusals" paragraph and AC `refusal-vocabulary-moves-as-one`
    # (the five tags `CaptureRejection` has today keep their bytes).
    # The daemon knows B and the label at this point and sends neither. A roster save failure maps to
    # the opaque `failed` in `classify_capture_failure` (src/daemon/classify.rs:167). Detection runs
    # after the roster is saved (design D-4), so a failed save compares nothing. The stash is an
    # orphan, which is F10's and not this item's.

Scenario: a refused capture compares no credential and names no shared one
  Given a roster A:work, whose stash holds the credential K
    And the signed-in account is B, which the roster does not hold, and its credential is K
   When a menu-bar capture asks for work
   Then the acknowledgement is rejected with the reason label-in-use, and carries no shared_with
    And no credential_shared event is written for the capture
    # Covers AC `oracle-socket-capture-refuses-a-collision`, its last sentence. Detection runs after the
    # roster is saved (design D-4) and the refusal comes before any write, so nothing is compared. No
    # pair is lost: B's credential was never stashed, so no second entry holds K. A rename refusal
    # compares nothing for the same reason.
```

## Rule 6 — the acknowledgement carries no credential, email or credential-derived value

> REQ-NTC-C-001, REQ-STSH-C-001, issue #15. The acknowledgement names accounts by label and by uuid.
> A uuid is an identifier and not a secret: `config-get` already returns each account's uuid and
> label on the same socket (`view`, src/config/settings.rs:65). Nothing derived from the credential's
> bytes reaches the wire, not a digest, not a fingerprint, not a length. The redaction meter is
> test-only (`meter`, src/redaction.rs:91).

```gherkin
Scenario: the filled goldens pass the redaction meter
  Given the committed golden of each variant, every field filled
    And the operator's labels, an email-shaped one among them, as the authored-label allow-set
   When the meter scans each serialized line
   Then it finds no token prefix, no credential bytes, no blob fingerprint, no unauthored email and no high-entropy run
    # Covers AC `no-email-no-token-no-digest`, first sentence. The existing captured-ack test applies
    # `unauthored_emails` (src/redaction.rs:324) to its wire line; `scan` (src/redaction.rs:190) adds the
    # credential fingerprint and the entropy backstop.

Scenario: an operator-authored email-shaped label passes and an unauthored email fails
  Given a roster whose label for A is an email-shaped string the operator authored
   When the meter scans an acknowledgement that names that label
   Then it finds nothing when the label is in the allow-set
    And it finds an unauthored email when the allow-set is empty
    # Covers AC `no-email-no-token-no-digest`, its second sentence.
    # Issue #444: an authored label is a permitted value and an unauthored email is a leak. The
    # harvested email is never an input to any field of the acknowledgement (design D-3).

Scenario: a digest of the credential in the position of a shared_with uuid is flagged
  Given a filled acknowledgement whose shared_with uuid is replaced by a digest of the credential K
   When the meter scans it with K as the secret
   Then it flags the line
    # Covers the AC's last sentence. The check has to be able to fail: a digest is the leak REQ-STSH-C-001
    # forbids, and a test that never plants one cannot say the meter would catch it. The backstop
    # flags an alphanumeric run of `ENTROPY_MIN_RUN` characters (src/redaction.rs:177) at 3.5 bits each.

Scenario: a hyphenated uuid is not one long alphanumeric run
  Given an acknowledgement whose uuids are hyphenated uuids
   When the meter scans it
   Then its entropy backstop finds nothing
    # Covers AC `no-email-no-token-no-digest`, its third sentence.
    # The longest alphanumeric run in a hyphenated uuid is 12; the backstop starts at 20
    # (`ENTROPY_MIN_RUN`, src/redaction.rs:177). An unhyphenated uuid would trip it, and validation accepts one
    # (`account_uuid_violation`, src/config/validate.rs:525). The fixtures use hyphenated uuids, as real ones are.
```

## Rule 7 — the Rust side pins the wire form with one golden per variant

> Design D-10, AC `wire-shape-and-golden`. The precedent is the config-set reject-detail golden: its
> emitter is `emit_config_set_reject_details_golden` (src/config/settings.rs:737) and its pin is
> `the_committed_config_set_reject_details_still_match_this_crate` (src/config/settings.rs:748), which reads the file through
> `include_str!`. Whichever of this item and #1558 lands first creates the shared emitter, and the
> second adds its golden. The item suggests the emitter name `emit_wire_ack_golden_fixtures` and
> the files `build/fixtures/wire-capture-ack-captured.json` and
> `build/fixtures/wire-capture-ack-refreshed.json`, plus one golden per refusal this item adds,
> `build/fixtures/wire-capture-ack-rejected-label-in-use.json` and
> `build/fixtures/wire-capture-ack-rejected-relabels-another-account.json`.

```gherkin
Scenario Outline: an optional key is omitted when it is empty, never sent as null or as an empty list
  Given a <variant> acknowledgement with nothing to report beyond its uuid
   When it is serialized
   Then the line carries result, label, count and account_uuid, and no other key

  Examples:
    | variant   |
    | captured  |
    | refreshed |
    # Covers AC `wire-shape-and-golden`, "the optional keys only when non-empty", and the serde seam
    # of AC `quiet-writes-stay-quiet`. An older client ignores an unknown key but not a `null` it
    # cannot decode, and the empty list is a claim ("no shared credential") the omitted key does not make.

Scenario: the pin fails when a committed golden differs from what the crate produces
  Given the committed golden of a variant
   When one byte of it differs from the line the crate produces for the filled value
   Then the pin test fails
    # Covers AC `wire-shape-and-golden`; RED is owed for the pin ("it fails on a drifted golden").

Scenario: a deleted golden is a build error and not a skipped test
  Given the pin reads each golden through `include_str!`
   When a committed golden is deleted
   Then the crate does not build
    # Covers AC `wire-shape-and-golden` through the Specification's pin paragraph: "so a deleted file
    # is a build error".

Scenario: a field added to a variant later is a compile error until the builder fills it
  Given the builder of each filled value names every field and uses no `..` rest pattern
   When a field is added to the variant
   Then the crate does not build until the builder fills it
    # Covers AC `wire-shape-and-golden` through the Specification's golden paragraph: "so a field
    # added later is a compile error until the builder fills it".

Scenario: only the named ignored emitter writes a golden
  Given a plain test run
   Then no golden is written or changed
    And running the emitter by name regenerates every golden this item adds
    # Covers AC `wire-shape-and-golden`: the goldens "are written only by the named ignored emitter".
    # Never run the suite with `--include-ignored`: it rewrites a frozen migration fixture
    # (`build/fixtures/migration-v1-encrypted.json`).
```

```gherkin
Scenario: the filled goldens carry the item's fields, and none carries a collision or a previous label
  Given the captured variant and the refreshed variant
   When their goldens are emitted
   Then the captured line carries account_uuid and shared_with, each filled
    And the refreshed line carries account_uuid and shared_with, each filled
    And no golden carries collisions or previous_label
    # Covers AC `wire-shape-and-golden` and the Specification's golden paragraph ("every optional
    # field filled"). Since the rulings, no completed capture carries a collision (Rule 2) or a
    # previous label (Rule 3), so both variants carry the same two keys. The golden is the exact line
    # the daemon writes, so the keys it holds are the keys the decoder must read (Rules 8 and 9).

Scenario Outline: each new refusal has a golden that is its whole line
  Given the refusal <tag>
   When its golden is emitted
   Then the golden holds the line {"result":"rejected","reason":"<tag>"} and nothing else

  Examples:
    | tag                      |
    | label-in-use             |
    | relabels-another-account |
    # Covers AC `wire-shape-and-golden` and AC `refusal-vocabulary-moves-as-one`. Pinned whole, as
    # `the_prior_configuration_tag_is_redacted` (src/daemon/socket.rs:2035) pins its own tag, because
    # the wire form is what a client sees: no label, uuid or count
    # (docs/specs/capture-rejection-parity.feature.md, Rule 2). The Swift side decodes each in Rule 8.
```

## Rule 8 — the app reads every fact, tolerates its absence, models both refusals and keeps the vocabulary closed

> Design D-10 and § 8.2, ACs `swift-decodes-values`, `refusal-vocabulary-moves-as-one` and
> `refusal-card-sentence-and-mock-row`, and R-12 of docs/requirements/roster-loss-prevention.md
> (user-ratified): a new refusal reason is present in the Rust `CaptureRejection`, in the Swift
> mirror and in the panel's authored capture-states reference, in the same change. The decoder is
> `CaptureAck.decode` (apps/menubar/Sources/CaptureAck.swift:92), pure. It probes `result`, then
> decodes a private body, and it drops unknown sibling keys. It exposes each fact on the `captured`
> and the `refreshed` outcome, whatever shape #1474 gives `refreshed`. No scenario here names a Swift
> accessor for the facts; the two refusal cases take the names fixed with their tags, `labelInUse`
> and `relabelsAnotherAccount`. The Swift test locates each golden from its own `#filePath`, as
> `ConfigSetRejectDetailParityTests` (apps/menubar/Tests/ConfigSetRejectDetailParityTests.swift:152) does.

```gherkin
Scenario Outline: the app reads the value the golden carries for <field>
  Given the committed <variant> golden
   When the app decodes it
   Then the decoded outcome carries <value>
    And that value is the golden's own and not an empty one

  Examples:
    | variant   | field        | value                                                |
    | captured  | account_uuid | the golden's uuid                                    |
    | captured  | shared_with  | each element's label and uuid, in the golden's order |
    | refreshed | account_uuid | the golden's uuid                                    |
    | refreshed | shared_with  | each element's label and uuid, in the golden's order |
    # Covers AC `swift-decodes-values`. A decode that merely does not throw proves nothing: unknown
    # keys are dropped silently, so the test asserts each value.

Scenario Outline: an acknowledgement from an older daemon decodes to empty facts
  Given the line an older daemon sends: <line>
   When the app decodes it
   Then it does not throw
    And the decoded outcome carries the label work, no uuid and no shared credentials

  Examples:
    | line                                            |
    | {"result":"captured","label":"work","count":3}  |
    | {"result":"refreshed","label":"work","count":3} |
    # Covers AC `swift-decodes-values`, its last sentence. The `refreshed` row can be decoded once #1474
    # gives the app a refreshed outcome. The lines carry `count`, as the daemon sends it today. The
    # app reads `accounts` and drops `count` (`CapturedBody`, apps/menubar/Sources/CaptureAck.swift:145), and this item leaves that alone.

Scenario: the golden's keys are exactly the keys the Swift model reads
  Given the committed golden of each variant
   When its top-level keys are compared with the keys the Swift model reads for that variant
   Then the two sets are equal, apart from the known pair count and accounts
    # Covers AC `swift-decodes-values`, its second sentence, which as the item words it cannot be met:
    # "equals the key set the Swift model reads" fails today, because the golden carries `count` and
    # the model reads `accounts`, and the item itself says whole-body equality fails on that pair.
    # Read it as equality over this work's fields, with the pair named as an exemption and left
    # unfixed. The precedent is `testTheGoldensKeysAreExactlyTheRejectDetailsFields` (apps/menubar/Tests/ConfigSetRejectDetailParityTests.swift:297), which also says what that
    # comparison does not prove. A golden key the model does not read is a field that decodes clean
    # and reaches nobody.

Scenario Outline: the closed vocabulary still throws on a value the app does not model
  Given the line <line>
   When the app decodes it
   Then it throws as unrecognized

  Examples:
    | line                                              |
    | {"result":"future_state"}                         |
    | {"result":"rejected","reason":"future-reason"}    |
    # Covers AC `refusal-vocabulary-moves-as-one`. `testDecodeUnknownResultThrows` and
    # `testDecodeUnknownRejectionReasonThrows` pass without edits: the vocabulary grows by two tags and
    # stays closed. `CaptureRejection` gains `label-in-use` and `relabels-another-account` with their
    # Swift cases in the same commit, and
    # `every_rust_rejection_tag_has_a_swift_case` (src/daemon/socket.rs:1979) gates them with no edit
    # to its own body; `tagged_rejections` (src/daemon/socket.rs:1764) gains the two variants in its
    # list and the two arms its `match` asks for, and
    # `testDecodeAllRejectionReasons` gains the two rows its `CaptureRejection.allCases` count asks for.

Scenario: the daemon's result vocabulary stays three values
  Given one acknowledgement of each variant the daemon can build
   When their result values are collected
   Then they are captured, refreshed and rejected
    And no relabel, collision or shared credential adds a fourth
    # AC-IDN-004's BUT NOT: "a new `result` value for the relabel, which today's decoder would throw
    # on". It holds under the rulings: a refused relabel is a `rejected` with its own reason, never a
    # fourth result. The `result` vocabulary's own parity gate is #1474's.

Scenario: a captured acknowledgement still ends the capture in its done phase with the same label
  Given a menu-bar capture whose acknowledgement is captured under the label work
   Then the model's phase is done with the label work
    And the model's phases, and the done line's copy, are unchanged
    # Covers the item's "`AccountCaptureModel` keeps its phases". The edit to the match in
    # `capture` (apps/menubar/Sources/AccountCaptureModel.swift:148) is mechanical. The only copy this
    # item adds is the two refusal sentences of the next outline. What the card does with the facts is
    # #1561's.
```

```gherkin
Scenario Outline: each new refusal decodes to its own case and reaches the card as its own sentence
  Given the line {"result":"rejected","reason":"<tag>"}
   When the app decodes it
   Then the decoded outcome is the refusal <case>, and decoding does not throw
    And the capture ends in its failed phase with that refusal
    And the card's sentence for it differs from every other capture failure's, names no command and does not say "try again"
    And that sentence carries no token or phrase that src/framing_vocabulary.rs bans, the list read from that file when the test runs
    And that sentence says <reason>, and names no label

  Examples:
    | tag                      | case                   | reason                                                   |
    | label-in-use             | labelInUse             | another account already carries the label asked for      |
    | relabels-another-account | relabelsAnotherAccount | the account is already on the roster under another label |
    # Covers AC `refusal-vocabulary-moves-as-one` and AC `refusal-card-sentence-and-mock-row`. The
    # decoder fails closed on an unknown reason, so a tag that lands in Rust alone breaks the capture
    # button rather than degrading it. The sentence lives in
    # `StatusPanelFormat.captureErrorText` (apps/menubar/Sources/StatusPanelFormat.swift:53), whose
    # switch has no default arm, so the compiler asks for it, and `PanelCaptureCardTests` measures it against the card. Both refusals are
    # stable, so a retry refuses the same way, which is why neither sentence invites one (the reason
    # #1441 gave its own tag). A remedy on the panel is an act or a launch path, never text naming a
    # command (docs/requirements/gui-cli-capability-parity.md, R-4 and R-5). The tag carries no label,
    # so the sentence names none, and no line of the card names the label the landed account carries
    # now, which the panel cannot know for certain (design § 12.3, C-6). Which rows carry the label
    # asked for, and the card's act and launch paths, are #1561's. The wording is not pinned here.
    # The framing step is C-9's binding (design § 12.3) and REQ-NTC-C-002's. The test reads
    # `BANNED_TOKENS` and `BANNED_PHRASES` from src/framing_vocabulary.rs when it runs, through
    # `FramingVocabularyScan` (apps/menubar/Tests/NotificationDeliveryTests.swift:198), whose parse
    # throws rather than return an empty list, and scans the sentence under the whole list with no
    # exemption set, as the notification screen in that file does. A copy of the list typed into the
    # test would be the second definition issue #1219 retired, which no Rust change could redden.

Scenario: the panel's capture-states reference authors a row for each new refusal
  Given apps/menubar/design/menubar-preview.html after the change
   When its two capture-states frames are read, light and dark
   Then each holds one Refused row per new refusal
    And each row carries the words of that refusal's card sentence, with the treatment of the existing Refused row
    # Covers AC `refusal-card-sentence-and-mock-row` and R-12's "the panel's authored capture-states
    # reference". The precedent is #1441, whose change added the prior-configuration row to both
    # frames and recorded the panel's divergences from it in apps/menubar/design/README.md
    # (docs/specs/capture-rejection-parity.feature.md, Rule 3). The two rows lengthen both frames, and
    # so the page, which is already past the render cap (the scope brief's `render-height-budget`).
    # Like #1441's, this change edits the HTML and does not regenerate renders/all-states.png; a
    # whole-page render waits on `mock-page-restructure`.
```

## Rule 9 — a field added on one side only fails the `test` job

> Design D-10, AC `field-gate-bites`. The model is `every_rust_rejection_tag_has_a_swift_case` (src/daemon/socket.rs:1979).
> Its Rust side is proven against serde and not asserted
> (`rust_wire_tags`, src/daemon/socket.rs:1793). Its Swift side is parsed strictly from source text and panics on a shape
> it cannot parse (`swift_rejection_tags`, src/daemon/socket.rs:1882), reading the one file the decoder lives in
> (`swift_capture_ack`, src/daemon/socket.rs:1866). A canary proves the extraction finds what it
> should. The list holds this work's fields: `account_uuid` and `shared_with` here, and
> `collisions` on the `config-set` refusal in #1558, which carries it on `rejected` since the
> rulings. Whichever item lands first creates the gate and its helpers, and the second adds its
> fields. The two refusal tags are not fields: `every_rust_rejection_tag_has_a_swift_case` gates them.

```gherkin
Scenario: a listed field that the Swift decoder does not read fails the gate
  Given the gate lists shared_with
   When the key the Swift decoder reads for shared_with is renamed
   Then the gate fails, and names shared_with
    # The mutation the item asks to record: rename the key, run the gate, show it fails, restore it.
    # Covers AC `field-gate-bites`, first mutation.

Scenario: a listed field that the daemon does not send fails the gate
  Given the gate lists a field that no acknowledgement the daemon builds carries
   When the gate runs
   Then it fails, and names that field
    # Covers AC `field-gate-bites`, second mutation. The Rust side is read from what serde emits for
    # a filled value, not from a list of names, so a field removed from the Rust type is caught as well.

Scenario: the gate's extraction finds the keys it should
  Given the current apps/menubar/Sources/CaptureAck.swift
   When the gate extracts the keys the decoder reads
   Then the extraction is not empty, and it includes label
    # Covers AC `field-gate-bites`: "The gate's extraction is proven non-empty by its canary".
    # A moved delimiter, or keys moved out of this file, would give an empty set, which compares equal
    # to nothing and passes. A shape the parser cannot read must panic. The keys compared are the
    # spellings the decoder matches against the JSON, never a Swift property name: a camelCase
    # property decoded without a conversion would read a key Rust never sends, decode as absent, and
    # pass every test that does not assert a value.

Scenario: the gate fails in the job that can fail
  Given a field the daemon sends that the Swift decoder does not read
   When CI runs
   Then the `test` job fails
    And `panel-goldens` is not what reports it
    And an edit to apps/menubar/Sources/CaptureAck.swift alone still runs the `test` job
    # `ci-ok` counts a skipped job as a pass. `apps/menubar/Sources/CaptureAck.swift` and
    # `build/fixtures/**` are both in the `rust` path filter of .github/workflows/ci.yml, so this
    # item owes no edit to it. Covers the item's first conformance criterion: emit the Rust goldens,
    # decode them in Swift, compare every field's value, and show that an unmodelled field fails
    # the `test` job and not `panel-goldens`.

Scenario: the drift that exists today does not fail the gate
  Given the daemon sends count and the Swift body reads accounts
   When the gate runs over this work's fields
   Then it passes
    # Covers the Specification's gate paragraph ("Whole-body equality is out of scope") under AC
    # `field-gate-bites`, and the item's Out of Scope bullet on the `count` against `accounts` mismatch.
    # Whole-body equality is out of scope because it fails today. The gate's subject is the named
    # list, and that pair is not on it.
```

## Red cards — the questions that no scenario above can close

This pass re-authored the two Red cards this section held, `duplicate-label-policy` and `login-mismatch-policy`, into the refuse-form scenarios of Rules 1, 2, 3, 5, 7 and 8 after the operator ruled both on 2026-11-03 (PRD § 8.1); no Red card remains.

## Calls this stub took

Each is a call made without the operator, and each is ratification-pending.

| Call | Why | If the answer is otherwise |
|---|---|---|
| DQ-6 is closed: the acknowledgement carries `shared_with` | The design's recorded default (§ 14.8) agrees with REQ-STSH-B-002 (Must) and with the operator's parity ruling of 2026-07-31 (`docs/requirements/gui-cli-capability-parity.md`) | Rule 4 and every `shared_with` row in Rules 5 to 9 go, and the goldens lose a key |
| The key-set comparison in `swift-decodes-values` is read as equality over this work's fields, with `count` and `accounts` a named exemption | The golden carries `count`, the model reads `accounts`, and the item says whole-body equality fails on that pair today | None. It is a reading of a criterion that cannot be met as worded; the item's wording needs the scope |
| "Every quiet scenario in that file" is read as every one a socket capture can reach | One capture writes one entry, so the swap has no counterpart; #1558 words the same clause with that scope | None. The item's wording needs the scope |
| No scenario names a Swift accessor for the decoded facts, or a Swift case for `refreshed` | #1474 has not chosen the shape; the item says "whatever shape #1474 gives". The two refusal cases are named, because their names were fixed with their tags | None |
| The derived-label outline is asserted at the socket and owes no RED here | The orchestrator's reading puts the free-suffix rule in the shared capture plan, which #1556 changes, so the rule is in the tree before this item starts | If #1556 lands without the suffix, its gate refuses a colliding derived label, which REQ-LGN-B-003 forbids, and this item waits for that to be settled |
| A refused capture's audit line names the label the request carried | `capture_failure` already builds every refusal's line from the request's label hint, and the event's token follows the tag, as `prior_configuration` follows `prior-configuration` | A refusal that found the account could name its current label instead; only the two audit steps change |
| "No key on the request lifts a refusal" is read as: the key is ignored, as today's parse ignores it | The socket accepts no consent, and the parse already drops the key, so the reading costs no change | A strict re-parse would answer `malformed request` instead; that scenario's Then changes and the refusals do not |
| Each new refusal has a golden of its whole line | The orchestrator's brief for this re-authoring asks for one golden per new refusal, and the wire form is what the panel sees | The two goldens go, and the whole-line pin moves to a unit test like `the_prior_configuration_tag_is_redacted` |
| The two refusal sentences are pinned by property, not by wording | R-12 asks this item for one sentence per tag, while the wording of card lines is #1561's | If the wording is pinned here, #1561's refusal presentation edits a pinned value |
| The framing scan of the two refusal sentences takes `BANNED_TOKENS` whole, with no exemption set | REQ-NTC-C-002 asks for the audience that matches the string's register; the notification screen, the one audience on the Swift side, takes the whole list, and no `captureErrorText` sentence spends a banned word today | An exemption set would let its tokens through; only the token list the scan passes changes |
| The mock rows land without a regenerated render | #1441's row did the same, and the page is already past the render cap | If the render must move with the rows, the item waits on `mock-page-restructure` |

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from something the
item does not spell out.

- The key order within an acknowledgement line. The golden fixes it once it is emitted.
- The names of the new tests, beyond the ones the item suggests, and the file that holds the pin.
- The Swift type that carries the facts: one payload shared by both outcomes, or one per case.
- The bytes of a `credential_shared` event. Those are #1557's, and its stub pins them.
- Which roster `shared_with` is read from when the daemon refuses a shrinking reload after the
  save. The report carries the roster that was written, and `count` is the one
  value the handler reads back from the live roster (`self.roster.len()`, src/daemon/commands.rs:416). #1558 says the same for `config-set`.
- The wording of the two refusal sentences, beyond the properties Rule 8 names, and the wording of
  any other card line, which is #1561's.
- The names of the two new `CaptureEventOutcome` variants. Their tokens follow the tags,
  `label_in_use` and `relabels_another_account`, and the audit steps assert the tokens.

## Acceptance criteria that no scenario expresses

- *Existing tests*: `testDecodeCapturedWithLabelAndCount`, `testDecodeCapturedWithoutCount` and
  `testCapturedAckModelsOnlyLabelAndCount` (apps/menubar/Tests/AccountCaptureTests.swift:67) change only by the shape of the
  decoded value, and the existing Rust tests that build or match the changed variants change only by the
  added fields: `perform_socket_capture_captures_the_active_account_and_reconciles_the_roster`,
  `perform_socket_capture_refreshes_an_already_rostered_account_without_a_duplicate_row`,
  `a_capture_onto_a_diverged_file_reports_the_live_count_not_the_saved_one`,
  `perform_socket_capture_still_onboards_a_genuine_first_run` and
  `perform_socket_capture_admits_a_present_config_whatever_the_witness_says` in `src/daemon/commands.rs`, and
  `run_loop_performs_an_authenticated_capture_and_writes_a_redacted_ack` and
  `redaction_meter_emits_no_secret_on_any_channel_across_the_full_loop` in `src/daemon.rs`.
- *Malformed new key*: `malformed-new-key-keeps-the-outcome` pins that a line whose new key is present but
  malformed decodes to its outcome with that fact empty (seam `CaptureAck.decode`, RED owed); no scenario
  here expresses it.
- *Vocabulary bookkeeping*: under AC `refusal-vocabulary-moves-as-one`, `testDecodeAllRejectionReasons`
  gains the two rows its `CaptureRejection.allCases` count asks for, `PanelCaptureCardTests`' `captureFailures`
  gains the two rows `testEveryCaptureFailureIsEnumerated` asks for, and
  `capture_event_outcome_rejected` (src/daemon.rs:6402) gains the two arms its `match` asks for.
- *Stale restatements*: `stale-restatements-updated` names the comments that still say a capture
  acknowledgement carries only the label and the count, and the Swift `CaptureRejection` doc that counts
  its tags; its check is a `git grep` that prints nothing.
- *Mutation records*: the two mutations in Rule 9, recorded in the pull request.
- *RED records*: the item owes a failing run, recorded on the issue, for each oracle. See the next
  section.
- *Local gates*: the pre-push gates in `CLAUDE.md`, plus `msrv` and both `deny` checks because the
  change is under `src/**`, plus the `swift` job and an armed `panel-goldens` run that prints its
  `[panel-goldens] max drift` line. A run that says tests were skipped compared nothing.

## Where each group is asserted

Taken from the acceptance criteria, for whoever binds the scenarios.

- Rules 1 to 5: `perform_socket_capture` (src/daemon/commands.rs:280) over the hermetic daemon, as
  the two existing tests build it, with a temporary config path, a stash fake seeded with `stash_with`,
  and the serde form of `CaptureAck` (src/daemon/socket.rs:656). Rule 4 also reads the events the
  handler returns. The fixtures write one roster to the file and to the daemon, as those tests do: a
  refusal is judged against the roster the handler reads from `config.toml` before the lock. Rule 2's
  override scenario is asserted at the request parse in `serve_control` (src/daemon/socket.rs:904).
- Rule 6: the redaction meter over the serialized goldens.
- Rule 7: the serde form, the emitter and the pin, in the `test` job, for both variants and both new
  refusals.
- Rule 8: Swift unit tests over `CaptureAck.decode`, which is pure, reading the goldens from disk; over
  `StatusPanelFormat.captureErrorText`, which is pure, with `PanelCaptureCardTests` measuring each
  sentence against the card and a framing scan reading src/framing_vocabulary.rs when it runs; and
  the mock's two capture-states frames, read as text.
- Rule 9: a Rust test in the `test` job that reads `CaptureAck.swift` as text.

## Oracle authorship

*Work class*: deterministic-output. The primary deliverable is a wire frame, the serialized
acknowledgement and its two new refusals, with byte-pinned goldens and a decoder over them. A golden
compare, a value assertion on the serde form and a value-asserting decode each falsify a wrong output
mechanically.

*Authorship*: no oracle item is filed yet. Since the operator ruled both policies, every outcome here
is decided, and the PRD's "fixes the assertion form of REQ-ROS-C-002's oracles" now reads as the
refusal. Two seams still hold the oracle back. The Swift assertions on `refreshed` go through the
outcome #1474 gives the app, and Rule 4's agreement compares the acknowledgement with the
`credential_shared` events #1557 adds. The capture plan's gate (#1556) is no longer an authoring
seam: every refusal and quiet assertion is made at `perform_socket_capture` over the serde form,
which compiles before #1556 lands. The refusal assertions fail until this item maps the refusals, and
the quiet ones pass before and after. The derived-label outline is #1556's rule seen through the
socket: it passes once #1556 lands and owes no RED here. The one oracle item is filed once #1474 and
#1557 have landed, and before this item starts.

For whoever files it:

- The new fields do not exist on `CaptureAck` today (src/daemon/socket.rs:656), so a test written
  before the change cannot build the filled value. Assert on `serde_json::to_value` of the
  acknowledgement: it compiles before the change and fails at runtime.
- `perform_socket_capture` (src/daemon/commands.rs:280) returns `(CaptureAck, Option<Event>)` today, and #1557 changes the
  second element to a `Vec<Event>`. An oracle over the events, Rule 4's agreement, can only be
  authored after #1557 lands.
- The Swift scenarios on `refreshed` wait for #1474, which gives the app a `refreshed` outcome. The
  two refusal cases do not exist before the change either: a Swift RED test reaches them through
  `CaptureRejection(rawValue:)` under `XCTUnwrap`, which compiles before the change and fails until
  the case exists.
- A refusal test asserts the tag and that nothing was written: the stash fake's entries, the bytes
  of `config.toml` and the live roster. A test that asserts only `rejected` passes over the `failed`
  an unmapped refusal produces.
- A RED oracle lands `#[ignore = "RED oracle: pre-authored ahead of its fix (issue #1559), which
  un-ignores it"]`, because `ci-ok` cannot merge a failing suite and `main-protection` has no bypass
  actor. It keeps a non-ignored companion on the same fixture and proves it discriminates by
  applying the fix in a scratch copy and mutating each guarded invariant. A Swift RED test lands in
  one of the two forms the open decision `swift-red-landing-form` names: a strict `XCTExpectFailure`
  tagged `[RED #1559]` whose `issueMatcher` matches on `contains`, or `try XCTSkipIf(true, "<reason>")`.
