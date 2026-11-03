# Feature: the Settings accounts section keeps a persistent note while one label names more than one account

Issue #1560 · parent #1550 · PRD REQ-ROS-B-001 / REQ-ROS-I-002 / REQ-ROS-Q-001 / REQ-NTC-C-001 / REQ-NTC-C-002 · design UX-3 / UX-7 / UX-8 / UX-11, § 9.5, § 9.6, § 11.5 CAP-04, § 12.3 Terms / C-7 / C-8 / C-9 / C-12 · **Tier B** — scenarios pending binding

Example Mapping: 🟦 5 rules · 🟩 31 examples · 🟥 4 open (panel-notices-name-the-uuid; panel-switches-by-identifier; label-normalization-policy; the Accounts-row frame)

Sources: the PRD, `docs/requirements/account-label-integrity.md`, and the design, `docs/design/account-label-integrity-solution-design.md`. Both are on `main` since the scoping pull request #1638 merged. Every requirement id, `UX-` id, `CAP-` id and section number below points into them.

> **Provisional.** The PRD's `dor_status` is `failed`: every requirement is unratified and its problem framing is unvalidated. Nothing below is the operator's decision except the refusal itself: the operator ruled on 2026-11-03 that a write which would make a label name a second account is refused (PRD § 8.1). Which saves count as a collision, judged per account; how `config-set` carries that refusal, with no override and with `collisions` on its `rejected` acknowledgement; and what Settings shows for it are the orchestrator's readings in design § 12.3 (Terms, C-7, C-9, C-12), confirmed or changed at ratification. A scenario whose outcome hangs on an open decision is not written as a scenario. It is held, named by its slug, under 🟥 Open.
>
> **Not executable.** This repo has no Gherkin runner (`docs/specs/import-duplicate-label.feature.md` records the convention). No test binds these scenarios yet. A scenario counts as covered once a named test binds it.
>
> **Facts, not bytes.** The draft wording is copy review's. The scenarios pin the facts the issue calls binding: the label, the count, the ordinals, their order, what is absent, and which rows a refused save marks. The view's fit against the design mock and a marked row under VoiceOver are manual steps (§ Not scenarios).

The draft text, from design § 9.6:

```text
note          ‘work’ names 2 accounts: work 1 of 2 (<uuid-A>) and work 2 of 2 (<uuid-B>). Until one is renamed, you can't switch to either from the panel.
```

Two parts of the note rest on open decisions and are not pinned below: the uuid beside each ordinal (card 2) and the closing sentence about switching (card 3). The design's announcement of a save that created a collision is retired: under the refusal no save creates one (design § 12.3, C-8). The reason's own sentence, in the footer, is #1558's, and it is fixed: it names no label. This feature names the label and pins the rows the refusal marks (Rule 4; design § 12.3, C-9).

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
    # An `import`, a `config restore`, a hand edit of `config.toml`, or a write made before the refusal
    # landed can each have made it (design § 12.3, C-8). The note reads the roster and never the way the
    # duplicate arrived (design UX-11).

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
   When the operator saves and the daemon acknowledges `applied` with the effect `restart_required`
   Then the accounts section still shows the note for `work`
    # A save moves every label baseline to its own draft, which here equals what it was:
    #   apps/menubar/Sources/SettingsModel.swift:472 `rebaselineFromDrafts`

Scenario Outline: renaming a bearer away ends the note only when no label is left with two bearers
  # AC: the-note-follows-the-saved-roster
  Given the roster holds `work` on <bearers>
   When the operator renames <renamed> to `spare` and saves, and the daemon acknowledges `applied`
   Then `work` is borne by <left>
    And the accounts section shows <note>

  Examples:
    | bearers                   | renamed | left              | note                                   |
    | uuid-A and uuid-B         | uuid-B  | uuid-A alone      | no note                                |
    | uuid-A, uuid-B and uuid-C | uuid-C  | uuid-A and uuid-B | the note for `work`, naming 2 accounts |
    # The second row is the edge: one rename out of three leaves a duplicate, so the note stays.
