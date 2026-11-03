# Feature: the panel keys its rows, their in-flight state and the next-swap callout on the account identifier

Issue #1578 · PRD § 10.4 REQ-ROW-B-001, REQ-ROW-C-001, REQ-ROW-C-002 (AC-ROW-001, AC-ROW-002) and § 10.11 REQ-NTC-C-001, REQ-NTC-C-002, REQ-NTC-C-003 · design D-6, § 8.3, § 9.1 UX-4 and UX-5, § 9.5, § 9.6, § 11.5

Example Mapping: 🟦 7 rules (over the 5-rule gate: reported for /decompose, not split here) · 🟩 31 examples · 🟥 2 open (DQ-4; `label-normalization-policy`)

Tier B — scenarios pending binding in #1578's execution, which starts after #1571, #1566 and the oracle item #1585 have landed. Work class: deterministic-output (the primary deliverable is the panel's row identities, flag, spoken labels and strings, which a mechanical grader can falsify). The callout's visible form and the shared-label mock frame are design-artifact aspects with no oracle.

- A scenario marked `RED-owed` and bound to a test named in oracle item #1585 has its grader pre-authored there, at a seam the fix does not change. #1585 lands after #1566 and before #1578.
- A scenario marked `RED-owed` and `executor-authored` has no stable seam before the fix, because its function, swap interface or registration is new. #1578's executor writes it, proves it by mutation and records the RED output on #1578.
- A scenario marked `passes once #1566 has landed` is a guard that is green on the tree #1578 starts from. It stops the fix from breaking what #1566 built.
- A scenario marked `passes today` is a regression guard on the unmodified tree.
- A scenario marked `review-bound` or `design artifact` is decided by recorded command output or by an independent reviewer, not by a test.

> **PROVISIONAL — stated once, here.** `docs/requirements/account-label-integrity.md` carries `dor_status: failed`: every requirement in it is pipeline-authored and ratification-pending. The operator has ratified the class of work this file rests on: R-6 in `docs/requirements/migration-credential-portability.md` (user-ratified 2026-08-04; it records duplicate labels as a documented, accepted state) and the parity ruling of 2026-07-31 in `docs/requirements/gui-cli-capability-parity.md` ("Choosing a surface is not accepting reduced capability"). The panel's own record R-2 (re-ratified 2026-07-09) gives next-swap one home, the footer callout, and no per-row marker. Everything below is a pipeline draft, written under these defaults, none of them ruled on except where marked:
>
> - The callout's wording is a default: `→ work 2 of 2`, and the spoken `Next swap to work, 2 of 2 accounts named work` (design § 9.6, a draft). The facts are binding: the label, the target row's ordinal and the bearer count. Copy review may refine the words inside the framing-vocabulary scan. The oracle pins the facts, and pins a phrase only in an assertion tagged `[DRAFT]`.
> - `panel-switches-by-identifier` is answered no, as the operator decided on 2026-11-03 (keep the switch blocked): the panel's switch keeps sending the label. A yes answer is a separate change to the switch's target and to the blocked-switch item's (#1573) rule for frames that carry identifiers.
>
> `account-key-form` (PRD DQ-1) changes no outcome in this file. The panel reads the identifier as an opaque string and never shows it. The form decides only the spelling of the key a fixture carries, and the oracle carries both candidate spellings (design § 8.3).

