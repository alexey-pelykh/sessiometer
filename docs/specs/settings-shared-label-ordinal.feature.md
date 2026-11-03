# Feature: Settings rows that share a label are told apart by an ordinal, on screen and aloud

Issue #1574 · parent #1550 · PRD § 10.4 (F4, Invariants) and § 10.11 (REQ-NTC-C-001, REQ-NTC-C-002, REQ-NTC-C-003; AC-NTC-002, AC-NTC-003) · design § 9.1 (UX-3, UX-8), § 9.5 (Settings accounts section), § 9.6, § 11.4, § 11.5 · **Tier B** — scenarios pending binding

Example Mapping: 🟦 5 rules · 🟩 17 examples · 🟥 1 open (`label-normalization-policy`); `duplicate-label-policy` was ruled and the Accounts-row frame's owner decided on 2026-11-03 (cards 2 and 3)

Sources: the PRD, `docs/requirements/account-label-integrity.md`, and the design, `docs/design/account-label-integrity-solution-design.md`. Both are on `main` since the scoping pull request #1638 merged. Every requirement id, `UX-` id and section number below points into them.

Work class: **deterministic-output** (the `oracle-authorship` call, ratification-pending). The primary deliverable is what a Settings row shows and says, a badge string and an accessible-label string, and the saved-roster state they are computed from. A mechanical grader falsifies a wrong one by exact comparison. The aspects that no mechanical grader reaches are listed under § Not scenarios, each with the check or the reviewer that covers it: the badge's place, look and contrast, what VoiceOver says aloud, and the mock frame.

How the scenarios bind. No scenario is Tier A: this run writes no test code.

- A scenario marked `RED-owed` and bound to a test named in oracle item #1584 has its grader pre-authored there. No seam exists today that it could be graded at. The view reads the placeholder constant directly, the model has no accessor for a row's badge or accessible label, and the view cannot be hosted in the test bundle:

  ```text
  apps/menubar/Sources/SettingsView.swift:231 `accountLabelFieldPlaceholder`
  apps/menubar/project.yml:138 `Sources/SettingsModel.swift`
  ```

  So #1584 creates the seam: one adapter function in its test file, whose body returns today's behaviour until #1574 wires it. #1584 lands after #1566 and before #1574.
- A scenario marked `RED-owed` whose `Binds:` line says `executor-authored` has no stable seam before the fix. #1574's executor authors it and proves it by mutation.
- A scenario marked `passes today` is a regression guard on the unmodified tree. It is green before and after the change, and it exists to catch a named wrong implementation.

> **PROVISIONAL — stated once, here.** `docs/requirements/account-label-integrity.md` carries `dor_status: failed`: every requirement in it is pipeline-authored and unratified, and its problem framing is unvalidated. The operator ratified the class of work this file rests on: R-6 in `docs/requirements/migration-credential-portability.md` (user-ratified 2026-08-04; it records duplicate labels as a documented, accepted state) and the parity ruling of 2026-07-31 in `docs/requirements/gui-cli-capability-parity.md` ("Choosing a surface is not accepting reduced capability"). The PRD never names the ordinal. It is the design's answer (UX-3 and UX-8, both author-chosen), which design § 9.7 traces to REQ-ROS-I-002 and REQ-ROW-C-002. Everything below is a pipeline draft under these defaults, none of them ruled on:
>
> - The ordinal "n of N" is the duplicate set's one handle on every surface, and `Label, n of N` is the draft accessible label. The facts below are binding. Copy review may refine the words inside the framing-vocabulary scan.
> - `settings-row-shows-uuid` is answered no: an ambiguous Settings row shows no uuid line. Nothing below asserts that a uuid line is present or absent.
> - The work class and the oracle's scope (`oracle-authorship`) are this run's calls.