```

## Rule 2 — the ordinal is the position among the accounts that share the label, in roster order

Traces: UX-3 · REQ-ROS-I-002. Seam: the one pure, Foundation-only ordinal function. #1561, #1566 and #1574 need it too, and #1566 owns it (the scope brief's register row `shared-piece-owners`).

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

## Rule 4 — a save that would leave an account it relabels sharing a label with another account is refused and marks its rows, and every other save marks nothing

Traces: REQ-ROS-B-001 · REQ-ROS-I-002 · REQ-ROS-Q-001 (`CollisionReportPrecision`, goal 0) · PRD AC-ROS-002 · design UX-7 · § 12.3 Terms, C-7, C-9, C-12. Seam: `SettingsModel.apply` over the same fake connection, with the daemon's reply scripted, and the pure marking function in `SettingsFormat`.

The daemon decides a collision: a save collides when an account it relabels would, after the save, carry a label another account also carries. It is judged per account, so a save can collide while no label's count of bearers rises (design § 12.3, Terms and C-7). The daemon refuses such a save whole, with zero writes and the reason `label-in-use`, and its `rejected` acknowledgement carries `collisions`: a list of `{label, bearers}`, where `bearers` names, in roster order, every account that would carry the label (C-7, C-12). #1558 adds the reason, decodes `collisions`, and writes the reason's sentence, which is fixed and names no label, because the Settings client renders a reason and its detail only (C-9). The sentence sits in the footer's apply-status slot, Settings' existing rejected-save state, as every rejection's does (design UX-7). This feature keeps the edit unsaved, as every rejection already does, and names the label and marks the rows from `collisions` (C-9): each marked row carries words that name the label it would share. Every label in `collisions` has at least one listed bearer, the account the save relabels, so each colliding label is named on a row. The sentence and the marking read the same rejected outcome, so they show together and clear together. Settings offers no override, because `config-set` can add or remove no account (C-7): the way through is renaming the holder first, or choosing another label. The marking's look waits on the Accounts-row frame (card 5); the facts below do not. A rejected outcome is set at the first line below and moves no baseline, since only an `applied` one reaches the second; the third is the footer arm that draws it:

```text
apps/menubar/Sources/SettingsModel.swift:442 `.rejected`
apps/menubar/Sources/SettingsModel.swift:463 `settleApplied`
apps/menubar/Sources/SettingsView.swift:311 `.rejected`
```

```gherkin
Scenario: a refused save keeps every edit unsaved and adds no note
  # AC: oracle-a-colliding-save-is-refused-and-its-rows-marked · the-note-follows-the-saved-roster
  Given the roster holds `a` on uuid-A and `b` on uuid-B
    And the operator has changed one tunable
   When the operator saves uuid-B as `a`
    And the daemon refuses the save with `label-in-use`, its `collisions` naming `a` borne by uuid-A and uuid-B
   Then uuid-B's label draft is still `a`, and its saved label is still `b`
    And the tunable's draft is still unsaved, so Save is still available for a fix and a retry
    And the accounts section shows no note
    # Refused whole, with zero writes (C-7), so no draft is rebaselined. The note reads the saved roster
    # (Rule 1), so it never shows a duplicate a refused save would have made. This resolves the held
    # warn example H5: under the refusal a Settings save never adds the note.

