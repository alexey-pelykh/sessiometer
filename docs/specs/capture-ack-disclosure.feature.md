# Feature: the capture acknowledgement says which account a write touched, what it collided with, which label it replaced and which credential it shares

Issue #1559 · PRD REQ-ROS-I-001 / REQ-ROS-C-001 / REQ-IDN-I-001 / REQ-STSH-B-002 / REQ-NTC-C-001, AC-ROS-003 / AC-IDN-004 / AC-STSH-001 · design D-3, D-4, D-10, § 8.1, § 11.5

Example Mapping: 🟦 9 rules · 🟩 41 examples · 🟥 2 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests
> on is unratified and its problem framing is unvalidated. The field names `account_uuid`,
> `collisions`, `previous_label` and `shared_with`, and their element shapes, are pipeline defaults
> that wait for ratification too. Each scenario waits to be bound in #1559's execution; none is a
> test yet. The stub maps the item as filed: the socket `capture` acknowledgement and the Swift
> decoder of it. The CLI notices are #1556, shared-credential detection is #1557, the `config-set`
> acknowledgement is #1558, and the menu-bar presentation is #1561 and #1560. An example is a
> scenario or an outline row.
>
> The Blue Card Gate trips: the item carries more rules than one item should. This stub does not
> split it. The rules are numbered so that a split has somewhere to cut. Rules 1 to 6 are what the
> daemon sends. Rule 7 is the Rust-side byte pin. Rules 8 and 9 are the Swift decoder and the gate
> between the two sides.
>
> A scenario marked 🟥 hangs on a question that has no answer yet. It names the question's handle
> and writes no outcome as decided; the item's own default sits in the comment beside it. The 🟩
> count is the examples that carry a decided outcome; the 🟥 ones are not in it. Both questions are
> operator decisions: `duplicate-label-policy` (warn or refuse when a write makes a label name a
> second account) and `login-mismatch-policy` (what a write does when it gives an already-rostered
> account a different label). Both are tabled at the end.
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
> for whole uuids. *The signed-in account* is the account the daemon harvests: the identity in
> `~/.claude.json` and the credential in the canonical keychain item. A *menu-bar capture* is the
> socket `capture` command; it *asks for work* when it carries that label and *asks for no label*
> when it carries none. K names a credential; no scenario quotes one. *The written entry* is the one
> roster entry a capture creates or relabels. A *bearer* of a label is a roster entry that carries
> it. *Quiet* means the acknowledgement carries `account_uuid` and none of `collisions`,
> `previous_label` and `shared_with`.
>
> The traps in this feature:
> - A decode that does not throw proves nothing. The Swift decoder drops an unknown sibling key
>   without a sound, and a key it reads under the wrong spelling decodes as absent, which it
>   tolerates. A field added in Rust alone reaches nobody and fails no test. Rules 8 and 9 assert
>   values and key sets for that reason.
> - A gate must live where it can fail. `panel-goldens` is soft: every step is `continue-on-error`,
>   so it always reports pass. Rule 9 puts the gate in the `test` job.
> - A bearer list with the written entry first reads like the CLI's notice and is wrong here. The
>   acknowledgement lists the bearers in roster order and marks the written entry by `account_uuid`.
> - A report on a write that created nothing trains the operator to dismiss the one that matters.
>   Rule 5's quiet rows are import's own fixtures.
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

🟥 Blocked on `login-mismatch-policy`. The scenario below waits for the operator's answer.

```gherkin
Scenario: 🟥 a relabel leaves the uuid where it was
  Given a roster B:spare
    And the signed-in account is B
   When a menu-bar capture asks for work
   Then the roster on disk holds B under the label work, and holds no other uuid
    And (pending `login-mismatch-policy`: whether the relabel completes at all. If it does, the acknowledgement is refreshed with account_uuid B. If the policy refuses it, there is no such acknowledgement)
    # Covers the invariant's third case, "a relabel". Capturing the signed-in account again under a
    # new label is the path `recapture_updates_the_label_when_a_new_one_is_given` pins as a feature,
    # and design § 8.2 says a refuse answer closes that path for the socket.
```

## Rule 2 — a write that makes a label name a second account says so in the acknowledgement

