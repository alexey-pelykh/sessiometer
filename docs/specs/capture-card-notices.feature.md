# Feature: the capture card says what a refused capture ran into — the label asked for, and how many accounts carry it — and, after a capture, a credential another entry holds (Tier B)

Issue #1561 · PRD REQ-ROS-B-001 / REQ-ROS-I-002 / REQ-STSH-B-002 / REQ-ROS-Q-001 / REQ-NTC-C-001 / REQ-NTC-C-002 (REQ-IDN-I-002 vacuous since 2026-11-03), AC-ROS-003 / AC-IDN-004 (both re-keyed 2026-11-03) / AC-STSH-001 / AC-NTC-002 · design UX-1 to UX-9, § 9.3, § 9.5, § 9.6, § 11.4, § 11.5, § 12.3 (C-2 to C-4, C-6, C-8, C-9, C-12)

Example Mapping: 🟦 8 rules · 🟩 50 examples · 🟥 0 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests
> on is unratified and its problem framing is unvalidated. The names `account_uuid` and `shared_with`,
> which the card reads from a capture's acknowledgement, and the tags `label-in-use` and
> `relabels-another-account`, on which it shows a refusal, are #1559's (design § 12.3, C-12) and wait for
> ratification too. The refuse form rests on the `/scope` orchestrator's readings of the operator's
> rulings of 2026-11-03 (design § 12.3, C-2, C-3, C-4, C-6, C-8, C-9), which the operator confirms or
> changes at ratification. Each scenario waits to be bound in #1561's execution; none is a test yet.
> The stub maps the item as filed: the capture card's lines, how long they stay, and how they are
> announced.
> What the daemon sends is #1559's, the CLI's lines are #1556's and #1557's, and the Settings note is
> #1560's. #1561 is blocked by #1559, which waits on #1474. An example is a scenario or an outline row.
>
> **The Blue Card Gate trips: the item carries more rules than one item should, and this stub does not
> split it.** The rules are numbered so that a split has somewhere to cut. Rules 1 to 4 say what the
> card shows, one fact each. Rules 5 and 6 say how long it stays and how it is announced. Rules 7 and 8
> constrain how every line is drawn and worded.
>
> **The Red Card Gate tripped at authoring (4 open), held 2 open from 2026-10-27, and holds none since
> the operator ruled both policies on 2026-11-03.** `duplicate-label-policy` and `login-mismatch-policy`
> are decided: refuse (PRD § 8.1, design § 12.2), and the socket `capture` carries no override (design
> § 12.3, C-6), so a menu-bar capture that would make a label name a second account, or rename the
> account it lands on, is refused before any write. Every 🟥 scenario that hung on them is replaced
> below by the decided example of the refusal, and the resolved cards are recorded at the end.
> `shared-line-per-holder` (one shared line per `shared_with` element, settled on 2026-10-27) stands.
> `relabel-persists` is retired: no finished card names a previous label, and the refused card that
> takes its place is a failed phase, which no beat resets (Rule 5).
>
> **Calls, not questions.** `panel-notices-name-the-uuid` is not marked 🟥. The item lists it among the
> decisions no operator has made, but design § 14.6 item 10a says what governs while it is unanswered
> ("the requirements as written govern and the notices name the uuid"), and REQ-STSH-B-002 (Must) and
> REQ-NTC-C-001 say the same. The stub builds on that and tables it among its calls. If the operator
> answers the other way, the uuid parts of the shared line go and its facts stay. The refused card names
> no uuid under either answer, because neither the rejection nor a roster row carries one.
> **Decided 2026-11-03 by the operator: keep the uuid** (design § 14.6 item 10a), so the call above is the ruling.
>
> **Questions no example here depends on.** The orchestrator's list also holds `account-key-form`
> (DQ-1), `label-normalization-policy`, `import-duplicate-uuid-policy`, `orphan-stash-policy` and PRD
> DQ-2 to DQ-10. None gates an example. The card shows no ordinal and needs no identifier on the status
> wire, so `account-key-form` is not read. DQ-4 concerns the panel's rows and callout, not this card, whose
> frames are the capture frames of the mock. The examples compare labels byte for byte, as the roster
> does today (`label_bearers`, src/cli.rs:5245, keys a map by the label string), and none uses two labels
> that differ only in case or form. DQ-6 is a call, tabled below. The rest concern import, the usage
> store, `stats`, orphan stashes or the CLI's count wording, none of which reaches this card.
> The item also names these, and none gates an example either: `panel-switches-by-identifier` (design
> § 14.6 item 15; decided 2026-11-03: keep the switch blocked) no longer reaches the card, because the only line that spoke of switching was the
> collision line the refuse form removes; `mock-page-restructure` gates the mock's render, not an
> outcome (see the criteria no scenario expresses); `swift-red-landing-form` picks the oracle's marker,
> not an outcome (see Oracle authorship); `ready-items-before-ratification` decides when the item is
> built, not what it does.
>
> Words. `A:work` is a roster entry whose uuid is A and whose label is `work`; A, B and C stand for
> whole uuids, and `<uuid-B>` in a line stands for B's. *A menu-bar capture* is the socket `capture`
> command; it *asks for work* when it carries that label. *The signed-in account* is the account the
> daemon harvests. *The written entry* is the one roster entry a capture creates or refreshes. A
> *bearer* of a label is a roster entry that carries it. K names a credential; no scenario quotes one.
> *The card* is the panel's capture card. Once a capture is acknowledged the card is *finished* (the
> model's `done` phase). Its *lines* run outcome, then shared, and a *notice line* is a shared line.
> *The beat* is the pause before a finished card returns to idle (1.8 s today).
> *Quiet* means the acknowledgement carries no `shared_with`: an older daemon also sends no
> `account_uuid`, and a current one always does. Under the refuse form a capture's acknowledgement never
> carries `collisions` or `previous_label` (design § 12.3, C-12).
> The card is *refused* when the daemon rejects the capture with `label-in-use` or
> `relabels-another-account` (the model's `failed` phase for those two tags). *The roster rows* are the
> labels of the panel's current `watch` snapshot, in roster order, with no active mark; the model reads
> them once, through an injected reader, when the card finishes or is refused. *The refusal's sentence*
> is the one #1559 gives the tag; *the fact line* is the line this item adds under a `label-in-use`
> refusal's sentence; *the Settings control* is the card's control that opens the Settings window.
>
> The traps in this feature:
> - A green `swift` job says nothing about this card. SwiftUI's `ImageRenderer` cannot rasterize the
>   capture text field, so the panel goldens never show the card, and `panel-goldens` is a soft gate:
>   every step is `continue-on-error`. The card is verified through the tree lane and the metrics lane
>   of `PanelCaptureCardTests` (apps/menubar/Tests/PanelCaptureCardTests.swift:57), and its fidelity
>   against the mock is a manual step.
> - The rejection carries no label, uuid or count (`docs/specs/capture-rejection-parity.feature.md`),
>   so a refused card's facts are the label the panel sent and the roster rows'. The snapshot's active
>   row can be another account than the one the refusal landed on: for about 15 s after the operator
>   signs in to a different account in Claude Code, and when two entries hold one credential and the
>   landed account is the later one (PRD § 2.5). The rows carry no identifier until #1571 and #1578, so
>   the card cannot tell, and naming the wrong entry is the harm PRD § 2.4 names. So the card never
>   names the landed account's current label (design § 12.3, C-6), and a card that names the active
>   row's label fails Rules 2 and 3.
> - A finished card derives nothing from the rows. Rule 3's scenario about a duplicate the roster
>   already holds fails a card that does.
> - A refused card tested only for the refusal's sentence passes over a card that never names the label
>   it sent. Rule 3 asserts the fact line, including when no row carries the label, and Rule 2 asserts
>   that a `relabels-another-account` card names no label.
> - A reset exemption tested in one direction passes over a model that never resets, or one that always
>   does. Rule 5 asserts both directions, with the beat injected so that no test sleeps 1.8 s.
> - An announcement tested for its text alone passes over a model that announces twice, or announces a
>   quiet capture. Rule 6 counts calls.
> - A wrap bound raised to fit new copy turns a gate into a tautology.
>   `wrapLineBound` (apps/menubar/Tests/PanelCaptureCardTests.swift:67) was measured for the strings that
>   existed, and its own message says "Shorten the copy rather than raising this bound." The notice
>   strings get their own bound, and the unscaled-card canary shows it can fail.

## Rule 1 — a capture that reports nothing new looks exactly as it did

> REQ-ROS-Q-001 (`CollisionReportPrecision`, goal 0), ACs `quiet-capture-unchanged` and
> `existing-tests-change-mechanically`. The outcome line keeps today's text byte for byte.
> `captureDoneText` (apps/menubar/Sources/StatusPanelFormat.swift:46) takes the label alone, and this
> item does not change it to take the new facts. The card stays one green line and returns to idle after
> the beat. Quiet is a decided outcome under every option of both policies.

```gherkin
Scenario Outline: a quiet acknowledgement shows the outcome line and nothing else
  Given the daemon acknowledges a capture: <acknowledgement>
   When the card finishes the capture
   Then the card shows one line, Captured ‘work’, with the success cue
    And it shows no collision line and no shared line
    And it announces nothing
    And once the beat has passed the card is idle

  Examples:
    | acknowledgement                                                 |
    | captured, label work, nothing else, as an older daemon sends it |
    | captured, label work, account_uuid B and nothing else           |
    # Covers AC `quiet-capture-unchanged` and the invariant "a quiet capture shows nothing new".
    # `testCaptureDoneTextEchoesTheAssignedLabel` (apps/menubar/Tests/AccountCaptureTests.swift:171)
    # passes without edits. The assertions on `.done(label:)`, for example
    # `testCaptureReachesDoneEchoingDaemonLabel` (apps/menubar/Tests/AccountCaptureTests.swift:210),
    # change only by the shape of the `done` payload (AC `existing-tests-change-mechanically`).
    # The first row is the line that second test already feeds the model, {"result":"captured","label":
    # "work","accounts":2}. The app reads `accounts` where the daemon sends `count`, and this item leaves
    # that alone. The second row is an ordinary new account under a fresh label, REQ-ROS-Q-001's fourth
    # fixture: no other entry bears the label, so nothing is created.
```

## Rule 2 — a capture refused for renaming the account it lands on shows the refusal and its remedies, and names no label

> REQ-ROS-I-002, AC-IDN-004 as re-keyed on 2026-11-03 ("the capture is refused with
> `relabels-another-account` before any write; the card names no label, since the watch snapshot cannot
> say for certain which account the capture landed on, and offers Settings to rename"), design § 12.3
> C-3, C-6 and C-9. The socket `capture` refuses a rename and accepts no consent (C-6), so no menu-bar
> capture completes one and REQ-IDN-I-002 is vacuous. The panel cannot know for certain which account
> the capture landed on (C-6), so the card shows the refusal's sentence and its remedies (Rule 3's
> Settings outline) and names no label, neither the one asked for nor any row's. The CLI's refusal
> names the landed account's current label, so this card is a parity gap, recorded in the scope brief
> as `refuse-form-relabel-card-names-no-label` and closable by a field beside the tag, never by data on
> the tag (C-9). #1559 owns the tag, its Swift case and the refusal's sentence (R-12 of
> `docs/requirements/roster-loss-prevention.md`); this rule leaves the sentence byte for byte and adds
> no line under it. A refresh still shows as a decode failure until #1474 lets the app decode
> `refreshed`: today `CaptureAck.decode` (apps/menubar/Sources/CaptureAck.swift:92) throws on that
> result.