Scenario Outline: the refusal marks every listed row that would bear the label, and no other row
  # AC: oracle-a-colliding-save-is-refused-and-its-rows-marked
  Given the roster holds <roster>
   When the operator saves <edit>
    And the daemon refuses the save with `label-in-use`, its `collisions` naming <collisions>
   Then the rows marked are <marked>, and each marked row's words name the label it would share
    And the accounts section shows <note>

  Examples:
    | roster                                            | edit                                               | collisions                               | marked                    | note                                   |
    | `a` on uuid-A, `b` on uuid-B                      | uuid-B as `a`                                      | `a`: uuid-A, uuid-B                      | uuid-A and uuid-B         | no note                                |
    | `work` on uuid-A and uuid-B, `personal` on uuid-C | uuid-C as `work`                                   | `work`: uuid-A, uuid-B, uuid-C           | uuid-A, uuid-B and uuid-C | the note for `work`, naming 2 accounts |
    | `a`, `b`, `c` and `d` on uuid-A to uuid-D         | uuid-A and uuid-B as `x`, uuid-C and uuid-D as `y` | `x`: uuid-A, uuid-B; `y`: uuid-C, uuid-D | all four                  | no note                                |
    | `x` on uuid-A, `b` on uuid-B                      | uuid-B as `a`                                      | `a`: uuid-B, uuid-D                      | uuid-B only               | no note                                |
    | `work` on uuid-A and uuid-B, `c` on uuid-C        | uuid-B as `c` and uuid-C as `work`                 | `work`: uuid-A, uuid-C                   | uuid-A and uuid-C         | the note for `work`, naming 2 accounts |
    # Row 1 resolves the held warn example H1. Row 2 resolves H3, the deepening: the note still shows
    # the saved duplicate of two, while the marking shows the three the save would have made.
    # Row 3 resolves H4, one save colliding on two labels. The marking is a set of rows, so it holds
    # whatever order `collisions` arrives in, which `config-set-ack-collisions` Red 2 leaves open
    # (C-12). `bearers` arrives in roster order (C-12), and the marking does not read that order either.
    # Row 4: uuid-D took `a` from another surface after the window loaded, so the window lists no row
    # for it. Only the listed bearer is marked; uuid-D's row appears at the next open (§ Not scenarios).
    # Row 5 is a save that a count of bearers would let through and the per-account rule refuses
    # (design § 12.3, Terms): `work` keeps two bearers, but uuid-C would join uuid-A's label. The
    # marking names uuid-A and uuid-C, the rows the save would make share `work`, while the note still
    # names the saved pair, uuid-A and uuid-B. A marking taken from the saved duplicate would mark
    # uuid-B in place of uuid-C.
    # A `collisions` the decoder drops as malformed leaves the refusal standing and marks no row
    # (C-12, register row `malformed-key-drops-the-fact`).
    # The marking reads the daemon's own `bearers`, so it needs no label comparison of its own. Only the
    # note's grouping re-implements one (card 4).

Scenario Outline: the marking lasts only as long as the refusal that made it
  # AC: only-a-refusal-marks-rows
  Given the roster holds `a` on uuid-A and `b` on uuid-B
    And a save of uuid-B as `a` was refused with `label-in-use`, marking uuid-A and uuid-B
   When <next>
   Then no row is marked

  Examples:
    | next                                                                                                                   |
    | the operator saves uuid-B as `c`, and the daemon acknowledges `applied` with the effect `live`                         |
    | the operator also sets a tunable out of range and saves, and the daemon refuses with `invalid`, which takes precedence |
    | the Settings model loads the roster again                                                                              |
    # Row 2 is the edge: `label-in-use` is reported only for a save that would otherwise be applied (C-7),
    # so a refusal for another reason carries no `collisions` and marks nothing, while uuid-B's draft
    # still reads `a`.

Scenario: a swap of two labels is applied and marks nothing
  # AC: only-a-refusal-marks-rows · PRD AC-ROS-002
  Given the roster holds `a` on uuid-A and `b` on uuid-B
   When the operator saves uuid-A as `b` and uuid-B as `a` in one save
    And the daemon acknowledges `applied` with the effect `live`
   Then no row is marked
    And the accounts section shows no note
    # The state the write passes through, with `b` borne twice, is never reported. Settings sends the
    # save as the operator made it: the daemon alone decides a collision (C-7), so the app runs no
    # uniqueness check of its own that could stop this swap.

