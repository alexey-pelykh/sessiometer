# Feature: the status and watch wire names each roster entry, and the next-swap target, by an identifier

Issue #1571 · PRD § 10.4 REQ-WIRE-I-001, REQ-WIRE-C-001, REQ-WIRE-C-002, REQ-WIRE-C-003 (AC-WIRE-001, AC-WIRE-002) and § 10.11 REQ-NTC-C-001 (AC-NTC-002) · design D-5, D-6, § 8.3, § 8.6, § 11.1, § 11.5 CAP-10, § 12.2

Example Mapping: 🟦 6 rules (over the 5-rule gate: reported for /decompose, not split here) · 🟩 20 examples · 🟥 2 open (`account-key-form`; `derived-form-derivation-unspecified`, live only under the derived form)

Tier B — scenarios pending binding in #1571's execution, which does not start until the operator answers `account-key-form`. Work class: deterministic-output (the primary deliverable is a wire frame, its goldens and the decoder that reads them, and a mechanical grader can falsify each). The oracle item is not filed: every grader below reads a key or a value that the answer fixes, so it is filed once the operator answers. Until then no scenario here has an independent grader.

- A scenario marked `RED-owed` fails on today's tree at a seam that exists today. Its grader is held back, not skipped, and is pre-authored by a party other than #1571's executor.
- A scenario marked `new seam` reads a field or a function that the change adds, so nothing can be written against it before the fix. #1571's executor authors it and proves it by mutation.
- A scenario marked `RED: account-key-form` restates what the item says about one of the two offered options, and binds only if the operator picks that option.
- A scenario marked `passes today` guards a shape the change must not move.
- A scenario marked `review-bound` or `judgment` is decided by recorded command output or by an independent reviewer, not by a test.

> **PROVISIONAL — stated once, here.** `docs/requirements/account-label-integrity.md` carries `dor_status: failed`: every requirement in it is pipeline-authored and unratified, and its problem framing is unvalidated. The operator has ratified the class of work this file rests on: R-6 in `docs/requirements/migration-credential-portability.md` (user-ratified 2026-08-04; it records duplicate labels as a documented, accepted state) and the parity ruling of 2026-07-31 in `docs/requirements/gui-cli-capability-parity.md` ("Choosing a surface is not accepting reduced capability"). Which identifier the wire carries is not ruled on, and no scenario below assumes an answer. One default is the pipeline's own: the next-swap target carries an identifier too. REQ-WIRE-I-001 asks for one "on each account line" only; design D-6 adds the target's and flags it for ratification together with the wire form.

> **Story.** The operator may hold two accounts under one label, which R-6 accepts. The `status` reply and the `watch` snapshot frame name every account by its label alone, and the next-swap target the same way. A client cannot tell two same-label accounts apart, cannot follow one account across a relabel, and cannot find the row the footer callout means. The daemon already holds a value that is unique per roster entry and survives a relabel: the entry's `account_uuid`. This item puts an *identifier* for it on each account line and on the target, and renders nothing. "Identifier" here means whatever the wire carries to name a roster entry. Its form is the open decision `account-key-form`.
>
> The trap in this feature is that three plausible fixes each pass one scenario and fail another.
>
> - An identifier built from the entry's position in the roster, or from a counter, differs per entry and survives a relabel. It moves when another entry is removed or the daemon restarts (Rule 1, the churn outline).
> - A field added on the Rust side alone passes every Rust test. It also passes every Swift test that only checks that a frame decodes, because the Swift decoder drops an unknown key without a sound (Rule 3, the by-value scenario).
> - A golden regenerated from a builder that never sets the new field passes both byte pins. It freezes a blank identifier (Rule 4, the content scenario).

## Rule 1 — each account line names its roster entry by an identifier that is unique per entry and unchanged by a relabel

