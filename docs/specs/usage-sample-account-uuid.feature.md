# Feature: a usage sample names the account it came from, and nothing that reads the store moves

Issue #1562 · PRD § 10.4 REQ-USG-B-001, REQ-USG-C-002, REQ-USG-C-003 and § 10.11 REQ-NTC-C-001 · design D-5, D-7, D-8, § 11.5

Example Mapping: 🟦 5 rules · 🟩 12 examples · 🟥 1 open

Tier B — scenarios pending binding in #1562's execution. The scenarios marked RED-owed (in Rules 1 and 3) have their grader pre-authored by the oracle item #1580, which lands first. Four of the other scenarios need the new field before they compile — «a sample with a uuid writes exactly one more key and reads back whole», «a line written before this item reads as it does today, and a torn line is still skipped», and the two redaction-meter scenarios — so they are written with the change; the rest pass on today's tree. All of them are bound by #1562's executor.

> **PROVISIONAL — stated once, here.** `docs/requirements/account-label-integrity.md` carries `dor_status: failed`: every requirement in it is pipeline-authored and unratified, and its problem framing is unvalidated. The operator has ratified one thing this file rests on, the class of work: R-6 in `docs/requirements/migration-credential-portability.md` (duplicate labels are an accepted state). Everything below is a pipeline draft, written under two defaults nobody has ruled on:
>
> - The stored key is named `account_uuid`, holds the uuid verbatim and is left out of a line that has none (design D-5, D-7). If the operator later rules for another name or form, lines already written keep this one until they age out of the raw window.
> - No reader consumes the key in this item (design D-8; open decision `stats-consumes-the-key`, PRD DQ-3). That bounds what #1562 changes. It decides no value below, because the item that first makes a reader consume the key amends Rule 4 when it lands.
>
> `account-key-form` (PRD DQ-1) governs the status and watch wire only (design D-5), so it does not touch the stored value here.

> **Story.** The operator holds two accounts under one label, which R-6 allows. The store keys each sample on the label, so it cannot say which account a reading belongs to, and a relabel starts a new series. The daemon already holds each polled entry's uuid, which is unique per entry and survives a relabel. This item records it on every new sample.
>
> The trap in this feature is that three plausible fixes each pass one scenario and fail another. Looking the uuid up by label at write time passes every one-entry example and gives both same-label accounts the first bearer's uuid (Rule 1, first scenario). Recording the uuid only where a label has more than one bearer passes the same-label example and records nothing for a one-entry roster (Rule 1, second scenario). Rebuilding the retained samples during compaction without the key passes every read-side check and loses the key on the first compaction (Rule 3).

## Rule 1 — a recorded sample names the entry that was polled, beside its label

```gherkin
Scenario: two accounts that share a label leave samples that name each account
  # AC: oracle-same-label-accounts-leave-distinct-samples · REQ-USG-B-001 · RED-owed
  # Binds: two_accounts_sharing_a_label_leave_samples_that_name_each_account (oracle #1580, daemon tests)
  Given a roster of two entries, uuid "u-A" and uuid "u-B", both labelled "work"
    And the poll for "u-A" reads a session of 0.11 and the poll for "u-B" reads 0.22
   When the daemon has polled each entry successfully
   Then each stored sample names the entry whose poll produced its reading
    And the 0.11 sample names "u-A" and the 0.22 sample names "u-B"
    And every stored sample keeps "work" as its label
    And both uuids appear in the store
    # The two readings differ so the assertion ties each uuid to the poll that produced it, in whatever
    # order the lines land. The label cannot do that: it is "work" on both lines, which is the defect.
    # The Given is a roster the ratified R-6 accepts. `duplicate-label-policy` decides whether a WRITE may
    # newly create one, not whether this state is legal, so it does not move this example.
    # The write is one call, `record_usage_sample`, `src/daemon.rs:2893`, and it hands over the label alone today.

Scenario: a relabel changes the label on later samples and never the uuid
  # AC: oracle-a-relabel-keeps-the-key and the invariant "a relabel never changes the uuid recorded on any sample" · REQ-USG-B-001 · RED-owed
  # Binds: a_relabel_keeps_the_account_uuid_on_every_sample (oracle #1580, daemon tests)
  Given a roster of one entry, uuid "u-A", labelled "work"
    And the daemon has polled it and stored a sample
   When the entry is relabelled "home"
    And the daemon polls it again
   Then the first stored sample names uuid "u-A" under the label "work"
    And the second stored sample names uuid "u-A" under the label "home"
    And the first sample's line is byte-for-byte what it was before the relabel
    # The label is unique before and after, so this example also pins that the uuid is recorded for an
    # unambiguous label: a fix that records it only where a label has more than one bearer fails here and
    # passes the first example.

Scenario: a failed poll records nothing
  # AC: Implementation Notes, "A poll that fails records nothing today. Keep that" · passes today
  # Binds: collector_records_nothing_for_a_gap (existing, re-pointed to the new signature, never weakened)
  Given a roster entry whose poll fails
   When the daemon polls it
   Then the store gains no sample for it
    # Gap honesty: a missing reading reads as unknown, never as a fabricated zero. Pinned today where the
    # sample is built, `append_sample_for_poll`, `src/daemon.rs:718`.
    # The pin itself is `collector_records_nothing_for_a_gap`, `src/daemon.rs:20013`.
```