```gherkin
Scenario: a capture that would rename the account it lands on is refused, and the card names no label
  Given a roster A:work, B:spare
    And the roster rows show work, then spare
    And the signed-in account is B
   When a menu-bar capture asks for main
    And the daemon refuses it with relabels-another-account
   Then the card shows the refusal's sentence and no fact line
    And no line names spare, work or main
    # The panel cannot know that the capture landed on B: the snapshot's active row can be another
    # account (see the traps above; design § 12.3, C-6). Covers AC `line-values-and-order`, AC-IDN-004 as
    # re-keyed and the item's conformance criterion to design § 12.3, C-9. Before the rulings this was
    # the 🟥 scenario "a refresh that gives the account a fresh label names the label it carried": the
    # write it described no longer completes on the socket (design § 12.3, C-3, C-6).

Scenario: a refresh under the label the account already carries names no previous label
  Given a roster A:work
    And the signed-in account is A
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid A, with no previous_label
   Then the card's outcome line carries no previous-label part
    And the card shows no collision line
    # Covers AC-IDN-001's "BUT NOT a changed reply when the label given equals the one the account
    # already carries", design § 9.3's edge case "a refresh under the same label shows no relabel and
    # no collision line", and the invariant "a quiet capture shows nothing new". It waits for #1474 to
    # give the app a `refreshed` outcome. It holds under every option of `login-mismatch-policy`: with
    # nothing renamed, every option agrees.
```

## Rule 3 — a capture refused because another account carries the label names it by its label, or counts them, and offers the way through

