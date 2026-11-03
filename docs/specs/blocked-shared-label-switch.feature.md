# Feature: a switch to an account whose label another account carries is blocked before it is requested

Issue #1573 · PRD § 10.4 (F4), which names no requirement for the blocked switch · design UX-10,
§ 9.1, § 9.3 TF-3, § 9.5, § 9.6, § 11.4, and § 11.5 CAP-11 (REQ-ROW-B-001, REQ-ROW-C-001,
REQ-ROW-C-002) · **Tier B** (every scenario pending binding)

Example Mapping: 🟦 5 rules · 🟩 21 examples · 🟥 2 open (`callout-swap-accessible-label`; `shared-label-frame-visual-form`); `panel-switches-by-identifier` was decided on 2026-11-03

> **PROVISIONAL.** The PRD's `dor_status` is `failed`: every requirement is unratified and the problem
> framing is unvalidated. Everything below is provisional, and that includes the wording. The sentences
> are the pipeline's defaults and wait for copy review; the facts they carry are binding. A change to a
> sentence moves this file, and the oracle that pins it, first.

This repo has no Gherkin runner; the executable gates are XCTest (`apps/menubar/Tests/`) and the armed
`panel-goldens` run. These scenarios pin each acceptance criterion in scenario form and name the test that
binds it. A binding marked `(oracle)` is a test pre-authored in #1583, which lands before the fix and
is RED against the tree without it. A scenario whose seam names the new `SwitchBlock` case is bound by
#1573's own test instead, because no test can name that case before the fix compiles.

**Story.** When two roster entries share a label, the panel offers a switch to either as available. The
request sends the label, and the daemon refuses it with `ambiguous-target`, because it resolves the
target before any viability gate: `resolve_target` is called at `src/daemon/commands.rs:109`, and the
refusal is built at `src/daemon/commands.rs:114` (`AmbiguousTarget`). The operator learns that after the
request. The status frame already proves the label is shared, and the panel already blocks, before the
request, every other refusal the frame proves.

"Shares a label" is the daemon's own test, exact string equality (`resolve_target`,
`src/use_account.rs:486-490`, which compares `account.label` to the query). This item consumes the test #1566 adds and does not define it. The scenarios use
clearly identical and clearly different labels only, so they hold under any answer to
`label-normalization-policy`.

## Rule 1 — a switch to an account whose label another account also carries is blocked before it is requested