> **Story.** The operator renames an account in Settings to end a duplicate label, a state R-6 allows. Two rows with one label show one title, one label in the field and one state cue, and VoiceOver reads each as "Account", "Label", the label and the state. The CLI's `list` tells the two apart by uuid. Settings cannot tell them apart at all, so the operator can rename the wrong one.
>
> **The trap in this feature is the source of the number.** The Settings model holds three copies of a row's label. The loaded list moves on a load and never on a save. The drafts move on every keystroke. The saved baseline moves on a load and on an acknowledged save:
>
> ```text
> apps/menubar/Sources/SettingsModel.swift:215 `accounts`
> apps/menubar/Sources/SettingsModel.swift:212 `labelDrafts`
> apps/menubar/Sources/SettingsModel.swift:251 `pristineLabels`
> ```
>
> Numbering from the loaded list keeps the badges after a save that ends the duplicate. Numbering from the drafts renumbers the rows while the operator types. Only the saved baseline passes both halves of Rule 3. A table keyed by label fails earlier, in Rule 1: it holds one entry for two rows.

## Rule 1 — a row whose label has more than one bearer shows its ordinal and speaks it; every other row reads as it does today

Traces: UX-3 · UX-8 · REQ-ROS-I-002 · WCAG 2.4.6 and 4.1.2. A *bearer* is a roster entry that carries a label, labels compared as exact strings. A bearer's *ordinal* is `n of N`: its 1-based place among the bearers of its label, in roster order. Seam: `SettingsModel` over the fake connection that `SettingsModelTests` uses, read through one adapter function (described above, under "How the scenarios bind").

```gherkin
Scenario: two rows that share a label are told apart
  # AC: oracle-a-duplicate-settings-row-is-told-apart · UX-3, UX-8 · RED-owed
  # Binds: testSettingsRowsThatShareALabelCarryDistinctOrdinalsAndAccessibleLabels (oracle #1584, Settings tests)
  Given the daemon's roster holds `work` on uuid-A and `work` on uuid-B, in that order
   When the Settings model loads the roster
   Then the row for uuid-A carries the badge `1 of 2` and the accessible label `Label, 1 of 2`
    And the row for uuid-B carries the badge `2 of 2` and the accessible label `Label, 2 of 2`
    # Today both rows read "Label" and neither shows a badge: the field's accessible label is its placeholder.
    #   apps/menubar/Sources/SettingsModel.swift:626 `accountLabelFieldPlaceholder`
    # The accessible label names the ordinal after the field's name and before its value, because the value is not part of the label. That is the order the issue asks VoiceOver to speak.
    # Two rows, two cues: the lookup is by the row's uuid. A table keyed by label holds one entry for two rows and cannot give both of these.
    # The strings are pipeline defaults. The facts are binding: the badge is the ordinal in "n of N" form, and the accessible label starts with the field's existing name and carries the same ordinal.

Scenario: a row with a unique label keeps its name and shows no badge, beside rows that share one
  # AC: oracle-a-duplicate-settings-row-is-told-apart (the unique row) · unique-labels-unchanged · UX-3 · RED-owed (the two sharing rows)
  # Binds: testARowWithAUniqueLabelKeepsItsNameBesideRowsThatShareOne (oracle #1584, Settings tests)
  Given the roster holds `work` on uuid-A, `spare` on uuid-B and `work` on uuid-C, in that order
   When the Settings model loads the roster
   Then the row for uuid-A carries `1 of 2` and `Label, 1 of 2`
    And the row for uuid-C carries `2 of 2` and `Label, 2 of 2`
    And the row for uuid-B carries no badge, and its accessible label is `Label`
    # The third account in the roster is "2 of 2": the number is the place among the rows that share the label, not the place in the roster.

Scenario: a roster with no duplicate label gives every row today's text
  # AC: unique-labels-unchanged · PRD § 10.4 Invariants ("No surface changes the text it displays for a roster without a duplicate label") · passes today
  # Binds: testARosterWithNoDuplicateLabelKeepsEveryRowsTodaysStrings (oracle #1584, companion)
  Given the roster of `Fixtures.configViewBasic`: `work` on uuid-A, in rotation, and `personal` on uuid-B, parked
   When the Settings model loads the roster
   Then neither row carries a badge
    And both accessible labels are `Label`
    And the row titles and state cues are the strings the unmodified tree produces: "Account" with "Active", and "Account (parked)" with "Parked"
    # The strings are the tree's own:
    #   apps/menubar/Sources/SettingsModel.swift:626 `accountLabelFieldPlaceholder`
    #   apps/menubar/Sources/SettingsModel.swift:700 `accountRowTitle`
    #   apps/menubar/Sources/SettingsModel.swift:703 `accountStateCue`
    # The test hard-codes them as literals taken from the unmodified tree, so a change to any of them for a row with a unique label is caught. The fixture is the one the model tests already load:
    #   apps/menubar/Tests/Fixtures.swift:528 `configViewBasic`

Scenario: a parked account is a bearer like any other
  # AC: oracle-a-duplicate-settings-row-is-told-apart · UX-3 · RED-owed
  # Binds: testAParkedRowIsABearerLikeAnyOther (oracle #1584, Settings tests)
  Given the roster holds `work` on uuid-A, in rotation, and `work` on uuid-B, parked
   When the Settings model loads the roster
   Then the row for uuid-A carries `1 of 2` and the row for uuid-B carries `2 of 2`
    # Settings lists parked accounts as rows, and the daemon's resolver never filters on the enabled flag, so a parked entry still makes its label ambiguous:
    #   src/use_account.rs:490 `account.label == query`
    # Numbering only the enabled rows would leave a duplicate pair with no ordinal on either row.
```