> REQ-ROS-B-001 in the form the ruling selects (a refusal before any write), REQ-ROS-I-002, AC-ROS-003
> as re-keyed on 2026-11-03 ("the capture is refused with `label-in-use`, and the card shows the
> refusal; no capture completes with a collision"), ACs `oracle-the-refused-card-names-the-label-in-use`
> and `refusal-launches-settings`, design § 12.3 C-2, C-4, C-6 and C-9. The socket `capture` refuses a
> write that would make a label name a second account and carries no override (C-6). A write that would
> also rename the account it lands on carries `label-in-use` too (C-6). The card names the label it
> asked for, which it sent and so knows whatever the roster rows show, and counts the rows that carry
> that label, byte for byte: with two or more it says how many, and with one or none it names the label
> alone, so it never states a count of zero (C-9). It names no other row's label, because it cannot know
> which row is the account the write landed on (C-6). The card's remedies are an `act` path and a
> `launch` path
> (`docs/requirements/gui-cli-capability-parity.md` R-4, R-5): its own label field, which keeps the
> label after a refusal, and the Settings control. It offers no override (C-6); the one-action replace
> the CLI offers is a parity gap recorded against #1019, not built here. Import still warns on the
> same roster (C-1, and AC-6 of `docs/requirements/migration-credential-portability.md` as narrowed on
> 2026-11-03), so the card never shows import's notice.

```gherkin
Scenario: a new account asking for a label one account carries is refused, and the card names the label
  Given a roster A:work
    And the roster rows show work
    And the signed-in account is B, which the roster does not hold
   When a menu-bar capture asks for work
    And the daemon refuses it with label-in-use
   Then the card shows the refusal's sentence, then a fact line
    And the fact line says that work is another account's label, naming work alone
    And no line says that work now names more than one account
    # The draft reads `‘work’ is another account’s label.` This is the roster of the first scenario of
    # docs/specs/import-duplicate-label.feature.md, one label on two uuids. Import warns on it with
    # `duplicate_label_notice` (src/cli.rs:5628) and the capture is refused, so the last Then is the
    # comparison, made by concretize-and-compare and not by reading prose. Covers AC
    # `oracle-the-refused-card-names-the-label-in-use`, AC-ROS-003 as re-keyed and the item's conformance
    # criterion to that file. Seam: AccountCaptureModel over the fake connection that AccountCaptureTests
    # feeds (`CommandFakeConnection`, apps/menubar/Tests/ControlCommandTransportTests.swift:346), with the
    # injected rows reader, plus the card's tree through `PanelA11y.tree`
    # (apps/menubar/Tests/PanelAccessibilityTreeTests.swift:172). Before the rulings these were the 🟥
    # scenarios "a new account under a label another account bears" and "the collision line names what
    # import's notice names" (design § 12.3, C-2, C-6).

Scenario: a label two accounts already carry is refused, and the card counts them
  Given a roster A:work, B:work
    And the roster rows show work twice
    And the signed-in account is C, which the roster does not hold
   When a menu-bar capture asks for work
    And the daemon refuses it with label-in-use
   Then the fact line says that 2 accounts on the roster carry work, and names neither
    # The draft reads `2 accounts are on the roster as ‘work’.` A duplicate that already exists came from
    # an import, a restore or a hand edit (design § 12.3, C-8); the Settings note (#1560) and the rows
    # (#1566) show it. The CLI refuses this write whatever its flags (C-5), and the panel has no flag.
    # Covers AC `oracle-the-refused-card-names-the-label-in-use` and AC `line-values-and-order`. Before the
    # rulings this was the 🟥 scenario "a third account joins a label two accounts already bear".

Scenario: a rostered account asking for a label two accounts carry is refused, and the card counts them and names no other label
  Given a roster A:work, B:work, C:spare
    And the roster rows show work, work, then spare
    And the signed-in account is C
   When a menu-bar capture asks for work
    And the daemon refuses it with label-in-use
   Then the fact line says that 2 accounts on the roster carry work, and names neither
    And no line names spare
    # The write would rename C and give it a label two accounts carry, so it carries `label-in-use`
    # (design § 12.3, C-6), and on the CLI no flag lets it through (C-4, C-5). The card counts the rows
    # that carry work and names no label C carries, because it cannot know the capture landed on C
    # (C-6). Covers AC `oracle-the-refused-card-names-the-label-in-use` and AC `line-values-and-order`.

Scenario: a capture that would rename the account onto another account's label is refused as label-in-use, and the card names that label and no other
  Given a roster A:work, B:spare
    And the roster rows show work, then spare
    And the signed-in account is B
   When a menu-bar capture asks for work
    And the daemon refuses it with label-in-use
   Then the fact line says that work is another account's label
    And no line names spare
    # A write that meets both conditions carries `label-in-use` (design § 12.3, C-6), and C-4 makes it two
    # steps on every surface: free the label first, then rename. The card cannot know the capture landed
    # on B (C-6), so it names the label asked for and not the one B carries. The draft reads `‘work’ is
    # another account’s label.` Covers AC `oracle-the-refused-card-names-the-label-in-use`. Before the
    # rulings this was the 🟥 scenario "the written entry is found by its uuid and not by its place in the
    # list", whose renaming refresh no longer completes.

Scenario: no roster row carries the label asked for, and the card still names it, with no count
  Given the roster rows show spare
   When a menu-bar capture asks for work
    And the daemon refuses it with label-in-use
   Then the card shows the refusal's sentence, then a fact line
    And the fact line names work and states no count
    And no line names spare
    # The rows lag a roster change another surface made since the last frame. The panel sent work, so it
    # names it whatever the rows show (design § 12.3, C-9). The call
    # `refused-card-names-nothing-it-cannot-read`: the card never states a count of zero. Covers AC
    # `oracle-the-refused-card-names-the-label-in-use` and AC `line-values-and-order`.

Scenario Outline: a refusal the operator resolves in Settings offers the Settings control, and no override
  Given the panel can open the Settings window
   When the daemon refuses a menu-bar capture with <reason>
   Then the card <offers> the Settings control
    And it offers no way to repeat the capture with an override

  Examples:
    | reason                   | offers         |
    | label-in-use             | offers         |
    | relabels-another-account | offers         |
    | prior-configuration      | does not offer |
    # Covers AC `refusal-launches-settings`: a `launch` path, never inert text naming a command
    # (`docs/requirements/gui-cli-capability-parity.md` R-4, R-5). The socket `capture` accepts no consent,
    # so no override exists to offer (design § 12.3, C-6). `prior-configuration` is the mock's other
    # Refused row: restoring `config.toml` is not a Settings action, so it keeps its own copy and no
    # control. With no Settings closure supplied the card offers no control (Calls,
    # `refused-card-launches-settings`).

Scenario: the Settings control asks for the Settings window once
  Given the card is refused with label-in-use and offers the Settings control
   When the operator uses the control
   Then the model calls the injected Settings closure once
    # The controller supplies the closure: it closes the panel, which resets the card, and opens the
    # Settings window through the same `onOpenSettings` seam the status item's menu uses
    # (apps/menubar/Sources/StatusItemController.swift:57). The controller is outside the test bundle, so
    # its half is checked by hand. Covers AC `refusal-launches-settings`.

Scenario: the account that already bears the label is not a collision
  Given a roster A:work
    And the signed-in account is A
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid A, and nothing else
   Then the card shows no collision line
    # The counterpart of import's "same label, same uuid" scenario in
    # docs/specs/import-duplicate-label.feature.md. A already carries work, so the write gives no account a
    # label it did not carry before, which is no collision (design § 12.3, Terms). AC-ROS-001 names a
    # report here as a BUT NOT. Covers AC `quiet-capture-unchanged` and the item's conformance criterion
    # to that file (a quiet scenario with a capture counterpart). It waits for #1474's decode of
    # `refreshed`.

Scenario: a duplicate the roster already holds is not announced again by a capture that adds none
  Given a roster A:work, B:work
    And the panel's roster already shows work twice
    And the signed-in account is B
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid B, and nothing else
   Then the card shows no collision line
    # The counterpart of import's "overwriting a duplicate the target already had" scenario. B already
    # carries work, so the refresh is no collision (design § 12.3, Terms) and is not refused. A finished
    # card's lines are facts of the write, never of the rows the panel holds. A duplicate that already
    # exists is the Settings note's (#1560) and the rows' (#1566). Covers REQ-ROS-Q-001's second fixture
    # and the item's conformance criterion to docs/specs/import-duplicate-label.feature.md, and fails a
    # card that derives its line from the roster. It waits for #1474's decode of `refreshed`.
```

## Rule 4 — a credential another entry holds is named on the card, in the line the two labels call for

> REQ-STSH-B-002 (Must), AC-STSH-001, AC `oracle-the-card-shows-a-shared-credential`, design UX-9,
> § 9.6. The card reads `shared_with` from the acknowledgement: #1557 detects the pair and #1559 carries
> it. The line names both entries by label, and by uuid beside a label with more than one bearer. A
> side's label counts as ambiguous when both sides carry the same label or when the panel's roster rows
> show it more than once. An ambiguous side is named by label and uuid without an ordinal: a capture's
> acknowledgement carries no `collisions` under the refuse form (design § 12.3, C-12), and a row cannot
> be matched to a uuid until the status frame carries account identifiers (#1571, consumed by the panel
> in #1578). A refused capture writes nothing, so it carries no shared credential. The card reports
> and never chooses: no entry is merged, disabled or removed, and no credential byte, digest or
> fingerprint reaches it. Design § 9.6 puts the wording of these lines under copywriter review, so
> whoever writes the oracle reads Not pinned first.

```gherkin
Scenario: both labels are unique
  Given a roster A:work
    And the signed-in account is B, which the roster does not hold, and its credential is the one A's stash holds
   When a menu-bar capture asks for spare
    And the daemon acknowledges it as captured, label spare, account_uuid B, with shared_with work A
   Then the card shows its outcome line, then a shared line
    And the shared line says that spare and work hold the same credential
    And it names no ordinal and no uuid
    # The item's draft: `‘work’ and ‘spare’ hold the same credential.` The order of the two sides is not
    # a rule (see Not pinned). Covers AC `oracle-the-card-shows-a-shared-credential` and the first of
    # AC `line-values-and-order`'s variants, two unambiguous labels.

Scenario: the other side's label is ambiguous only in the roster the panel holds
  Given a roster A:work, B:work
    And the panel's roster rows show work twice
    And the signed-in account is C, which the roster does not hold, and its credential is the one B's stash holds
   When a menu-bar capture asks for spare
    And the daemon acknowledges it as captured, label spare, account_uuid C, with shared_with work B and no collision
   Then the shared line names spare by its label alone
    And it names the other side by its label work and B's uuid, with no ordinal
    # The item's rule: the acknowledgement gives B no ordinal, because `collisions` is absent (the write
    # created no collision), and a row cannot be matched to B's uuid, so the ordinal waits for #1578.
    # Design § 9.6's own draft, `work 2 of 2 (<uuid-B>)`, takes the ordinal from the watch frame, which
    # needs the frame to carry identifiers. The form of a side named by label and uuid without an
    # ordinal is undrafted (see Not pinned). This is design CAP-07's fixture, `work` A, `work` B, then
    # `spare` holding B's credential. Covers AC `line-values-and-order`, "one side ambiguous through
    # the roster rows".

Scenario: a refused capture shows no shared line, even when its credential is another entry's
  Given a roster A:work, C:spare
    And the signed-in account is B, which the roster does not hold, and its credential is the one C's stash holds
   When a menu-bar capture asks for work
    And the daemon refuses it with label-in-use
   Then the card shows the refusal and no shared line
    # A refused write stashes nothing, so no comparison runs (REQ-STSH-B-001 compares at the write), and
    # the rejection carries no other field (design § 12.3, C-12). Covers AC `line-values-and-order`.
    # Before the rulings this was the 🟥 scenario "the written side's label is ambiguous because the
    # capture made it so", whose fixture needed a completed capture that carries a collision.

Scenario: both sides carry one label, on a refresh of a duplicate the roster already holds
  Given a roster A:work, B:work
    And the panel's roster rows show work twice
    And the signed-in account is B, and its credential is the one A's stash holds
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid B, with shared_with work A
   Then the card shows its outcome line, then a shared line
    And the shared line names B as this one and A as the other, each with its uuid and with no ordinal
    # B already carries work, so the refresh is no collision and is allowed (design § 12.3, Terms). The
    # acknowledgement carries no `collisions` (C-12) and a row cannot be matched to a uuid until #1578,
    # so neither side gets an ordinal, and the form of that line is undrafted (see Not pinned). It waits
    # for #1474's decode of `refreshed`. Covers AC `line-values-and-order`, "equal labels". Before the
    # rulings this was the 🟥 scenario "both sides carry one label", whose capture created the duplicate.

Scenario: the credential is held by several other entries
  Given a roster A:work, C:spare
    And the signed-in account is B, which the roster does not hold, and its credential is the one both A's and C's stashes hold
   When a menu-bar capture asks for third
    And the daemon acknowledges it as captured, label third, account_uuid B, with shared_with work A, then spare C
   Then the card shows two shared-credential lines, one per `shared_with` element, in roster order: work A first, then spare C
    # Settled in #1561 on 2026-10-27 (§ The lines): one line per `shared_with` element, as the CLI prints
    # one line per detected pair (design § 8.5).
    # The item's "at most one" is the collision line, and its drafts show pair lines only. #1559's Rule 4
    # (docs/specs/capture-ack-disclosure.feature.md) puts several holders in `shared_with`, in roster
    # order, and the CLI prints one line per pair (Rule 2 of docs/specs/shared-credential-detection.feature.md),
    # which is why the item pins one line per element: a card that named only the first holder would
    # drop a pair the daemon sent. Covers AC `oracle-the-card-shows-a-shared-credential`, for a
    # variant the item's drafts do not show.
```

## Rule 5 — a card that carries a notice or a refusal stays until the operator's next action on it

> Design UX-2, AC `persistence`. The confirmation reset applies only to a finished card that carries no
> notice line. It is `scheduleIdleReset` (apps/menubar/Sources/AccountCaptureModel.swift:188).
> The operator's next action on the card is a new capture, a cancel, the card's dismissal, or the
> panel's close.
> - A cancel is `cancelEditing` (apps/menubar/Sources/AccountCaptureModel.swift:120).
> - The dismissal is `dismissCaptureSurface` (apps/menubar/Sources/AccountCaptureModel.swift:138).
> - The panel's close calls the dismissal on every close path, in `closePanel` (apps/menubar/Sources/StatusItemController.swift:339).
> - Beginning to edit the label clears only a failed card, in `setEditing` (apps/menubar/Sources/AccountCaptureModel.swift:110),
>   so it does not clear a notice.
>
> Opening Settings, from the status item's menu or from the refused card's Settings control, closes the
> panel and so resets the card. The notice fixtures use a shared-credential notice, the only notice a
> finished card can carry under the refuse form (design § 12.3, C-8). A refused card is a failed phase,
> and two rules that hold for every failed phase today hold for it: no beat resets it, because only a
> finished card schedules the reset, and beginning to edit the label clears it, because `setEditing`
> clears a failed phase. Editing the label is the remedy the card offers, so that clearing is wanted.

```gherkin
Scenario Outline: the beat resets a card only when it carries no notice
  Given the card has finished a capture: <acknowledgement>
   When the beat has passed
   Then the card is <state>

  Examples:
    | acknowledgement                                           | state                                |
    | captured, label work, quiet                               | idle                                 |
    | captured, label spare, account_uuid B, shared_with work A | still finished, with its shared line |
    # Covers AC `persistence`, both directions. The beat is injectable, so no test sleeps for the real one.

Scenario Outline: the operator's next action clears a card that carries a notice
  Given the card has finished a capture that carries a shared line
   When the operator <action>
   Then the card is <state>

  Examples:
    | action                                  | state                                 |
    | starts a new capture                    | pending, then showing the new outcome |
    | cancels                                 | idle                                  |
    | dismisses the card, or closes the panel | idle                                  |
    | begins editing the label                | still finished, with its shared line  |
    # Covers AC `persistence`: "A new capture, `cancelEditing` and `dismissCaptureSurface` each clear
    # it, and focusing the field does not." `closePanel` is outside the test bundle (apps/menubar/project.yml
    # records why), so the panel's close is checked through the dismissal it calls.

Scenario: an earlier quiet capture's reset does not clear a later card that carries a notice
  Given a quiet capture under work has finished, so a reset is waiting for the beat
   When a second capture finishes before that beat, with a shared line
    And the beat of the first capture passes
   Then the card is still finished, with its shared line
    # Covers AC `persistence`: "a `done` that carries a notice is still `done` after the beat the reset
    # would fire in". `scheduleIdleReset` (apps/menubar/Sources/AccountCaptureModel.swift:188) clears a
    # card only while it is still the finished phase it was scheduled for, and that phase now carries
    # facts. A comparison loosened to "any finished card" would clear the second one. The fixture of
    # the second capture is Rule 4's first scenario. This stub adds the example; the AC does not name it.

Scenario Outline: a refused card stays until the operator acts on it, and editing the label is such an act
  Given the card is refused with label-in-use and shows its fact line
   When <event>
   Then the card is <state>

  Examples:
    | event                                                | state                                 |
    | the beat passes                                      | still refused, with its fact line     |
    | the operator starts a new capture                    | pending, then showing the new outcome |
    | the operator cancels                                 | idle                                  |
    | the operator dismisses the card, or closes the panel | idle                                  |
    | the operator begins editing the label                | idle                                  |
    # Covers AC `persistence`, the refused half. The first row holds because a failed phase is never
    # scheduled for the reset (`scheduleIdleReset`, apps/menubar/Sources/AccountCaptureModel.swift:188),
    # the last because `setEditing` (apps/menubar/Sources/AccountCaptureModel.swift:110) clears a failed
    # phase. The field keeps the label it asked for, since the view blanks it only on a finished capture;
    # that is a view fact outside the model and is checked by hand. Before the rulings this was the 🟥
    # scenario "a refresh that renamed the account and found nothing else": `relabel-persists` is retired,
    # since no finished card names a previous label (design § 12.3, C-6).
```

## Rule 6 — each notice, and each of the two refusals of Rules 2 and 3, is announced once, in line order, without moving focus

> Design UX-8, and WCAG 2.2 Success Criterion 4.1.3 (Status Messages), https://www.w3.org/TR/WCAG22/#status-messages
> (the link is the item's), as to its intent: a status message reaches assistive technology without
> receiving focus. That criterion scopes itself to content in markup languages, so the native panel
> applies its intent and not its letter. AC `announcement`. When a card finishes with a notice, or is
> refused with one of the two tags of Rules 2 and 3, the model calls an injected closure, the way
> `panelKeyRequest` is injected:
> - `panelKeyRequest` is declared at apps/menubar/Sources/AccountCaptureModel.swift:98.
> - `panelKeyRequest` is supplied by the controller at apps/menubar/Sources/StatusItemController.swift:117.
>
> The controller supplies the real post for this closure too. It is outside the test bundle and is
> checked by hand, as the design README's manual checklist records. The sentence never carries a uuid;
> the uuids stay in the visible text. The app has no announcement code today, and its deployment floor
> is macOS 13 (apps/menubar/project.yml).

```gherkin
Scenario: a quiet capture announces nothing
  Given the daemon acknowledges a capture as captured, label work, and nothing else
   When the card finishes the capture
   Then the announcement closure is not called
    # Covers AC `announcement`, "never for a quiet capture".

Scenario: a shared credential is announced once, and the sentence names no uuid
  Given the daemon acknowledges a capture as captured, label spare, account_uuid B, with shared_with work A
   When the card finishes the capture
   Then the announcement closure is called once
    And the sentence holds no uuid
    # The item says the shared notice's sentence is "drafted for copy review", so the oracle asserts the
    # count and the absence of a uuid, not the bytes. Covers AC `announcement`.

Scenario: a notice that is already shown is not announced again
  Given the card has finished a capture that carries a shared line, and has announced it
   When the operator begins editing the label
   Then the announcement closure has still been called once
    # "Once" means on entering the finished state, not on each redraw or focus change. Covers AC
    # `announcement`.

Scenario: a refused capture is announced once, with its fact line, and names no uuid
  Given the roster rows show work
   When a menu-bar capture asks for work
    And the daemon refuses it with label-in-use
   Then the announcement closure is called once
    And the sentence holds the refusal's sentence, then the fact line's label, work
    And the sentence holds no uuid
    # The refusal reaches assistive technology without taking focus, as a notice does (design UX-8). The
    # card announces only the two refusals of Rules 2 and 3; the other failures keep today's
    # silence (see Not pinned). Covers AC `announcement`. Before the rulings this was the 🟥 scenario "a
    # collision is announced with its ordinal and no uuid".

Scenario: two shared lines are announced in line order, once each
  Given the daemon acknowledges a capture as captured, label third, account_uuid B, with shared_with work A, then spare C
   When the card finishes the capture
   Then the announcement closure is called twice
    And the first sentence names work and the second names spare
    # Covers AC `announcement`, "once per notice, in line order". Rule 4's several-holders scenario gives
    # the fixture. Before the rulings this was the 🟥 scenario "two notices are announced in line order,
    # once each", whose first notice was a collision line.
```

## Rule 7 — no notice or refusal rests on colour alone, notice and fact lines wrap rather than truncate, and the outcome stays on one line

> Design § 9.5, and WCAG 2.2 Success Criterion 1.4.1 (Use of Color), https://www.w3.org/TR/WCAG22/#use-of-color
> (the link is the item's), as to its intent. ACs `not-colour-alone`, `wrap-not-truncate` and
> `tree-shape`. A refused card keeps the failure treatment, the warning glyph in red, and a
> `label-in-use` card's fact line wraps under the refusal's sentence; each notice line carries a glyph or
> a text cue besides its colour.
> The glyph and tone of each line are a pure value in `StatusPanelFormat`, so a test can pin that the
> refused card's glyph differs from the success glyph. A notice line and a fact line wrap within the
> card's text budget, `captureCardTextBudget` (apps/menubar/Sources/StatusPanelFormat.swift:160),
> and are never elided, so no label or uuid loses a character.
>
> The outcome line stays on one line:
> - The `status` view (apps/menubar/Sources/StatusPanelCapture.swift:99) gives a finished card's line a one-line limit.
> - Its `truncationMode` (apps/menubar/Sources/StatusPanelCapture.swift:108) comes from `identityElision`, which elides the middle of a long label.

```gherkin
Scenario: the shared line carries a cue beyond its colour
  Given a card that finished with a shared line
   Then the shared line carries a glyph or a text cue, so its colour is not the only thing that tells it from the confirmation
    # Covers AC `not-colour-alone`: "each notice line carries a glyph or text cue beyond colour".
    # Seam: the pure treatment value in `StatusPanelFormat`. The AC pins no glyph for this line.

Scenario: the refused card's cue is the failure glyph, which differs from the success glyph
  Given a card refused with label-in-use
   Then the card's glyph is the failure glyph, and it differs from the success glyph
    And the fact line is text, so its colour is not the only thing that tells it from a confirmation
    # Covers AC `not-colour-alone`. The failure glyph is `exclamationmark.triangle.fill`
    # (apps/menubar/Sources/StatusPanelCapture.swift:110), the success glyph `checkmark.circle.fill`
    # (apps/menubar/Sources/StatusPanelCapture.swift:104). Before the rulings this was the 🟥 scenario
    # "the collision line takes an attention treatment".

Scenario: a long label in the outcome line is elided in the middle and never wraps
  Given a capture whose label is longer than the card's text budget
   When the card finishes the capture
   Then the outcome line stays on one line, with the middle of the label elided
    # An unchanged behaviour that the new lines must not disturb. Covers the Specification's "Treatment"
    # paragraph: "The outcome line stays on one line", and the outcome line's `identityElision` policy.

Scenario: the shared lines wrap within their own bound at every Dynamic Type class
  Given each shared line this stub decides, and a card at each Dynamic Type class
   When the line is measured against the card's text budget
   Then it wraps within the notice bound, which the test states and which is measured for these strings
    And it is never elided
    And `wrapLineBound` is not raised
    # Covers AC `wrap-not-truncate`. `wrapLineBound` (apps/menubar/Tests/PanelCaptureCardTests.swift:67)
    # was measured for the strings that existed. Seam: `TextMetrics` in the metrics lane of
    # `PanelCaptureCardTests`.

Scenario: the refused card's fact lines wrap within the notice bound
  Given both label-in-use fact lines, the one that names the label alone and the one that counts the rows carrying it, and a card at each Dynamic Type class
   When each is measured against the card's text budget
   Then each wraps within the notice bound and is never elided
    # Covers AC `wrap-not-truncate`. The longest shared line, the equal-labels one with two uuids, is
    # decided now and is measured by the scenario above. Before the rulings this was the 🟥 scenario "the
    # collision lines and the equal-labels shared line wrap within the notice bound": the collision lines
    # are gone, and the equal-labels line now comes from Rule 4's refresh of a duplicate the roster
    # already holds.

Scenario: the notice bound can fail
  Given the notice bound, and a card whose padding was not scaled with the Dynamic Type class
   When the longest decided shared line is measured at the panel's ceiling class
   Then the line exceeds the notice bound
    And the same line in a scaled card is within it
    # Covers AC `wrap-not-truncate`, "an unscaled card must trip the notice bound". The model is
    # `testTheWrapGateFiresOnAnUnscaledCard` (apps/menubar/Tests/PanelCaptureCardTests.swift:385). The ceiling is
    # `PanelTypeScale.ceiling` (apps/menubar/Sources/StatusPanelTypeScale.swift:116), the class that test uses.
    # A bound that the unscaled card cannot trip gates nothing.

Scenario: a finished card with a notice reaches the accessibility tree with each notice as its own text
  Given a card that finished with a shared line
   When its accessibility tree is read
   Then the outcome line and the shared line are each their own reachable text
    And the decorative glyphs are hidden from the tree
    And the tree's role histogram, which holds no string, is pinned for this state
    And the resting card's pin passes without edits
    # Covers AC `tree-shape`. The resting pin is
    # `testTheCaptureCardAccessibilityShapeIsUnchanged` (apps/menubar/Tests/PanelCaptureCardTests.swift:465).
    # Seam: `PanelA11y.tree` (apps/menubar/Tests/PanelAccessibilityTreeTests.swift:172).

Scenario: a refused card reaches the accessibility tree with its sentence, its fact line and its Settings control
  Given a card refused with label-in-use that names work alone
   When its accessibility tree is read
   Then the refusal's sentence, the fact line and the Settings control are each their own reachable element
    And the decorative glyphs are hidden from the tree
    And the tree's role histogram, which holds no string, is pinned for this state
    And the resting card's pin passes without edits
    # Covers AC `tree-shape`, the refused half. Seam: `PanelA11y.tree`.
```

## Rule 8 — a notice or a refusal names accounts by label, ordinal and uuid only, and every new string passes the framing scan

> REQ-NTC-C-001, REQ-NTC-C-002, AC-NTC-002, issue #15, issue #160, and ACs `only-labels-and-uuids` and
> `copy-passes-the-framing-scan`. The card's only inputs are labels and uuids from the acknowledgement,
> the label the capture sent, and labels from the roster rows, so it never carries a credential, an
> email address the operator did not author, or a value derived from a credential. Every new string
> passes the framing-vocabulary scan with no exemption: the whole banned-token list and the banned
> phrases that `src/framing_vocabulary.rs` holds, read at test time by `FramingVocabularyScan`
> (apps/menubar/Tests/NotificationDeliveryTests.swift:198).

```gherkin
Scenario: each line is its template plus the sentinel labels and uuids
  Given acknowledgements and roster rows whose labels and uuids are sentinel values
   When each line the card can show is rendered from them
   Then each line holds the template text and the sentinels
    And it holds no other identifying text: no email address, no token, no digest
    # Covers AC `only-labels-and-uuids` and the invariant that the card never carries a credential.

Scenario Outline: a label that holds a space or an apostrophe renders intact
  Given a capture whose label is <label>
   When the card finishes the capture
   Then every line that names the label shows it unchanged

  Examples:
    | label       |
    | my spare    |
    | ops' spare  |
    | ops’ spare  |
    # Covers AC `line-values-and-order`, "A label that holds a space or an apostrophe renders intact".
    # The last row holds U+2019, the character the card's own quotes close with.

Scenario: every new string passes the framing scan, and the scan can fail
  Given every string the card can now show, rendered from every fixture of this feature
   When the shared scan reads each of them against the banned tokens and the banned phrases
   Then none is flagged, with no exemption
    And the vocabulary the scan reads is not empty
    And a string with an injected banned word is flagged
    # Covers AC `copy-passes-the-framing-scan`. A scan over an empty vocabulary passes everything.
```

## Red cards — the questions that no scenario above can close
> **Re-authored 2026-11-03 to the refuse form (design § 12.3):** both cards below are resolved, and every 🟥 scenario that named them is now a decided example of the refusal.

| Was blocked on | Question | Resolved to | Where the examples are |
|---|---|---|---|
| `duplicate-label-policy` | When a capture makes a label name a second account, does the card show a collision line on the completed capture (warn), or its Refused state with nothing written (refuse)? Refuse needs AC-6 of `docs/requirements/migration-credential-portability.md` ("BUT NOT by enforcing label uniqueness") re-ratified first, a `label-in-use` tag on `CaptureRejection`, and its Swift case in the same commit | Refuse (the operator, 2026-11-03, PRD § 8.1). AC-6 is narrowed by its amendment note of 2026-11-03; #1559 adds the tag and its Swift case. The socket `capture` refuses with `label-in-use` before any write and carries no override, and the card names the label asked for whatever the rows show, with how many rows carry it when two or more do, and never the landed account's current label (design § 12.3, C-2, C-4, C-6, C-9: orchestrator readings) | Rule 3's refusal scenarios, Rule 4's refused-capture scenario, and the refused-card scenarios of Rules 5, 6 and 7 |
| `login-mismatch-policy` | When a capture gives an already-rostered account a different label, does the card show the refresh with the old label named, show it with a further warning, show its Refused state (a `relabels-another-account` tag), or compare an email-shaped label with the harvested address? | Refuse unless `--rename` (the operator, 2026-11-03, PRD § 8.1). The socket `capture` accepts no consent, so it refuses every rename with `relabels-another-account`, and the card names no label, because the panel cannot know for certain which account the capture landed on (design § 12.3, C-3, C-6, C-9: orchestrator readings; the parity gap `refuse-form-relabel-card-names-no-label`) | Rule 2's refusal scenario |

`relabel-persists` and `shared-line-per-holder`, two handles this stub gave to gaps in the item, are
no longer open. `shared-line-per-holder` stands: the card shows one shared-credential line per
`shared_with` element in roster order. `relabel-persists` (a `done` whose outcome line names a
previous label carries a notice, settled on 2026-10-27) is retired by the refuse form: no finished
card names a previous label (design § 12.3, C-6), and the refused card that takes its place is a
failed phase, which no beat resets (Rule 5).

## Calls this stub took

Each is a call made without the operator, and each is ratification-pending.

| Call | Why | If the answer is otherwise |
|---|---|---|
| `panel-notices-name-the-uuid` is built as the item builds it: a notice names an entry by its uuid only beside a label with more than one bearer | Design § 14.6 item 10a: "If unanswered, the requirements as written govern and the notices name the uuid." REQ-STSH-B-002 (Must) and REQ-NTC-C-001 name the uuid. Decided 2026-11-03 by the operator: keep the uuid | None: the operator decided it on 2026-11-03 |
| The shared line's rule is the item's: an ambiguous side is named by label and uuid, without an ordinal | The item states it, and says it is this pipeline's reading of the design. Under the refuse form the acknowledgement carries no `collisions` (design § 12.3, C-12), so nothing on the wire places a side; design § 9.6 takes the ordinal from the watch frame, which needs the frame to carry identifiers (#1571, #1578) | Rule 4's two ambiguous-side scenarios change; the others do not |
| The card reads `shared_with` from the acknowledgement (DQ-6 closed as "both channels") | Design § 14.8 records it, REQ-STSH-B-002 (Must) says "on the surface that performed the write", and #1559 builds on it | Rule 4, Rule 6's shared sentences and Rule 5's notice fixtures go |
| The item is Must-have | The tracker gives it no priority label, and it serves REQ-ROS-I-002 and REQ-STSH-B-002, both Must | At Should the Red Card Gate would not block |
| "Notice line" means a shared line in Rules 5 to 7, and a refused card is not a notice: it follows the rules of a failed phase | Design § 9.5 names the notice lines as collision and shared lines, and the refuse form leaves only the shared line on a finished card (design § 12.3, C-8) | Rules 5 to 7 already treat the refused card on its own |
| Rule 5's notice fixtures use a shared-credential notice | It is the only notice a finished card can carry (design § 12.3, C-8) | None |
| Rule 5's stale-timer example is this stub's addition | AC `persistence` does not name it. The reset compares the phase it scheduled for with the phase it finds, and that comparison changes with the `done` payload | Drop the example |
| `refused-card-names-nothing-it-cannot-read`: a refused card states only what the panel holds for certain. A `label-in-use` card names the label it sent whatever the rows show, and states a count only when two or more rows carry that label, so rows that lag a change another surface made never yield a count of zero; neither refused card names the landed account's current label | Design § 12.3 C-6 and C-9 read it so: the panel sent the label, the rows can lag a change another surface made, and the active row can be another account. The card never guesses a row and never states a count of zero | Rule 2's scenario and Rule 3's last refusal scenario change |
| `refused-card-keeps-the-tag-sentence`: the fact line sits under the refusal's sentence, which stays byte for byte | #1559 owns that sentence and its tests (R-12 of `docs/requirements/roster-loss-prevention.md`); keeping it whole keeps the two items' outputs separate | Copy review merges the two into one sentence; the facts stay |
| `refused-card-launches-settings`: the Settings control calls an injected closure, which the controller supplies by closing the panel and opening Settings through `onOpenSettings`; with no closure supplied the card offers no control | Design § 12.3 C-9 asks for a `launch` path; the panel's only way to Settings today is the status item's menu, and an unwired control would do nothing when used | The control's form and place change; the fact lines do not |
| A fresh edit of the label clears a refused card, as it clears every failed card today | Editing the label is the `act` path the card offers (design § 12.3, C-9), and `setEditing` already clears a failed phase | A refused card would stay through the edit, as a notice does |
| Only the two refusals of Rules 2 and 3 are announced | Rule 6 carries over to the refused states (design UX-8); announcing every failure is a wider change than the rulings make | The other failures gain an announcement |

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from something the
item does not spell out.

- The words of every line except the quiet outcome line, which an existing test pins. The item calls its
  lines drafts, and design § 9.6 puts the shared lines under copywriter review. An oracle either asserts
  the facts by containment (the label, the count, the uuids, the ordinals, the marking, the order) or
  pins the draft bytes knowing that a copy change edits them in the same change.
- The words of the fact lines, and whether copy review merges the refusal's sentence and the fact line
  into one sentence once both exist.
- The form of a side named by label and uuid without an ordinal.
- The shared line when both sides carry one label and the acknowledgement gives no ordinal.
- The order of the two sides of a shared line. The item's drafts differ on which side comes first.
- The announcement sentences of the shared notice (copy review) and of a refused card, beyond what Rule
  6 asserts, and the closures' names: `announce` and `openSettingsRequest` are suggestions.
- A refresh's outcome line, which is #1474's.
- The Swift type that carries the facts in the finished and the refused phases, and the rows reader's
  name: `rosterRows` is a suggestion.
- The Settings control's title, its accessible label and its place on the card.
- Whether the failures other than the two refusals of Rules 2 and 3 are announced: out of the item's scope.
- The glyphs and tones, beyond: the refused card keeps the failure glyph, which differs from the
  success glyph, and each notice line carries a glyph or a text cue.
- The number of the notice wrap bound, which the pull request measures and states in the test.
- The values in the mock rows, which the design README calls directional.

## Acceptance criteria that no scenario expresses

- *Mock first* (AC `mock-first`): both capture frames, `capture-states-light` and `capture-states-dark`,
  hold a row for each new state, authored in a commit that precedes the Swift commits of the same pull
  request, with the render regenerated and the figures and register in apps/menubar/design/README.md
  updated (its sections Regenerating the render and Expected reconciliations). Beyond the two refusal
  rows #1559 adds (R-12 of `docs/requirements/roster-loss-prevention.md`), the new states are the
  `label-in-use` card that names the label alone and the one that counts two or more rows carrying it,
  each with the Settings control, #1559's `relabels-another-account` row with the Settings control added
  and no fact line, and a finished card with a shared line. A reviewer reads the commit order and the
  diff of `apps/menubar/design/`; no script decides it. The render step is stated per page while
  `mock-page-restructure` is open.
- *Existing tests* (AC `existing-tests-change-mechanically`): the counts in `PanelCaptureCardTests`
  change only where the card gained strings. The `.done(label:)` assertions are Rule 1's. The
  `.failed(.rejected(_))` assertions for every reason but the two refusals of Rules 2 and 3 do not
  change.
- *Golden acknowledgement* (AC `golden-ack-line-decodes`): at least one model test is fed #1559's
  committed Rust-emitted `captured` golden with `shared_with` filled (under build/fixtures/), located
  from the test's own `#filePath`, and reaches a finished card whose shared line matches Rule 4's
  fixtures, so the card's fixtures are not self-confirming.
- *Output conforms to design § 12.3, C-9*: for each refusal scenario of Rules 2 and 3, the rendered
  card states each fact C-9's panel bullet gives the panel card, names nothing the bullet withholds, and
  offers the remedies the bullet names, with no override. A reviewer compares the rendered card with the
  bullet as design § 12.3 reads in the tree under review, never with a copy of it or with prose.
- *Output conforms to the mock*: build the card in each new state, open the mock beside it, and compare
  glyph, tone, line order and wrapping state by state. It is manual by design, because the card cannot be
  rasterized with its text field. The result goes in the pull request, with any intended divergence
  entered under Expected reconciliations.
- *Output conforms to docs/specs/import-duplicate-label.feature.md*: the loud half has no card
  counterpart, because import still warns (design § 12.3, C-1) while a capture that asks for a carried
  label is refused; Rule 3's first scenario uses the import stub's first roster and shows that no line
  says the label now names more than one account. Rule 3's last two scenarios are the quiet half. The
  import stub's swap scenario and its "artifact carrying its own duplicate" scenario have no capture
  counterpart, because one capture writes one entry.
- *Mutation canaries*: each new predicate carries one, driven through the same function the real
  assertion calls, as `testTheWrapGateFiresOnAnUnscaledCard` does.
- *The real announcement*: the controller's post is outside the test bundle. The manual checklist in
  apps/menubar/design/README.md (Capture + notification pre-release checklist (manual)) gains the
  announcement, and the VoiceOver step is run and recorded in the pull request. The real Settings
  launch is the controller's too, and the checklist gains it.
- *Gates*: `cd apps/menubar && xcodegen generate && xcodebuild test -project Menubar.xcodeproj -scheme
  Menubar -configuration Debug -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO`, and the armed
  `panel-goldens` run, which must print its `[panel-goldens] max drift` line. A run that says tests were
  skipped compared nothing.
- *RED records*: the oracle item drafted for #1561 records a failing run for each criterion it
  pre-authors, before it merges. See the last section.

## Where each group is asserted

Taken from the acceptance criteria, for whoever binds the scenarios.

- Rules 1 to 4: the pure line and fact-line functions in `StatusPanelFormat`, and `AccountCaptureModel`
  over the fake connection with the injected rows reader and Settings closure, with the card's tree
  through `PanelA11y.tree`.
- Rule 5: `AccountCaptureModel` with the beat injectable.
- Rule 6: `AccountCaptureModel` with the announcement closure.
- Rule 7: the pure treatment value; `TextMetrics` in the metrics lane of `PanelCaptureCardTests`; and
  `PanelA11y.tree` in the tree lane.
- Rule 8: the pure line and fact-line functions over sentinel acknowledgements and rows, and
  `FramingVocabularyScan`.

## Oracle authorship

*Work class*: deterministic-output. The primary deliverable is the card's lines and the refused card's
fact lines (strings from pure functions in `StatusPanelFormat`), the finished and refused phases'
persistence, announcement and Settings launch in `AccountCaptureModel`, and the card's accessibility
tree. A value assertion, a phase assertion and a tree assertion each falsify a wrong output
mechanically. The mock rows and the README figures are an
accompanying prose/config aspect. Their fidelity is a judgment aspect for an independent reviewer, by
concretize-and-compare, and no existing script decides it: the card never appears in the panel goldens,
and `panel-goldens` is a soft gate.

*Authorship*: the oracle is owed now (the scope brief's `oracle-authorship` rule; design § 12.3,
C-11). Its assertion form waited on `duplicate-label-policy` and `login-mismatch-policy`, which the
operator ruled on 2026-11-03 (PRD § 8.1), and every outcome above is decided, the uuid parts of Rule 4
under the call on `panel-notices-name-the-uuid`. One `(test)` oracle item is drafted for the
orchestrator to file once the planning pull request merges, delivered by a party other than #1561's
executor, and #1561 is `Blocked by` it. It lands after #1559, which adds the two tags and
`shared_with`, and before the fix. No test of it names #1566's ordinal function, since the card shows
no ordinal (design § 12.3, C-6, C-9). The Blue Card
Gate still trips and the item stays whole under `keep-1561-whole`, so the oracle covers the whole item;
a split would split the oracle with it.

For whoever files it:

- The card's new states do not exist until #1559 decodes the two tags and `shared_with` and #1474
  decodes `refreshed`, and the rows reader, the Settings closure, the announcement closure, the
  injectable beat and the pure fact-line functions do not exist on today's tree. A test that names one
  of them does not compile, and a compile error breaks the whole `MenubarTests` bundle. Assert through
  seams that compile before the change, the fake connection that feeds `AccountCaptureModel` an
  acknowledgement line and `PanelA11y.tree` over the real card, and through one adapter declared in
  the oracle's own test file, whose body is today's behaviour and which #1561's executor replaces and
  nothing else, the shape #1584 uses. Such a test compiles and fails at run time.
- `StatusItemController` is outside the test bundle, so the real announcement post, the real Settings
  launch and the panel's close are not oracle material.
- A RED oracle cannot be merged failing, because `ci-ok` cannot merge a failing suite and
  `main-protection` has no bypass actor, and the repo's landing rule for it is stated in Rust terms
  (`#[ignore = "RED oracle: ..."]`). Swift has no `#[ignore]`. This stub found no Swift precedent: no
  test under apps/menubar/Tests uses `XCTExpectFailure`, `git log -S'XCTExpectFailure' -- apps/menubar`
  returns nothing, and the `XCTSkipUnless` uses there are environment-gated measurement gates and not
  RED oracles. The XCTest form is the operator's pick under `swift-red-landing-form`: a strict
  `XCTExpectFailure` tagged `[RED #1561]` whose `issueMatcher` matches on `contains`, as #1582, #1584
  and #1585 do, or `try XCTSkipIf(true, …)` with the same reason string, as #1583 does. The oracle item
  names both and follows the pick, keeps a non-ignored companion on the same fixture, and proves the
  oracle discriminates by applying the fix in a scratch copy and mutating each guarded invariant.