## Rule 2 — a sample with no uuid is still a valid sample, written and read

```gherkin
Scenario: a sample with no uuid writes no uuid key
  # AC: bare-sample-omits-the-key · passes today
  # Binds: sample_round_trips_with_and_without_optionals (existing, extended: "account_uuid" joins its list of absent keys)
  Given a sample built with no uuid
   When it is written as a store line
   Then the line has no "account_uuid" key
    And the line reads back as the same sample, with no uuid
    # Left out, not written as null or as an empty string: the four existing optional fields already work
    # this way, `skip_serializing_if`, `src/usage_store.rs:152`.
    # The round-trip test is `sample_round_trips_with_and_without_optionals`, `src/usage_store.rs:870`.

Scenario: a sample with a uuid writes exactly one more key and reads back whole
  # AC: Specification "The field" · REQ-USG-B-001 · needs the new builder, so it cannot be written before the fix
  # Binds: a_keyed_sample_serializes_exactly_the_intended_keys_plus_account_uuid (new, beside the existing key-set pin)
  Given a sample carrying uuid "u-A" and every optional field
   When it is written as a store line
   Then the line's keys are exactly the sample's existing keys plus "account_uuid"
    And the line reads back as the same sample
    # The key-set pin is the store's structural redaction proof, "there is no field that could carry an
    # email or token": `sample_serializes_exactly_the_intended_keys`, `src/usage_store.rs:1305`. Without a
    # keyed case it stops covering the new key.

Scenario: a line written before this item reads as it does today, and a torn line is still skipped
  # AC: old-lines-still-read · written with the change: the binding test needs the new field to compile (see the header)
  # Binds: a_store_line_written_before_the_account_uuid_still_reads (new, usage_store tests)
  Given a samples file holding a line with a uuid, a line with none, and a torn final line
   When the store is read
   Then the line with a uuid is returned with that uuid
    And the line with none is returned with no uuid
    And the torn line is skipped
    # `read_samples` parses each record on its own bytes and skips any that fail, `src/usage_store.rs:449`.
    # An old line carries no key, so it must parse, not read as torn.
```

## Rule 3 — compaction carries what a sample carries, and leaves the aggregates alone

```gherkin
Scenario: compaction keeps each retained sample's uuid and leaves the aggregates as they were
  # AC: compaction-carries-the-key · REQ-USG-B-001 · RED-owed
  # Binds: compaction_carries_the_account_uuid_on_every_retained_sample (oracle #1580, usage_store tests)
  Given a raw tier holding five samples
      | age           | label | uuid   |
      | about 30 days | work  | u-A    |
      | about 30 days | work  | (none) |
      | 2 days        | work  | u-A    |
      | 2 days        | work  | u-B    |
      | 2 days        | spare | (none) |
    And a twin store holding the same five samples with no uuid on any
   When both stores are compacted at the same instant
   Then the three samples inside the raw window are retained
    And the two that carried a uuid still carry it, unchanged
    And the one that carried none still carries none
    And the two older samples are folded into the aggregates and leave the raw tier
    And the keyed store's aggregates are identical to its twin's
    # `compact_and_roll`, `src/usage_store.rs:524`, reads the raw tier into samples and rewrites the retained
    # ones through `write_samples`, `src/usage_store.rs:622`, so a key survives only if the sample type carries it.
    # The raw window is `DEFAULT_RAW_WINDOW_SECS`, `src/usage_store.rs:101`.
    # The aggregates, `Rollup`, `src/usage_store.rs:223`, hold no account and must not start to.
    # A keyless sample stays keyless. Attributing one is the backfill's job, issue #1572, which is gated on the PRD.
```

## Rule 4 — nothing that reads the store changes its answer