## Rule 2 — the ordinal is a bearer's place among the rows that carry its label, in roster order, labels compared as exact strings

Traces: UX-3 · REQ-ROS-I-002. The function that numbers the bearers is the panel-rows item's (#1566), which adds and owns it (the scope brief's `shared-piece-owners`); this item and #1560 reuse it, and both are `Blocked by #1566`. These scenarios grade it through Settings. Its own table belongs to #1566's feature file.

```gherkin
Scenario: three bearers are numbered one to three in roster order
  # AC: oracle-a-duplicate-settings-row-is-told-apart · UX-3 · RED-owed
  # Binds: testThreeBearersAreNumberedOneToThreeInRosterOrder (oracle #1584, Settings tests)
  Given the roster holds `work` on uuid-A, `work` on uuid-B, `work` on uuid-C and `spare` on uuid-D, in that order
   When the Settings model loads the roster
   Then the rows for uuid-A, uuid-B and uuid-C carry `1 of 3`, `2 of 3` and `3 of 3`, with accessible labels `Label, 1 of 3`, `Label, 2 of 3` and `Label, 3 of 3`
    And the row for uuid-D carries no badge, and its accessible label is `Label`

Scenario: the order is the roster's, not the uuid's
  # AC: oracle-a-duplicate-settings-row-is-told-apart · design UX-3 (the ordinal is the row's position among the rows sharing its label, in roster order; Settings does not sort) · RED-owed
  # Binds: testOrdinalsFollowRosterOrderNotUuidOrder (oracle #1584, Settings tests)
  Given the roster holds `work` on uuid-C and then `work` on uuid-A, in that order
   When the Settings model loads the roster
   Then the row for uuid-C carries `1 of 2`
    And the row for uuid-A carries `2 of 2`
    # Settings does not sort. It draws the list in the order the daemon sends it, and the daemon sends it in roster order:
    #   apps/menubar/Sources/SettingsView.swift:199 `accountUuid`
    #   src/config/settings.rs:65 `fn view`
    # The uuids are out of order on purpose, so a number taken from sorted uuids or sorted labels gives the rows the wrong way round.

Scenario: labels that only share a prefix are different labels
  # AC: oracle-a-duplicate-settings-row-is-told-apart (exact strings) · UX-3 · passes today
  # Binds: testLabelsThatOnlyShareAPrefixAreDifferentLabels (oracle #1584, companion)
  Given the roster holds `work-alice` on uuid-A, `work-bob` on uuid-B and `work` on uuid-C
   When the Settings model loads the roster
   Then no row carries a badge, and every accessible label is `Label`
    # The daemon resolves a label by exact string:
    #   src/use_account.rs:490 `account.label == query`
    # Green on the unmodified tree, because no row carries a badge yet. It earns its place as the guard against a prefix or substring match.
    # Labels that differ only by case, surrounding whitespace or Unicode form are held (🟥 Open, card 1). No scenario states what they do.
```