> REQ-ROS-I-001, REQ-ROS-B-001, REQ-ROS-B-002, design D-1 and § 8.1. The rule is import's: a write
> creates a collision on a label when the label has more than one bearer after the write and more
> bearers than it had before (`created_ambiguity`, src/cli.rs:5425, over the counts of
> `label_bearers`, src/cli.rs:5245). #1556 moves it beside `Account` as `label_collisions`. The
> acknowledgement carries one element per collided label, `{label, bearers}`. The bearers are the
> uuids of every entry that bears the label after the write, in roster order, the written entry
> among them. One capture writes one entry, so it collides at most one label. The list form is
> shared with `config-set` (#1558), where several labels can collide at once. The form of the
> report is `duplicate-label-policy`'s, so the loud scenarios wait and the quiet ones do not.

```gherkin
Scenario: the account that already bears the label is not a collision
  Given a roster A:work
    And the signed-in account is A
   When a menu-bar capture asks for work
   Then the acknowledgement is refreshed with account_uuid A
    And the acknowledgement is quiet
    # Covers AC `oracle-socket-capture-reports-a-collision`, its second sentence: "It must not report
    # when the captured uuid is A itself". One bearer before and one after, so the count did not
    # rise. AC-ROS-001 names a report here as a BUT NOT. This is also AC `quiet-writes-stay-quiet`'s
    # "relabel to the label the entry already carries", so Rule 5's table repeats it only for a
    # two-bearer roster (its third row).
```

🟥 Blocked on `duplicate-label-policy`. The scenarios below wait for the operator's answer.

```gherkin
Scenario: 🟥 a new account under a label another account bears
  Given a roster A:work
    And the signed-in account is B, which the roster does not hold
   When a menu-bar capture asks for work
   Then the acknowledgement of this write is not silent about the collision on work
    And (pending `duplicate-label-policy`: the form of the report, a collision carried by a completed captured acknowledgement or a refusal before any write)
    # The item's default is warn: captured, account_uuid B, and one collision, the label work with the
    # bearers A and B in roster order. Under refuse the write stops before the stash is written, so
    # nothing is stashed and nothing is rostered; a refusal after the stash write would leave an
    # orphan stash (AC-ROS-001, BUT NOT). The acknowledgement is then a `rejected` one. By Rule 2 of
    # docs/specs/capture-rejection-parity.feature.md it carries a redacted machine code and no label,
    # so, unless that rule is amended, the app would learn that a label is in use and not which one.
    # Refuse also needs AC-6 of the migration PRD re-ratified ("BUT NOT by enforcing label
    # uniqueness"), a `label-in-use` tag on `CaptureRejection` (src/daemon/socket.rs:672), and its
    # Swift case in the same commit, which `every_rust_rejection_tag_has_a_swift_case` (src/daemon/socket.rs:1979) gates.
    # Covers AC `oracle-socket-capture-reports-a-collision`; the item suggests the name
    # `a_socket_capture_that_makes_a_label_name_a_second_account_says_so_in_its_ack`.

Scenario: 🟥 a relabel onto another account's label
  Given a roster A:work, B:spare
    And the signed-in account is B
   When a menu-bar capture asks for work
   Then the acknowledgement of this write is not silent about the collision on work
    And (pending `duplicate-label-policy`: the form of the report)
    And (pending `login-mismatch-policy`: whether the relabel completes, and whether the acknowledgement names spare as the label B carried)
    # The item's default: refreshed, account_uuid B, previous_label spare, and one collision, the
    # label work with the bearers A and B. Covers AC `oracle-socket-refresh-reports-the-previous-label`.

Scenario: 🟥 a third account joins a label two accounts already bear
  Given a roster A:work, B:work
    And the signed-in account is C, which the roster does not hold
   When a menu-bar capture asks for work
   Then the acknowledgement of this write is not silent: work now names three accounts
    And (pending `duplicate-label-policy`: the form of the report)
    # The item's default: bearers A, B and C. A report that fired only for a first duplicate would
    # swallow a worse one the operator has just made. This is import's "deepening" scenario
    # (docs/specs/import-duplicate-label.feature.md). Covers AC `oracle-socket-capture-reports-a-collision`
    # (the bearers) and the item's second conformance criterion (the bearer count import finds).

Scenario: 🟥 the bearers come back in roster order, with the written entry where it sits
  Given a roster A:work, B:spare, C:work
    And the signed-in account is B
   When a menu-bar capture asks for work
   Then the collision on work lists the bearers A, B, C in that order
    And account_uuid is B, so the app marks the written entry without it being listed first
    And (pending `duplicate-label-policy`: the form of the report)
    And (pending `login-mismatch-policy`: whether the relabel completes)
    # The CLI's notice puts the written entry first and marks it (design § 8.5). The acknowledgement
    # does not: design § 8.1 says the bearers' roster order gives each one its ordinal (UX-3). Covers
    # AC `oracle-socket-capture-reports-a-collision` ("bearers A and B in roster order").

Scenario: 🟥 the collision names what import's notice names
  Given a roster A:work
    And a menu-bar capture of the uuid B asks for work
   When import's duplicate-label notice is rendered for work, and the acknowledgement of that capture is read
   Then both name the label work
    And the collision lists as many bearers as `label_bearers` counts for work after the write
    And (pending `duplicate-label-policy`: the collision exists only if the form is a report on the completed write)
    # Import's notice is `duplicate_label_notice` (src/cli.rs:5628) and its count is `label_bearers` (src/cli.rs:5245).
    # The acknowledgement carries facts and never rendered prose (design D-3), so the facts compared
    # here are the label and the bearer count. The notice's cost-and-remedy text belongs to the CLI
    # and the card. Covers the item's second conformance criterion, by concretize-and-compare.
```

## Rule 3 — a refresh that renames an account names the label it replaced

> REQ-IDN-I-001, AC-IDN-004, design § 8.1. `previous_label` rides `refreshed` only, and only when
> the write changed the label of an already-rostered entry. A new account has none. Today
> `plan_capture` (src/capture.rs:635) overwrites the label in place, with `existing.label = l.to_owned()` (src/capture.rs:662),
> and returns the stash and the outcome only, so the label it replaced is lost; #1556 makes the plan
> return it. The Swift decoder cannot read `refreshed` at all today. That is #1474, which this item
> waits for, and which has not chosen the Swift shape of the outcome.

```gherkin
Scenario: a new account has no previous label
  Given a roster B:spare
    And the signed-in account is A, which the roster does not hold
   When a menu-bar capture asks for work
   Then the acknowledgement is captured with account_uuid A
    And the acknowledgement is quiet
    # AC-IDN-002's second half: "BUT NOT a previous-label disclosure, because a new account has
    # none". The key belongs to `refreshed`; no captured acknowledgement carries it.
```

🟥 Blocked on `login-mismatch-policy`. The scenario below waits for the operator's answer.

```gherkin
Scenario: 🟥 a refresh that gives the account a fresh label names the label it carried
  Given a roster B:spare
    And the signed-in account is B
   When a menu-bar capture asks for work
   Then the roster on disk holds B under the label work
    And (pending `login-mismatch-policy`: whether the relabel completes, and whether the acknowledgement is refreshed with the previous label spare, or a refusal)
    # The item's default: refreshed, account_uuid B, previous_label spare, and no collision, because
    # work was a fresh label. Covers AC-IDN-004's first Then. Whatever the answer, the relabel adds
    # no `result` value (Rule 8): a refusal is a `rejected` with a new reason, never a fourth result.
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

## Rule 5 — a write that created nothing says nothing, and a refusal is unchanged

> REQ-ROS-Q-001 (`CollisionReportPrecision`, goal 0) and the invariant "a write that creates no
> collision reports nothing". Quiet is a decided outcome under every option of both decisions:
> nothing is reported, refused or warned about when nothing was created, relabelled or shared. The
> fixtures are import's quiet ones (docs/specs/import-duplicate-label.feature.md), because a socket
> capture touches one entry. A `rejected` acknowledgement is unchanged: a redacted machine code and
> no uuid, label or count (Rule 2 of docs/specs/capture-rejection-parity.feature.md).

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
    # Row 1 is an ordinary new account under a fresh label: `after > 1` is what keeps it quiet. Rows 2
    # and 3 refresh an entry whose label already had two bearers, without and with the label given:
    # the count stays at two, so `after > before` is what keeps them quiet. Both clauses carry
    # weight, as import's stub shows. The third fixture the AC names, a relabel to the label the
    # entry already carries, is Rule 2's first scenario.

Scenario: every quiet scenario of the import stub that one capture can reach has a counterpart that stays quiet
  Given the quiet scenarios of docs/specs/import-duplicate-label.feature.md
   When each is restated as a menu-bar capture
   Then each is quiet
    # Covers the item's second conformance criterion, by concretize-and-compare rather than by reading.
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
    # Covers the item's "Quiet writes and refusals" paragraph. The reasons are the five tags of the enum
    # `CaptureRejection` (src/daemon/socket.rs:672): no-active-account, keychain-locked,
    # swap-lock-busy, failed and prior-configuration. One of them is already pinned whole by
    # `the_prior_configuration_tag_is_redacted` (src/daemon/socket.rs:2035). The same line is the Swift side's input in Rule 8.

Scenario: a roster save that fails after the stash landed names nothing
  Given a menu-bar capture whose credential was stashed under the uuid B
    And the roster save then fails
   Then the acknowledgement is rejected with the reason failed, and carries no uuid, no label and no count
    And no credential_shared event is written for the capture
    # Covers the item's "Quiet writes and refusals" paragraph and AC `vocabulary-gates-unchanged`
    # (`CaptureRejection` keeps its tags).
    # The daemon knows B and the label at this point and sends neither. A roster save failure maps to
    # the opaque `failed` in `classify_capture_failure` (src/daemon/classify.rs:167). Detection runs
    # after the roster is saved (design D-4), so a failed save compares nothing. The stash is an
    # orphan, which is F10's and not this item's.
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
> `build/fixtures/wire-capture-ack-refreshed.json`.

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
    # cannot decode, and the empty list is a claim ("no collision") the omitted key does not make.

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
    And running the emitter by name regenerates both goldens
    # Covers AC `wire-shape-and-golden`: the goldens "are written only by the named ignored emitter".
    # Never run the suite with `--include-ignored`: it rewrites a frozen migration fixture
    # (`build/fixtures/migration-v1-encrypted.json`).
```

🟥 Blocked on `duplicate-label-policy` and `login-mismatch-policy`. The scenario below waits for the operator's answer.

```gherkin
Scenario: 🟥 the filled goldens carry the item's fields
  Given the captured variant and the refreshed variant
   When their goldens are emitted
   Then each line carries every field its variant can carry, each filled
    And account_uuid and shared_with are among them
    And (pending `duplicate-label-policy`: whether collisions is among them. Warn puts it there; refuse moves the report to a rejection tag)
    And (pending `login-mismatch-policy`: whether previous_label is among them on refreshed. Refuse moves the report to a rejection tag)
    # Covers AC `wire-shape-and-golden` and the Specification's golden paragraph ("every optional
    # field filled").
    # The item's default: captured with account_uuid, collisions and shared_with; refreshed with all
    # four. The golden is the exact line the daemon writes, so the keys it holds are the keys the
    # decoder must read (Rules 8 and 9).
```

## Rule 8 — the app reads every fact, tolerates its absence and keeps the vocabulary closed

> Design D-10, ACs `swift-decodes-values` and `vocabulary-gates-unchanged`. The decoder is
> `CaptureAck.decode` (apps/menubar/Sources/CaptureAck.swift:92), pure. It probes `result`, then
> decodes a private body, and it drops unknown sibling keys. It exposes each fact on the `captured`
> and the `refreshed` outcome, and `previous_label` only where the outcome is refreshed, whatever shape
> #1474 gives `refreshed`. No scenario here names a Swift case or accessor. The Swift test locates
> each golden from its own `#filePath`, as `ConfigSetRejectDetailParityTests` (apps/menubar/Tests/ConfigSetRejectDetailParityTests.swift:152) does.

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
    And the decoded outcome carries the label work and no uuid, no collisions, no previous label and no shared credentials

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
    # Covers AC `vocabulary-gates-unchanged`. `testDecodeUnknownResultThrows` and
    # `testDecodeAllRejectionReasons` pass without edits. `CaptureRejection` keeps its five tags and
    # `every_rust_rejection_tag_has_a_swift_case` (src/daemon/socket.rs:1979) passes without edits under the item's
    # defaults, warn and keep the rename. A refuse answer adds a tag with its Swift case in the same
    # commit, and that test gates it.

Scenario: the daemon's result vocabulary stays three values
  Given one acknowledgement of each variant the daemon can build
   When their result values are collected
   Then they are captured, refreshed and rejected
    And no relabel, collision or shared credential adds a fourth
    # AC-IDN-004's BUT NOT: "a new `result` value for the relabel, which today's decoder would throw
    # on". This holds under every option of both decisions. The `result` vocabulary's own parity gate is #1474's.

Scenario: a captured acknowledgement still ends the capture in its done phase with the same label
  Given a menu-bar capture whose acknowledgement is captured under the label work
   Then the model's phase is done with the label work
    And the model's phases and copy are unchanged
    # Covers the item's "`AccountCaptureModel` keeps its phases and its copy". The edit to the match in
    # `capture` (apps/menubar/Sources/AccountCaptureModel.swift:148) is mechanical. What the card does
    # with the facts is #1561's.
```

🟥 Blocked on `duplicate-label-policy` and `login-mismatch-policy`. The scenarios below wait for the operator's answer.

```gherkin
Scenario Outline: 🟥 the app reads the value the golden carries for <field>
  Given the committed <variant> golden
   When the app decodes it
   Then the decoded outcome carries <value>

  Examples:
    | variant   | field          | value                                                        |
    | captured  | collisions     | each element's label and bearer uuids, in the golden's order |
    | refreshed | collisions     | each element's label and bearer uuids, in the golden's order |
    | refreshed | previous_label | the golden's previous label                                  |
    # Pending per row: the two `collisions` rows wait on `duplicate-label-policy` (whether `collisions` is a
    # field); the `previous_label` row waits on `login-mismatch-policy` (whether `previous_label` is a field).
    # Covers AC `swift-decodes-values`: "each collision's label and bearers in order, the previous
    # label". The previous label is read on a refreshed outcome only.
```

## Rule 9 — a field added on one side only fails the `test` job

> Design D-10, AC `field-gate-bites`. The model is `every_rust_rejection_tag_has_a_swift_case` (src/daemon/socket.rs:1979).
> Its Rust side is proven against serde and not asserted
> (`rust_wire_tags`, src/daemon/socket.rs:1793). Its Swift side is parsed strictly from source text and panics on a shape
> it cannot parse (`swift_rejection_tags`, src/daemon/socket.rs:1882), reading the one file the decoder lives in
> (`swift_capture_ack`, src/daemon/socket.rs:1866). A canary proves the extraction finds what it
> should. The list holds this work's fields: `account_uuid`, `collisions`, `previous_label` and
> `shared_with` here, and `collisions` on `applied` in #1558. Whichever item lands first creates the
> gate and its helpers, and the second adds its fields.

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

| Blocked on | Question | Hangs on it | The item's own default |
|---|---|---|---|
| `duplicate-label-policy` | When a write makes a label name a second account, is the report a `collisions` element on the completed write's acknowledgement (warn) or a refusal before any write (refuse)? Refuse needs AC-6 of the migration PRD re-ratified, a `label-in-use` tag on `CaptureRejection` and its Swift case in the same commit | Rule 2, the 🟥 scenarios; Rule 7, whether `collisions` is a golden key; Rule 8, the `collisions` rows; the future oracle for `oracle-socket-capture-reports-a-collision` | warn, the only form that needs no re-ratification |
| `login-mismatch-policy` | When a socket `capture` gives an already-rostered account a different label, does the write complete with the old label named (`previous_label`), complete with a warning, refuse (a `relabels-another-account` tag), or compare an email-shaped label with the captured address? | Rule 1, the relabel scenario; Rule 2, the relabel-onto and roster-order scenarios; Rule 3, the 🟥 scenario; Rule 7, whether `previous_label` is a golden key; Rule 8, the `previous_label` row; the future oracle for `oracle-socket-refresh-reports-the-previous-label` | keep the rename and disclose it |

## Calls this stub took

Each is a call made without the operator, and each is ratification-pending.

| Call | Why | If the answer is otherwise |
|---|---|---|
| DQ-6 is closed: the acknowledgement carries `shared_with` | The design's recorded default (§ 14.8) agrees with REQ-STSH-B-002 (Must) and with the operator's parity ruling of 2026-07-31 (`docs/requirements/gui-cli-capability-parity.md`) | Rule 4 and every `shared_with` row in Rules 5 to 9 go, and the goldens lose a key |
| The key-set comparison in `swift-decodes-values` is read as equality over this work's fields, with `count` and `accounts` a named exemption | The golden carries `count`, the model reads `accounts`, and the item says whole-body equality fails on that pair today | None. It is a reading of a criterion that cannot be met as worded; the item's wording needs the scope |
| "Every quiet scenario in that file" is read as every one a socket capture can reach | One capture writes one entry, so the swap has no counterpart; #1558 words the same clause with that scope | None. The item's wording needs the scope |
| No scenario names a Swift case or accessor for the decoded facts or for `refreshed` | #1474 has not chosen the shape; the item says "whatever shape #1474 gives" | None |

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from something the
item does not spell out.

- The key order within an acknowledgement line. The golden fixes it once it is emitted.
- The names of the new tests, beyond the ones the item suggests, and the file that holds the pin.
- The Swift type that carries the facts: one payload shared by both outcomes, or one per case.
- The bytes of a `credential_shared` event. Those are #1557's, and its stub pins them.
- Which roster `collisions` and `shared_with` are read from when the daemon refuses a shrinking
  reload after the save. The report carries the roster that was written, and `count` is the one
  value the handler reads back from the live roster (`self.roster.len()`, src/daemon/commands.rs:416). #1558 says the same for `config-set`.
- The wording of any card line. That is #1561's.

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
  handler returns.
- Rule 6: the redaction meter over the serialized goldens.
- Rule 7: the serde form, the emitter and the pin, in the `test` job.
- Rule 8: Swift unit tests over `CaptureAck.decode`, which is pure, reading the goldens from disk.
- Rule 9: a Rust test in the `test` job that reads `CaptureAck.swift` as text.

## Oracle authorship

*Work class*: deterministic-output. The primary deliverable is a wire frame, the serialized
acknowledgement, with its byte-pinned golden and a decoder over it. A golden compare, a value
assertion on the serde form and a value-asserting decode each falsify a wrong output mechanically.

*Authorship*: no oracle item is filed yet. The item's collision oracle and its previous-label oracle
assert a form that `duplicate-label-policy` and `login-mismatch-policy` decide, and the PRD says as
much: the answer "fixes the assertion form of REQ-ROS-C-002's oracles". The scenarios blocked on
them are Rule 1's relabel scenario, Rule 2's five 🟥, Rule 3's 🟥, Rule 7's filled-golden scenario and
Rule 8's 🟥 outline. The rest are decided. The one oracle item is filed once the operator decides, so
they wait for it too.

For whoever files it once the operator decides:

- The new fields do not exist on `CaptureAck` today (src/daemon/socket.rs:656), so a test written
  before the change cannot build the filled value. Assert on `serde_json::to_value` of the
  acknowledgement: it compiles before the change and fails at runtime.
- `perform_socket_capture` (src/daemon/commands.rs:280) returns `(CaptureAck, Option<Event>)` today, and #1557 changes the
  second element to a `Vec<Event>`. An oracle over the events, Rule 4's agreement, can only be
  authored after #1557 lands.
- The Swift scenarios wait for #1474, which gives the app a `refreshed` outcome.
- A RED oracle lands `#[ignore = "RED oracle: pre-authored ahead of its fix (issue #1559), which
  un-ignores it"]`, because `ci-ok` cannot merge a failing suite and `main-protection` has no bypass
  actor. It keeps a non-ignored companion on the same fixture and proves it discriminates by
  applying the fix in a scratch copy and mutating each guarded invariant.