Scenario: an unrelated save on a roster that already holds a duplicate is applied and marks nothing
  # AC: only-a-refusal-marks-rows
  Given the roster holds `work` on uuid-A and `work` on uuid-B
    And the operator has changed one tunable
   When the operator saves and the daemon acknowledges `applied` with the effect `restart_required`
   Then no row is marked
    And the accounts section still shows the note for `work`
    # A save that gives no account a label another account carries after the save is applied, and so is
    # one that renames an account out of an existing duplicate (C-7). An existing duplicate makes no
    # later save collide. The note is that duplicate's channel. Marking its rows on every later save
    # would train dismissal of the marking that matters (REQ-ROS-Q-001).

Scenario: the `unchanged` effect marks nothing
  # AC: only-a-refusal-marks-rows
  Given the roster holds `a` on uuid-A and `b` on uuid-B
   When the operator saves and the daemon acknowledges `applied` with the effect `unchanged`
   Then no row is marked
    # `unchanged`: the daemon found the submitted values already in place, through a stale baseline or a
    # concurrent change. Nothing was written, so there is nothing to report.

Scenario: a daemon that predates `label-in-use` applies the save, and the note follows the saved roster
  # AC: only-a-refusal-marks-rows · the-note-follows-the-saved-roster
  Given a daemon that predates `label-in-use`, so it applies a save that makes a label name a second account
    And the roster holds `a` on uuid-A and `b` on uuid-B
   When the operator saves uuid-B as `a` and the daemon acknowledges `applied` with the effect `live`
   Then no row is marked
    And the accounts section shows the note for `a`
    # Two channels, deliberately: the marking follows a refusal, the note follows the saved roster. A
    # note tied to the acknowledgement would miss this duplicate, and every duplicate an `import`, a
    # `config restore` or a hand edit makes (design UX-11; § 12.3, C-8).
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
    # A marked row's words join the scan (Rule 4). The refusal's footer sentence is #1558's, scanned there.

Scenario: the note and a marked row carry labels and uuids and no other identifying text
  # AC: only-labels-and-uuids · invariant: Settings never reads or writes a credential
  Given a roster whose labels and uuids are sentinels
   When the note is built, and a refused save naming those sentinels marks its rows
   Then the note and each marked row's words hold the template text and the sentinels
    And they hold nothing else that identifies an account
    # No email, no token, no digest of a credential. The only inputs are the roster the window holds and
    # the refusal it received.

Scenario: the note says what import's notice says, and never that anything was refused
  # AC: output conforms to import-duplicate-label.feature.md (concretize-and-compare)
  Given the roster of import's first scenario: label `L` on uuid-X and on uuid-Y
   When the note is built and import's duplicate-label notice is rendered for `L`
   Then both name `L`
    And both say it names more than one account
    And neither claims that a write was refused
    And neither enforces uniqueness
    # Parity of the facts, not of the words. R-6 and AC-6: "BUT NOT by enforcing label uniqueness",
    # which AC-6's amendment of 2026-11-03 narrows to load time and to `import`. The note shows a
    # duplicate the roster holds at load and refuses nothing. A Settings save that would make one is
    # refused instead (Rule 4), which is where Settings departs from import's warn form, as that
    # amendment records.
    # One deliberate difference: the note persists while the duplicate exists; import's notice reports one write.
    #   src/cli.rs:5628 `duplicate_label_notice`
    # The quiet half of that file reachable from a Settings save is Rule 4: a swap of two labels, and an
    # unrelated save on a roster that already held the duplicate, both applied with no row marked. Import's
    # same-label, same-uuid case has its Settings counterpart in a relabel to the label an entry already
    # carries, which #1558 has the daemon report as `unchanged`.
