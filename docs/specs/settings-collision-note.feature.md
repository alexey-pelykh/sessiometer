# Feature: the Settings accounts section keeps a persistent note while one label names more than one account

Issue #1560 · parent #1550 · PRD REQ-ROS-B-001 / REQ-ROS-I-002 / REQ-ROS-Q-001 / REQ-NTC-C-001 / REQ-NTC-C-002 · design UX-3 / UX-8 / UX-11, § 9.5, § 9.6, § 11.5 CAP-04 · **Tier B** — scenarios pending binding

Example Mapping: 🟦 5 rules · 🟩 22 examples · 🟥 5 open (duplicate-label-policy; panel-notices-name-the-uuid; panel-switches-by-identifier; label-normalization-policy; the Accounts-row frame)

Sources: the PRD, `docs/requirements/account-label-integrity.md`, and the design, `docs/design/account-label-integrity-solution-design.md`. Both land with the scoping pull request. Every requirement id, `UX-` id, `CAP-` id and section number below points into them.

> **Provisional.** The PRD's `dor_status` is `failed`: every requirement is unratified and its problem framing is unvalidated. Nothing below is the operator's decision. A scenario whose outcome hangs on an open decision is not written as a scenario. It is held, named by its slug, under 🟥 Open.
>
> **Not executable.** This repo has no Gherkin runner (`docs/specs/import-duplicate-label.feature.md` records the convention). No test binds these scenarios yet. A scenario counts as covered once a named test binds it.
>
> **Facts, not bytes.** The draft wording is copy review's. The scenarios pin the facts the issue calls binding: the label, the count, the ordinals, their order, and what is absent. The view's fit against the design mock and the real announcement under VoiceOver are manual steps (§ Not scenarios).

The draft text, from design § 9.6:

```text
note          ‘work’ names 2 accounts: work 1 of 2 (<uuid-A>) and work 2 of 2 (<uuid-B>). Until one is renamed, you can't switch to either from the panel.
announcement  ‘work’ now names 2 accounts; this one is 2 of 2.
```

Two parts of the note rest on open decisions and are not pinned below: the uuid beside each ordinal (card 2) and the closing sentence about switching (card 3). The announcement is held whole (card 1).

## Rule 1 — the note's groups come from the saved roster, and from nothing else

Traces: UX-11 · REQ-ROS-I-002 · CAP-04. Seam: `SettingsModel` over the fake connection that `SettingsModelTests` uses (a scripted `config-get` reply, then a scripted `config-set` reply), and the pure derivation in `SettingsFormat`.

```gherkin
Scenario: a duplicate made anywhere shows the note when the roster loads
  # AC: oracle-a-duplicate-in-the-roster-shows-the-note
  # Binding name the issue suggests: a_duplicate_label_in_the_loaded_roster_shows_the_collision_note
  Given the daemon's roster holds `work` on uuid-A and `work` on uuid-B, in that order
   When the Settings model loads the roster
   Then it derives exactly one group, for `work`, holding uuid-A then uuid-B
    And the accounts section shows the note
    # A CLI `capture`, `login` or `import`, and a panel capture, can each have made it. The note reads
    # the roster and never the way the duplicate arrived (design UX-11).

Scenario: a roster with no duplicate yields no group and no note
  # AC: oracle-a-duplicate-in-the-roster-shows-the-note (the negative half)
  Given the daemon's roster holds `work` on uuid-A and `personal` on uuid-B
   When the Settings model loads the roster
   Then it derives no group
    And the accounts section shows no note

Scenario: bearers keep roster order, and a parked account still counts as a bearer
  # AC: oracle-a-duplicate-in-the-roster-shows-the-note
  Given the roster holds `work` on uuid-A, `spare` on uuid-B and `work` on uuid-C
    And uuid-C is parked
   When the Settings model loads the roster
   Then the group for `work` holds uuid-A then uuid-C
    And uuid-C counts although it is parked
    # Roster order: not uuid order, not label order, and not "neighbouring rows only".
    # A parked entry is still a bearer. The resolver filters on label or uuid and never on the
    # enabled flag, at
    #   src/use_account.rs:486 `resolve_target`
    # so a note that skipped parked rows would stay silent over a label the daemon will not resolve.

Scenario Outline: an unsaved edit never changes the note
  # AC: the-note-follows-the-saved-roster
  Given the roster holds <roster>
   When the draft label of uuid-B becomes <draft>, unsaved
   Then the accounts section shows <note>
    # The derivation reads the labels as last saved, never the drafts and never the loaded list:
    #   apps/menubar/Sources/SettingsModel.swift:251 `pristineLabels`
    #   apps/menubar/Sources/SettingsModel.swift:215 `accounts`

  Examples:
    | roster                                  | draft   | note                |
    | `work` on uuid-A and on uuid-B          | `spare` | the note for `work` |
    | `work` on uuid-A, `personal` on uuid-B  | `work`  | no note             |

Scenario: an unrelated save leaves the note where it was
  # AC: the-note-follows-the-saved-roster
  Given the roster holds `work` on uuid-A and `work` on uuid-B
    And the operator has changed one tunable
   When the operator saves and the daemon acknowledges `applied` with the effect `restart_required` and no collisions
   Then the accounts section still shows the note for `work`
    # A save moves every label baseline to its own draft, which here equals what it was:
    #   apps/menubar/Sources/SettingsModel.swift:472 `rebaselineFromDrafts`

Scenario Outline: renaming a bearer away ends the note only when no label is left with two bearers
  # AC: the-note-follows-the-saved-roster
  Given the roster holds `work` on <bearers>
   When the operator renames <renamed> to `spare` and saves, and the daemon acknowledges `applied` with no collisions
   Then `work` is borne by <left>
    And the accounts section shows <note>

  Examples:
    | bearers                   | renamed | left              | note                                   |
    | uuid-A and uuid-B         | uuid-B  | uuid-A alone      | no note                                |
    | uuid-A, uuid-B and uuid-C | uuid-C  | uuid-A and uuid-B | the note for `work`, naming 2 accounts |
    # The second row is the edge: one rename out of three leaves a duplicate, so the note stays.
```