```gherkin
Scenario: stats and reliability report the same figures for a keyed store and its keyless twin
  # AC: readers-unchanged · design D-8 · passes today · default "stats-consumes-the-key", see the header
  # Binds: stats_and_reliability_read_a_keyed_store_as_its_keyless_twin (new; the HistoryStore fake for stats, the sample-taking core for reliability)
  Given two stores holding the same readings and labels, one with a uuid on every sample and one with none
    And the readings include two accounts that share a label, one of them relabelled between two of its readings
   When stats reads each store, as text and as JSON
    And reliability reads each store
   Then the outputs are identical for the keyed store and its twin
    And the two same-label accounts are still one series under their label
    # Both readers group by the label, `by_acct`, `src/usage_stats.rs:527`.
    # This pins that adding the key moves no figure. It does not say that continuity across a relabel, or
    # separate rows for same-label accounts, are wrong outcomes: they are the open decision, and whichever
    # item delivers one amends this scenario.

Scenario: the stats wire contract and its version do not move
  # AC: readers-unchanged · REQ-USG-C-003 · AC-USG-002, "BUT NOT a version bump when the shape did not change" · passes today
  # Binds: the_committed_stats_wire_golden_still_matches_the_socket_encoder (existing, unchanged) and a diff check on apps/menubar/ and build/fixtures/
  Given the committed stats wire golden and stats' own schema version
   When this item's change is applied
   Then the golden still matches what the encoder emits, byte for byte
    And the schema version is unchanged and the Swift mirror is not edited
    And nothing under apps/menubar/ or build/fixtures/ changes
    # The pin is `the_committed_stats_wire_golden_still_matches_the_socket_encoder`, `src/stats.rs:11010`.
    # The version is `JSON_SCHEMA_VERSION`, `src/stats.rs:80`.
    # The golden encodes a fixed report and reads no store, so it guards the SHAPE only. The scenario above
    # guards the VALUES.

Scenario Outline: the prior-configuration witness reads the usage store as it did
  # AC: witness-reads-the-same · REQ-USG-C-002 · ADR-0036 (docs/adr/0036-prior-configuration-witness.md) · passes today
  # Binds: a_keyed_usage_store_is_a_witness_like_a_keyless_one (new, beside the existing pin)
  Given a usage samples file that is <file>
   When the witness asks whether the usage store shows that a roster once existed
   Then the answer is <answer>

  Examples:
    | file                           | answer |
    | a file holding keyed samples   | yes    |
    | a file holding keyless samples | yes    |
    | an empty file                  | no     |
    | absent                         | no     |
    # ADR-0036 treats a non-empty store as evidence that a roster existed.
    # The question is `is_non_empty`, `src/witness.rs:215`, and it reads metadata only, so the key cannot change the answer.
    # This pins that reliance, so a later change that reads the contents has to meet it.
    # The existing pin is `a_populated_usage_store_is_a_witness_and_an_empty_one_is_not`, `src/witness.rs:468`.
```

## Rule 5 — the stored uuid is not a secret, and the meter can see it

```gherkin
Scenario: a serialized sample carrying a hyphenated uuid passes the issue #15 redaction meter
  # AC: no-email-no-token-no-digest · REQ-NTC-C-001 · passes once the field exists
  # Binds: a_keyed_sample_passes_the_redaction_meter (new, usage_store tests, beside the existing store redaction test)
  Given a sample carrying the synthetic uuid "7d3e0f1a-2b4c-4d5e-8f60-718293a4b5c6"
   When the redaction meter scans its serialized line
   Then it reports no finding
    # The entropy backstop flags an alphanumeric run of at least `ENTROPY_MIN_RUN`, `src/redaction.rs:177`,
    # at or above `ENTROPY_MIN_BITS`, `src/redaction.rs:179`. This uuid's longest run is 12 characters.
    # An unhyphenated uuid, which `account_uuid_violation` accepts, `src/config/validate.rs:525`, would be a
    # candidate (PRD assumption A-6). The meter is a test-time instrument, not a runtime gate.
    # The existing store redaction test is `persisted_store_carries_no_unauthored_email_or_token`, `src/usage_store.rs:1211`.

Scenario: a credential digest in the uuid's position would be flagged
  # AC: no-email-no-token-no-digest, the control that shows the meter can see the field · passes once the field exists
  # Binds: a_credential_digest_in_the_uuid_position_is_flagged_by_the_meter (new, usage_store tests)
  Given the same sample with the sha256 of the meter fixture's credential blob in place of the uuid
   When the redaction meter scans its serialized line
   Then it reports a finding
    # Without this control the previous scenario would pass over a field the meter might not read.
    # The meter's fixture is `meter_fixture`, `src/redaction.rs:122`.
```

## Open — one Red card

🟥 `empty-uuid-records-keyless-or-nothing`. #1562's Implementation Notes say "do not record a sample with an empty uuid" and do not say what the daemon does instead: record the sample without a uuid, or record nothing. No scenario is written, because the two outcomes differ and nothing in the PRD, the design or the code picks one. `Config::validate` rejects an empty uuid, `is_empty` at `src/config/validate.rs:297`, so a roster loaded from the config cannot hold one. Whether any other path can build one was not traced. Neither the oracle nor the scenarios above depend on it.

## Aspects that are not scenarios

- **`module-docs-reconciled`** (judgment). No committed check decides whether a sentence is still true. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` decides only that intra-doc links resolve. The doc sentences the item's own locator finds describe `acct` as the account's roster label, and each would need to say the uuid sits beside it: the module's redaction paragraph, `acct` at `src/usage_store.rs:61`; the doc on `Sample`, `acct` at `src/usage_store.rs:128`; the field's doc, `acct` at `src/usage_store.rs:141-145`; and the collector's doc, `acct` at `src/daemon.rs:705`. Whether each is reconciled, or still true as it stands, goes to the independent fresh-context review (design § 11.5, gate G-REVIEW), which reads every sentence the locator finds. A green `cargo doc` is not evidence about it.
- **The pre-push gates** are the repo's CI jobs named in the issue (`test`, `msrv`, `deny`). They are mechanical and already exist, so this file adds no scenario for them.
