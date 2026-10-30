# Feature: the capture card says what a capture did beyond naming it — a second bearer of the label, the label an account carried, a credential another entry holds (Tier B)

Issue #1561 · PRD REQ-ROS-I-002 / REQ-IDN-I-002 / REQ-STSH-B-002 / REQ-ROS-Q-001 / REQ-NTC-C-001 / REQ-NTC-C-002, AC-ROS-003 / AC-IDN-004 / AC-STSH-001 / AC-NTC-002 · design UX-1 to UX-9, § 9.3, § 9.5, § 9.6, § 11.4, § 11.5

Example Mapping: 🟦 8 rules · 🟩 27 examples · 🟥 2 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests
> on is unratified and its problem framing is unvalidated. The names `account_uuid`, `collisions`,
> `previous_label` and `shared_with`, which the card reads, are #1559's pipeline defaults and wait for
> ratification too. Each scenario waits to be bound in #1561's execution; none is a test yet. The stub
> maps the item as filed: the capture card's lines, how long they stay, and how they are announced.
> What the daemon sends is #1559's, the CLI's lines are #1556's and #1557's, and the Settings note is
> #1560's. #1561 is blocked by #1559, which waits on #1474. An example is a scenario or an outline row.
>
> **The Blue Card Gate trips: the item carries more rules than one item should, and this stub does not
> split it.** The rules are numbered so that a split has somewhere to cut. Rules 1 to 4 say what the
> card shows, one fact each. Rules 5 and 6 say how long it stays and how it is announced. Rules 7 and 8
> constrain how every line is drawn and worded.
>
> **The Red Card Gate tripped at authoring (4 open) and no longer does (2 open since 2026-10-27).**
> `duplicate-label-policy` (warn or refuse when a write makes a label name a second account) and
> `login-mismatch-policy` (what a write does when it gives an already-rostered account a different
> label) are operator decisions. `relabel-persists` and `shared-line-per-holder` were gaps
> in the item; #1561 settled both on 2026-10-27 (a relabel-only `done` carries a notice; one shared line per
> `shared_with` element), and their scenarios are decided below. The two policies are tabled at the end. A
> scenario marked 🟥 hangs on one of them. It names the handle and writes no outcome as decided; the
> item's own default sits in the comment beside it. The 🟩 count is the examples that carry a decided
> outcome, and the 🟥 ones are not in it. Rules 2 and 3 hold only their quiet halves as decided
> examples: what the card says when the policies bite is all 🟥.
>
> **Calls, not questions.** `panel-notices-name-the-uuid` is not marked 🟥. The item lists it among the
> decisions no operator has made, but design § 14.6 item 10a says what governs while it is unanswered
> ("the requirements as written govern and the notices name the uuid"), and REQ-STSH-B-002 (Must) and
> REQ-NTC-C-001 say the same. The stub builds on that and tables it among its calls. If the operator
> answers the other way, the uuid parts of the lines go and the facts and the ordinals stay.
>
> **Questions no example here depends on.** The orchestrator's list also holds `account-key-form`
> (DQ-1), `label-normalization-policy`, `import-duplicate-uuid-policy`, `orphan-stash-policy` and PRD
> DQ-2 to DQ-10. None gates an example. The item's ordinal rule needs no identifier on the status wire,
> so `account-key-form` is not read. DQ-4 concerns the panel's rows and callout, not this card, whose
> frames are the capture frames of the mock. The examples compare labels byte for byte, as the roster
> does today (`label_bearers`, src/cli.rs:5245, keys a map by the label string), and none uses two labels
> that differ only in case or form. DQ-6 is a call, tabled below. The rest concern import, the usage
> store, `stats`, orphan stashes or the CLI's count wording, none of which reaches this card.
>
> Words. `A:work` is a roster entry whose uuid is A and whose label is `work`; A, B and C stand for
> whole uuids, and `<uuid-B>` in a line stands for B's. *A menu-bar capture* is the socket `capture`
> command; it *asks for work* when it carries that label. *The signed-in account* is the account the
> daemon harvests. *The written entry* is the one roster entry a capture creates or relabels. A
> *bearer* of a label is a roster entry that carries it. K names a credential; no scenario quotes one.
> *The card* is the panel's capture card. Once a capture is acknowledged the card is *finished* (the
> model's `done` phase). Its *lines* run outcome, collision, shared, and a *notice line* is a collision
> line or a shared line. *The beat* is the pause before a finished card returns to idle (1.8 s today).
> *Quiet* means the acknowledgement carries none of `collisions`, `previous_label` and `shared_with`:
> an older daemon also sends no `account_uuid`, and a current one always does.
>
> The traps in this feature:
> - A green `swift` job says nothing about this card. SwiftUI's `ImageRenderer` cannot rasterize the
>   capture text field, so the panel goldens never show the card, and `panel-goldens` is a soft gate:
>   every step is `continue-on-error`. The card is verified through the tree lane and the metrics lane
>   of `PanelCaptureCardTests` (apps/menubar/Tests/PanelCaptureCardTests.swift:57), and its fidelity
>   against the mock is a manual step.
> - The ordinals are the acknowledgement's, not the roster's. A bearer's position in `bearers` is its
>   ordinal. The panel's roster rows carry no identifier until #1571 and #1578, so a row cannot be
>   matched to a uuid, and a line built from rows could name the wrong entry, which is the harm PRD
>   § 2.4 names. Rule 3's scenario about a duplicate the roster already holds fails a card that derives
>   its collision line from the rows it holds.
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