```gherkin
Scenario: two accounts that share a label are told apart on the status reply
  # AC: oracle-same-label-accounts-carry-different-identifiers · AC-WIRE-001 · REQ-WIRE-I-001 · RED-owed
  # Binds: two_accounts_sharing_a_label_carry_different_identifiers_on_the_status_wire (the item's suggested name; the oracle, once filed)
  Given a roster of two entries, uuid "u-A" and uuid "u-B", both labelled "work"
   When a client reads the status reply
   Then the two account lines carry different identifiers
    And neither identifier is blank
    And both lines still read "work" as their label
    # Seam: `status_response`, `src/daemon/snapshot.rs:988`, over the snapshot a hermetic daemon builds from that roster, checked on the serialized JSON.
    # The watch snapshot frame is the same projection: `versioned_status_response` serves the status reply at `src/daemon/socket.rs:725` and the watch channel at `src/daemon/socket.rs:398`.
    # The roster is one that R-6 accepts. `duplicate-label-policy` decides whether a WRITE may newly create one, not whether the state is legal.
    # Give both entries the same readings, so that nothing but the identifier can tell their lines apart.

Scenario: a relabel changes the label on a line and never its identifier
  # AC: oracle-same-label-accounts-carry-different-identifiers, second half; invariant "a relabel never changes an account's identifier on any surface" · AC-WIRE-001 · RED-owed
  # Binds: a_relabel_leaves_an_accounts_identifier_unchanged_on_the_status_wire (new, suggested name)
  Given the same roster, and the identifier on A's line noted from a first read
   When the operator relabels A "home"
    And a client reads the status reply again
   Then A's line reads "home" and carries the identifier noted before
    And B's line still reads "work" and carries the identifier it carried before
    # The target is the same entry's other face (Rule 2). It carries the identifier on its entry's line, so this scenario covers the target's relabel too.

Scenario Outline: an identifier survives the churn around its entry
  # AC: Specification, "The value" (deterministic across restarts) · design § 12.2 (an identifier that is not stable across restarts or roster edits is dominated) · RED-owed
  # Binds: an_identifier_survives_a_restart_and_the_removal_of_another_entry (new, suggested name)
  Given a roster of three entries, uuid "u-A", "u-B" and "u-C", and the identifier on B's line noted from a first read
   When <event>
    And a client reads the status reply again
   Then B's line carries the identifier noted before

  Examples:
    | event                                      |
    | the daemon is rebuilt from the same roster |
    | the entry "u-A" is removed from the roster |
    # An identifier built from the roster position, or from a counter, passes every scenario above and fails here: B moves to the first place, or the counter starts over.
    # Only B's identifier is asserted. What happens to A's is not this scenario's business.
```

> **RED — blocked on `account-key-form`.** The next two scenarios are the item's own per-option criteria. Each binds only if the operator selects its option, and the other is struck.

```gherkin
Scenario: under the uuid form, the identifier is the entry's uuid, byte for byte
  # AC: identifier-is-the-roster-uuid (applies only if the operator selects the uuid itself) · RED: account-key-form
  # Binds: named once the form is chosen — `status_response` and the `config-get` view over one roster
  Given a roster entry with uuid "u-A"
   When a client reads the status reply and the `config-get` view
   Then A's line carries "u-A" under `account_uuid`, and a target at A carries it under `to_account_uuid`
    And that is the uuid `list` and `config-get` give for the same entry
    # The two key names are design D-6's for this form. The `config-get` view is `AccountView`, `src/config/settings.rs:60-76`.

Scenario: under a derived form, the identifier is a pure function of the uuid and of nothing else
  # AC: derived-identifier-contract (applies only if the operator selects a derived value) · RED: account-key-form, and `derived-form-derivation-unspecified`
  # Binds: named once the derivation is fixed — the pure derivation function
  Given the derivation that the operator's answer fixes, and a set of generated distinct uuids
   When the derivation is applied to each uuid, in two rosters whose stashes hold different credential bytes
   Then the identifiers are pairwise distinct
    And the same uuid gives the same identifier in both rosters, after a restart and after a relabel
    And each identifier passes the entropy backstop of the issue #15 meter
    # "Nothing else" is design SEC-1 (§ 11.1): nothing derived from credential bytes is persisted or sent. The two key names are design D-6's for this form.
    # Known-answer vectors cannot be written yet; the second Red card says why. That the function cannot be run backwards is a judgment, listed under the aspects below.
```