```gherkin
Scenario: both accounts that share a label are blocked, and the active one is not a target
  Given a status frame listing `work`, `work` and `spare`, in that order
    And `spare` is the active account
    And no account is quarantined or weekly-exhausted
   When the panel decides what a switch to each account is
   Then a switch to the first `work` is blocked, for the shared label
    And a switch to the second `work` is blocked, for the shared label
    And `spare`, being active, is not a switch target
    # Covers AC `oracle-a-shared-label-row-is-blocked-before-the-click`. "Blocked" keeps its existing
    # meaning (`rowSwitchState`, `apps/menubar/Sources/StatusPanelFormat.swift:210`): a disabled control
    # carrying its reason. The daemon refuses a switch to EITHER `work`, so neither is offered.
    # Binds: `testARowWhoseLabelTwoAccountsShareIsBlockedBeforeTheClick` (oracle). The AC's own
    # suggested name is snake_case; an XCTest method needs the `test` prefix, so the words are kept
    # and the case is changed.

Scenario Outline: an account with a unique label keeps its own verdict beside a shared pair
  Given a status frame listing `work`, `work`, `spare` and `home`
    And `spare` is the active account
    And `home` is <state>
   When the panel decides what a switch to `home` is
   Then a switch to `home` is <verdict>

  Examples:
    | state            | verdict                                        |
    | healthy          | available                                      |
    | quarantined      | blocked, because its credential is quarantined |
    | weekly-exhausted | blocked, because its weekly limit is reached   |
    # Covers AC `oracle-a-shared-label-row-is-blocked-before-the-click` ("A row with a unique label is
    # available or blocked for its own reason"). Guards a flag computed for the whole frame instead of
    # for one label: with two `work` accounts present, every other row would read as shared.
    # Binds: `testAUniqueLabelRowBesideASharedPairKeepsItsOwnVerdict` (companion; green before the fix).

Scenario: the active account of a shared pair stays a plain row, and its sibling is blocked
  Given a status frame listing `work`, `work` and `spare`
    And the first `work` is the active account
   When the panel decides what a switch to each account is
   Then the first `work` is not a switch target
    And a switch to the second `work` is blocked, for the shared label
    And a switch to `spare` is available
    # Covers AC `oracle-a-shared-label-row-is-blocked-before-the-click` ("The active row stays
    # `notATarget`", Specification). The sibling is still refused by the daemon: it resolves `work` to
    # two entries before it asks which one is active.
    # Binds: `testTheActiveAccountOfASharedPairStaysPlainAndItsSiblingIsBlocked` (oracle).

Scenario: a blocked switch is a disabled control, so nothing reaches the daemon
  Given the frame of the first scenario
   When the operator tries to switch to either `work`
   Then no `swap` command is sent
    And the same attempt on an available account sends exactly one `swap` command naming it
    # Covers AC `no-click-reaches-the-daemon`. The negative half is OBSERVED as the disabled flag in the
    # hosted accessibility tree, which is this repo's own seam for a blocked row
    # (`testBlockedRowsAreExposedAsDisabled`, `apps/menubar/Tests/PanelAccessibilityTreeTests.swift:443`).
    # The AC's named seam, the swap model over a counting fake connection, shows only the positive half:
    # nothing in `apps/menubar/Tests` activates a disabled SwiftUI control (a search for accessibility
    # press, click and send-action drivers finds only comments about the status item), so a send count
    # over a model that was never asked proves nothing about the row.
    # Binds: `testARowWhoseLabelTwoAccountsShareIsBlockedBeforeTheClick` (the disabled flag; oracle) and
    # the existing `testTheGivenTargetIsTheTargetOnTheWire` (`apps/menubar/Tests/AccountSwapTests.swift:73`).

Scenario: a rename lifts the block on the next frame
  Given a frame listing `work`, `work` and `spare`, in which both `work` accounts are blocked
   When the next frame lists `work`, `home` and `spare`, the second `work` having been renamed
   Then a switch to `work` is available
    And a switch to `home` is available
    # Covers AC `oracle-a-shared-label-row-is-blocked-before-the-click`: the shared-label fact is
    # "computed by the caller from the roster rows" (Specification), so it is a function of the frame in
    # hand and never of an earlier one. It is the Necessity's own claim, that "a rename in Settings makes
    # either account reachable from the app".
    # Binds: `testARenameLiftsTheBlockOnTheNextFrame` (oracle).
```

## Rule 2 — the shared-label block outranks every other block

```gherkin
Scenario Outline: a duplicate account that is blocked for its own reason too shows the shared-label reason
  Given a status frame listing `work`, `work` and `spare`, with `spare` active
    And one `work` is <own gate>
   When the panel decides what a switch to that `work` is
   Then it is blocked for the shared label
    And the reason it gives is not the <own gate> reason, and it offers no remedy for it

  Examples:
    | own gate                         |
    | quarantined                      |
    | weekly-exhausted                 |
    | quarantined and weekly-exhausted |
    # Covers AC `shared-label-is-checked-first`. The daemon resolves the target before any viability
    # gate: `resolve_target` at `src/daemon/commands.rs:109`, and `quarantined` is first read at
    # `src/daemon/commands.rs:142`. A quarantined duplicate that showed the quarantine remedy would send
    # the operator to poke a label that is still ambiguous (the doc comment above `resolve_target`,
    # `src/use_account.rs:480-490`, names `poke` among its callers). A weekly-exhausted one would wait for a
    # reset that does not unblock it.
    # No Rust test pins this order at the daemon: the order is code order. This item changes no daemon
    # code, so none is owed here.
    # Binds: `testASharedLabelBlockOutranksAQuarantinedOrWeeklyExhaustedBlock` (oracle).
```

## Rule 3 — one reason, three channels, and the spoken one adds the ordinal