## Rule 2 — a refresh that renames an account names the label it carried

> REQ-IDN-I-002 (Should), the second Then of AC-IDN-004, AC `oracle-the-card-shows-the-previous-label`,
> design UX-6. The previous label folds into the outcome line, so one write reads as one verb. It
> appears only once #1474 lets the app decode `refreshed`. Today `CaptureAck.decode` (apps/menubar/Sources/CaptureAck.swift:92)
> throws on that result, so a refresh shows as a decode failure. This item adds ` (was ‘spare’)` to
> whatever outcome line #1474 gives a refresh and decides none of that copy.

```gherkin
Scenario: 🟥 a refresh that gives the account a fresh label names the label it carried
  Given a roster B:spare
    And the signed-in account is B
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid B, previous_label spare
   Then the card's outcome line names both labels, work and spare
    And (pending `login-mismatch-policy`: whether the relabel completes at all. If it does, the card shows this line. Under a refuse option the card shows its Refused state and no refresh acknowledgement exists)
    # The item's default: the line #1474 gives a refresh, followed by ` (was ‘spare’)`. The draft reads
    # `Refreshed ‘work’ (was ‘spare’)`: one line, with the success cue, under the
    # `identityElision` policy (apps/menubar/Sources/StatusPanelFormat.swift:708). Covers AC
    # `oracle-the-card-shows-the-previous-label` and AC-IDN-004. This card stays after the beat: a `done`
    # whose outcome line names a previous label carries a notice (#1561 § Persistence), see Rule 5.

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

## Rule 3 — a capture that makes a label name a second account says so, naming every bearer

> REQ-ROS-I-002, AC-ROS-003, AC `oracle-the-card-shows-a-collision`, design UX-3, UX-9, § 9.6. The facts
> are the acknowledgement's. `collisions[].bearers` lists the uuids of every entry that bears the label
> after the write, in roster order, so a bearer's position is its ordinal, and `account_uuid` marks the
> written entry. Every bearer of a collided label is ambiguous by definition, so each is named by its
> ordinal and its uuid. The line says what the duplicate costs (a switch to either is refused from the
> panel) and where the remedy is (Settings). It never says the write was refused, and it never enforces
> uniqueness (AC-6 of `docs/requirements/migration-credential-portability.md`, ratified: "BUT NOT by
> enforcing label uniqueness"). The form of the report is `duplicate-label-policy`'s, so the loud
> scenarios wait and the quiet ones do not.

```gherkin
Scenario: 🟥 a new account under a label another account bears
  Given a roster A:work
    And the signed-in account is B, which the roster does not hold
   When a menu-bar capture asks for work
    And the daemon acknowledges it as captured, label work, account_uuid B, with the collision on work, bearers A then B
   Then the card shows its outcome line, then a collision line
    And the collision line says work now names 2 accounts
    And it names B as this one, work 2 of 2, with B's uuid, and A as the other, work 1 of 2, with A's uuid
    And it says that until one is renamed a switch to either is refused from the panel, and that Settings is where to rename
    And (pending `duplicate-label-policy`: whether the report is this line on the completed capture, or a refusal before any write)
    # The item's default is warn. The draft reads
    #   ‘work’ now names 2 accounts: this one is work 2 of 2 (<uuid-B>), the other work 1 of 2 (<uuid-A>).
    #   Until one is renamed, you can't switch to either from the panel. Rename one in Settings.
    # Under refuse the card shows its Refused state, nothing is written, and the `collisions` fixture of
    # this scenario cannot occur. Covers AC `oracle-the-card-shows-a-collision`; the item suggests the
    # name `a_capture_that_makes_a_label_name_a_second_account_shows_the_collision_line`.
    # Seam: AccountCaptureModel over the fake connection that AccountCaptureTests feeds
    # (`CommandFakeConnection`, apps/menubar/Tests/ControlCommandTransportTests.swift:346), plus the
    # card's tree through `PanelA11y.tree` (apps/menubar/Tests/PanelAccessibilityTreeTests.swift:172).