## Rule 2 — the ordinal is the position among the accounts that share the label, in roster order

Traces: UX-3 · REQ-ROS-I-002. Seam: the one pure, Foundation-only ordinal function. #1561, #1566 and #1574 need it too, and whichever item lands first adds it.

```gherkin
Scenario Outline: the ordinal counts only the accounts that share the label
  # AC: oracle-a-duplicate-in-the-roster-shows-the-note (the sentence's ordinals) · design UX-3
  Given the roster holds <roster>
   When the ordinal of each account is asked for, in roster order
   Then the answers are <ordinals>

  Examples:
    | roster                  | ordinals               |
    | `work`, `work`          | 1 of 2, 2 of 2         |
    | `work`, `work`, `work`  | 1 of 3, 2 of 3, 3 of 3 |
    | `work`, `spare`, `work` | 1 of 2, none, 2 of 2   |
    | `work`, `personal`      | none, none             |
    # Row 3 separates "position among the sharers" from "position in the roster": the third account
    # is `2 of 2`, not 3. Row 4: a label with one bearer has no ordinal, so a roster without a
    # duplicate renders as it does today (design UX-3).
```

"Roster order" is the order `config-get` lists the accounts in, because it builds the list by walking the roster:

```text
src/config/settings.rs:65 `ConfigView`
```

## Rule 3 — the note names every bearer by ordinal, one sentence per label, and wraps without eliding anything

Traces: § 9.5 collision note · § 9.6 · REQ-ROS-I-002 · CAP-04. Seam: the pure note function in `SettingsFormat`. The wrap scenario sits in `SettingsTextMetricsTests`.

```gherkin
Scenario: one sentence per colliding label, in the order of each label's first bearer
  # AC: oracle-a-duplicate-in-the-roster-shows-the-note
  Given the roster holds `b` on uuid-A, `a` on uuid-B, `a` on uuid-C and `b` on uuid-D
   When the note is built
   Then it holds two sentences, the first for `b` and the second for `a`
    # `b`'s first bearer (uuid-A) precedes `a`'s (uuid-B). Alphabetical order would put `a` first.

Scenario: a sentence names the label, how many accounts bear it, and every bearer by ordinal in roster order
  # AC: oracle-a-duplicate-in-the-roster-shows-the-note
  Given the roster holds `work` on uuid-A, uuid-B and uuid-C
   When the note is built
   Then its sentence says that `work` names 3 accounts
    And it names the bearers in roster order as `work 1 of 3`, `work 2 of 3` and `work 3 of 3`
    # Held, so not asserted here: the uuid beside each ordinal (card 2) and the closing sentence about
    # switching (card 3).

Scenario: the note wraps and loses no character
  # AC: wrap-not-truncate
  Given the note built for each of three fixtures
        | one label with two bearers          |
        | one label with three bearers        |
        | two labels with two bearers each    |
   When each is measured against the form row text budget at the shipped window width
   Then it is drawn in full over as many lines as it needs, with no line limit
    And nothing it shows is elided, and a uuid it shows is never cut
    And the measured line count of each fixture is stated in the test
    And a mutation canary shows the predicate trips on an over-wide fixture
    # The budget and the measuring tool, as SettingsTextMetricsTests already uses them:
    #   apps/menubar/Sources/SettingsModel.swift:786 `formRowTextBudget`
    #   apps/menubar/Tests/TextMetrics.swift:113 `wrapped`
    # The window is not resizable, so the shipped width is the only width the note gets.
```