```gherkin
Scenario: the resting line states the reason alone
  Given a blocked shared-label account
   When the panel draws its resting line
   Then the line reads "Can’t switch — another account has this label."
    And it does not carry the remedy
    # Covers AC `row-copy-and-spoken-label`. MEASURED on 2026-10-27 at `.caption1`, the way the `width`
    # function of `TextMetrics` measures (`apps/menubar/Tests/TextMetrics.swift:40`): this cue fits the
    # row-cue line with a wide margin, so the existing fit test (`testEveryPersistentBlockedCueFitsItsRowLine`,
    # `apps/menubar/Tests/PanelTextMetricsTests.swift:223`) passes for it once the case joins its list.
    # The full reason-plus-remedy sentence ALSO fits, by only a few points. For this case the split of
    # cue from remedy is therefore the item's specification and the existing convention, not a width
    # necessity, and no assertion may rely on the full sentence overrunning (the quarantined pin at
    # `PanelTextMetricsTests.swift:236` does, for its own case only).
    # Binds: #1573's own test `testTheSharedLabelCueIsTheReasonAlone` (its seam, `switchBlockedCue` at
    # `apps/menubar/Sources/StatusPanelFormat.swift:252`, must name the new case). The sentence itself is
    # pinned independently through the spoken label below.

Scenario: the tooltip is the reason plus the remedy, and the resting line is its first sentence
  Given a blocked shared-label account
   Then its tooltip reads "Can’t switch — another account has this label. Rename one in Settings."
    And the resting line is that tooltip's first sentence, unchanged
    # Covers AC `row-copy-and-spoken-label`. Composed the way the other cases compose theirs:
    # `switchBlockedText` (`apps/menubar/Sources/StatusPanelFormat.swift:279`) is `blockedWhy` plus
    # `blockedRemedy`, so the channels cannot drift. The tooltip carries no ordinal: that is this item's
    # Specification and design § 9.6's table. Design UX-10's prose said a duplicate row's tooltip also
    # carries the ordinal; on 2026-10-27 UX-10 was reconciled to the table (the spoken label and the Swap
    # button's accessible label carry the ordinal; the tooltip does not, since the badge shows it).
    # Binds: #1573's own test `testTheSharedLabelTooltipIsTheReasonPlusTheRemedy`, and the existing drift
    # test (`switchRowHelp`, `apps/menubar/Sources/StatusPanelFormat.swift:319`) with its list extended.

Scenario: the spoken label carries the reason, the remedy and the row's own ordinal
  Given a status frame listing `work`, `work` and `spare`, with `spare` active
   When the panel speaks the first `work`, and then the second
   Then the first ends with "Can’t switch — another account has this label. Rename one in Settings."
    And the first carries "1 of 2"
    And the second ends with that same sentence and carries "2 of 2"
    # Covers AC `row-copy-and-spoken-label` and WCAG 2.2 SC 4.1.2 Name, Role, Value: a blocked row states
    # its reason to VoiceOver before any request. Composed by `rowSwitchAccessibilityLabel`
    # (`apps/menubar/Sources/StatusPanelFormat.swift:324`) as the row's base name, then the full text.
    # The ordinal's form is #1566's draft ("n of N", spoken `work, 2 of 2 accounts named work`). The
    # oracle pins only the "n of N" pair, so a wording change in #1566 moves this scenario with it.
    # Binds: `testABlockedDuplicateRowSpeaksItsReasonRemedyAndOrdinal` (oracle).

Scenario: no existing enumeration of the block cases can stay green over two of three
  Given the tests that list the block cases by hand, each naming `SwitchBlock`:
        `apps/menubar/Tests/AccountSwapTests.swift:484`, `:541`, `:580`, `:638` and `:653`,
        and `apps/menubar/Tests/PanelTextMetricsTests.swift:225` (`SwitchBlock` again)
    And the cardinality guard at `apps/menubar/Tests/PanelTextMetricsTests.swift:231`, which asserts the
        literal 2 over `SwitchBlock` cases measured
   When the shared-label case is added
   Then every one of those lists names it
    And the guard counts three
    # Covers AC `row-copy-and-spoken-label` (its seams are these functions) and the degenerate-subject
    # rule: a guard that still says 2 stays green over two of three cases and reports a complete run.
    # Re-derive the sites with `grep -rnF 'SwitchBlock.quarantined, .weeklyExhausted' apps/menubar/Tests`
    # and `grep -rnF '[nil, .quarantined, .weeklyExhausted]' apps/menubar/Tests`; each hit must then name
    # the new case.
    # Binds: the extended lists and guard themselves, reviewed independently. They are existing tests, so
    # extending them is #1573's own edit, recorded here so the spec moves first.
```