Scenario: 🟥 a third account joins a label two accounts already bear
  Given a roster A:work, B:work
    And the signed-in account is C, which the roster does not hold
   When a menu-bar capture asks for work
    And the daemon acknowledges it as captured, label work, account_uuid C, with the collision on work, bearers A, B, C in that order
   Then the collision line says work now names 3 accounts
    And it names C as this one, work 3 of 3, and names A and B as the others in roster order, work 1 of 3 and work 2 of 3, each with its uuid
    And (pending `duplicate-label-policy`: whether the report is this line)
    # The item says "with three or more bearers it lists the others in roster order" and drafts no
    # three-bearer line, so the wording is not pinned (see Not pinned). This is the deepening scenario of
    # docs/specs/import-duplicate-label.feature.md: a report that fired only on a first duplicate would
    # swallow a worse one. Covers AC `line-values-and-order`, the three-bearer fixture.

Scenario: 🟥 the written entry is found by its uuid and not by its place in the list
  Given a roster A:work, B:spare, C:work
    And the signed-in account is B
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid B, previous_label spare, with the collision on work, bearers A, B, C in that order
   Then the collision line names B as this one, work 2 of 3, and names A and C as the others, work 1 of 3 and work 3 of 3
    And (pending `duplicate-label-policy`: whether the report is this line)
    And (pending `login-mismatch-policy`: whether the relabel completes)
    # The acknowledgement lists the bearers in roster order and marks the written entry by
    # `account_uuid` (design § 8.1). The card takes "this one" from the uuid, never from a position.
    # Covers AC `oracle-the-card-shows-a-collision` ("marks the written entry").

Scenario: 🟥 the collision line names what import's notice names
  Given a roster A:work
    And a menu-bar capture of the uuid B asks for work
   When import's duplicate-label notice is rendered for work, and the card's collision line is rendered for that capture
   Then both name the label work and say it now names more than one account
    And neither says the write was refused
    And (pending `duplicate-label-policy`: the collision line exists only if the form is a report on the completed capture)
    # Import's notice is `duplicate_label_notice` (src/cli.rs:5628). Covers the item's conformance
    # criterion to docs/specs/import-duplicate-label.feature.md, by concretize-and-compare and not by
    # reading prose: render both for the roster of that file's first scenario, one label on two uuids.