## Rule 2 — the next-swap target names its roster entry the same way, and the verdicts that name no target are untouched

```gherkin
Scenario: the next-swap target carries its entry's identifier beside its label
  # AC: oracle-next-swap-target-carries-its-identifier · design D-6 · RED-owed
  # Binds: the_next_swap_target_carries_the_identifier_of_its_roster_entry (new, suggested name; the item names the seam: `Daemon::next_swap` and the serde form of `NextSwap`)
  Given a roster in which "u-A" is active and "u-B" is the only viable target, both labelled "work"
   When a client reads the status reply
   Then `next_swap` is a target whose label reads "work"
    And it carries the identifier on B's line
    And it does not carry the identifier on A's line
    # Today the target's "to" reads "work" and names the active account as readily as the target. That is the defect: the callout cannot say which of two same-label rows it means (design D-6).
    # `Daemon::next_swap` is `next_swap`, `src/daemon.rs:5626`, and it builds the target from the roster entry's label at `NextSwap::Target`, `src/daemon.rs:5650`.

Scenario Outline: the verdicts that name no target serialize as they did
  # AC: oracle-next-swap-target-carries-its-identifier ("the two other `next_swap` states are unchanged") · passes today
  # Binds: status_response_projects_the_next_swap_candidate_and_drops_last_swap (existing, `src/daemon.rs:18114`, kept and never weakened)
  Given a snapshot whose next-swap verdict is <verdict>
   When it is projected to the status reply
   Then `next_swap` serializes as <wire>

  Examples:
    | verdict                                    | wire                                                                 |
    | no viable target, the weekly window gating | {"state":"no_viable_target","cause":"weekly","resets_at":1893800500} |
    | awaiting data                              | {"state":"awaiting_data"}                                            |
    | no active account to swap from             | null                                                                 |
    # The existing test pins all three shapes with `contains`. The other variants of `NextSwap`, `src/daemon/snapshot.rs:878`, do not change.
```

## Rule 3 — the change is additive: a frame without the keys still decodes, and the decoder reads the keys by value

```gherkin
Scenario: the Rust client decodes an account line and a target written without the keys
  # AC: additive-and-tolerant · REQ-WIRE-C-003 · new seam
  # Binds: a_line_and_a_target_written_without_the_identifier_decode_with_none (new, suggested name, beside `a_pre_393_target_without_a_reason_decodes_as_none`, `src/daemon/snapshot_build.rs:5013`)
  Given an account line and a next-swap target as an older daemon writes them, with no identifier key
   When the Rust status client decodes them
   Then each decodes with no identifier
    And an explicit null decodes the same way
    # A current daemon always sends the keys and an older one omits them, so both fields are `Option` with `#[serde(default)]`, as the target's `reason` is.

Scenario: the app decodes an older daemon's frame, two same-label lines included
  # AC: additive-and-tolerant, Swift half · design § 8.3 (a fixture pinned at the outgoing minor, with two account lines of one label, proves the fallback) · new seam
  # Binds: testPreIdentifierFrameDecodesWithNoIdentifiers (new, WireDecoderTests, beside `testPreReasonTargetDecodesWithNilReason`); it runs over #1566's older-daemon fixture when that has landed, else over a pinned fixture this item adds at the outgoing minor
  Given a snapshot frame at the outgoing minor with two account lines labelled "work" and a next-swap target, none carrying an identifier
   When the app decodes it
   Then it yields two account lines, each with no identifier
    And the target has no identifier
    And no line is dropped
    # Whichever item adds the fixture, the sweep in Rule 4 leaves it at its minor, and the pull request says so. A pre-change daemon sends exactly this shape.