## Rule 3 — the ordinal follows the roster as last saved: never the text being typed, and never the roster as first loaded

Traces: UX-8 ("during an unsaved edit the ordinal still reflects the saved roster") · UX-11 · REQ-ROS-I-002. Seam: `SettingsModel` over the fake connection, with a scripted `config-get` reply and then a scripted `config-set` reply, read through the adapter. The model keeps the rows' uuids and order across every scenario here, because a relabel never changes an account's identifier.

```gherkin
Scenario: editing a label to end a duplicate changes nothing until the edit is saved
  # AC: ordinal-ignores-unsaved-edits (the resolving direction) · UX-8 · RED-owed
  # Binds: testAnUnsavedEditThatWouldResolveADuplicateChangesNoOrdinal (oracle #1584, Settings tests)
  Given the roster holds `work` on uuid-A and `work` on uuid-B, loaded
   When the operator edits uuid-B's label to `spare` and has not saved
   Then the row for uuid-A still carries `1 of 2` and `Label, 1 of 2`
    And the row for uuid-B still carries `2 of 2` and `Label, 2 of 2`
    And the form has unsaved changes
    # The unsaved text lives in the drafts, not in the saved baseline:
    #   apps/menubar/Sources/SettingsModel.swift:212 `labelDrafts`
    #   apps/menubar/Sources/SettingsModel.swift:251 `pristineLabels`

Scenario: editing a label to start a duplicate changes nothing until the edit is saved
  # AC: ordinal-ignores-unsaved-edits (the creating direction) · UX-8 · passes today
  # Binds: testAnUnsavedEditThatWouldCreateADuplicateChangesNoOrdinal (oracle #1584, companion)
  Given the roster holds `work` on uuid-A and `spare` on uuid-B, loaded
   When the operator edits uuid-B's label to `work` and has not saved
   Then neither row carries a badge, and both accessible labels are `Label`
    And the form has unsaved changes
    # Green on the unmodified tree, because no row carries a badge yet. It earns its place as the guard against numbering from the drafts: that implementation turns the badges on at the first keystroke that matches.

Scenario: a saved rename that ends the duplicate removes the ordinals
  # AC: ordinal-ignores-unsaved-edits ("After a save that resolves the duplicate, the ordinals and badges are gone") · Invariant, a relabel never changes an account's identifier · RED-owed
  # Binds: testASaveThatResolvesTheDuplicateRemovesTheOrdinals (oracle #1584, Settings tests)
  Given the roster holds `work` on uuid-A and `work` on uuid-B, loaded, and both rows carry their ordinals
    And the operator has edited uuid-B's label to `spare`
   When the operator saves and the daemon acknowledges `applied` with the effect `live`
   Then neither row carries a badge, and both accessible labels are `Label`
    And the rows are still the rows for uuid-A and uuid-B, in that order
    # The loaded list does not move on a save. It is set by a load only, in `adopt`:
    #   apps/menubar/Sources/SettingsModel.swift:372 `accounts`
    # An acknowledged save moves the baseline, not the list:
    #   apps/menubar/Sources/SettingsModel.swift:463 `settleApplied`
    #   apps/menubar/Sources/SettingsModel.swift:472 `rebaselineFromDrafts`
    # So numbering from the loaded list keeps the badges after this save. On the unmodified tree the red part is the Given, because no row carries a badge yet. The Then alone is green there.

Scenario: a saved rename that starts a duplicate adds the ordinals
  # AC: ordinal-ignores-unsaved-edits ("after one that creates a duplicate they appear") · RED-owed
  # Binds: testASaveThatCreatesADuplicateAddsTheOrdinals (oracle #1584, Settings tests)
  Given the roster holds `work` on uuid-A and `spare` on uuid-B, loaded
    And the operator has edited uuid-B's label to `work`
   When the operator saves and the daemon acknowledges `applied` with the effect `live`
   Then the row for uuid-A carries `1 of 2` and `Label, 1 of 2`
    And the row for uuid-B carries `2 of 2` and `Label, 2 of 2`
    # The scenario scripts the daemon's `applied` reply and pins only the model's reaction to it. Whether a daemon answers this save with `applied` at all is `duplicate-label-policy` (🟥 Open, card 2). Today's daemon applies it: its validation keys uniqueness on the account uuid alone.
    #   src/config/validate.rs:290 `account_uuid`

Scenario: a save the daemon rejects changes no ordinal
  # AC: ordinal-ignores-unsaved-edits (the roster the section holds changes only when a save does) · RED-owed
  # Binds: testARejectedSaveChangesNoOrdinal (oracle #1584, Settings tests)
  Given the roster holds `work` on uuid-A and `work` on uuid-B, loaded
    And the operator has edited uuid-B's label to `spare`
   When the operator saves and the daemon rejects the save as `invalid`
   Then the row for uuid-A still carries `1 of 2` and the row for uuid-B still carries `2 of 2`
    And the edit to `spare` is kept for a retry
    # A rejected save does not move the baseline. Only an acknowledged save does:
    #   apps/menubar/Sources/SettingsModel.swift:463 `settleApplied`
    # Numbering from the drafts fails this one too, because the retained draft says `spare`.

Scenario: a window shown again follows the roster another surface changed
  # AC: oracle-a-duplicate-settings-row-is-told-apart · "Which roster" ("A window left open from before keeps the roster it loaded until it is shown again") · RED-owed (the model half)
  # Binds: testAReloadFollowsTheRosterAnotherSurfaceChanged (oracle #1584, Settings tests); the window half is manual (§ Not scenarios)
  Given the roster holds `work` on uuid-A and `spare` on uuid-B, loaded, and no row carries a badge
    And a CLI `capture`, `login` or `import`, or a panel capture, then renames uuid-B to `work`
   When the window is shown again and the Settings model loads the roster
   Then the row for uuid-A carries `1 of 2` and the row for uuid-B carries `2 of 2`
    And once uuid-B is renamed away elsewhere and the window is shown again, neither row carries a badge
    # Until a load, nothing changes. The only load trigger is the window controller's `show`, which the test bundle does not compile:
    #   apps/menubar/Sources/SettingsWindowController.swift:39 `show`
```