Scenario: the account that already bears the label is not a collision
  Given a roster A:work
    And the signed-in account is A
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid A, and nothing else
   Then the card shows no collision line
    # The counterpart of import's "same label, same uuid" scenario in
    # docs/specs/import-duplicate-label.feature.md: one bearer before the write and one after, so the
    # count did not rise. AC-ROS-001 names a report here as a BUT NOT. Covers AC `quiet-capture-unchanged`
    # and the item's conformance criterion to that file (a quiet scenario with a capture counterpart).
    # It waits for #1474's decode of `refreshed`.

Scenario: a duplicate the roster already holds is not announced again by a capture that adds none
  Given a roster A:work, B:work
    And the panel's roster already shows work twice
    And the signed-in account is B
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid B, and nothing else
   Then the card shows no collision line
    # The counterpart of import's "overwriting a duplicate the target already had" scenario: two
    # bearers before the write and two after. The collision line is a fact of the write and never of
    # the rows the panel holds. A duplicate that already exists is the Settings note's (#1560) and the
    # rows' (#1566). Covers REQ-ROS-Q-001's second fixture and the item's conformance criterion to
    # docs/specs/import-duplicate-label.feature.md, and fails a card that derives its line from the
    # roster. It waits for #1474's decode of `refreshed`.
```

## Rule 4 — a credential another entry holds is named on the card, in the line the two labels call for

> REQ-STSH-B-002 (Must), AC-STSH-001, AC `oracle-the-card-shows-a-shared-credential`, design UX-9,
> § 9.6. The card reads `shared_with` from the acknowledgement: #1557 detects the pair and #1559 carries
> it. The line names both entries by label, and by ordinal and uuid beside a label with more than one
> bearer. A side's label counts as ambiguous when `collisions` lists it, when both sides carry the same
> label, or when the panel's roster rows show it more than once. A side whose ordinal the
> acknowledgement gives shows it. A side that only the roster rows show to be ambiguous is named by
> label and uuid without an ordinal, because a row cannot be matched to a uuid until the status frame
> carries account identifiers (#1571, consumed by the panel in #1578). The card reports and never
> chooses: no entry is merged, disabled or removed, and no credential byte, digest or fingerprint
> reaches it. Design § 9.6 puts the wording of these lines under copywriter review, so whoever writes
> the oracle reads Not pinned first.

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

Scenario: 🟥 the written side's label is ambiguous because the capture made it so
  Given a roster A:work, C:spare
    And the signed-in account is B, which the roster does not hold, and its credential is the one C's stash holds
   When a menu-bar capture asks for work
    And the daemon acknowledges it as captured, label work, account_uuid B, with the collision on work, bearers A then B, and shared_with spare C
   Then the card shows its outcome line, the collision line, then the shared line
    And the shared line names spare by its label alone and names B as work 2 of 2, with B's uuid
    And (pending `duplicate-label-policy`: the fixture needs a completed capture that carries a collision)
    # The draft reads `‘spare’ and work 2 of 2 (<uuid-B>) hold the same credential.` Covers AC
    # `line-values-and-order`, "one side ambiguous through `collisions`", and its line order.

Scenario: 🟥 both sides carry one label
  Given a roster A:work
    And the signed-in account is B, which the roster does not hold, and its credential is the one A's stash holds
   When a menu-bar capture asks for work
    And the daemon acknowledges it as captured, label work, account_uuid B, with the collision on work, bearers A then B, and shared_with work A
   Then the card shows its outcome line, the collision line, then the shared line
    And the shared line names B as this one, work 2 of 2, and A as work 1 of 2, each with its uuid
    And (pending `duplicate-label-policy`: the fixture needs a completed capture that carries a collision)
    # The draft reads `This one, work 2 of 2 (<uuid-B>), and work 1 of 2 (<uuid-A>) hold the same
    # credential.` Equal labels with no collision, which is a refresh of an entry whose label already had
    # two bearers, give the acknowledgement no ordinal, and the item drafts no line for that (see Not
    # pinned). Covers AC `line-values-and-order`, "equal labels".

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