> **Story.** The operator holds two accounts under one label, which R-6 allows. Once the wire carries an identifier on each account line and on the next-swap target (#1571), the panel can stop identifying a row by its place and its label. This item keys a row's identity, its in-flight swap state, the next-swap flag and the footer callout's lookup of its row on that identifier. A daemon that sends none keeps position plus label. The callout then says which `work` the daemon means, "→ work 2 of 2", where without an identifier it can only say "one of 2 accounts named work". Nothing displays the identifier.
>
> The trap in this feature is that four plausible fixes each pass one scenario and fail another. Keeping identity on position plus label passes the pair and breaks on a relabel or a reorder (Rule 1). Comparing the line's optional identifier with the target's optional identifier flags every row of an older daemon's frame, because two absent values are equal (Rule 3). Finding the callout's row by label alone names the first bearer, or the last, and is wrong for the other (Rule 4). Reading the identifier as a UUID passes with the uuid-shaped value and fails with any other (Rule 5).

## Rule 1 — a row's identity is its account identifier, and position plus label only for a line that carries none

```gherkin
Scenario: two lines with one label and different identifiers are two rows with two identities
  # AC: oracle-row-identity-is-the-identifier (its first Given/then clause: "the rows have two distinct identities") · REQ-ROW-B-001 · passes once #1566 has landed
  # Binds: testTwoSameLabelLinesWithDifferentIdentifiersAreTwoRowsWithDistinctIdentities (oracle #1585, companion); testTheIdentifierBearingFramesDecodeToRowsInDaemonOrderWithTheirOwnFigures (oracle #1585, companion 6: the decode guard under every RED test, which asserts labels, figures and `isActive` in daemon order and nothing about identity)
  Given a status frame whose account lines are "work" (identifier A), "work" (identifier B) and "spare" (identifier C)
   When the panel builds its rows
   Then there are three rows with three distinct identities
    # Green on arrival, once #1566 has made position plus label the identity. It is here as a guard: the change that moves identity onto the identifier must not collapse the pair. Only the relabel and the reorder below are RED.

Scenario: a row keeps its identity when its account is relabelled between frames
  # AC: oracle-row-identity-is-the-identifier (its second Given/then clause: "that row's identity is unchanged", and its clause "It must not use the label or the position when an identifier is present") · REQ-ROW-B-001 · RED-owed
  # Binds: testARowKeepsItsIdentityWhenItsAccountIsRelabelledBetweenFrames (oracle #1585); this is the name #1578 suggests, `a_row_keeps_its_identity_when_its_account_is_relabelled_between_frames`, in the form XCTest discovers, because it runs only methods whose names start with `test`
  Given a status frame whose first two lines are "work" (identifier A, session 42%, weekly 66%) and "work" (identifier B, session 18%, weekly 51%)
    And the next frame lists the same two accounts with the first one relabelled "home"
   When the panel builds the rows of each frame
   Then the row that carries identifier A has the same identity in both frames
    And the row that carries identifier B has the same identity in both frames
    # Position plus label is the fallback, and it fails here: the label is part of that identity, so the relabel moves it. Today a row's identity is its label alone (`var id: String { label }`, `apps/menubar/Sources/HonestStateMachine.swift:363`) and #1566 makes it position plus label. Neither survives a relabel.
    # The rows are found by their figures, which differ on every line, and not by their place. The assertion compares two identities with `==` and never reads one as a string, so it holds whatever type the fix gives `id`.

Scenario: a row keeps its identity when the next frame lists the accounts in another order
  # AC: oracle-row-identity-is-the-identifier · Specification "Row identity", "Two successive frames that list the same account give the same identity" · #1578 Why: "a frame that lists the accounts in a different order, changes it" · RED-owed
  # Binds: testARowKeepsItsIdentityWhenTheNextFrameListsTheAccountsInAnotherOrder (oracle #1585)
  Given a status frame whose first two lines are "work" (identifier A, session 42%, weekly 66%) and "work" (identifier B, session 18%, weekly 51%)
    And the next frame lists the same two accounts in the other order
   When the panel builds the rows of each frame
   Then the row that carries identifier A has the same identity in both frames
    And the row that carries identifier B has the same identity in both frames
    # Position plus label fails this one: A moves from place 0 to place 1, and its identity moves with it. Under today's label-only identity the two rows share one identity, so this assertion would pass for the wrong reason on a tree without #1566. The oracle therefore runs after #1566 and also asserts the pair's distinct identities in a companion.

Scenario: a daemon that sends no identifier is read exactly as before
  # AC: identifier-absent-falls-back · REQ-ROW-C-001 · AC-ROW-002 · passes once #1566 has landed
  # Binds: testAFrameWithNoIdentifierKeepsPositionPlusLabelIdentities (oracle #1585, companion); testAnOlderDaemonFrameWithTwoLinesOfOneLabelBuildsTwoDistinctRows (oracle #1582), which must pass without being edited
  Given a status frame in which two lines carry the label "work" and no line carries an identifier
   When the panel decodes the frame and builds its rows
   Then decoding raises no error
    And there are two rows with two distinct identities, and no line is dropped
    And #1566's older-daemon fixture passes without an edit to it
    # No identifier is the shape of every daemon until #1571 ships and of any older one after. The fixture is pinned at the outgoing minor, which design § 8.3 calls for ("a fixture pinned at the outgoing minor, carrying two account lines with one label, proves the fallback"), so #1571's sweep of the current-minor fixtures leaves it alone. This item must not move it either.
    # The same frame builds the same identities each time it is decoded, because the fallback is a function of place and label and of nothing else.
```

## Rule 2 — an in-flight swap belongs to the row whose identity started it

```gherkin
Scenario: a switch started from one of two same-label rows is in flight on that row only
  # AC: switching-state-follows-identity · UX-4 · RED-owed · executor-authored
  # Binds: executor-authored; the swap model and the pure function that decides a row's switching. Both are new with #1566 or with this item, so a test that calls them does not compile on today's tree and no seam exists before the fix
  Given a frame whose lines are "work" (identifier A), "work" (identifier B) and "spare" (identifier C, active)
    And the operator starts a switch from the second "work" row
   When the swap is in flight
   Then the second "work" row shows its in-flight state
    And the first "work" row does not
    And every row stays disabled until the swap settles
    # The row compares the swap model's pending target with its label (`private var isSwitching`, `apps/menubar/Sources/StatusPanelRoster.swift:256`), and the model records a label (`case pending(target: String)`, `apps/menubar/Sources/AccountSwapModel.swift:89`). #1566 changes both to record which row started the swap, by place and label. This item moves that record to the row's identity.
    # Not claimed here: the callout's own Swap compares labels too (`private var isSwitchingToTarget`, `apps/menubar/Sources/StatusPanelChrome.swift:193`). #1573 disables it for a shared target before the click.

Scenario Outline: the in-flight state stays with the account when the next frame changes the roster
  # AC: switching-state-follows-identity ("including when the next frame reorders the roster or relabels the account") · UX-4 · RED-owed · executor-authored
  # Binds: executor-authored
  Given the same switch in flight from the second "work" row, whose identifier is B
   When the next frame arrives and <change>
   Then the in-flight state is shown on the row that carries identifier B
    And it is shown on no other row

  Examples:
    | change                                                 |
    | lists the two "work" accounts in the other order       |
    | relabels the account that carries identifier B "home"  |
    # Position plus label moves the state to the other account when the roster is reordered, and a label-keyed state loses it when the account is relabelled. The identity is what survives both.

Scenario: with no identifier the in-flight state is what #1566 built
  # AC: identifier-absent-falls-back · passes once #1566 has landed
  # Binds: #1566's executor-authored in-flight tests stay green
  Given a frame at a daemon that sends no identifier, whose lines are "work", "work" and "spare" (active)
    And the operator starts a switch from the second "work" row
   When the swap is in flight
   Then only the second "work" row shows its in-flight state
```

## Rule 3 — the next-swap flag marks the one row the target's identifier names, and is never guessed

```gherkin
Scenario Outline: the flag marks the row whose identifier the target carries
  # AC: next-swap-flag-flags-one-row · RED-owed
  # Binds: testTheNextSwapFlagMarksTheOneRowTheTargetsIdentifierNames (oracle #1585)
  Given a frame in which <bearers> lines are labelled "work", each with its own identifier
    And the target is "work" and carries the identifier of <target line>
   When the panel builds its rows
   Then exactly one row is flagged as the next-swap target
    And it is <flagged row>

  Examples:
    | bearers | target line             | flagged row            |
    | 2       | the second "work" line  | the second "work" row  |
    | 2       | the first "work" line   | the first "work" row   |
    | 3       | the third "work" line   | the third "work" row   |
    # The flag compares labels today (`account.label == targetLabel`, `apps/menubar/Sources/HonestStateMachine.swift:445`), so every "work" row is flagged. The flag exists on `AccountRow` and `rows(from:)` already (`static func rows(from`, `apps/menubar/Sources/HonestStateMachine.swift:429`), so this is graded at a seam the fix does not change. No view renders the flag (R-2), and an existing test reads it (`XCTAssertTrue(target.isNextSwapTarget)`, `apps/menubar/Tests/StatusPanelFormatTests.swift:1578`).
    # Both bearers are tried, and a third, so that "the last match" and "the first match" are each caught.

Scenario: the flag marks no row when an older daemon names an ambiguous target
  # AC: next-swap-flag-flags-one-row ("for none when the target has no identifier and an ambiguous label") · RED-owed
  # Binds: testTheNextSwapFlagMarksNoRowForAnAmbiguousTargetWithoutAnIdentifier (oracle #1585)
  Given a frame at a daemon that sends no identifier, whose lines are "work", "work" and "spare"
    And the target is "work" and carries no identifier
   When the panel builds its rows
   Then no row is flagged as the next-swap target
    # The flag is not guessed: with two bearers and nothing to choose between them, any row it marked would be a coin toss. Today both "work" rows are flagged.
    # This is the one place where a frame without identifiers differs from what #1566 leaves behind, and only in a flag that no view renders. #1566 names the flag as out of its scope (its Out of Scope bullet on `isNextSwapTarget`), and this item's Specification states the outcome: "no row is flagged". The Context sentence that an older daemon's frame "behaves exactly as it does after the panel-rows item" is read as covering what the panel displays, which this scenario does not change.

Scenario: an older daemon's unique target still flags its own row, and only that row
  # AC: next-swap-flag-flags-one-row ("never for two rows") · Context "A frame without identifiers behaves as it does after the panel-rows item" (in every respect but the ambiguous-target flag) · passes today
  # Binds: testTheNextSwapFlagMarksTheUniqueLabelRowWhenTheTargetHasNoIdentifier (oracle #1585, companion)
  Given a frame at a daemon that sends no identifier, whose lines are "work", "work" and "spare"
    And the target is "spare" and carries no identifier
   When the panel builds its rows
   Then exactly the "spare" row is flagged
    # This guards the trap in the flag's obvious fix. Comparing the line's optional identifier with the target's optional identifier reads two absent values as equal, so it flags every row of this frame, the unique one and both "work" rows alike. It would pass the first two scenarios of this rule, and it fails here.
    # The next scenario covers a target that carries an identifier which no line carries; the callout's behaviour for that target is in Rule 4. Not an example either: a frame in which two lines carry one identifier. That breaks REQ-WIRE-I-001, the wire owns the guarantee that an identifier is unique per roster entry, and a conforming daemon never sends it.

Scenario: the flag marks no row when the target's identifier matches no line
  # AC: next-swap-flag-flags-one-row ("for none when the target's identifier matches no line") · Specification "The next-swap flag" · RED-owed · a pipeline default, ratification-pending: `unmatched-identifier-flags-nothing-1578`
  # Binds: testTheNextSwapFlagMarksNoRowWhenTheTargetsIdentifierMatchesNoLine (oracle #1585, RED 6, a `[DRAFT]` assertion: a ruling that marks a row instead edits it on #1585 before this item's executor starts); the same `ids-pair-target-unmatched` frame feeds #1585's companion 9, which asserts the callout's form (Rule 4)
  Given a frame whose lines are "work", "work" and "spare", each with its own identifier
    And the target is "work" and carries an identifier that no line carries
   When the panel builds its rows
   Then no row is flagged as the next-swap target
    # The daemon and the panel disagree about the roster in that frame, so any row the flag marked would be a guess. Today both "work" rows are flagged, because the flag compares labels. The callout keeps the "one of N" form (Rule 4), which no view's flag depends on.
```

## Rule 4 — the callout finds its row by the target's identifier, names it by ordinal, and keeps the "one of N" form when it cannot

```gherkin
Scenario Outline: the callout names the row the target's identifier names
  # AC: oracle-callout-finds-its-row · UX-5 · RED-owed (the spoken half has an oracle; the visible half is executor-authored)
  # Binds: testTheCalloutNamesTheTargetsRowByOrdinalWhenTheTargetsIdentifierMatchesALine (oracle #1585, the spoken half); the visible half is executor-authored over the pure callout text function the fix extends, and is read again against the mock frame by Rule 7
  Given a frame in which <bearers> lines are labelled "work", each with its own identifier
    And the target is "work" and carries the identifier of <target line>
   When the panel reads its next-swap callout
   Then the callout reads <visible>
    And its spoken label begins <spoken>
    And it does not say "one of"
    # DRAFT, ratification-pending (🟥 DQ-4): the words in the last two columns are the wording defaults of design § 9.6. The facts are binding: the label "work", the target row's place among the bearers and the bearer count. The oracle pins the facts as the label and as the digits of the callout's spoken text, in order (`22`, `12`, `33`), which does not depend on the words around them, and it pins each phrase only in an assertion tagged [DRAFT].

  Examples:
    | bearers | target line             | visible            | spoken                                           |
    | 2       | the second "work" line  | "→ work 2 of 2"    | "Next swap to work, 2 of 2 accounts named work"  |
    | 2       | the first "work" line   | "→ work 1 of 2"    | "Next swap to work, 1 of 2 accounts named work"  |
    | 3       | the third "work" line   | "→ work 3 of 3"    | "Next swap to work, 3 of 3 accounts named work"  |
    # The spoken label is built by one function (`static func swapCalloutAccessibilityLabel`, `apps/menubar/Sources/StatusPanelFormat.swift:2160`), and its caller passes it the target's label alone (`if let target = StatusPanelFormat.swapCalloutTarget(store.nextSwap)`, `apps/menubar/Sources/StatusPanelView.swift:456`). `swapCalloutTarget` returns the label of the target (`static func swapCalloutTarget`, `apps/menubar/Sources/StatusPanelFormat.swift:2128`).
    # Both bearers are tried so that "the ordinal of the first row that matches the label" and "the ordinal of the last" are each caught, and the third example reads the bearer count from the frame instead of fixing it at two.
    # The visible text is not observable at a seam the fix leaves alone. The card draws an arrow and the label as two Text views (`Text("→ ").fixedSize()`, `apps/menubar/Sources/StatusPanelChrome.swift:209`) inside one combined element whose spoken label replaces them (`.accessibilityLabel(accessibilityText)`, `apps/menubar/Sources/StatusPanelChrome.swift:224`), so the accessibility tree carries the spoken sentence only. The visible form is graded by an executor-authored test over the pure function, by the re-baselined golden (Rule 7), and by the comparison against the mock's frame. Only the last two are independent of the executor.

Scenario: a target identifier that matches no line keeps the form without an identifier
  # AC: oracle-callout-finds-its-row ("a target identifier that matches no line") · UX-5 · passes once #1566 has landed
  # Binds: testTheCalloutKeepsTheOneOfNFormWhenTheTargetsIdentifierMatchesNoLine (oracle #1585, companion)
  Given a frame in which two lines are labelled "work", each with its own identifier
    And the target is "work" and carries an identifier that no line carries
   When the panel reads its next-swap callout
   Then the callout says its target is "one of 2 accounts named work"
    And its spoken label begins "Next swap to work, one of 2 accounts named work"
    And it states no ordinal such as "1 of 2" or "2 of 2"
    # DRAFT wording, as above. The callout can count the bearers and cannot choose between them, so an ordinal here would name a row the panel is guessing at.

Scenario: a target with no identifier keeps the same form
  # AC: oracle-callout-finds-its-row ("or no target identifier") · UX-5 · REQ-ROW-C-001 · passes once #1566 has landed
  # Binds: testTheCalloutKeepsTheOneOfNFormWhenTheTargetHasNoIdentifier (oracle #1585, companion)
  Given a frame at a daemon that sends no identifier, whose lines are "work", "work" and "spare"
    And the target is "work" and carries no identifier
   When the panel reads its next-swap callout
   Then the callout says its target is "one of 2 accounts named work"
    And its spoken label begins "Next swap to work, one of 2 accounts named work"
    And it states no ordinal such as "1 of 2" or "2 of 2"

Scenario: a target whose label has one bearer is unchanged
  # AC: oracle-callout-finds-its-row ("For a unique target it is unchanged") · passes today
  # Binds: testTheCalloutIsUnchangedForAUniqueTarget (oracle #1585, companion)
  Given a frame at a daemon that sends no identifier, whose lines are "work", "work" and "spare"
    And the target is "spare" and carries no identifier
   When the panel reads its next-swap callout
   Then the callout reads as it does today
    And its spoken label is "Next swap to spare."
    # The spoken sentence is the target, then the daemon's reason when one is sent, and it ends at the target when none is (`"Next swap to \(target)."`, `apps/menubar/Sources/StatusPanelFormat.swift:2164`). The oracle's frames send no reason, so the sentence above is the whole of it.
```

## Rule 5 — the identifier is an opaque string of any shape, and it is never shown

```gherkin
Scenario Outline: the examples of Rules 1, 3 and 4 hold for a uuid-shaped identifier and for one of another shape
  # AC: identifier-is-opaque ("every criterion above passes with a uuid-shaped identifier and with a differently shaped one") · the item marks this criterion "RED: not owed", because the RED of each shape is the RED of Rules 1, 3 and 4 run once for each shape
  # Binds: the RED tests of oracle #1585 run once for each shape
  Given every identifier in those examples is <shape>
   When those examples run
   Then each gives the outcome it states

  Examples:
    | shape                                                                 |
    | a hyphenated value of the uuid shape (8-4-4-4-12 hexadecimal digits)  |
    | a short separated value of another length, such as five characters with one hyphen |
    # The fix must not parse or validate the identifier. An implementation that builds a `UUID` from it, or checks its length, passes the first row and falls back for the second, where the relabel and the reorder move the row's identity again.
    # Which form the wire carries is the operator's open decision `account-key-form`. It is the uuid or a value derived from it, and the item's criteria hold under both, so the two shapes stand in for the two answers.

Scenario: no text, spoken label or tooltip in the roster, the header or the callout contains the identifier
  # AC: identifier-is-opaque ("No text, spoken label or tooltip ... contains the identifier") · the item's Invariant ("no surface displays it") · PRD AC-WIRE-001, its clause "BUT NOT displaying the identifier to the operator" · RED: not owed; passes today, and guards the fix
  # Binds: testNoPanelTextTooltipOrSpokenLabelContainsTheIdentifier (oracle #1585, companion); testNoPanelTextIsShapedLikeAUuid (oracle #1582) stays green
  Given the shared-label frames of this file, with identifiers of each shape
   When the panel is read through its accessibility tree
   Then no element's text, spoken label or tooltip contains an identifier that the frame carries
    # Green today for a weak reason: the decoder drops the key it does not read (`private enum CodingKeys: String, CodingKey`, `apps/menubar/Sources/WireModel.swift:682`), so no identifier reaches a view. It becomes a real check the moment the fix reads the identifier. The check carries its own canary, so a predicate that cannot match is caught: the same predicate, fed a text that holds an identifier, reports a match.
    # Each tree node exposes a label, a value (the node's `text` is one of the two), a tooltip (`help`) and an accessibility identifier (`struct A11yNode`, `apps/menubar/Tests/PanelAccessibilityTreeTests.swift:97`). The check reads the first three. It does not read the accessibility identifier: that is a test hook, neither displayed nor spoken.
```

## Rule 6 — a roster with no shared label, and every golden but the shared-label pair, reads and renders as today

```gherkin
Scenario: a roster with no shared label reads as it does today, though its frame carries identifiers
  # AC: unique-labels-unchanged ("a roster with no duplicate label renders as it does today") · PRD § 10.4 Invariants: "No surface changes the text it displays for a roster without a duplicate label." · RED: not owed; passes today
  # Binds: testTheUniqueLabelRowsKeepTheirSpokenNamesWhenTheFrameCarriesIdentifiers (oracle #1585, companion)
  Given a frame at a current daemon whose lines are "alpha", "beta" (active) and "gamma", each with an identifier, and whose target is "gamma"
   When the panel is read through its accessibility tree
   Then each row's spoken name, the header's line and the callout's spoken label are what the unmodified tree produces for these labels
    And no row speaks an ordinal
    # The expected strings are captured from the tree before the oracle's other tests are written, hard-coded, and posted on the oracle item. Row names are composed by one function (`static func rowAccessibilityLabel`, `apps/menubar/Sources/StatusPanelFormat.swift:2174`) and the header's line by another (`static func headerSubtitle`, `apps/menubar/Sources/StatusPanelFormat.swift:2104`).

Scenario: every committed panel golden except the shared-label pair is byte-identical
  # AC: unique-labels-unchanged · passes today
  # Binds: the existing golden comparison, armed (`func testEveryRenderMatchesItsCommittedGolden`, `apps/menubar/Tests/PanelGoldenParityTests.swift:1285`)
  Given the panel goldens committed before this item, apart from the shared-label pair that #1566 commits
   When this item's change is diffed and the armed golden comparison runs
   Then no such golden file is modified
    And the armed comparison reports every state within its drift ceiling
    # Read the result off the `[panel-goldens] max drift` line, never off `** TEST SUCCEEDED **`. Both golden comparisons skip unless `TEST_RUNNER_SESSIOMETER_PANEL_GOLDEN_GATE=1` arms them, and a run that says tests were skipped compared nothing.

Scenario: the panel still sends a label as the swap target
  # AC: Specification "The panel keeps sending the label as the swap target" · DECIDED 2026-11-03: `panel-switches-by-identifier` = keep the switch blocked · review-bound
  # Checks: no test; read off the diff
  Given this item's change
   When its diff is read
   Then a row's switch and the callout's Swap each still pass a label to `swap(to:)`
    And no identifier is sent
    # The two sites are the roster row's switch (`Task { await swap.swap(to: row.label) }`, `apps/menubar/Sources/StatusPanelRoster.swift:319`) and the callout's button (`Button(action: { Task { await swap.swap(to: target) } })`, `apps/menubar/Sources/StatusPanelChrome.swift:228`).
    # If the operator answers yes, switching by identifier is a separate change to the switch's target and to #1573's rule, and this scenario is replaced.

Scenario: the wire, the daemon, Settings and the capture card are untouched
  # AC: Specification "Nothing else changes" ("No wire, daemon or Settings change") · review-bound
  # Checks: no test; read off the diff's paths
  Given this item's change
   When its diff is read
   Then it changes nothing under `src/` or `build/fixtures/`
    And it changes nothing in the Settings window's or the capture card's sources
    # The wire is #1571's, the capture card #1561's and the Settings rows #1574's and #1560's. A change here that reaches them is another item's work. The diff's paths decide this.
```

## Rule 7 — the design reference leads, the golden pair follows, and every added string is scanned

```gherkin
Scenario: the mock carries the callout's found form before any Swift draws it
  # AC: found-form-in-the-mock-first · Build Reference ("The found form of the callout, `→ work 2 of 2`, must be authored there before Swift draws it") · goldens-and-mock ("the design README's expected-differences list no longer names the found form") · REQ-ROW-C-002 · a design artifact: no oracle is owed
  # Checks: the render command in `apps/menubar/design/README.md` § Regenerating the render, which is judged on its output file and not on its exit code; `apps/menubar/design/build-comparison.py`, which stops on a missing or duplicate frame name; an independent design-fidelity reviewer for the rest
  Given the shared-label frame pair that #1566 authors in the mock, whose callout is in the "one of 2" form that #1566 builds
   When this item's Swift is about to draw the callout's found form
   Then the mock already shows that form, "→ work 2 of 2", in the pair or in a frame this item adds
    And the page still renders to an image
    And, once the built panel draws that form, the README's Expected reconciliations list no longer names it
    # The item's "expected-differences list" is the README's paragraph headed Expected reconciliations. #1566's pair shows the callout in the form #1566 builds (its Rule 7), so the found form is this item's to add to the mock.
    # Mechanical aspects, each decided by a check that exists: the frame name appears once, and the page renders. A document past the height cap writes no image at all, and the README records that the render was close to its limit when it was last measured, so the executor measures the room again before authoring.
    # Judgment aspects go to an independent design-fidelity reviewer who reads the mock: whether the found form reads as the callout's row, and whether the Expected reconciliations list is complete. A green render says the page renders, not that the frame is right.
    # If the pair does not already show the found form, this item adds it within the render budget, or records the trade (fewer frames, a shorter frame, or a second page) on #1578 first (🟥 DQ-4).

Scenario: the shared-label golden pair is re-baselined if the callout's form changes in it
  # AC: goldens-and-mock · RED-owed (the golden comparison fails until the pair is re-baselined) · executor-authored
  # Binds: executor-authored; the existing golden comparison. The trailer is decided by `scripts/check-panel-golden-rebaseline.sh`
  Given the harness fixture for the shared-label frame draws the callout in the found form
   When the armed golden comparison runs
   Then it fails until the shared-label golden pair is re-baselined
    And the re-baselined pair carries the `Panel-Goldens-Rebaselined:` trailer
    # The pair is produced by the executor, so it records what the built panel draws and is not an independent oracle. Its independent check is the conformance scenario below. If the fixture keeps the "one of 2" form, no golden moves and no trailer is owed.

Scenario: every string this item adds is a named constant that the framing scan reads
  # AC: strings-are-scanned · REQ-NTC-C-002 · RED-owed (the registration) · executor-authored
  # Binds: executor-authored
  Given the strings this item adds: the callout's found-form clause and its spoken clause
   When the framing-vocabulary scan runs over them
   Then each one is a named constant that the scan reads
    And a banned token injected into any of them is caught
    # The scan lives in a test file (`enum FramingVocabularyScan`, `apps/menubar/Tests/NotificationDeliveryTests.swift:198`) and screens notification strings. #1566 adds the first panel strings to it, or it does not. Whichever lands first adds the registration seam and the other reuses it. "Registered" has to mean something a test can fail on, such as a constant added without being registered. The injected-token half is the mutation proof.

Scenario: the built panel conforms to the mock's frame, read by concretize-and-compare
  # AC: "Output conforms to «`apps/menubar/design/menubar-preview.html`»" · verification, not a test
  # Checks: an independent reviewer reads the rendered pair; no test grades it
  Given the built panel rendered with `--render-panel` and the comparison page built by `build-comparison.py`
   When each difference between a built frame and its mock frame is read against the README's Expected reconciliations list
   Then every difference is one the list records
    And no difference is a mismatch the list does not record
    # An independent reviewer reads the rendered pair. Reading the Swift does not count. The mock is the oracle only for what it authors, and its hex and pixel values are directional.
```

The item's definition of done also includes the `swift` job's checks (`./scripts/check-menubar-zero-egress.sh`, then the app build and tests) and an armed `panel-goldens` run that prints its `[panel-goldens] max drift` line. They are gates on the change, not scenarios.

## 🟥 Open

- **DQ-4 — the panel frame for two rows with one label.** The mock must author it before Swift draws the state (REQ-ROW-C-002), and its spoken identity cue and the next-swap callout are named in the question. For this item it decides how the found form `→ work 2 of 2` looks, how it composes with the why line, and whether one frame pair carries the whole duplicate-set design or the item records a trade first. No scenario states a visual outcome. The draft strings are wording defaults, and the scenarios pin facts (the label, the ordinal, the count), with each phrase pinned only in an assertion tagged `[DRAFT]`.
- **`label-normalization-policy` — what makes two labels the same.** The panel compares labels as exact strings, because the daemon does (`pub(crate) fn resolve_target`, `src/use_account.rs:486`; `account.label == query`, `src/use_account.rs:490`). Labels that differ only by case, surrounding whitespace or Unicode form (`Work` and `work`) form one duplicate set or two depending on a policy that F8 (#1575 to #1577) may adopt, so no example states which. The callout's "more than one bearer" test and its ordinal read the same function #1566 adds. If F8 changes the daemon's comparison, that function is the one place to change.

Not Red cards, and why. `account-key-form` changes no outcome here: the identifier is opaque, and the form decides only a key's spelling in a fixture. `panel-switches-by-identifier` was decided on 2026-11-03, keep the switch blocked, which is what this item builds and states (Rule 6); `colour-seed-from-identifier` stays a default #1566 builds.