## Rule 4 — a save announces only a collision it created, and a quiet save announces nothing

Traces: UX-8 · UX-11 · REQ-ROS-Q-001 (`CollisionReportPrecision`, goal 0) · PRD AC-ROS-002. Seam: `SettingsModel.apply` over the same fake connection, with the announcement closure injected the way `onRequestAuthorization` is (the issue suggests the name `onAnnounce`):

```text
apps/menubar/Sources/SettingsModel.swift:246 `onRequestAuthorization`
```

The quiet half is decided and written below. The creating half — a save that makes a label name a second account — hangs on `duplicate-label-policy`. It is held under 🟥 Open (card 1) and is not written as a scenario.

```gherkin
Scenario: a swap of two labels announces nothing
  # AC: quiet-saves-announce-nothing · PRD AC-ROS-002
  Given the roster holds `a` on uuid-A and `b` on uuid-B
   When the operator saves uuid-A as `b` and uuid-B as `a` in one save
    And the daemon acknowledges `applied` with the effect `live` and no collisions
   Then the announcement closure is not called
    And the accounts section shows no note
    # The state the write passes through, with `b` borne twice, is never reported.

Scenario: an unrelated save on a roster that already holds a duplicate announces nothing
  # AC: quiet-saves-announce-nothing
  Given the roster holds `work` on uuid-A and `work` on uuid-B
    And the operator has changed one tunable
   When the operator saves and the daemon acknowledges `applied` with the effect `restart_required` and no collisions
   Then the announcement closure is not called
    And the accounts section still shows the note for `work`
    # The duplicate was announced when it was made. Repeating it on every later save would train
    # dismissal of the report that matters (REQ-ROS-Q-001).

Scenario: the `unchanged` effect announces nothing
  # AC: quiet-saves-announce-nothing
  Given the roster holds `a` on uuid-A and `b` on uuid-B
   When the operator saves and the daemon acknowledges `applied` with the effect `unchanged`
   Then the announcement closure is not called
    # `unchanged`: the daemon found the submitted values already in place, through a stale baseline or a
    # concurrent change. Nothing was written, so there is nothing to report.

Scenario: a daemon that predates `collisions` announces nothing, and the note still follows the roster
  # AC: quiet-saves-announce-nothing · the-note-follows-the-saved-roster
  Given a daemon whose acknowledgements carry no `collisions` key
    And the roster holds `a` on uuid-A and `b` on uuid-B
   When the operator saves uuid-B as `a` and the daemon acknowledges `applied` with the effect `live`
   Then the announcement closure is not called
    And the accounts section shows the note for `a`
    # Two channels, deliberately: the announcement follows the acknowledgement, the note follows the
    # saved roster. A note tied only to the acknowledgement would miss this duplicate, and every
    # duplicate that another surface makes (design UX-11). An acknowledgement without the key decodes
    # to an empty list (#1558).
    # This is not the held example H1. The daemon described here predates `duplicate-label-policy`,
    # so it accepts the write whichever way the operator decides. What stays open is what a daemon
    # that has the policy does with the same save.
```

## Rule 5 — the words are labels and uuids only, and they pass the framing scan

Traces: REQ-NTC-C-001 · REQ-NTC-C-002 · AC-NTC-002 · issue #15 · issue #160. Seam: the shared scanner over the pure functions, a sentinel render, and a concretize-and-compare against import's notice.