## Rule 5 — a card that carries a notice stays until the operator's next action on it

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
> Opening Settings closes the panel and so resets the card, which is why #1560 shows the same collision
> there. The fixtures use a shared-credential notice, which neither policy touches; the rule is the same
> for a collision line.

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

Scenario: 🟥 a refresh that renamed the account and found nothing else
  Given a roster B:spare
    And the signed-in account is B
   When a menu-bar capture asks for work
    And the daemon acknowledges it as refreshed, label work, account_uuid B, previous_label spare, with no collision and no shared_with
   Then the card is still `done` after the beat the reset would fire in, and the outcome line is announced once
    # Settled in #1561 on 2026-10-27 (§ Persistence): a `done` whose outcome line names a previous label
    # carries a notice, because the relabel is disclosed nowhere else in the app.
    And (pending `login-mismatch-policy`: whether the relabel completes)
    # Before #1561's amendment both readings were in the sources: the item's title and design § 9.4 call
    # the relabel a notice, and UX-1 says a transient element vanishes before it is read, so it would stay;
    # design § 9.5 and UX-6 fold the relabel into the outcome line and name the notice lines as collision
    # and shared only, so the card would return to idle after 1.8 s with the previous label gone before it
    # can be read. The item now pins the first reading (§ Persistence). Covers AC `persistence` and AC `announcement`.
```

## Rule 6 — each notice is announced once, in line order, without moving focus

> Design UX-8, and WCAG 2.2 Success Criterion 4.1.3 (Status Messages), https://www.w3.org/TR/WCAG22/#status-messages
> (the link is the item's), as to its intent: a status message reaches assistive technology without
> receiving focus. That criterion scopes itself to content in markup languages, so the native panel
> applies its intent and not its letter. AC `announcement`. When a card finishes with a notice, the model
> calls an injected closure, the way `panelKeyRequest` is injected:
> - `panelKeyRequest` is declared at apps/menubar/Sources/AccountCaptureModel.swift:98.
> - `panelKeyRequest` is supplied by the controller at apps/menubar/Sources/StatusItemController.swift:117.
>
> The controller supplies the real post for this closure too. It is outside the test bundle and is
> checked by hand, as the design README's manual checklist records. The sentence carries the ordinal and
> never a uuid; the uuids stay in the visible text. The app has no announcement code today, and its
> deployment floor is macOS 13 (apps/menubar/project.yml).

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

Scenario: 🟥 a collision is announced with its ordinal and no uuid
  Given the daemon acknowledges a capture as captured, label work, account_uuid B, with the collision on work, bearers A then B
   When the card finishes the capture
   Then the announcement closure is called once, with a sentence that says work now names 2 accounts and that this one is 2 of 2
    And the sentence holds no uuid
    And (pending `duplicate-label-policy`: the fixture needs a completed capture that carries a collision)
    # The draft reads `‘work’ now names 2 accounts; this one is 2 of 2.` Covers AC `announcement`.

Scenario: 🟥 two notices are announced in line order, once each
  Given the daemon acknowledges a capture as captured, label work, account_uuid B, with the collision on work, bearers A then B, and shared_with spare C
   When the card finishes the capture
   Then the announcement closure is called twice
    And the first sentence is the collision's and the second is the shared credential's
    And (pending `duplicate-label-policy`: the fixture needs a completed capture that carries a collision)
    # Covers AC `announcement`, "once per notice, in line order".
```

## Rule 7 — no notice rests on colour alone, notice lines wrap rather than truncate, and the outcome stays on one line