## Rule 4 — the panel and Settings give an account the same ordinal

Traces: UX-3 ("every surface computes the same ordinal with no wire change") · REQ-ROW-B-001. Seam: both pure computations over one roster, with the panel read through its accessibility tree. The panel half needs #1566, so #1584 is delivered after it.

```gherkin
Scenario Outline: the panel row and the Settings row of an account carry the same ordinal
  # AC: panel-and-settings-agree · UX-3 · RED-owed (the Settings half; the panel half is #1566's)
  # Binds: testSettingsAndThePanelAgreeOnEveryOrdinal (oracle #1584; runs both rosters and counts them)
  Given a roster whose accounts, in the order the daemon lists them, are <accounts>
   When the panel reads a status frame of that roster and the Settings model loads a `config-get` reply of that roster
   Then each account's ordinal on its panel row equals its ordinal on its Settings row
    And both are <ordinals>

  Examples:
    | accounts                                                                         | ordinals                     |
    | `work` on uuid-A, `work` on uuid-B                                               | 1 of 2, 2 of 2               |
    | `work` on uuid-C, `spare` on uuid-A, `work` on uuid-D, `work` on uuid-B (parked) | 1 of 3, none, 2 of 3, 3 of 3 |
    # No wire change feeds either surface. Both listings are built by walking the roster in order, and neither sorts or filters on the enabled flag:
    #   src/config/settings.rs:65 `fn view`
    #   src/daemon/snapshot_build.rs:417 `AccountReading`
    # The second row keeps the uuids out of order and puts a parked account among the bearers, so a surface that sorts, or that counts only enabled rows, disagrees with the other.
    # The assertion is not only that the two sides are equal: both are also compared with the ordinals above. Two sides that both print no ordinal are equal, which is the state of the baseline tree before #1566 lands.
    # This scenario cannot see a daemon-side reorder, because both of its sides are fixtures built on the Swift side. The config view's projection test checks the roster's length and its first account only (`src/config/settings.rs:578` `config_view_projects_tunables_and_roster`), and a search of the status builder's tests by name found none that pins the order of the accounts it lists. Both searches were by test name and are not exhaustive.
```