Scenario: the app reads each snapshot golden's identifiers by value
  # AC: additive-and-tolerant ("the tests assert the decoded identifier by value") and the conformance criterion, verified by concretize-and-compare · new seam
  # Binds: testEverySnapshotGoldenDecodesItsIdentifiersByValue (new, WireDecoderTests, beside `testEveryExpiryStateDecodesFromTheGoldenAndTheAbsentFieldIsUnknown`)
  Given each snapshot fixture that `WireGoldenTests` pins byte for byte to a golden
   When the app decodes it
   Then each account line's decoded identifier equals the value at that place in the golden's JSON
    And the target's decoded identifier equals the golden's, in each golden that has a target
    # Pinning bytes is not enough. `WireGoldenTests` compares a literal with a file and decodes nothing, and the decoder drops an unknown key without a sound, so a field added on the Rust side alone fails no existing Swift test.
    # The precedent for decoding a byte-pinned fixture and asserting what comes out is the test named in Binds (issue #886).

Scenario: a reader built before the change reads a frame that carries the keys
  # AC: additive-and-tolerant ("an older-style reader that ignores unknown keys") · RED-owed (the carries-the-keys assertion)
  # Binds: a_reader_without_the_identifier_keys_reads_a_regenerated_golden (new, suggested name, status tests)
  Given a regenerated golden frame that carries the identifier keys
   When a reader whose model of an account line and a target lacks those keys decodes it
   Then the frame decodes
    And every field that reader models keeps its value
    # A reader built before the change cannot be run, so this models one with a struct that lacks the keys.
    # The test first asserts that the frame carries the keys, so it cannot pass over a frame without them.
```

## Rule 4 — the version moves by one minor, and nothing else in the contract moves

```gherkin
Scenario: the version moves by exactly one minor and no other version moves with it
  # AC: minor-bump-in-lockstep · REQ-WIRE-C-003 · AC-WIRE-001 ("BUT NOT a major version bump") · RED-owed, by mutation
  # Binds: the existing pin `the_committed_wire_goldens_still_match_the_daemon_encoders`, `src/daemon/snapshot_build.rs:3929`, plus the diff in the pull request
  Given `STATUS_SCHEMA_VERSION`, `src/daemon/snapshot.rs:538`, reads major 1 and minor M on the branch after it is rebased onto main
   When the identifier lands
   Then it reads major 1 and minor M + 1, and its doc comment gains an entry for M + 1
    And the stats, log and reliability outputs keep their own versions
    # M is read at merge time. #1457 pre-assigns the next minor to its own wire change, and whichever change merges second rebases, regenerates the goldens and takes the one after.
    # RED, by mutation: move the constant and regenerate no golden, and the pin fails. Add the field and regenerate no golden, and it fails the same way.
    # The other three versions are `JSON_SCHEMA_VERSION` in `src/stats.rs:80`, in `src/log.rs:167` and in `src/reliability.rs:264`.