> Design § 9.5, and WCAG 2.2 Success Criterion 1.4.1 (Use of Color), https://www.w3.org/TR/WCAG22/#use-of-color
> (the link is the item's), as to its intent. ACs `not-colour-alone`, `wrap-not-truncate` and
> `tree-shape`. A collision line takes an attention treatment and not the green success check, and each
> notice line carries a glyph or a text cue besides its colour. The glyph and tone of each line are a
> pure value in `StatusPanelFormat`, so a test can pin that the collision glyph differs from the success
> glyph. A notice line wraps within the card's text budget, `captureCardTextBudget` (apps/menubar/Sources/StatusPanelFormat.swift:160),
> and is never elided, so no uuid loses a character.
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

Scenario: 🟥 the collision line takes an attention treatment
  Given a card that finished with a collision line
   Then the collision line's glyph differs from the success glyph
    And it carries a glyph or a text cue beyond its colour
    And (pending `duplicate-label-policy`: the line exists only under a report on the completed capture)
    # Covers AC `not-colour-alone`: "the collision glyph differs from the success glyph".

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

Scenario: 🟥 the collision lines and the equal-labels shared line wrap within the notice bound
  Given the two-bearer and the three-bearer collision lines, the equal-labels shared line, and a card at each Dynamic Type class
   When each is measured against the card's text budget
   Then each wraps within the notice bound and is never elided
    And (pending `duplicate-label-policy`: these strings exist only under a report on the completed capture)
    # Covers AC `wrap-not-truncate`, "the two-bearer and three-bearer collision lines and the longest
    # shared line". The longest shared line is the equal-labels one, whose ordinals need a collision.

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
```

## Rule 8 — a notice names accounts by label, ordinal and uuid only, and every new string passes the framing scan

> REQ-NTC-C-001, REQ-NTC-C-002, AC-NTC-002, issue #15, issue #160, and ACs `only-labels-and-uuids` and
> `copy-passes-the-framing-scan`. The card's only inputs are labels and uuids from the acknowledgement,
> so it never carries a credential, an email address the operator did not author, or a value derived
> from a credential. Every new string passes the framing-vocabulary scan with no exemption: the whole
> banned-token list and the banned phrases that `src/framing_vocabulary.rs` holds, read at test time by
> `FramingVocabularyScan` (apps/menubar/Tests/NotificationDeliveryTests.swift:198).

```gherkin
Scenario: each line is its template plus the sentinel labels and uuids
  Given acknowledgements whose labels and uuids are sentinel values
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

| Blocked on | Question | Hangs on it | The item's own default |
|---|---|---|---|
| `duplicate-label-policy` | When a capture makes a label name a second account, does the card show a collision line on the completed capture (warn), or its Refused state with nothing written (refuse)? Refuse needs AC-6 of `docs/requirements/migration-credential-portability.md` ("BUT NOT by enforcing label uniqueness") re-ratified first, a `label-in-use` tag on `CaptureRejection`, and its Swift case in the same commit | the 🟥 scenarios of Rules 3, 4, 6 and 7 that name it; the future oracle for `oracle-the-card-shows-a-collision` | warn, the only form that needs no re-ratification |
| `login-mismatch-policy` | When a capture gives an already-rostered account a different label, does the card show the refresh with the old label named, show it with a further warning, show its Refused state (a `relabels-another-account` tag), or compare an email-shaped label with the harvested address? | the 🟥 scenarios of Rules 2, 3 and 5 that name it; the future oracle for `oracle-the-card-shows-the-previous-label` | keep the rename and disclose it |

`relabel-persists` and `shared-line-per-holder`, two handles this stub gave to gaps in the
item, are no longer open: on 2026-10-27 #1561 was amended so that a `done` whose outcome line names a
previous label carries a notice, and the card shows one shared-credential line per `shared_with`
element in roster order.

## Calls this stub took

Each is a call made without the operator, and each is ratification-pending.

| Call | Why | If the answer is otherwise |
|---|---|---|
| `panel-notices-name-the-uuid` is built as the item builds it: a notice names an entry by its uuid only beside a label with more than one bearer, next to its ordinal | Design § 14.6 item 10a: "If unanswered, the requirements as written govern and the notices name the uuid." REQ-STSH-B-002 (Must) and REQ-NTC-C-001 name the uuid | The uuid parts of Rules 3, 4 and 8 go; the facts and the ordinals stay |
| The shared line's ordinal rule is the item's: a side that only the roster rows show to be ambiguous is named by label and uuid, without an ordinal | The item states it, and says it is this pipeline's reading of the design. Design § 9.6 takes the ordinal from the watch frame, which needs the frame to carry identifiers (#1571, #1578) | The scenario of Rule 4 whose other side is ambiguous only in the panel's rows changes; the others do not |
| The card reads `shared_with` from the acknowledgement (DQ-6 closed as "both channels") | Design § 14.8 records it, REQ-STSH-B-002 (Must) says "on the surface that performed the write", and #1559 builds on it | Rule 4, Rule 6's shared sentence and Rule 5's fixtures go |
| The item is Must-have | The tracker gives it no priority label, and it serves REQ-ROS-I-002 and REQ-STSH-B-002, both Must | At Should the Red Card Gate would not block |
| "Notice line" means a collision line or a shared line in Rules 5 to 7, and a `done` whose outcome line names a previous label counts as carrying a notice | Design § 9.5 names the notice lines as those two kinds; #1561 § Persistence (since 2026-10-27) adds the relabel-bearing `done` | Rules 5 and 6 already include the relabel |
| Rule 5's fixtures use a shared-credential notice | Neither policy touches it, so the rule stays decided; the rule is the same for a collision line | None |
| Rule 5's stale-timer example is this stub's addition | AC `persistence` does not name it. The reset compares the phase it scheduled for with the phase it finds, and that comparison changes with the `done` payload | Drop the example |

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from something the
item does not spell out.

- The words of every line except the quiet outcome line, which an existing test pins. The item calls its
  lines drafts, and design § 9.6 puts the shared lines under copywriter review. An oracle either asserts
  the facts by containment (the label, the count, the uuids, the ordinals, the marking, the order) or
  pins the draft bytes knowing that a copy change edits them in the same change.
- The three-bearer collision line, which the item does not draft.
- The form of a side named by label and uuid without an ordinal.
- The shared line when both sides carry one label and the acknowledgement gives no ordinal.
- The order of the two sides of a shared line. The item's drafts differ on which side comes first.
- The announcement sentence of the shared notice (copy review), and the closure's name: `announce` is a
  suggestion.
- The first half of a refresh's outcome line, which is #1474's.
- The Swift type that carries the facts in the finished phase.
- The glyphs and tones, beyond: the collision glyph differs from the success glyph, and each notice
  line carries a glyph or a text cue.
- The number of the notice wrap bound, which the pull request measures and states in the test.
- Where the ordinal function lives. #1566, #1574 and #1560 need it too, and whichever item lands first
  adds it as a pure Foundation-only function next to the formatting code.
- The values in the mock rows, which the design README calls directional.
- What the card shows when `collisions` is present but its bearers do not hold `account_uuid`, or when
  `collisions` arrives without `account_uuid`. The item defines the card for well-formed
  acknowledgements only.

## Acceptance criteria that no scenario expresses

- *Mock first* (AC `mock-first`): both capture frames, `capture-states-light` and `capture-states-dark`,
  hold a row for each new state, authored in a commit that precedes the Swift commits of the same pull
  request, with the render regenerated and the figures and register in apps/menubar/design/README.md
  updated (its sections Regenerating the render and Expected reconciliations). A reviewer reads the
  commit order and the diff of `apps/menubar/design/`; no script decides it.
- *Existing tests* (AC `existing-tests-change-mechanically`): the counts in `PanelCaptureCardTests`
  change only where the card gained strings. The `.done(label:)` assertions are Rule 1's.
- *Output conforms to the mock*: build the card in each new state, open the mock beside it, and compare
  glyph, tone, line order and wrapping state by state. It is manual by design, because the card cannot be
  rasterized with its text field. The result goes in the pull request, with any intended divergence
  entered under Expected reconciliations.
- *Output conforms to docs/specs/import-duplicate-label.feature.md*: Rule 3's import-notice scenario is
  the loud half, and Rule 3's last two scenarios are the quiet half. The import stub's swap scenario and
  its "artifact carrying its own duplicate" scenario have no capture counterpart, because one capture
  writes one entry.
- *Mutation canaries*: each new predicate carries one, driven through the same function the real
  assertion calls, as `testTheWrapGateFiresOnAnUnscaledCard` does.
- *The real announcement*: the controller's post is outside the test bundle. The manual checklist in
  apps/menubar/design/README.md (Capture + notification pre-release checklist (manual)) gains the
  announcement, and the VoiceOver step is run and recorded in the pull request.
- *Gates*: `cd apps/menubar && xcodegen generate && xcodebuild test -project Menubar.xcodeproj -scheme
  Menubar -configuration Debug -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO`, and the armed
  `panel-goldens` run, which must print its `[panel-goldens] max drift` line. A run that says tests were
  skipped compared nothing.
- *RED records*: the item owes a failing run, recorded on the issue, for each oracle AC. See the next
  section.

## Where each group is asserted

Taken from the acceptance criteria, for whoever binds the scenarios.

- Rules 1 to 4: the pure line functions in `StatusPanelFormat`, and `AccountCaptureModel` over the fake
  connection, with the card's tree through `PanelA11y.tree`.
- Rule 5: `AccountCaptureModel` with the beat injectable.
- Rule 6: `AccountCaptureModel` with the announcement closure.
- Rule 7: the pure treatment value; `TextMetrics` in the metrics lane of `PanelCaptureCardTests`; and
  `PanelA11y.tree` in the tree lane.
- Rule 8: the pure line functions over sentinel inputs, and `FramingVocabularyScan`.

## Oracle authorship

*Work class*: deterministic-output. The primary deliverable is the card's lines (strings from pure
functions in `StatusPanelFormat`), the finished phase's persistence and announcement in
`AccountCaptureModel`, and the card's accessibility tree. A value assertion, a phase assertion and a
tree assertion each falsify a wrong output mechanically. The mock rows and the README figures are an
accompanying prose/config aspect. Their fidelity is a judgment aspect for an independent reviewer, by
concretize-and-compare, and no existing script decides it: the card never appears in the panel goldens,
and `panel-goldens` is a soft gate.