## Rule 5 — every string this item adds is a named constant the framing scan reads, and names no identifier

Traces: REQ-NTC-C-001 · REQ-NTC-C-002 · AC-NTC-002 · issue #15 · issue #160. Seam: the shared scanner over the new constants, and the cues the earlier scenarios produce.

```gherkin
Scenario: the badge's and the accessible label's strings are scanned
  # AC: strings-are-scanned · REQ-NTC-C-002 · RED-owed (the registration tripwire)
  # Binds: executor-authored
  Given the strings this item adds: the badge's format and the accessible label's format
   When the framing-vocabulary scan runs over them, with no token exempted
   Then each one is a named constant that the scan reads
    And a banned token injected into either is caught
    And the vocabulary the scan read is not empty
    # The scan exists, and only one test file uses it, over notification strings:
    #   apps/menubar/Tests/NotificationDeliveryTests.swift:198 `FramingVocabularyScan`
    # No Settings string reaches it today. The one Settings inventory of strings checks that each is non-empty and distinct and counts them; it measures no widths:
    #   apps/menubar/Tests/SettingsTextMetricsTests.swift:389 `accountLabelFieldPlaceholder`
    # So the registration seam is new work, and "registered" has to mean something a test can fail on, such as a constant added without being registered. The injected token is the mutation proof.

Scenario: the cues name no identifier
  # AC: strings-are-scanned (labels and ordinals only) · PRD AC-NTC-002 ("BUT NOT the harvested email address, a token, or a digest of a credential") · passes today
  # Binds: testNoSettingsCueIsShapedLikeAnIdentifier (oracle #1584, companion)
  Given the cues of the rosters in Rules 1 and 2
   When each badge and each accessible label is read
   Then none holds a uuid-shaped run, an email address or a token
    # Green on the unmodified tree, and a guard for the day an implementation puts the row's uuid into the badge. The check carries its own canary: the same predicate, fed a string that is uuid-shaped, reports a match.
    # Under `settings-row-shows-uuid` answered yes, the uuid sits in a separate line and in the field's hint, not in these two strings (design UX-9).
```

## Not scenarios — manual by design