## Rule 4 — the footer callout mirrors the block for a shared target

```gherkin
Scenario: the callout for a shared target keeps its lead and why line, adds the reason, and disables Swap
  Given a status frame listing `work`, `work`, `spare` and `home`, with `spare` active
    And the daemon's next swap names `work`, because its weekly window resets soonest
   When the panel draws the footer
   Then the footer still leads with `work` and still gives "weekly resets soonest"
    And it adds "Can’t switch — another account has this label." as a resting line
    And its Swap action is disabled
    # Covers AC `callout-is-blocked-for-a-shared-target`. This is NOT the mock's "Swaps paused" variant
    # (`apps/menubar/design/menubar-preview.html:1803`, `Swaps paused`), which replaces the lead and the
    # why line. The daemon still plans this swap and the why line holds its reason (`swapCalloutReason`,
    # `apps/menubar/Sources/StatusPanelFormat.swift:2142`). The Swap action's disabled state today has
    # one condition, `swap.phase.isPending` (`apps/menubar/Sources/StatusPanelChrome.swift:243`).
    # The callout's blocked state entered the design in its fourth revision, which no lens graded
    # (design § 14.9.5).
    # Binds: `testTheCalloutForASharedTargetIsBlocked` (oracle).

Scenario: the callout for a unique target is unchanged
  Given the same frame
    And the daemon's next swap names `home`
   When the panel draws the footer
   Then the footer carries no reason line of its own
    And its Swap action is enabled
    # Covers AC `callout-is-blocked-for-a-shared-target` ("For a unique target it is unchanged"). The
    # roster rows still show the shared reason, so the assertion is about the footer alone.
    # Binds: `testTheCalloutForAUniqueTargetIsUnchanged` (companion; green before the fix).

Scenario: the Swap action's accessible label names the ambiguity
  Given the callout of the first scenario in this rule
   Then the Swap action's accessible label is not the plain invitation "Switch to work"
    And it still names `work`
    # Covers AC `callout-is-blocked-for-a-shared-target` ("Its Swap button's accessible label names the
    # ambiguity in the form the callout already has", Specification). Only the RELATION is pinned: the
    # label differs from the plain invitation (`switchHelpText`,
    # `apps/menubar/Sources/StatusPanelFormat.swift:285`) and keeps the target. Its exact text is the
    # open card `callout-swap-accessible-label`.
    # Binds: `testTheCalloutSwapLabelForASharedTargetNamesTheAmbiguity` (oracle, partial).
```

## Rule 5 — the refusal that still arrives after a request reads as the block does

```gherkin
Scenario: a daemon refusal of a shared label says what the blocked row says
  Given a frame that lags a new duplicate, so the panel offered a switch
   When the daemon answers that switch with `ambiguous-target`
   Then the panel says "Can’t switch — another account has this label. Rename one in Settings."
    # Covers AC `after-click-refusal-reworded`. It replaces "Two accounts share that label — rename one,
    # then switch." (`swapErrorText`, `apps/menubar/Sources/StatusPanelFormat.swift:829`, with
    # `.ambiguousTarget` at `:834`). A fixed-string search of `apps/menubar/Tests` finds no test that
    # pins the old sentence, so no machine-pinned value is overturned.
    # Binds: `testTheAfterClickRefusalForAnAmbiguousTargetReadsTheSameSentence` (oracle).

Scenario: every refusal still reads humanly and distinctly
  When every daemon rejection reason is mapped to its copy
  Then all nine read as different sentences, and none leaks its machine tag
   And the wire value `ambiguous-target` still lands in the rejected phase
    # Covers AC `after-click-refusal-reworded` ("the existing test that walks every rejection reason
    # passes"). `testErrorTextIsHumanForEveryRejection` (`apps/menubar/Tests/AccountSwapTests.swift:684`)
    # and `testEveryRejectionReasonLandsInFailedRejected` (`:87`) stay unchanged; the new sentence must
    # differ from the other eight.
    # Binds: those two existing tests, unchanged.
```