Scenario Outline: each golden differs from its old bytes only by what the change adds
  # AC: minor-bump-in-lockstep ("differ from their old bytes only by the added keys and the `schema_version` minor"; `wire-stats-basic.json` byte-identical) · heartbeat-golden-moves-by-version-only · review-bound
  # Binds: review — with the added keys and the minor removed, the old bytes equal the new bytes; the diff goes in the pull request
  Given <golden>, as committed before the change
   When the change is made and the status goldens are regenerated by `cargo test -- --ignored emit_wire_golden_fixtures` and by no other way
   Then <golden> differs from its old bytes by <delta>

  Examples:
    | golden                             | delta                                           |
    | wire-snapshot-basic.json           | the identifier keys and the minor               |
    | wire-snapshot-canonical-scrub.json | the identifier keys and the minor               |
    | wire-snapshot-expiry.json          | the identifier keys and the minor               |
    | wire-snapshot-next-swap.json       | the identifier keys and the minor               |
    | wire-heartbeat-basic.json          | the minor and nothing else                      |
    | wire-stats-basic.json              | nothing: it is not regenerated and is identical |
    # The emitter is `emit_wire_golden_fixtures`, `src/daemon/snapshot_build.rs:3862`. It does not write the stats golden, which belongs to `stats --json`. Never run the suite with `--include-ignored`: it rewrites a frozen migration fixture.
    # The heartbeat frame holds `type`, `generated_at` and `schema_version`, and no account line, `HeartbeatFrame`, `src/daemon/socket.rs:1464`. REQ-WIRE-I-001 says "both `watch` frames", and a heartbeat has no line to put an identifier on.

Scenario: the Swift fixtures at the outgoing minor move, and every pinned fixture stays
  # AC: minor-bump-in-lockstep (the sweep rule) · review-bound
  # Binds: review — the output of the two greps before and after, in the pull request
  Given the output, before the change, of `grep -n '"minor":<outgoing>' apps/menubar/Tests/Fixtures.swift` and of `grep -nE 'minor: <outgoing>|"minor":<outgoing>' apps/menubar/Tests/WireDecoderTests.swift`
   When the change is made
   Then every fixture and assertion those commands listed reads the new minor
    And every fixture at any other minor is byte-identical, #1566's older-daemon fixture among them when it exists
    And the inline "future daemon" literal in `WireDecoderTests.swift`, which the spaced grep misses and which stops being future on the bump, is accounted for in the pull request
    # Run the two commands again after the change and put both outputs in the pull request. A golden-backed fixture left behind fails `WireGoldenTests`. A current-minor fixture with no golden behind it is caught only by this sweep.

Scenario: the regenerated goldens carry a real identifier, not a blank one
  # AC: minor-bump-in-lockstep and additive-and-tolerant; Implementation Notes ("name every field with no `..` rest pattern") · RED-owed
  # Binds: the_wire_goldens_carry_a_distinct_non_blank_identifier_on_every_account_line (new, suggested name, beside `the_expiry_wire_golden_covers_all_four_states_and_says_unknown_for_the_absent_field`, `src/daemon/snapshot_build.rs:3986`)
  Given each regenerated snapshot golden
   When its account lines are read
   Then every line carries an identifier that is not blank
    And the lines of one golden carry pairwise different identifiers
    And the target in each golden that has one carries a non-blank identifier
    # The golden builder `watch_snapshot`, `src/daemon/snapshot_build.rs:3305`, builds each reading with `..Default::default()`. A new field left to that default freezes a blank identifier: both byte pins pass over it, and a Swift test that compares the model with the golden agrees about a blank.
    # The precedent for asserting a golden's content and not only its bytes is the test named in Binds (issue #886).

Scenario: no surface changes the text it displays for a roster without a duplicate label
  # AC: Invariant "no surface changes the text it displays for a roster without a duplicate label" · "the panel goldens are unchanged" · passes today
  # Binds: the existing `the_committed_status_goldens_still_match_the_render`, `src/cli.rs:17144`, and an armed panel-goldens run
  Given a roster in which no two entries share a label
   When the `status` text view and the panel are rendered before and after the change
   Then the text is byte-identical, and the panel stays within its committed goldens
    And neither the CLI render goldens nor the panel goldens are regenerated
    # A regenerated CLI golden would owe a `CLI-Goldens-Rebaselined:` trailer, and a regenerated panel golden a `Panel-Goldens-Rebaselined:` one. Neither is owed.
    # No `status` text golden mentions the schema, so the bump alone cannot move one. `status --json` is the wire and does print the identifier, which is the exposure design T-04 weighs.
    # An armed panel-goldens run prints its `[panel-goldens] max drift` line. "2 tests skipped" compared nothing.