```gherkin
Scenario: every string this feature adds passes the framing scan with no exemption
  # AC: copy-passes-the-framing-scan
  Given the strings this feature adds, rendered from every fixture above
   When each is scanned against the whole banned-token list and both banned phrases, with no token exempted
   Then none is found
    And the vocabulary the scan read is non-empty
    And a string with a banned word injected is caught
    # The scanner and the vocabulary it reads:
    #   apps/menubar/Tests/NotificationDeliveryTests.swift:198 `FramingVocabularyScan`
    #   src/framing_vocabulary.rs:182 `BANNED_TOKENS`
    #   src/framing_vocabulary.rs:252 `BANNED_PHRASES`
    # The announcement's strings join the scan if card 1 selects warn.

Scenario: the note carries labels and uuids from the roster and no other identifying text
  # AC: only-labels-and-uuids · invariant: Settings never reads or writes a credential
  Given a roster whose labels and uuids are sentinels
   When the note is built
   Then the text holds the template text and the sentinels
    And it holds nothing else that identifies an account
    # No email, no token, no digest of a credential. The note's only input is the roster the window holds.

Scenario: the note says what import's notice says, and never that anything was refused
  # AC: output conforms to import-duplicate-label.feature.md (concretize-and-compare)
  Given the roster of import's first scenario: label `L` on uuid-X and on uuid-Y
   When the note is built and import's duplicate-label notice is rendered for `L`
   Then both name `L`
    And both say it names more than one account
    And neither claims that a write was refused
    And neither enforces uniqueness
    # Parity of the facts, not of the words. R-6 and AC-6: "BUT NOT by enforcing label uniqueness".
    # One deliberate difference: the note persists while the duplicate exists; import's notice reports one write.
    #   src/cli.rs:5628 `duplicate_label_notice`
    # The quiet half of that file reachable from a Settings save is Rule 4: a swap of two labels, and a
    # save on a roster that already held the duplicate. Import's same-label, same-uuid case has its
    # Settings counterpart in a relabel to the label an entry already carries, which #1558 has the
    # daemon report as `unchanged`.
```

## Not scenarios — manual by design

- **The view against the mock.** Open the window on a roster that holds a duplicate, open the mock's Settings frame beside it, and compare the note's text, place and wrapping. Record the result in the pull request, with any intended divergence entered under the README's Expected reconciliations. The Settings window has no render harness (the issue's own words), so this stays manual. The mock does not author the Accounts section today (card 5).
- **The announcement under VoiceOver.** The closure that posts it is supplied where `onRequestAuthorization` is supplied, outside the test bundle:

  ```text
  apps/menubar/Sources/main.swift:203 `onRequestAuthorization`
  ```

  Its wiring is pinned by `announce-is-wired`, a source-text test over that file; the spoken result is
  checked by hand with VoiceOver on, and only if card 1 selects warn.
- **The Swift gates.** `xcodebuild test`, and an armed `panel-goldens` run that prints its `[panel-goldens] max drift` line. A run that says tests were skipped compared nothing.
- **Out of scope, stated so the silence is not read as a gap.** A window left open keeps the roster it loaded. A duplicate another surface makes while it stays open appears at the next open:

  ```text
  apps/menubar/Sources/SettingsWindowController.swift:39 `show`
  ```

## Acceptance criteria → where they land

| Criterion on #1560 | Lands in |
|---|---|
| `oracle-a-duplicate-in-the-roster-shows-the-note` | Rules 1, 2 and 3; the uuid half is held (card 2) |
| `the-note-follows-the-saved-roster` | Rule 1 and Rule 4's last scenario; the "colliding save adds the note" half is held (card 1) |
| `oracle-a-save-that-creates-a-collision-announces-it-once` | held (card 1) |
| `quiet-saves-announce-nothing` | Rule 4 |
| `announcement-sentence` | held (card 1) |
| `announce-is-wired` | no scenario: a source-text test over `apps/menubar/Sources/main.swift` (§ Not scenarios) |
| `wrap-not-truncate` | Rule 3, third scenario |
| `copy-passes-the-framing-scan` | Rule 5, first scenario |
| `only-labels-and-uuids` | Rule 5, second scenario |
| `mock-first` | no scenario: a process check on the diff of `apps/menubar/design/` and the commit order (card 5) |
| Invariant: a save that creates no collision announces nothing | Rule 4 |
| Invariant: Settings never reads or writes a credential | Rule 5, second scenario |
| Output conforms to `docs/specs/import-duplicate-label.feature.md` | Rule 5, third scenario, and Rule 4's quiet half |
| Output conforms to `apps/menubar/design/menubar-preview.html` | manual (§ Not scenarios) |
| The repo's Swift gates | manual (§ Not scenarios) |