## Constraints — carried as invariants, not rules

The PRD lists the first of these as an invariant (§ 10.4, "No surface changes the text it displays for a
roster without a duplicate label"), and the second is a cross-cutting constraint (§ 10.11,
REQ-NTC-C-002). Neither adds a rule a reader must weigh against the five above.

```gherkin
Scenario: a roster with no duplicate label renders exactly as it does today
  Given a frame in which no two accounts share a label
   Then every row's switch verdict, resting line, tooltip and spoken label are what they are today
    And the footer is what it is today
    And every committed panel golden other than the shared-label pair is byte-identical
    # Covers AC `unique-labels-unchanged` (RED not owed).
    # Binds: the existing suites, unchanged, and the armed `PanelGoldenParityTests` run. Read the
    # `[panel-goldens] max drift` line; a run that says tests were skipped compared nothing
    # (`CLAUDE.md` § Menu-bar app).

Scenario: the new copy passes the framing vocabulary, and the screen can fail
  Given the blocked-switch sentences, as the rows, the footer and the refusal render them
   When they are screened against the central banned vocabulary
   Then the screen finds nothing
    And the same screen, given an injected banned word, finds it
    # Covers AC `strings-are-scanned` (PRD REQ-NTC-C-002). `FramingVocabularyScan`
    # (`apps/menubar/Tests/NotificationDeliveryTests.swift:198`) is read only by the notification tests
    # today, so no panel string reaches it and this item adds the surface. The screen reads RENDERED
    # strings, so a string that is never registered cannot hide from it; the subject guard (the reason
    # sentence must be among the screened text) is what makes it red before the fix. The named-constant
    # half of the AC is a code-structure requirement, checked by the independent reviewer.
    # Binds: `testTheBlockedSwitchCopyPassesTheFramingVocabularyAndTheScreenCanFail` (oracle).
```

## 🟥 Open

- **`panel-switches-by-identifier`** — **resolved 2026-11-03**: the operator chose to keep the switch blocked, the
  default this item builds, so every scenario above stands. As it stood (design § 14.6 item 15, weighed with
  `account-key-form`): whether the panel's switch should send an identifier as its target, in which
  case the block "would stay only for frames without identifiers". This item builds the default, the
  blocked switch, and the item states it is correct under either answer for the daemon as it is. Every
  scenario above uses a frame without identifiers, which is the only frame any daemon sends today. None
  asserts what a frame that carries identifiers does. Ratification-pending.
- **`callout-swap-accessible-label`** — the exact text of the callout's Swap accessible label for a shared
  target. The item says it "names the ambiguity in the form the callout already has"; design § 9.6's table
  has no row for it; UX-10 says only that it carries the ordinal; the callout's own ambiguity form is
  #1566's draft. Only the relation is pinned (Rule 4, third scenario). Close it with the landed wording
  of #1566's callout, or with copy review.
- **`shared-label-frame-visual-form`** (PRD § 8.3 DQ-4) — what a blocked duplicate row and the callout's
  resting line look like. The mock authors no shared-label frame (a search of
  `apps/menubar/design/menubar-preview.html` for `shared-label` finds none), and #1566 authors it first.
  No scenario here asserts appearance. Visual conformance is checked after #1566 lands by the armed
  `panel-goldens` run and by concretize-and-compare against the mock's frame pair, read against the
  README's expected-differences list. The remaining judgment aspects (a VoiceOver listen, the README's
  checklist line for the blocked duplicate row and the disabled callout) go to an independent reviewer.