```

## Rule 5 — the identifier is safe to paste: no credential, token, email or digest reaches the wire

```gherkin
Scenario: the redaction meter passes over a corpus that holds the identifier
  # AC: no-email-no-token-no-digest · REQ-WIRE-C-001 · AC-NTC-002 · RED-owed (the corpus-contains-the-identifier assertion)
  # Binds: redaction_meter_emits_no_secret_on_any_channel_across_the_full_loop (existing, `src/daemon.rs:19069`, extended with a roster of two entries that share a label)
  Given the full-loop meter fixture, with a roster of two entries both labelled "work"
   When every channel of a tick is harvested into one corpus, the status JSON included
   Then the corpus contains the identifier of each entry
    And the meter then reports no finding
    # The order is the point: the contains-assertion runs first, so a pass over a corpus without the identifier cannot happen. The harvester is `harvest_channels`, `src/daemon.rs:18748`, and it already adds the status reply's JSON.
    # The fixture's uuids today are repeated digits under distinct labels, so the existing pass says nothing about a roster that shares a label. A hyphenated uuid's longest alphanumeric run is below `ENTROPY_MIN_RUN`, `src/redaction.rs:177`.
    # An unhyphenated uuid, which config validation accepts, can reach the backstop. What follows from that is part of `account-key-form`, and is not decided here.
    # The meter is a test-time instrument: `meter`, `src/redaction.rs:91`, sits under `#[cfg(test)]`. "Passes the meter" is a claim about a fixture, and not a runtime filter.
```

## Rule 6 — every statement about what the status wire carries is reconciled in the same change

```gherkin
Scenario: both steps run, and each proves that it ran
  # AC: statements-reconciled (the sweep's canary and the read step's canary) · review-bound
  # Binds: review — the verbatim sweep output and the list of units read, in the pull request
  Given the item's sweep query, run at the start over src, apps, build, README.md, CONTRIBUTING.md and docs
    And the doc comment of each type and function on the wire's path, read unit by unit
   When their results are recorded
   Then the sweep output includes the hit on "account-uuid" in the `canary` field's doc, `src/daemon/snapshot.rs:665-668`
    And the read step has found the phrase "a roster label or a bare reason" in the `NextSwap` type doc, `src/daemon/snapshot.rs:871-878`
    # A zero is read as absence unless the query is shown to have run, and that hit is the proof. The sweep matches one line at a time, and that phrase wraps across two, so only a reader finds it.
    # Both steps run again at the end, and the disposition of each hit goes in the pull request.

Scenario: the canary payload stays label-only, and its docs say that and no more
  # AC: statements-reconciled ("none is rewritten to say more than the code does") · REQ-WIRE-C-002 · judgment
  # Binds: the independent fresh-context review (design § 11.5, gate G-REVIEW)
  Given the `canary` field's doc says it carries "Labels only — never a token, email, or account-uuid (issue #15)"
   When the identifier joins the account lines
   Then the doc still describes the canary payload truthfully
    And it no longer reads as if issue #15 forbids an account uuid anywhere on the wire
    # The canary payload does not change, so "labels only" stays true of it. Its Swift mirror, the panel's canary-banner formatter doc and a test comment say the same and are treated the same. The canary module doc and two more canary comments are read, and kept where they still hold for the canary alone. The item's table lists them all.