*Authorship*: no oracle item is filed yet. The item's three oracles (the collision, the previous label
and the shared credential) assert a form that `duplicate-label-policy` and `login-mismatch-policy`
decide, and the PRD says as much: the answer "fixes the assertion form of REQ-ROS-C-002's oracles"
(§ 8.1), and design § 14.6 item 3 says it "decides whether F1's oracles assert a report or a refusal".
The scenarios blocked on them are the 🟥 scenarios of Rules 2 to 7 whose pending line names either
policy. The relabel scenario of Rule 5 and Rule 4's several-holders scenario are decided since
2026-10-27 (#1561 settled both). The one oracle item is filed once the operator
decides. The decided scenarios wait for it too, because an item gets one oracle item. The Blue Card
Gate also routes the item back to /decompose first, so the oracle's scope follows the split.

For whoever files it once the operator decides:

- The card's new state does not exist until #1559 decodes the facts and #1474 decodes `refreshed`, so a
  test written before cannot name the finished phase's new payload. Assert through seams that compile
  before the change: the fake connection that feeds `AccountCaptureModel` an acknowledgement line, and
  `PanelA11y.tree` over the real card. Such a test compiles and fails at run time.
- `StatusItemController` is outside the test bundle, so the real announcement post and the panel's close
  are not oracle material.
- A RED oracle cannot be merged failing, because `ci-ok` cannot merge a failing suite and
  `main-protection` has no bypass actor, and the repo's landing rule for it is stated in Rust terms
  (`#[ignore = "RED oracle: ..."]`). Swift has no `#[ignore]`. This stub found no Swift precedent: no
  test under apps/menubar/Tests uses `XCTExpectFailure`, `git log -S'XCTExpectFailure' -- apps/menubar`
  returns nothing, and the `XCTSkipUnless` uses there are environment-gated measurement gates and not
  RED oracles. The filer chooses the XCTest form, names it in the oracle item, keeps a non-ignored
  companion on the same fixture, and proves the oracle discriminates by applying the fix in a scratch
  copy and mutating each guarded invariant.
