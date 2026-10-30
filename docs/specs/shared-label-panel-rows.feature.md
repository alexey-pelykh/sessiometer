# Feature: accounts that share a label are separate, distinguishable rows in the panel

Issue #1566 · PRD § 10.4 REQ-ROW-B-001, REQ-ROW-C-001, REQ-ROW-C-002 (AC-ROW-001, AC-ROW-002) and § 10.11 REQ-NTC-C-001, REQ-NTC-C-002, REQ-NTC-C-003 · design § 9.1 UX-3 to UX-5, § 9.5, § 9.6, § 11.4, § 11.5

Example Mapping: 🟦 7 rules (over the 5-rule gate: reported for /decompose, not split here) · 🟩 35 examples · 🟥 2 open (DQ-4; `label-normalization-policy`)

Tier B — scenarios pending binding in #1566's execution. Work class: deterministic-output (the primary deliverable is the panel's rows, names and strings, which a mechanical grader can falsify); the mock frame in Rule 7 is a design-artifact aspect with no oracle.

- A scenario marked `RED-owed` and bound to a test named in oracle item #1582 has its grader pre-authored there, at seams the fix does not change. #1582 lands first.
- Every other `RED-owed` scenario has no stable seam before the fix, because its function, tie-break or swap interface is new. #1566's executor authors it and proves it by mutation.
- A scenario marked `passes today` is a regression guard on the unmodified tree.

> **PROVISIONAL — stated once, here.** `docs/requirements/account-label-integrity.md` carries `dor_status: failed`: every requirement in it is pipeline-authored and unratified, and its problem framing is unvalidated. The operator has ratified the class of work this file rests on: R-6 in `docs/requirements/migration-credential-portability.md` (user-ratified 2026-08-04; it records duplicate labels as a documented, accepted state) and the parity ruling of 2026-07-31 in `docs/requirements/gui-cli-capability-parity.md` ("Choosing a surface is not accepting reduced capability"). Everything below is a pipeline draft, written under these defaults, none of them ruled on:
>
> - The ordinal "n of N" is the duplicate set's one handle on every surface, and the draft strings in #1566's Specification are wording defaults (design UX-3, § 9.6). The facts below are binding. Copy review may refine the words inside the framing-vocabulary scan.
> - `colour-seed-from-identifier` is answered no: only a duplicate set gets distinct colour slots, and a unique label keeps its hash slot. If the operator answers yes, the unique-label colour scenario in Rule 3 and every golden in Rule 6 change.
> - One frame pair carries the whole duplicate-set design: the rows, the header, the callout and the blocked treatment #1573 draws. The alternative is a second page or a shorter frame, and the item records that trade on #1566 before authoring.
>
> `account-key-form` (PRD DQ-1) does not touch this file: the panel works with the daemon as it is, with no identifier on the wire.

> **Story.** The operator holds two accounts under one label, which R-6 allows. The panel's row identity is the label, so SwiftUI is handed two rows with one identity, a state its rules leave undefined. Even if both rows are drawn, they carry one monogram, one colour and one spoken name. The CLI's `list` tells the two apart by uuid. The panel cannot tell them apart at all. This item draws one row for each account line and tells same-label rows apart by their place among the label's bearers, with no daemon change.
>
> The trap in this feature is that three plausible fixes each pass one scenario and fail another. Dropping a line whose label repeats makes every identity unique and loses an account (Rule 1). Ordering the ordinals by the figures, or alphabetically, passes the pair and breaks the moment the daemon reorders its roster (Rule 2). Giving the callout the ordinal of the first row that matches its label passes for the first bearer and names the wrong row for the second, because the wire carries no identifier to choose with (Rule 4).

## Rule 1 — every account line is its own row, whatever its label

