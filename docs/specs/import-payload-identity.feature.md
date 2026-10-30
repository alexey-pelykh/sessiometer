# Feature: import reports a contradictory artifact before it writes, and refuses a mismatched entry alone

Issue #1565 · PRD REQ-MIG-B-001 / B-002 / B-003 / C-001 · design D-11 · **Tier B**, scenarios pending binding

Example Mapping: 🟦 5 rules · 🟩 18 examples · 🟥 1 open (`import-duplicate-uuid-policy`)

> **Provisional.** The PRD's `dor_status` is `failed`: none of its requirements is ratified and its
> problem framing is unvalidated. The operator ratified the class of work ("/scope the gaps
> discovered") and nothing finer. Every rule, refusal form and wording below is pipeline-decided and
> waits for ratification.
>
> An artifact is coherent when its credential list agrees with its roster and each entry's stored
> identity names the account the entry is filed under. The exporter cannot build a duplicate or an
> unmatched entry, and it carries a mismatch only if its source stash held one (unmeasured; #1551). A
> hand-built or corrupted artifact can hold all three, and today `import` accepts each without a word.
>
> **The trap.** The checks `import` already has cannot see these defects, so a green import says nothing
> about them. `write_and_verify` (`src/cli.rs:5451`) compares what it wrote with what it read back, and
> an entry whose `oauthAccount` names another account round-trips perfectly. A duplicate is gone before
> anyone can look for it: `let secrets` (`src/cli.rs:5313`) builds a map keyed by uuid, so the last of
> two entries wins. And the merge loop walks the roster, so an entry the roster does not name is never
> looked up. Both checks therefore sit where the defect is still visible: on the payload before the map
> is built, and on the entry before it is written.
>
> Each scenario ends with `binds:`. `pending` means #1565 has not named the test yet.

## Rule 1 — the credential list is checked against the roster, and every disagreement is reported

```gherkin
Scenario: two credential entries for one uuid are reported, not resolved by position
  Given a payload whose roster names uuid A once
    And whose credential list holds two entries for A
   When import reads the payload
   Then the operator is told that A has two credential entries
    But NOT by silently keeping the last entry
    # What "told" means (an error that stops the import, or a notice that lets it go on) is the open
    # decision under 🟥. What does not depend on it: the finding exists, it names A, and it is made
    # before the swap lock and before any write (Rule 2).
    # AC `payload-findings-calibration` (row `A, A`) · PRD AC-MIG-001 · binds: pending

Scenario: a credential entry that no roster entry names is reported, not dropped
  Given a payload whose roster names uuid A
    And whose credential list holds an entry for A and an entry for uuid Z
   When import reads the payload
   Then the operator is told that Z has a credential entry no roster entry names
    But NOT by silently dropping it
    # The merge loop walks the roster (`for incoming_account in &incoming.roster`, `src/cli.rs:5329`)
    # and looks each uuid up (`secrets.get`, `src/cli.rs:5349`), so an entry nothing names is never read.
    # PRD REQ-MIG-B-002 is a Should and ASSUMED; REQ-MIG-B-001 is the Must.
    # AC `payload-findings-calibration` (row `A, Z`) · PRD AC-MIG-002 · binds: pending

Scenario Outline: the calibration table, which credential lists give which findings
  Given a payload whose credential list holds entries for <credential list>, in that order
    And whose roster names <roster>
   When the credential list is checked against the roster
   Then the findings are exactly <findings>

Examples:
  | credential list | roster | findings                             |
  | A               | A      | none                                 |
  | A, A            | A      | duplicate A (2 entries)              |
  | A, Z            | A      | unmatched Z                          |
  | Z, Z            | A      | duplicate Z (2 entries), unmatched Z |
  | A, B, A         | A, B   | duplicate A (2 entries)              |
  | A               | A, B   | none                                 |
  | (empty)         | A, B   | none                                 |
  # The last two rows are the negative space and matter as much as the rest. A roster account with
  # no credential entry is what `export --no-secrets` leaves as "needs re-login", and an empty list is
  # a config-only artifact. Neither is a finding, and the check still runs on a config-only payload.
  # AC `payload-findings-calibration` · binds: pending

Scenario: every finding is returned, in the order the uuids first appear
  Given a payload whose credential list holds entries for Z, A, Z, A, in that order
    And whose roster names A
   When the credential list is checked against the roster
   Then the findings are, in order: duplicate Z (2 entries), unmatched Z, duplicate A (2 entries)
    And no finding is withheld because an earlier one was found
    # Derived, not a row of the item's table. The Specification orders findings by where each uuid first
    # appears in the credential list, and the `Z, Z` row puts a uuid's duplicate before its unmatched
    # finding. Z appears first, so both of Z's findings precede A's. The table pins the order inside one
    # uuid; this example pins it across two.
    # AC `payload-findings-calibration` ("all findings rather than the first") · binds: pending

Scenario: a finding names the uuid, and the roster's label where the roster names it
  Given a payload whose roster names uuid A with the label `alice`, and does not name uuid Z
    And whose credential list holds two entries for A and one entry for Z
   When the findings are rendered for the operator
   Then the duplicate is named by A and by the label `alice`
    And the unmatched entry is named by Z alone
    And no email address, token or credential byte appears
    # A credential entry carries no label: labels live only in the roster half of the payload
    # (`Payload`, `src/migration.rs:200`). The uuid is the one name every finding has, and the label is
    # added only where the roster supplies one. The naming rules hold under every option.
    # AC `refusal-carries-facts-not-prose` (the naming half) · PRD REQ-NTC-C-001 · binds: pending
```

## Rule 2 — the check runs before the swap lock and before anything is written, whichever way the policy is answered

```gherkin
Scenario: the findings are made before the swap lock is taken and before anything is written
  Given the body of `apply_import`
   When the order of its steps is read from the shipped source
   Then the findings call comes after the roster half has parsed
    And it comes before the credential map is built
    And it comes before the swap lock is acquired
    # The anchors: `from_toml_str` (`src/cli.rs:5289`), `let secrets` (`src/cli.rs:5313`) and
    # `SwapLock::acquire` (`src/cli.rs:5324`). Each bound matters. After `let secrets` a duplicate no
    # longer exists. A check after the lock would first wait for it, and a busy lock would end the
    # import before any finding was made.
    # The scan carries a canary that finds all three anchors it orders. A scan that finds none of its
    # anchors passes over nothing and reports green.
    # Model: `the_roster_save_is_reached_only_after_the_swap_lock_has_been_released` (`src/capture.rs:2184`)
    # AC `findings-precede-the-lock-and-every-write` (1) · PRD AC-MIG-001 · binds: pending
```

## Rule 3 — a self-contradicting entry is refused alone, and the rest of the artifact imports

```gherkin
Scenario: the entry whose identity names another uuid is refused, and the others import
  Given a payload whose roster names accounts A (`alice`), B (`bob`) and D (`dana`)
    And whose entry for B holds an `oauthAccount` that names a different uuid, X
   When import runs over an empty target
   Then A and D are imported, and a stash exists for each
    And B is reported `failed`
    And no stash exists under B's name
    And B is not in the resulting roster
    And the report line for B names `bob`, the uuid B the entry is filed under, and the uuid X its `oauthAccount` names
    And the report line for B says which of the two uuids is which
    But NOT adopting the credential as it is
    But NOT aborting the whole import
    But NOT any credential byte in the report
    # B sits between two good entries, so a refusal that aborts the import shows up as D missing.
    # Today B is `imported`: `AccountImport::failed` (`src/cli.rs:5354`) is reached only when
    # `write_and_verify` (`src/cli.rs:5451`) returns an error, and it cannot object here: it compares
    # what it wrote with what it read back.
    # The test compiles and fails on the pre-change tree.
    # AC `oracle-mismatched-entry-is-refused-alone` · PRD AC-MIG-003
    #   · binds: `import_refuses_an_entry_whose_oauth_account_names_another_uuid` (named in #1565)

Scenario Outline: the check applies to exactly what is about to be restored
  Given account B is <on the target>, with the overwrite policy <overwrite>
    And the artifact's entry for B is <entry>
   When import runs
   Then B is reported <outcome>
    And <effect>

Examples:
  | on the target | overwrite    | entry              | outcome  | effect                                                          |
  | absent        | either       | self-contradicting | failed   | no stash is written and B is not added to the roster            |
  | present       | in force     | self-contradicting | failed   | B's existing stash and roster entry are untouched               |
  | present       | not in force | self-contradicting | skipped  | nothing is restored for B, so nothing is checked                |
  | absent        | either       | missing            | imported | B lands as a roster entry that needs a login, as today          |
  # A skipped account restores nothing, so there is nothing to check. An account with no credential
  # entry has nothing to check either.
  # AC `oracle-mismatched-entry-is-refused-alone`, `a-refused-entry-is-failed-and-counted` · binds: pending

Scenario: a refused entry counts as failed, and the import ends through the existing incomplete path
  Given the payload of the first scenario under this rule
   When import runs and prints its report
   Then the tally counts B under `failed`, and A and D under `imported`
    And the merged roster holds A and D, and the caller saves it
    And import exits non-zero through the error it already uses when any account failed
    # No new exit path. `count_import_outcomes` (`src/cli.rs:5209`) counts a failed row, and `import`
    # returns `MigrationImportIncomplete` (`src/error.rs:1118`) when any account failed.
    # The line keeps its `` failed `label` `` prefix, so these must pass without edits:
    # `a_credential_write_failure_is_surfaced_not_swallowed` (`src/cli.rs:12141`) and
    # `a_write_that_does_not_persist_is_caught_by_read_back_verification` (`src/cli.rs:12165`)
    # AC `a-refused-entry-is-failed-and-counted` · binds: pending

Scenario: whatever the mix, no stash holds another account's identity
  Given payloads of up to three entries, built from every mix of matching and mismatching blobs
   When each is imported over an empty target
   Then every stash the target holds carries an `oauthAccount` that names the uuid its stash name carries
    # A bounded sweep, with no property-testing crate. Three entries put a bad entry first, in the
    # middle and last among good ones.
    # AC Invariant "an imported stash always holds an `oauthAccount` whose uuid is the entry's own"
    #   · PRD § 10.6 · binds: pending

Scenario: the refused line says what was detected, and never an email or a token
  Given a mismatched entry whose `oauthAccount` carries a distinctive email-shaped string
    And whose credential is a distinctive token-shaped string
   When the import report is rendered
   Then neither string appears in the report
    And the account's label and both uuids do
    And the issue #15 redaction meter passes over the text
    # The line says only that the entry contradicts itself. It does not say which half is the stray
    # one. The repo records the token and the `oauthAccount` as independent (a mismatch is tolerated
    # at runtime; `build/version-compat.md`, H2), so the comparison cannot tell. The design's own draft
    # says the credential "belongs to" the second uuid, which is more than was detected.
    # The meter is `crate::redaction::meter` (`src/redaction.rs:91`), test-only.
    # AC `no-email-no-token-in-any-new-line` · PRD REQ-NTC-C-001 · binds: pending
```

## Rule 4 — the identity comparison is exact, one function serves both callers, and an unparseable blob stays a parse failure

```gherkin
Scenario Outline: what counts as the same uuid
  Given an entry filed under uuid A
    And an `oauthAccount` whose `accountUuid` is <blob uuid>
   When the entry's identity is checked
   Then the result is <result>

Examples:
  | blob uuid                  | result                        |
  | A                          | ok                            |
  | a different uuid           | mismatch, carrying both uuids |
  | A in the other letter case | mismatch                      |
  | A followed by a space      | mismatch                      |
  # Byte for byte: no trimming and no case folding. If #1551's fourth premise finds that Claude Code
  # ever varies a uuid's case, a byte comparison would refuse a good entry. The fix then lands in the
  # one comparison function (the last scenario under this rule), and the case row changes with it.
  # AC `entry-check-boundaries` · binds: pending

Scenario Outline: an oauthAccount that does not parse is not a mismatch
  Given an entry whose `oauthAccount` is <blob>
   When import runs
   Then the account is reported `failed`, as today
    And nothing is written for it
    And its report line names no second uuid, because there is none to name

Examples:
  | blob                            |
  | JSON with a blank `accountUuid` |
  | bytes that are not JSON         |
  # Both fail before any write, inside `write_and_verify`.
  # `OauthAccount::from_object_bytes` (`src/cli.rs:5458`) rejects them.
  # `account_uuid.trim().is_empty()` (`src/claude_state.rs:113`) is the blank test, so "blank" includes
  # an `accountUuid` of only whitespace.
  # AC `entry-check-boundaries` · binds: pending

Scenario: the capture path and the entry check compare uuids through one function
  Given the roster match in `plan_capture`, and the identity check on an import entry
   When the source of both is read
   Then each calls the same uuid comparison, and neither spells `==` on the two uuids itself
    And every existing capture test passes without edits
    # Today `plan_capture` compares inline: `roster.iter_mut().find(|a| a.account_uuid == account_uuid)`
    # is `src/capture.rs:658`. One home for the comparison is what lets a finding of #1551 be fixed once.
    # The comparison's home is the implementer's call.
    # AC `one-comparison-function` · binds: pending
```

## Rule 5 — nothing outside the two checks moves, and nothing that describes them goes stale

```gherkin
Scenario: a payload with no finding imports exactly as before
  Given any payload the checks find nothing in, including one the exporter builds from well-formed stashes
   When import runs
   Then it yields no finding and no refusal
    And every existing `apply_import` test passes without edits
    And that includes the duplicate-label tests and the active-account non-adoption tests
    And `import_report` returns the same text for every existing fixture
    # The round trip proves the exporter: `gather_payload` (`src/cli.rs:4957`) builds a payload from a
    # roster of well-formed stashes, and `apply_import` (`src/cli.rs:5277`) imports it with no finding
    # and no refusal. The exporter can carry a mismatch only if its source stash held one.
    # The diff does not touch `gather_payload`, so the exporter's output is unchanged.
    # AC `well-formed-payloads-are-untouched`, Invariant "the exporter's output is unchanged"
    #   · binds: pending

Scenario: the artifact format and its frozen fixtures do not move
  Given a full test run, with no `--include-ignored`
   Then `git status --porcelain build/fixtures/` prints nothing
    And the three frozen-fixture tests pass
    And the diff touches no field, derive or serde attribute of `Payload` or `ManagedAccount`
    And the diff does not touch `FORMAT_VERSION`
    # The tests: `the_frozen_v1_plaintext_fixture_still_parses_to_its_payload` (`src/migration.rs:1825`),
    # `the_frozen_v1_encrypted_fixture_still_decrypts` (`src/migration.rs:1840`) and
    # `the_frozen_fixtures_are_pinned_to_the_expected_format_version` (`src/migration.rs:1862`).
    # The types: `Payload` (`src/migration.rs:200`), `ManagedAccount` (`src/migration.rs:220`) and
    # `FORMAT_VERSION` (`src/migration.rs:97`).
    # `cargo test -- --include-ignored` also runs the emitter `emit_v1_golden_fixtures`
    # (`src/migration.rs:1800`), which re-seals the frozen encrypted fixture with a fresh salt. Run the
    # named tests only.
    # AC `format-and-fixtures-untouched` · PRD REQ-MIG-C-001 · binds: the three frozen-fixture tests above

Scenario: every new operator-facing string is a named template that a framing scan covers
  Given the findings text and the refused line's clause
   When the framing scans run
   Then the error template passes the error scan
    And every template outside `Error` is registered with the advisory scan
    And a tripwire fails when a template is declared and not registered
    And an injected banned token is caught
    And the pinned totals in the inline-prose gate do not move
    # The error scan: `every_error_template_carries_no_banned_framing_beyond_the_pinned_ledger` (`src/error.rs:2599`)
    # The advisory scan: `scan_advisory_banned` (`src/framing_vocabulary.rs:456`)
    # The inline-prose gate: `every_function_spelling_inline_prose_is_dispositioned` (`src/cli.rs:14420`)
    # A new word-bearing literal in `src/cli.rs` moves that gate's totals, so the strings live outside
    # that file. `import_report`, `duplicate_label_notice` and `non_adoption_notice` are listed
    # `Unscanned` in `INLINE_PROSE_REGISTER` (`src/cli.rs:14321`), a gap #1167 tracks. This item must
    # not widen it.
    # AC `notices-pass-the-scans` · PRD REQ-NTC-C-002 · binds: the two scans above, and pending for the tripwire

Scenario: every statement of what `failed` means stays true
  Given `failed` in an import report meant that a credential write or its read-back verification failed
    And it now also covers an entry refused because its `oauthAccount` names another uuid
   When the surfaces that restate the old meaning are enumerated by the token pair `read-back` and `import`
   Then each returned surface carries a disposition
    And the pull request reports the query's cardinality
    # A disposition is one of: corrected in-band; propagated as a tracked item that carries the
    # corrected fact verbatim; recorded as not restating the posture, with the reason; recorded as
    # frozen by construction, with the reason.
    # The query: `git grep -lI -i -E --all-match -e 'read-?back' -e 'import' -- src docs README.md CONTRIBUTING.md build apps`
    # Its canary: the same invocation limited to `src/cli.rs` must return that file.
    # Keyed on the token pair, not on citations to `ImportOutcome::Failed`. A surface that restates the
    # meaning without citing its home is the one a citation sweep misses.
    # One such surface is the `Import` event doc (`src/observability.rs:1906-1913`).
    # AC `failed-meaning-propagated` · binds: the pull request's own record
```

## Acceptance criteria that no scenario expresses

- *Bounded echo*: `echoed-uuids-are-bounded` *(any option)* — the two uuids the new lines echo that never
  passed the roster validator (an unmatched entry's, and the `accountUuid` a blob names) are rendered
  through a bound before they are echoed: length-checked first, and interpolated with `{:?}` so a value
  carrying newlines or control bytes lands on one printable line. A test seeds a 2 KB uuid and a uuid
  holding a newline into an unmatched entry and into a blob, and asserts that each rendered line is one
  line of bounded length. No scenario here expresses it.

## 🟥 Open

- **`import-duplicate-uuid-policy`**: what `import` does after a finding (design § 12.2, PRD § 8.1).
  The operator has not answered. The options and what each costs:
    - Refuse the whole payload. It is the safest, and a hand-built artifact costs one round trip.
    - Warn, and land that account roster-only as "needs re-login". It is a partial import, and the
      operator logs that account in again.
    - Warn, and keep today's last-entry-wins. It is audible, but a credential is still picked by
      position.

  #1565 builds **refuse** unless an answer reaches it before the item starts, and says so in its pull
  request. The design brief recommends refuse (item 6). Rules 1 and 2 hold under every option. What an
  option changes is the *Then* of the four examples below, so none of them is written as a scenario:

  | Example (Given and When as in Rule 1) | Refuse (built) | Warn, roster-only | Warn, last-wins |
  |---|---|---|---|
  | two entries for A | an error names A; the stash is empty; no roster is saved, no audit event is emitted, no per-account report is printed; exit code 1 | a notice names A; neither entry is stashed; A lands as a roster entry that needs a login | a notice names A; the last entry's credential is the one stashed |
  | an entry for Z that no roster entry names | an error names Z; the stash holds nothing | Z is reported and ignored; the rest imports | Z is reported and ignored, as today but audibly |
  | the carrier of the findings | a new `Error` variant holds them as fields, its text names every one, and `exit_code` (`src/error.rs:1208`) maps it to `1` through its default arm | a payload-level notice from a named template | the same notice |
  | a finding while another process holds the swap lock | the findings error is returned, not `SwapLockBusy` | the item drops this test for the source-order scan; `import` goes on to the lock after the notice | the same |

  The criteria these examples block, all from #1565: `oracle-two-credentials-for-one-uuid`,
  `oracle-credential-for-an-unrostered-uuid`, `refusal-carries-facts-not-prose` (its carrier half; the
  naming half is the last scenario of Rule 1) and the behavioural half of
  `findings-precede-the-lock-and-every-write`.