- **The view against the mock.** Open the window on a roster that holds a duplicate, open the mock's frame beside it, and compare the badge's place, spacing and look. Record the result in the pull request, with any intended divergence entered under the README's Expected reconciliations:

  ```text
  apps/menubar/design/README.md:94 `Expected reconciliations`
  apps/menubar/Tests/SettingsTextMetricsTests.swift:100 `no Settings render harness exists`
  ```

  Settings has no render harness (issue #1324), so this comparison is manual. `design/build-comparison.py` has no row for Settings frames (README § The Settings window), so it does not check this frame either.
- **The mock frame (AC `mock-frame-precondition`).** Since 2026-11-03 #1652 draws it, and that item's own checks render
  the page and send the frame to a design-fidelity reviewer (the operator's `settings-accounts-frame-owner` decision,
  card 3). This item's check is that the frame is on `main` before any Swift draws the badge, and that its diff
  leaves everything under `apps/menubar/design/` other than its `README.md` untouched.
- **VoiceOver (AC `voiceover-check-recorded`).** The view is outside the test bundle, and its accessibility tree is issue #840's:

  ```text
  apps/menubar/Tests/SettingsTextMetricsTests.swift:97 `#840`
  apps/menubar/design/README.md:676 `VoiceOver pre-release checklist (manual)`
  apps/menubar/design/README.md:709 `Settings window`
  ```

  The README's checklist gains a line under its Settings window list: with two duplicate rows, VoiceOver reads `Label, 1 of 2` and then the value for the first, `Label, 2 of 2` and then the value for the second, and reads each badge only once. The badge is hidden from VoiceOver where the field's label already speaks it. The result of running the line by hand is recorded on #1574.
- **The badge's contrast.** The Specification says the badge's text meets 4.5:1 and its boundary 3:1 in both themes (WCAG 1.4.3 and 1.4.11). AC `badge-contrast-recorded` enforces it by hand: the colours come from the mock frame and Settings has no render harness, so the executor reads the four ratios off the built window with the Accessibility Inspector's colour-contrast calculator in both themes and records them on #1574, and the design-fidelity reviewer reads the frame's colours.
- **A window left open.** It keeps the roster it loaded until it is shown again. The model half is the last scenario of Rule 3. The controller half is `show`, the single load trigger, which the test bundle does not compile.
- **The Swift gates.** `./scripts/check-menubar-zero-egress.sh`, then `cd apps/menubar && xcodegen generate && xcodebuild test -project Menubar.xcodeproj -scheme Menubar -configuration Debug -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO`, and an armed `panel-goldens` run that prints its `[panel-goldens] max drift` line. A run that says tests were skipped compared nothing. The design README lives under `apps/menubar/`, so the `swift` and `panel-goldens` jobs are owed even before any Swift changes.

## Acceptance criteria → where they land

| Criterion on #1574 | Lands in |
|---|---|
| `mock-frame-precondition` | no scenario: a design artifact (§ Not scenarios; 🟥 card 3) |
| `oracle-a-duplicate-settings-row-is-told-apart` | Rules 1 and 2 |
| `ordinal-ignores-unsaved-edits` | Rule 3 |
| `panel-and-settings-agree` | Rule 4 |
| `unique-labels-unchanged` | Rule 1, second and third scenarios |
| `strings-are-scanned` | Rule 5 |
| `voiceover-check-recorded` | manual (§ Not scenarios) |
| `badge-contrast-recorded` | manual (§ Not scenarios) |
| `row-value-column-charged` | no scenario: the extended bound in `apps/menubar/Tests/SettingsTextMetricsTests.swift` (`rowContent`) and its canary |
| Invariant: a relabel never changes an account's identifier | Rule 3, the intro and the third scenario's last clause |
| Output conforms to the mock | manual (§ Not scenarios) |
| The repo's pre-push gates | manual (§ Not scenarios) |
| Added at Stage 3.5: the oracle tests from #1584 pass with their markers removed | the `Binds:` lines above |

## 🟥 Open

### 1. `label-normalization-policy` — what makes two labels the same (design § 12.2, PRD § 2.10)

The item compares labels as exact strings, which is what the daemon does today when it resolves one:

```text
src/use_account.rs:490 `account.label == query`
```

Under any other answer the daemon's comparison key changes (design D-1), and a Settings row's ordinal has to follow it. The ordinal function is #1566's, reused by this item and #1560, so it is the one place to change. Held, so no scenario states them: whether `Work` and `work`, two labels that differ only by surrounding whitespace, or two Unicode forms of one label form a duplicate set. Every scenario above uses labels that are byte-identical, or that differ under every option (`work-alice` and `work`).

Settings adds a wrinkle of its own. It trims surrounding whitespace from a draft when it saves, while the daemon refuses only an empty label, so which labels the comparison sees depends on this answer too:

```text
apps/menubar/Sources/SettingsModel.swift:289 `trimmingCharacters`
src/config/validate.rs:312 `label.trim().is_empty()`
```

### 2. `duplicate-label-policy` — warn or refuse (design § 12.2, PRD § 8.1)
> **Ruling, 2026-11-03.** Refuse: once #1558 lands, a save that would make a label name a second account is
> rejected rather than applied, so the scenario that scripts an `applied` reply for such a save describes today's
> daemon, which the ruling retires. The scenario holds under both answers, as this card says, and #1574 stays
> READY; its executor binds the scenario against whichever daemon is in the tree. Rosters that already hold
> duplicates still render the ordinals.


Does a daemon answer a Settings save that makes a label name a second account with `applied` (warn) or with a rejection such as `label-in-use` (refuse)? The issue lists no such decision, yet one acceptance criterion rests on it: "after one that creates a duplicate they appear". The scenario for it scripts the daemon's `applied` reply and pins only the model's reaction, which holds under both answers. Held: whether that save can happen at all. Under refuse, a duplicate reaches Settings only by a load, the creating-save scenario has no case in operation, and nothing else changes.

### 3. The Accounts-row frame in the design mock — who authors it, and whether it fits (`settings-accounts-frame-owner`)
> **Ruling, 2026-11-03.** The operator decided `settings-accounts-frame-owner`: a new design item, #1652, draws the
> Accounts-section frames this item and #1560 build against, the ordinal badge among them. This item is blocked by
> it, and its executor draws nothing in the mock. The render budget below is that item's precondition
> (`mock-page-restructure`).

The mock's Settings frames do not render the Accounts section. The README lists its row anatomy among the axes nobody has decided; it routed the anatomy to #946 (whose open scope names other surfaces) and, since 2026-11-03, routes it to #1652:

```text
apps/menubar/design/README.md:1546 `What these frames deliberately do not author`
apps/menubar/design/README.md:1285 `Accounts`
```

The issue left the author open under `settings-accounts-frame-owner` until the operator decided it on 2026-11-03 (the ruling note above): #1652 draws the frame, and this item does not invent the row anatomy from silence. The render budget bears on it too:

```text
apps/menubar/design/README.md:42 `Only ~686 px of genuine bump room is left`
apps/menubar/design/README.md:58 `row costs 662 px`
```

The last recorded room is about 686 px, a Settings row of frames costs 662 px, and #1566's frame pair, authored first, is expected to take about one popover row of that room. So the frame probably needs the trade the README names: fewer frames, a shorter frame, or a second page. That last step is an inference from these numbers and not a measurement. Held: the badge's place, look and spacing against the 160 pt field and the state cue, its contrast, and the manual conformance check:

```text
apps/menubar/Sources/SettingsModel.swift:741 `accountLabelFieldWidth`
```

Nothing in Rules 1 to 5 depends on it.

### Noted, not open for this feature

- `settings-row-shows-uuid` (design § 14.6 item 10b) is not built until the operator answers yes. Nothing above asserts that a uuid line is present or absent, and the two strings Rule 5 pins hold under both answers.
- `account-key-form` (PRD DQ-1) does not reach this feature. Settings keys its rows on the `accountUuid` that `config-get` already sends, and the ordinal needs only the labels in roster order:

  ```text
  apps/menubar/Sources/SettingsView.swift:199 `accountUuid`
  ```

- `panel-notices-name-the-uuid` and `panel-switches-by-identifier` belong to #1560 and #1573. The operator decided both on 2026-11-03: keep the uuid, and keep the switch blocked. `colour-seed-from-identifier` and DQ-4 (the panel frame) belong to #1566.