```

## Not scenarios — manual by design

- **The view against the mock.** Open the window on a roster that holds a duplicate, open the mock's Settings frame beside it, and compare the note's text, place and wrapping. Then make a save the daemon refuses with `label-in-use`, and compare the marked rows and the footer's refusal with the frame. Record the result in the pull request, with any intended divergence entered under the README's Expected reconciliations. The Settings window has no render harness (the issue's own words), so this stays manual. The mock does not author the Accounts section today (card 5).
- **A marked row under VoiceOver.** `SettingsView` is outside the test bundle, so whether VoiceOver reads a marked row's words on that row is checked by hand with VoiceOver on, and recorded in the pull request. The words come from the pure function Rules 4 and 5 pin, so the marking is never colour alone. Nothing is announced: the warn form's announcement is retired (card 1), and the refusal reuses the rejected-save state, which no announcement accompanies today. Design § 12.3, C-9 does not call for one.
- **The Swift gates.** `xcodebuild test`, and an armed `panel-goldens` run that prints its `[panel-goldens] max drift` line. A run that says tests were skipped compared nothing.
- **Out of scope, stated so the silence is not read as a gap.** A window left open keeps the roster it loaded. A duplicate another surface makes while it stays open appears at the next open, and so does an account another surface adds, which a refusal can name as a bearer the window does not list (Rule 4):

  ```text
  apps/menubar/Sources/SettingsWindowController.swift:39 `show`
  ```

## Acceptance criteria → where they land

| Criterion on #1560 | Lands in |
|---|---|
| `oracle-a-duplicate-in-the-roster-shows-the-note` | Rules 1, 2 and 3; the uuid half is held (card 2) |
| `the-note-follows-the-saved-roster` | Rule 1, and Rule 4's first and last scenarios |
| `oracle-a-colliding-save-is-refused-and-its-rows-marked` | Rule 4, first and second scenarios |
| `only-a-refusal-marks-rows` | Rule 4, third to seventh scenarios |
| `wrap-not-truncate` | Rule 3, third scenario; a marked row's words, if the frame draws them in the row, are held with the frame (card 5) |
| `copy-passes-the-framing-scan` | Rule 5, first scenario |
| `only-labels-and-uuids` | Rule 5, second scenario |
| `mock-first` | no scenario: a process check on the diff of `apps/menubar/design/` and the commit order (card 5) |
| Invariant: a save that creates no collision marks no row | Rule 4, fourth to seventh scenarios |
| Invariant: Settings never reads or writes a credential | Rule 5, second scenario |
| Output conforms to `docs/specs/import-duplicate-label.feature.md` | Rule 5, third scenario, and Rule 4's swap and unrelated-save scenarios |
| Output conforms to `apps/menubar/design/menubar-preview.html` | manual (§ Not scenarios) |
| The repo's Swift gates | manual (§ Not scenarios) |

Changed from the warn form on 2026-11-03, because no save creates a collision for an announcement to report: `announcement-sentence` and `announce-is-wired` are retired, and `oracle-a-save-that-creates-a-collision-announces-it-once` and `quiet-saves-announce-nothing` are replaced by `oracle-a-colliding-save-is-refused-and-its-rows-marked` and `only-a-refusal-marks-rows`.

## 🟥 Open

### 1. `duplicate-label-policy` — resolved

Re-authored on 2026-11-03 to the operator's refuse ruling of that day (PRD § 8.1; design § 12.3, C-7 to C-9): the held examples H1, H3, H4 and H5 are Rule 4's Green examples now, and H2, the announcement's ordinal clause, is retired with the announcement.

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
- `import-duplicate-uuid-policy` and `orphan-stash-policy` belong to other features of #1550. `account-key-form` reaches this feature only through card 3.
- `login-mismatch-policy` was ruled by the operator on 2026-11-03: a rename is refused unless `--rename` is given (PRD § 8.1). It does not reach this feature. It gates CLI `capture`, CLI `login` and the socket `capture` (design § 12.3, C-3), while `config-set` renames with no consent, because Settings is the panel's way to a deliberate rename (C-6). Rule 1's renames are such saves.