```gherkin
Scenario: two lines with one label are two rows, each with its own figures
  # AC: oracle-two-lines-one-label-two-rows · REQ-ROW-B-001 · RED-owed
  # Binds: testTwoStatusLinesWithOneLabelMakeTwoRowsWithTheirOwnFigures (oracle #1582, panel tests); this is the name #1566 suggests, `two_status_lines_with_one_label_make_two_rows_with_their_own_figures`, in the form XCTest discovers, because it runs only methods whose names start with `test`; companion testTheSharedLabelFrameDecodesToThreeRowsInDaemonOrderWithTheirOwnFigures (oracle #1582, same frame, asserts nothing about identity)
  Given a status frame whose account lines are "work" (session 42%, weekly 66%), "work" (session 18%, weekly 51%) and "spare" (session 24%, weekly 39%, active)
   When the panel builds its rows
   Then there are three rows, in the order the daemon sent them
    And the three rows have three distinct identities
    And the first "work" row carries 42% and 66%, and the second carries 18% and 51%
    And no line is dropped or merged, and no row carries another line's figures
    # A row's identity is its label (`var id: String { label }`, `apps/menubar/Sources/HonestStateMachine.swift:363`), and the roster draws the rows in one loop over them (`ForEach(rows)`, `apps/menubar/Sources/StatusPanelRoster.swift:44`). So the two "work" rows share an identity. SwiftUI requires the identities in a list to be unique, so what the panel does with this frame is undefined (inferred from that rule; the mock authors no frame for the state).
    # Only the identity assertion is RED. The figures already travel in order, because the rows are built one line at a time (`static func rows(from`, `apps/menubar/Sources/HonestStateMachine.swift:429`). A test that asserts the figures alone is green on arrival and proves nothing about the change.

Scenario: a daemon that predates the identifier still yields one row for each line
  # AC: older-daemon-fixture · REQ-ROW-C-001 · RED-owed
  # Binds: testAnOlderDaemonFrameWithTwoLinesOfOneLabelBuildsTwoDistinctRows (oracle #1582, panel tests); companion testTheOlderDaemonFrameDecodesWithoutErrorAndKeepsBothLines (oracle #1582, same frame)
  Given a status frame at the newest minor that predates the account identifier, with two lines labelled "work" and no identifier on either
   When the panel decodes the frame and builds its rows
   Then decoding raises no error
    And there are two rows with two distinct identities, and no line is dropped
    # No daemon sends an identifier yet, so this is every daemon today: the status schema is 1.14 (`pub(crate) const STATUS_SCHEMA_VERSION`, `src/daemon/snapshot.rs:538`). If #1571 has landed, use the minor before the identifier's. The fixture is a version-pinned compatibility fixture, named and commented as one, because #1571's fixture sweep selects fixtures by the current minor and would otherwise carry this one forward.
    # Decoding and the row count are green on arrival. The distinct-identity assertion is the RED one: a test that only counts rows passes before the change, which proves nothing about it (AC-NTC-003).

Scenario: no text in the roster, the header or the callout is shaped like a uuid
  # AC: no-identifier-displayed · PRD AC-WIRE-001, its clause "BUT NOT displaying the identifier to the operator" · passes today
  # Binds: testNoPanelTextIsShapedLikeAUuid (oracle #1582, companion)
  Given the shared-label roster, with its header and its callout
   When the panel's spoken text is read
   Then no text is shaped like a uuid
    # A regression guard for #1571 and #1578, which put an identifier on the wire. It must never reach a row, the header or the callout. The check carries its own canary, so a predicate that cannot match is caught.
```

## Rule 2 — a label's bearers are counted by exact string, in the order the daemon lists them

```gherkin
Scenario Outline: the ordinal is a bearer's place among the rows that carry its label
  # AC: ordinal-function · UX-3 · RED-owed
  # Binds: executor-authored; the pure function over a roster's labels (its name is chosen by the first of #1560, #1561 and #1566 to land)
  Given a roster whose labels, in the order the daemon lists them, are <labels>
   When the ordinal is computed for each position
   Then each position's ordinal is <ordinals>

  Examples:
    | labels                   | ordinals                       |
    | work, spare, work        | 1 of 2, none, 2 of 2           |
    | work, work, work         | 1 of 3, 2 of 3, 3 of 3         |
    | work, spare, work, spare | 1 of 2, 1 of 2, 2 of 2, 2 of 2 |
    | work                     | none                           |
    | (no labels)              | (no positions)                 |
    | "", ""                   | 1 of 2, 2 of 2                 |
    # The function is pure and Foundation-only. It compares labels as exact strings, as the daemon does when it resolves one (`pub(crate) fn resolve_target`, `src/use_account.rs:486`; `account.label == query`, `src/use_account.rs:490`). Two empty labels are one pair because identical strings are the same under any comparison.
    # It is new, so its direct test cannot be pre-authored: a test that calls it does not compile on today's tree. Its effect is graded through the panel in Rule 3 (oracle #1582). #1566's executor proves this table by mutation. If #1560 or #1561 has already added the function, reuse it and add no second one.

Scenario: the ordinal follows the frame's order, not the account
  # AC: invariant, "a row's ordinal depends only on the order and labels of the frame it is drawn from" · UX-3 · RED-owed
  # Binds: executor-authored
  Given one frame that lists the "work" account with figures 42%/66% before the "work" account with figures 18%/51%
    And a later frame that lists the same two accounts in the other order
   When the panel reads each frame
   Then in the first frame the 42%/66% row is "1 of 2" and the 18%/51% row is "2 of 2"
    And in the later frame the 18%/51% row is "1 of 2" and the 42%/66% row is "2 of 2"
    # The wire carries no identifier, so the panel has nothing to remember an account by from one frame to the next. An ordinal is a position, not a name: two same-label rows swap ordinals when the daemon reorders its roster. #1578 adds the identifier that lets later items do better.

Scenario: labels that merely resemble each other carry no ordinal
  # AC: ordinal-function, exact-string comparison · UX-3 · RED-owed
  # Binds: executor-authored
  Given a roster whose labels are "work-alice", "work-bob" and "work"
   When the ordinal is computed for each position
   Then no position has an ordinal
    # The existing `same-local-part` stress frame has this shape (`stress("same-local-part"`, `apps/menubar/Sources/PanelRenderHarness.swift:526`): labels that share a prefix are different labels, and #445's monogram escalation already tells them apart. That frame's unchanged golden (Rule 6) is the end-to-end check.
```

## Rule 3 — rows that share a label are told apart by more than colour

```gherkin
Scenario: each row of a shared label speaks its own ordinal, and no other element does
  # AC: badge-and-spoken-name (the spoken half) · WCAG 4.1.2 and 1.4.1 · UX-3, UX-8 · RED-owed
  # Binds: testEachRowOfASharedLabelSpeaksItsOrdinalAndNoOtherRowDoes (oracle #1582, panel tests)
  Given the shared-label roster, where "spare" is the active row
   When the panel is read through its accessibility tree
   Then the "work" row with figures 42%/66% speaks "1 of 2", and the "work" row with figures 18%/51% speaks "2 of 2"
    And each row speaks its own figures, and no other row speaks them
    And both names still lead with the label "work"
    # Figures-per-row is RED today, not a given: while a row's identity is its label, SwiftUI draws the first bearer twice and the second bearer's figures nowhere (measured on a stand-in by #1582's readiness gate, 2026-10-28, and re-run 2026-10-30; the real tree is dumped before any test is written, PRD A-5).
    And the "spare" row speaks no ordinal
    And no other element of the panel speaks "1 of 2" or "2 of 2"
    # The facts are the ordinal and the label. The issue's example name, "work, 2 of 2 accounts named work", is a draft. Row names are composed by one function (`static func rowAccessibilityLabel`, `apps/menubar/Sources/StatusPanelFormat.swift:2174`), which starts from the label (`var parts: [String] = [label]`, `apps/menubar/Sources/StatusPanelFormat.swift:2191`).
    # "No other element" is the badge staying hidden from VoiceOver where the row's name already speaks the ordinal. A badge left in the tree makes VoiceOver say it twice. The badge has no tooltip of its own: a row-level tooltip wins over a child's (`WINS over a child's`, `apps/menubar/Sources/StatusPanelRoster.swift:328`), and #953 recorded the deliberate absence of a tooltip on the active row (`NO TOOLTIP HERE, DELIBERATELY`, `apps/menubar/Sources/StatusPanelRoster.swift:275`).
    # VoiceOver itself is checked by hand. The README's checklist (`### VoiceOver pre-release checklist (manual)`, `apps/menubar/design/README.md:676`) gains a line for the duplicate row names.

Scenario: a row with a unique label keeps today's spoken name and speaks no ordinal
  # AC: badge-and-spoken-name (unique rows) · unique-labels-unchanged · passes today
  # Binds: testTheUniqueLabelRowsKeepTheirSpokenNames (oracle #1582, companion)
  Given a roster in which no label has a second bearer
   When the panel is read through its accessibility tree
   Then each row's spoken name, up to its first reading of the session window, is what the unmodified tree produces for it
    And no row speaks an ordinal

Scenario: a duplicate row shows its ordinal as a badge
  # AC: badge-and-spoken-name (the visible half) · UX-3 · RED-owed · visual form decided by the mock frame (🟥 DQ-4)
  # Binds: executor-authored; read against the mock frame by Rule 7's comparison
  Given the shared-label roster
   When the panel is drawn
   Then each "work" row shows a badge that reads "1 of 2" or "2 of 2" for its place in the roster
    And a row with a unique label shows no badge
    # Where the badge sits, how it is set apart from the label text and what it is filled with belong to the mock frame. No scenario here states a visual outcome. The badge's text and boundary are measured in the contrast scenario below.

Scenario Outline: the rows of a duplicate set take colour slots that no other row uses while the palette has a free one
  # AC: colour-tie-break · UX-3 · RED-owed
  # Binds: executor-authored; the pure tie-break over the roster's labels in order
  Given a roster in which the unique labels occupy <occupied> of the eight palette slots
    And two more rows share one label
   When the panel assigns colour slots
   Then <outcome>

  Examples:
    | occupied | outcome                                                                                                                            |
    | 3        | the two sharing rows take two different slots, and no other row uses either slot                                                   |
    | 7        | one sharing row takes the eighth slot, which no other row uses, and the other takes a slot that the fewest other rows use          |
    | 8        | each sharing row takes a slot that the fewest other rows use                                                                       |
    # The hash gives two rows with one label the same slot by construction. The tie-break is applied by the caller that holds every row, because the hash function sees one label at a time. The assertion is a property of the result, not a literal slot: which free slot a row takes is the executor's choice.
    # `occupied` counts the slots the other rows' hashes land on. A palette slot is one of the eight in the fill table (`private static let accountFillPalette`, `apps/menubar/Sources/StatusPanelFormat.swift:2737`).

Scenario: a row with a unique label keeps the colour its label hashes to
  # AC: colour-tie-break (the hash and the slot of a unique label do not change) · unique-labels-unchanged · passes today
  # Binds: executor-authored; the existing hash tests stay green
  Given any roster
   When the panel assigns colour slots
   Then each row whose label has one bearer takes the slot that the label's hash gives it
    # The hash is a fixed FNV-1a of the trimmed label, modulo the palette size (`static func accountColorIndex(for label: String)`, `apps/menubar/Sources/StatusPanelFormat.swift:2755`). It stays.
    # DEFAULT, ratification-pending: `colour-seed-from-identifier` is answered no. Yes would seed every row's colour from its identifier. Colours would then survive a relabel, but every panel would repaint once and every committed panel golden would move.

Scenario: the badge's text and boundary are legible in both themes on every colour slot
  # AC: colour-tie-break (contrast) · WCAG 1.4.3 and 1.4.11 · UX-3 · RED-owed
  # Binds: executor-authored; the contrast helper the palette tests use
  Given each of the eight colour slots, in the light theme and in the dark theme
   When the badge's text and its boundary are measured against their backgrounds
   Then the text is at least 4.5:1 against the badge fill
    And the boundary is at least 3:1 against the panel's reference base
    # Asserted as the palette's existing contrast test asserts the monogram (`func testAccountPaletteMeetsWcagAAAgainstThePanelReferenceBase`, `apps/menubar/Tests/StatusPanelFormatTests.swift:1703`): against the mock's opaque popover base, light #f7f7fa and dark #26262b, because the real panel floats on live vibrancy and cannot be measured headlessly.

Scenario: two rows of one label share a monogram
  # AC: "The monogram function is unchanged" · UX-3 · passes today
  # Binds: the existing monogram tests stay green
  Given the shared-label roster
   When the panel resolves the monograms
   Then both "work" rows carry the same monogram
    And the two rows differ in their ordinal and, while the palette allows, in their colour slot
    # A repeated label resolves once (`if result[label] != nil { continue }`, `apps/menubar/Sources/StatusPanelFormat.swift:2788`). The fix leaves that alone. The monogram is not what tells the pair apart.
```

## Rule 4 — the header and the callout say when their label is shared

```gherkin
Scenario Outline: the header names the active row's ordinal when the active label is shared
  # AC: header-and-callout · UX-3 · RED-owed (the third example passes today)
  # Binds: testTheHeaderNamesTheActiveRowsOrdinalWhenItsLabelIsShared (oracle #1582, panel tests); testTheHeaderIsUnchangedWhenTheActiveLabelIsUnique (oracle #1582, companion)
  Given the shared-label roster, where the active row is <active>
   When the panel reads its header
   Then the header says <header>

  Examples:
    | active            | header                          |
    | the second "work" | 3 accounts · work 2 of 2 active |
    | the first "work"  | 3 accounts · work 1 of 2 active |
    | "spare"           | 3 accounts · spare active       |
    # The ordinal is the place of the row that the frame marks active, found from the frame alone. `headerSubtitle` takes the active label (`static func headerSubtitle`, `apps/menubar/Sources/StatusPanelFormat.swift:2104`; `let base = activeLabel.map`, `apps/menubar/Sources/StatusPanelFormat.swift:2119`), and its caller picks the first active row (`let activeLabel = store.rows.first(where: \.isActive)?.label`, `apps/menubar/Sources/StatusPanelView.swift:222`). A frame marks at most one line active (`active: active == Some(i)`, `src/daemon/snapshot_build.rs:425`).
    # The oracle pins the facts: the count, the label with its ordinal, and the word "active". The exact string is a draft.
    # The published header node is `Sessiometer. ` plus this sub-line (`.accessibilityLabel("Sessiometer. \(subtitle)")`, `apps/menubar/Sources/StatusPanelChrome.swift:57`); the oracle selects the node by that prefix and asserts the text after it.

Scenario Outline: the callout says its target is one of N accounts with that label, and claims no ordinal
  # AC: header-and-callout · UX-5 · RED-owed
  # Binds: testTheCalloutCountsTheBearersOfASharedTargetAndClaimsNoOrdinal (oracle #1582, panel tests)
  Given a roster in which <bearers> accounts are labelled "work"
    And the daemon names "work" as the next-swap target, with no identifier
   When the panel reads its next-swap callout
   Then the callout says its target is "one of <bearers> accounts named work"
    And its spoken label begins "Next swap to work, one of <bearers> accounts named work"
    And it states no ordinal such as "1 of 2" or "2 of 2"

  Examples:
    | bearers |
    | 2       |
    | 3       |
    # The callout names its target by label alone (`static func swapCalloutTarget`, `apps/menubar/Sources/StatusPanelFormat.swift:2128`), and the daemon's target carries no identifier. With two bearers, "work" names neither row. The callout can count and cannot choose, so an ordinal here would name a row the panel is guessing at. The three-bearer example pins that the count is read from the frame.
    # The visible form (where the clause sits and how it composes with the why line) is the mock frame's (🟥 DQ-4). The spoken fragment is the draft the issue states.

Scenario: the callout is unchanged when its target's label has one bearer
  # AC: header-and-callout (a unique target) · passes today
  # Binds: testTheCalloutIsUnchangedForAUniqueTarget (oracle #1582, companion)
  Given the shared-label roster, with "spare" named as the next-swap target and no reason sent with it
   When the panel reads its next-swap callout
   Then the callout reads as it does today
    And its spoken label is "Next swap to spare."
    # The spoken sentence is the target, then the daemon's reason when one is sent (`"Next swap to \(target). \(reason)."`, `apps/menubar/Sources/StatusPanelFormat.swift:2162`), and it ends at the target when none is (`"Next swap to \(target)."`, `apps/menubar/Sources/StatusPanelFormat.swift:2164`). The oracle's frames send no reason, so the sentence above is the whole of it.
```

## Rule 5 — a swap's in-flight state belongs to the row that started it

```gherkin
Scenario: a switch started from one of two same-label rows shows its in-flight state on that row only
  # AC: switching-state-per-row · UX-4 · RED-owed
  # Binds: executor-authored; the swap model and the pure function that decides a row's switching
  Given the shared-label roster, where "spare" is the active row
    And the operator starts a switch from the second "work" row
   When the swap is in flight
   Then the second "work" row shows its in-flight state
    And the first "work" row does not
    And every row stays disabled until the swap settles
    # Today a row is switching when the pending target equals its label (`private var isSwitching`, `apps/menubar/Sources/StatusPanelRoster.swift:256`), and the swap model records only the target's label (`case pending(target: String)`, `apps/menubar/Sources/AccountSwapModel.swift:89`), so both "work" rows show it. The model has to record which row started the swap, and a row has to compare itself with that.
    # The row sends its label (`Task { await swap.swap(to: row.label) }`, `apps/menubar/Sources/StatusPanelRoster.swift:319`), and the daemon refuses an ambiguous target. #1573 shows that refusal before the click. This scenario is about the in-flight display only.
    # Not claimed here: the callout's own Swap compares labels too (`private var isSwitchingToTarget`, `apps/menubar/Sources/StatusPanelChrome.swift:193`). #1573 disables it for a shared target before the click.

Scenario: a switch started from a row with a unique label is unchanged
  # AC: switching-state-per-row (a unique row) · passes today
  # Binds: the existing in-flight tests stay green
  Given a roster in which no label has a second bearer
    And the operator starts a switch from the row labelled "spare"
   When the swap is in flight
   Then only the "spare" row shows its in-flight state
```

## Rule 6 — a roster with no shared label looks and reads as it does today

```gherkin
Scenario: every committed panel golden is byte-identical
  # AC: unique-labels-unchanged · passes today
  # Binds: the existing golden comparison, armed (`func testEveryRenderMatchesItsCommittedGolden`, `apps/menubar/Tests/PanelGoldenParityTests.swift:1285`)
  Given the panel goldens committed before this item
   When this item's change is diffed and the armed golden comparison runs
   Then no pre-existing golden file is modified
    And the armed comparison reports every pre-existing state within its drift ceiling
    # Read the result off the `[panel-goldens] max drift` line, never off `** TEST SUCCEEDED **`. Both golden comparisons skip unless `TEST_RUNNER_SESSIOMETER_PANEL_GOLDEN_GATE=1` arms them, and a run that says tests were skipped compared nothing.
    # A modified pre-existing golden would also need a `Panel-Goldens-Rebaselined:` trailer. This item owes that trailer only for the new pair in Rule 7.

Scenario: a roster whose labels merely resemble each other renders as today
  # AC: unique-labels-unchanged · passes today
  # Binds: the existing `same-local-part` golden pair
  Given the existing `same-local-part` stress roster, whose labels share a prefix and are all different strings
   When the panel is drawn
   Then it matches the committed golden
    And no row shows an ordinal

Scenario: the Stats tab, the capture card, Settings and the wire are untouched
  # AC: "Nothing else changes" · passes today
  # Checks: no test; read off the diff's paths at review
  Given this item's change
   When its diff is read
   Then it changes nothing in the Stats tab's, the capture card's or the Settings window's sources
    And it changes nothing in `src/daemon/snapshot.rs` or under `build/fixtures/`
    # Settings has its own ordinal item (#1574), the capture card has its notices (#1561), and the wire has its identifier (#1571). A change here that reaches them is another item's work. The diff's paths decide this.
```

## Rule 7 — the design reference leads, a golden pair follows, and every string is scanned

```gherkin
Scenario: the mock carries the shared-label frame before any Swift draws it
  # AC: mock-frame-first · REQ-ROW-C-002 · a design artifact: no oracle is owed
  # Checks: the render command in `apps/menubar/design/README.md` § Regenerating the render, which is judged on its output file and not on its exit code; `apps/menubar/design/build-comparison.py`, which stops on a missing or duplicate frame name; an independent design-fidelity reviewer for the rest
  Given no Swift draws the shared-label state yet
   When the design mock is read
   Then it carries one frame pair, light and dark, for a roster in which two rows share a label
    And the pair shows the rows with their badges and colour slots, the header line, the callout in the form this item builds, and the blocked treatment of #1573
    And the page still renders to an image
    And the README's Expected reconciliations list names each part of the frame that the built panel draws only when a later item lands
    # Mechanical aspects, each decided by a check that exists. A frame name must appear exactly once: the comparison tool stops on a duplicate (`if name in pops:`, `apps/menubar/design/build-comparison.py:54`) and, for a paired frame the mock lacks, names the frames the mock does carry (`def design(name)`, `apps/menubar/design/build-comparison.py:70`). The page must render: the headless Chrome command in the README writes `renders/all-states.png`, and a document past the height cap writes no image at all. When the README last re-measured the room (at #763) it recorded about 686 px left, roughly one popover row. The mock has gained frames since then (48 distinct frames today, 46 at that measurement), so the executor measures the room again before authoring and does not rely on that figure.
    # Judgment aspects go to an independent design-fidelity reviewer who reads the mock: whether the frame shows two rows a reader can tell apart, and whether the Expected reconciliations list is complete. A green render says the page renders, not that the frame is right.
    # If the pair cannot fit, the item records the trade (fewer frames, a shorter frame or a second page) on #1566 before authoring (🟥 DQ-4).

Scenario: the new frame is rendered, paired with the mock by name, and has a committed golden pair
  # AC: goldens-for-the-new-frame · REQ-ROW-C-002 · RED-owed (the missing-golden failure)
  # Binds: executor-authored; the existing golden comparison
  Given the shared-label fixture in the render harness and its frame in the mock
   When the comparison tool and the golden comparison run
   Then the comparison tool pairs the built panel with the mock's frame by name
    And a golden pair, light and dark, is committed with the `Panel-Goldens-Rebaselined:` trailer
    And a render of a fixture with no committed golden fails the golden comparison
    # The pairing is by name, as the pathological-content frames' is (`stress("degenerate-label"`, `apps/menubar/Sources/PanelRenderHarness.swift:551`; `STATES = [`, `apps/menubar/design/build-comparison.py:92`).
    # The missing-golden failure exists today only when the gate is armed. The loop skips a cell that has no golden (`guard let golden = loadGolden(cell)`, `apps/menubar/Tests/PanelGoldenParityTests.swift:1294`), and the final count is what fails (`XCTAssertEqual(checked, all.count`, `apps/menubar/Tests/PanelGoldenParityTests.swift:1318`). Unarmed, the comparison is skipped and says nothing.
    # The pair is produced by the executor, so it is not an independent oracle: it records what the built panel draws. Its independent check is the conformance scenario below.

Scenario: every string this item adds is a named constant that the framing scan reads
  # AC: strings-are-scanned · REQ-NTC-C-002 · RED-owed (the registration)
  # Binds: executor-authored
  Given the strings this item adds: the ordinal badge, the spoken name's clause, the header's clause and the callout's clause
   When the framing-vocabulary scan runs over them
   Then each one is a named constant that the scan reads
    And a banned token injected into any of them is caught
    # The scan (`enum FramingVocabularyScan`, `apps/menubar/Tests/NotificationDeliveryTests.swift:198`) reads the banned list from `src/framing_vocabulary.rs` (`pub const BANNED_TOKENS`, `src/framing_vocabulary.rs:182`). Only that one test file uses it, and it screens notification strings. No test reads a panel string through it today, so the registration seam is new work, and "registered" has to mean something a test can fail on, such as a constant added without being registered. The injected-token half is the mutation proof.

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

- **DQ-4 — the panel frame for two rows with one label.** The mock must author it before Swift draws the state (REQ-ROW-C-002). It decides the badge's position and look, how the colour slots appear, the callout's visual form and how that composes with its why line, and whether one frame pair carries the whole duplicate-set design or the item records a trade first. No scenario states a visual outcome. Rule 7's conformance scenario defers every visual fact to the frame once it exists. The draft strings are wording defaults, and the scenarios pin facts (the ordinal, the label, the count) and not phrases.
- **`label-normalization-policy` — what makes two labels the same.** The panel compares labels as exact strings, because the daemon does (`account.label == query`, `src/use_account.rs:490`). Labels that differ only by case, surrounding whitespace or Unicode form (`Work` and `work`) form one duplicate set or two depending on a policy that F8 (#1575 to #1577) may adopt, so no example states which. If F8 changes the daemon's comparison, the panel's must follow it. The ordinal function is the one place to change.