## 🟥 Open

### 1. `duplicate-label-policy` — warn or refuse (design § 12.2, PRD § 8.1)
> **Ruling, 2026-11-03.** Refuse: the creating half, a save that would make a label name a second account, is a
> rejected save rather than a notice, and the note keeps showing duplicates that already exist. Re-authored at
> #1560's re-gate; the header's count predates the ruling.


When a Settings save makes a label name a second account, does the daemon accept the write and report it (warn), or reject it before any write (refuse, with a new `label-in-use` rejection)? The design records no fallback for this one and says the answer fixes the assertion form of the oracles. The issue builds warn. An assertion of either branch would be a guess, so these are held as examples and are not scenarios:

```text
H1  Given `a` on uuid-A and `b` on uuid-B, when the operator saves uuid-B as `a` and the daemon
    acknowledges `applied` with `collisions` naming `a`:
      warn   -> the announcement closure is called once; the note appears
      refuse -> the save is rejected through the rejected-save state; nothing is announced; no note
H2  The ordinal clause. The announcement names the renamed account's ordinal when exactly one bearer was
    among the labels the save changed, and omits the clause when more than one was:
      uuid-B alone renamed             -> "… this one is 2 of 2"
      uuid-A and uuid-B both onto `x`  -> no clause
H3  Deepening a duplicate. `work` on uuid-A and uuid-B, `personal` on uuid-C; renaming uuid-C to `work`
    raises the bearer count from 2 to 3, so it is announced.
H4  A save that creates two collisions announces each one once.
H5  Starting from a roster with no duplicate, a save that renames one account onto another's label adds the note.
```

Under refuse, H1 to H5 do not exist as written, and the note exists only for duplicates made elsewhere. Rules 1 to 3 and the quiet half of Rule 4 stand under both answers. Blocks: `oracle-a-save-that-creates-a-collision-announces-it-once`, `announcement-sentence`, and the "adds the note" half of `the-note-follows-the-saved-roster`.

### 2. `panel-notices-name-the-uuid` (design § 14.6 item 10a)

Does the note name each bearer by uuid beside its ordinal, or by ordinal alone? PRD § 3 defers a display of the uuid. REQ-NTC-C-001 and REQ-STSH-B-002 name the uuid where the label alone is ambiguous. The design follows the requirements and records "If unanswered, the requirements as written govern and the notices name the uuid". Held: the uuid beside each ordinal, the uuid half of the sentinel assertion in Rule 5, and the "a uuid it shows is never cut" clause in Rule 3. Under the other answer the uuid parts go and the ordinals stay.

### 3. `panel-switches-by-identifier` (design § 14.6 item 15; weighed with `account-key-form`, DQ-1)

Will the panel switch to either account of a shared label by identifier, or stay blocked until one is renamed? The note's closing sentence, "Until one is renamed, you can't switch to either from the panel", is true only while the panel switches by label. The design records "If unanswered, the blocked switch stands". The claim about today's panel comes from the design and was not re-verified here. Held: the closing sentence of the note.

### 4. `label-normalization-policy` — what makes two labels the same (design § 12.2, PRD § 8.1)

No scenario here depends on it: every scenario uses byte-identical labels, which are the same under every option. It stays open because the Swift derivation re-implements a comparison the daemon owns. Today the resolver compares labels exactly (`src/use_account.rs:486` `resolve_target`). Under any option other than "none", the daemon's comparison key changes (design D-1), and the issue names no mechanism that keeps the Swift derivation in step with it. Left open: whether `Work` and `work`, or two NFC variants, form a group.

### 5. The Accounts-row frame in the design mock (`settings-accounts-frame-owner`; the issue's own precondition)

Who authors the mock frame for the Accounts section's rows and the note? The mock's four Settings frames do not render that section. The README routes the row anatomy to #946 (register R-11), and the issue records that #946's open scope names other surfaces. The issue carries this as a precondition and not as a blocker edge. Held: the `mock-first` criterion, the manual comparison, and the note's place and wrap in the view. Nothing in Rules 1 to 5 depends on it.

### Noted, not open for this feature

- `settings-row-shows-uuid` (design § 14.6 item 10b) is not built until the operator answers yes. No scenario depends on it.
- `login-mismatch-policy`, `import-duplicate-uuid-policy` and `orphan-stash-policy` belong to other features of #1550. `account-key-form` reaches this feature only through card 3.