Scenario: a hit that the item's table omits is dispositioned all the same
  # AC: statements-reconciled ("a disposition for each hit") · REQ-WIRE-C-002 · found by this formulation · judgment
  # Binds: the independent fresh-context review, reading the sweep's full output and not only the table
  Given the sweep's output, including hits that the item's table does not list
   When each hit is read
   Then each carries a disposition in the pull request: corrected, unaffected with its reason, or frozen with its reason
    # The table is a starting disposition and not the list. A partial read of the daemon's wire files, the README and the Swift sources found three statements about what the status wire carries that neither the table nor the read step lists:
    # the control channel's module doc, "carrying handles + percentages only", at `UnixControl`, `src/daemon.rs:57-58`;
    # two comments in `status_response_projects_the_next_swap_candidate_and_drops_last_swap`, `src/daemon.rs:18114-18136`, "a label plus the daemon's #393 selection reason" and "a label only";
    # the comment in `redaction_meter_emits_no_secret_on_any_channel_across_the_full_loop`, `src/daemon.rs:19069-19079`, "only the label reaches the log/status/UDS channels; the uuid reaches only the `list` view". No pattern of the sweep's query matches it.
    # That is what a partial read found, and not the complete list. The review reads every hit.
```

## Open — two Red cards

🟥 `account-key-form` (PRD DQ-1; design § 12.2). Which identifier the wire carries: the account's uuid itself (`account_uuid`, `to_account_uuid`), or a value derived one-way from it (`account_key`, `to_account_key`). #1571 is gated on the answer. The item states both options and their costs and picks none, and so does this file. Every `RED-owed`, `new seam` and `RED: account-key-form` scenario reads a key name or a value that the answer fixes, so those wait on it. The same scenarios also assert properties: distinct, stable under a relabel and across a restart, not blank. These hold under both offered options and do not wait. The third option, a daemon-local opaque id, is not offered, because it fails the churn outline in Rule 1.

Part of the same decision, and not decided here: under the uuid form, a uuid with no hyphens goes onto the wire. Config validation accepts one (`account_uuid_violation`, `src/config/validate.rs:525`), and it can reach the entropy backstop of the issue #15 meter. The operator either accepts that as a cost of the form or answers it with a format rule.

🟥 `derived-form-derivation-unspecified` (a consequence of `account-key-form`'s derived option; live only if the operator selects it). The item says its executor names and documents the derivation in the code, and that a known-answer test "pins the output for fixed uuids". A vector written after the executor picks the derivation confirms the code against itself. It is green whatever the derivation is. For the grader to be independent, two things have to hold. The answer has to fix the derivation: the function, its input, the output length and the format. And a tool outside the code has to compute the vectors. Nothing in the PRD, the design or the code fixes any of it. No code defines `account_key`, and the design's stub says only that the format "needs separators or a short length". No known-answer scenario is written. The derived-form scenario in Rule 1 states the properties that hold whatever derivation is picked.

## Aspects that are not scenarios

- **`gate-answer-recorded`** (mechanical, a recorded decision). The operator's answer to `account-key-form` is recorded on #1571 before any code is written, and the field names and the value follow it. The check is reading the issue. No test grades it.
- **`issue-791-comment`** (mechanical, a tracker action). When the change lands, a comment on #791 records that its acceptance line "account labels only, no identifiers" no longer describes the wire. The issue's text is not edited. The check is reading #791's comments.
- **`derivation-is-one-way-and-named`** (judgment; live only under the derived form). That the code which makes the identifier cannot turn it back into the uuid, and that the derivation is named and documented in the code, are not decided by any committed check. They go to the independent fresh-context review.
- **The positional `.target(` matches** in the Swift sources and tests are the output of `grep -rn '\.target(' apps/menubar/Sources apps/menubar/Tests`. A pattern of the wrong arity fails the Swift build, and an equality against a frame that carries the identifier fails the test run.
- **The truth of each sentence in Rule 6** (judgment). No committed check decides whether a sentence is still true. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` decides only that intra-doc links resolve, and `scripts/check-doc-citation-rot.sh` decides only that a cited symbol sits at its cited lines. A green doc build is not evidence about whether a statement is corrected, unaffected or frozen.
- **The pre-push gates** are the CI jobs the item names: `test`, `msrv`, `deny` and `swift`, and an armed `panel-goldens` run that prints its `[panel-goldens] max drift` line. They are mechanical and already exist, so this file adds no scenario for them.
