# Feature: older usage samples are attributed only where one account bore their label, once, and never at the cost of a sample

Issue #1572 · PRD § 10.4 REQ-USG-C-001, REQ-USG-C-002, AC-USG-001 and § 10.11 REQ-NTC-C-001 · design D-7, D-8, § 11.3, § 11.5, § 14.6 item 11

Example Mapping: 🟦 6 rules · 🟩 17 examples · 🟥 3 open

Tier B — scenarios pending binding in #1572's execution. The scenarios marked RED-owed are the ones a pre-authored grader would cover. That grader is not filed yet, because the item's first criterion bars writing any code before the reading of "re-keyed" is recorded (Red card 1). See "Oracle — deferred" at the end.

> **PROVISIONAL — stated once, here.** `docs/requirements/account-label-integrity.md` carries `dor_status: failed`: every requirement in it is pipeline-authored and unratified, and its problem framing is unvalidated. The operator has ratified one thing this file rests on, the class of work: R-6 in `docs/requirements/migration-credential-portability.md` (duplicate labels are an accepted state). Everything below holds **under the migration reading** of "re-keyed" (Red card 1) and is written under four defaults nobody has ruled on:
>
> - A pass that attributes nothing still writes its sentinel. The item's own body records this as the pipeline's reading.
> - The sentinel's name and its place beside the samples file are pipeline defaults, so no scenario names the file: each says "a file beside the samples file".
> - No reader consumes the stored key (design D-8; open decision `stats-consumes-the-key`, PRD DQ-3). That bounds what #1572 changes and decides no value below: the item that first makes a reader consume the key amends Rule 6 when it lands.
> - A stored label matches a roster label by exact string equality, the rule the tree applies today.
>   The counting helper is `label_bearers`, `src/cli.rs:5245`. Red card 3 asks whether that should change.

> **Story.** #1562 makes every new usage sample name the account that was polled. The samples written before it still carry only a label. Where one roster entry bears that label today, the pass can say whose they are. Where several do, or none, it cannot, and it must not guess. It runs once, at daemon start, and it can only ever touch the raw window, because the hourly and daily aggregates hold no account.
> The raw window's default length is `DEFAULT_RAW_WINDOW_SECS`, `src/usage_store.rs:101`.
>
> The trap in this feature is that three plausible implementations each pass one rule and fail another. Giving a shared label to its first bearer passes the `work` example and fails `team` (Rule 1). Rewriting the samples file whenever the pass runs, rather than only when it changed a sample, passes every attribution example and turns a file of one torn line into an empty one, which flips the prior-configuration witness (Rule 2). Writing the sentinel only after attributing something passes every single-run example and lets a later start match a later roster against samples an older binary wrote after a downgrade (Rule 3).
>
> **Six rules.** The `/scope` Blue Card gate reads more than five rules as a sign the item is too big. These six are facets of one safety unit: the attribution is unsafe without the sentinel and the failure rules. This file formulates the item whole and leaves any split to `/decompose`.

## Rule 1 — a keyless sample is attributed only to the sole bearer of its label, and all the pass writes is that bearer's uuid

```gherkin
Scenario: one-bearer labels are attributed, shared and unclaimed labels are kept
  # AC: oracle-one-bearer-labels-are-attributed-the-rest-are-kept · REQ-USG-C-001 · AC-USG-001 · RED-owed
  # Binds: the_backfill_attributes_a_sample_only_where_its_label_has_exactly_one_bearer (suggested name, from the item)
  Given a roster of three entries
      | uuid | label |
      | u-A  | work  |
      | u-B  | team  |
      | u-C  | team  |
    And a raw tier of five keyless samples
      | label      | session |
      | work       | 0.11    |
      | work       | 0.12    |
      | team       | 0.21    |
      | team       | 0.22    |
      | old-laptop | 0.31    |
   When the pass runs
   Then the samples reading 0.11 and 0.12 name "u-A"
    And the samples reading 0.21, 0.22 and 0.31 are keyless and are what they were
    # AC-USG-001's roster, as the PRD and the item now word it: "work" borne by A alone, "team" by B and C (an entry
    # bears one label, so a shared label needs two other entries). This roster is that example, and any grader for
    # it has to use it.
    # "work" has one bearer, "team" two, "old-laptop" none: the three outcomes REQ-USG-C-001 names.
    # Giving "team" to its first bearer, its last, or the lowest uuid passes the "work" rows and fails the "team" rows.

Scenario: a sample that already names an account is never rewritten to another
  # AC: oracle-one-bearer-labels-are-attributed-the-rest-are-kept ("samples that already carry a uuid") · Invariant "a relabel never changes the uuid recorded on any sample" · RED-owed
  # Binds: a_sample_that_already_names_an_account_keeps_it (suggested name)
  Given a roster of one entry, uuid "u-A", labelled "work"
    And one sample under "work" that names "u-B"
   When the pass runs
   Then that sample still names "u-B"
    # "u-B" bore "work" when it wrote the sample and was relabelled since, so "u-A" holds the label now. A pass that sets
    # the uuid on every sample whose label has one bearer, and forgets to ask whether the sample is keyless, overwrites it.
    # The pass counts bearers in the roster. It never reads a stored key to decide anything.
    # The limit of the rule is not exercised: no scenario asserts an outcome for a KEYLESS sample whose label changed hands
    # inside the window. The pass cannot see the handover, and the item says not to try to be cleverer about it.

Scenario Outline: a parked entry is a bearer like any other
  # AC: Terms, "Every roster entry counts, parked or not" · REQ-USG-C-001 · RED-owed
  # Binds: a_parked_entry_counts_as_a_bearer_of_its_label (suggested name)
  Given a roster in which <label> is borne by <bearers>
    And one keyless sample under <label>
   When the pass runs
   Then the sample <outcome>

  Examples:
    | label | bearers                            | outcome       |
    | spare | "u-A" in rotation and "u-C" parked | stays keyless |
    | bench | "u-D" alone, parked                | names "u-D"   |
    # A parked entry is one with `enabled = false`: it stays in the roster and is not polled.
    # The flag is `enabled`, `src/config.rs:381`.
    # The daemon holds every entry, parked ones included, so the pass sees them all.
    # The field is `roster`, `src/daemon.rs:2457`.
    # Skipping parked entries when counting would give "spare" one bearer and attribute its samples to "u-A" alone.

Scenario: the pass adds a uuid and nothing else
  # AC: no-email-no-token-no-digest · Invariant "the pass attributes a sample to an account only through that account's uuid" · REQ-NTC-C-001 · not RED-owed
  # Binds: the_pass_adds_only_the_roster_uuid_and_the_file_passes_the_redaction_meter (suggested name)
  Given the store of the first scenario, seeded through the store's own writer
   When the pass runs
   Then each attributed sample's line is its seed line plus one "account_uuid" key and nothing else
    And the issue #15 redaction meter finds nothing in the rewritten file
    # The value comes from the roster entry. Config validation limits a uuid to ASCII letters, digits, hyphen and underscore,
    # under a length cap (`account_uuid_violation`, `src/config/validate.rs:525`), so it cannot break a JSON line.
    # The label is already in the line as `acct` and the pass does not touch it.
    # The store's existing redaction test is `persisted_store_carries_no_unauthored_email_or_token`, `src/usage_store.rs:1211`.
```

## Rule 2 — nothing is dropped, nothing is created, and the witness reads what it read

```gherkin
Scenario: every sample that parsed before the pass parses after it, in the same order
  # AC: no-sample-dropped-no-file-created · REQ-USG-C-001 ("keeping every existing sample") · RED-owed
  # Binds: the_pass_keeps_every_parseable_sample_in_order (suggested name)
  Given a raw tier of eight samples in time order, under labels with one bearer, with several and with none
    And two of the eight already name an account
    And a torn final line
   When the pass runs
   Then the eight samples read back in the same order
    And every field of each, other than an added uuid, is what it was
    # The rewrite goes through `write_samples`, `src/usage_store.rs:622`, which writes the samples it is given and nothing else.
    # A torn line is not a sample, so the scenario says nothing about it. The rewrite does not carry it, as the rewrite in `compact_and_roll` does not, `src/usage_store.rs:524`.
    # Seed through `append_sample`, `src/usage_store.rs:431`. An unchanged line is then byte-identical after the rewrite,
    # whereas a hand-written line can differ in key order or float spelling without having changed.
    # "In order" is file order, which is time order here. The compaction sorts what it keeps, in `plan_roll`, `src/usage_store.rs:557`. The pass has no reason to.

Scenario: no samples file is created where none stood
  # AC: no-sample-dropped-no-file-created · RED-owed
  # Binds: the_pass_creates_no_samples_file (suggested name)
  Given a support directory with no samples file
    And a roster of one entry
   When the pass runs
   Then there is still no samples file
    And a file beside where the samples file would be marks the pass done
    # Taking the lock creates `usage.lock` if it is absent, `acquire`, `src/usage_store.rs:376`. The directory gains that file and the sentinel.
    # The samples file is the one the pass must not create.
    # `append_sample` creates it when it appends, via `create_private_file`, `src/usage_store.rs:435`. The pass has no append.

Scenario Outline: the prior-configuration witness reads the usage store as it did before the pass
  # AC: witness-reads-the-same · REQ-USG-C-002 · ADR-0036 (docs/adr/0036-prior-configuration-witness.md) · not RED-owed
  # Binds: the_witness_reads_the_usage_store_the_same_before_and_after_the_pass (suggested name, beside the existing pin)
  Given a samples file that <file>
   When the witness is asked about the usage store before the pass and again after it
   Then both answers are <answer>
    And the file after the pass <after>

  Examples:
    | file                                  | answer | after                              |
    | holds samples the pass attributes     | yes    | holds the same samples, attributed |
    | holds only a line that does not parse | yes    | is byte-for-byte what it was       |
    | is empty                              | no     | is empty                           |
    | is absent                             | no     | is still absent                    |
    # The second row is the one a careless rewrite breaks. With nothing parseable there is nothing to attribute, and a rewrite
    # given no samples writes an empty file, `write_samples`, `src/usage_store.rs:622`. That would turn yes into no.
    # The question is `is_non_empty`, `src/witness.rs:215`, which reads metadata only.
    # The existing pin is `a_populated_usage_store_is_a_witness_and_an_empty_one_is_not`, `src/witness.rs:468`.
    # The witness's usage-store source names two files, the samples file and the rollup document, `usage_store`, `src/witness.rs:115`. The pass touches only the first.

Scenario: a pass that attributes nothing rewrites nothing and still writes its sentinel
  # AC: Specification, "A pass that finds no samples to attribute still reaches step 5" · witness-reads-the-same ("never rewrites a file that holds no parseable sample") · RED-owed
  # Binds: a_pass_that_attributes_nothing_leaves_the_file_alone_and_writes_the_sentinel (suggested name)
  Given a raw tier whose samples are all under "team", which two entries bear
   When the pass runs
   Then the samples file is byte-for-byte what it was
    And a file beside it marks the pass done
    # The rewrite is conditional on the pass having changed a sample (Specification step 4). The sentinel is not (step 5).
    # Marking the pass done here is the item's own default, recorded in the header. Writing the sentinel only after
    # attributing something would let a later start match a later roster against samples an older binary wrote after a downgrade (PM-21).
```

## Rule 3 — the pass runs once, and a sibling sentinel written last says so

```gherkin
Scenario: with the sentinel present the pass reads nothing and changes nothing
  # AC: runs-once-and-survives-a-crash ("with the sentinel present the pass reads nothing and changes nothing") · RED-owed
  # Binds: a_pass_with_the_sentinel_present_reads_and_changes_nothing (suggested name)
  Given a completed pass has written its sentinel
    And an older binary has since compacted the samples file and dropped the keys it did not know
    And a roster in which "work" has one bearer, "u-A"
    And the samples file holds keyless samples under "work"
   When the daemon starts
   Then the samples are still keyless
    # This is the downgrade the sentinel exists for. Those samples read as unattributed, never misattributed.
    # A second pass against whatever roster stands now could misattribute them (RK-17).
    # "Reads nothing" shows only through a samples file that cannot be read: with the sentinel present the pass logs no failure for it, because it never opens it.

Scenario: a relabel after the pass changes no sample
  # AC: Invariant, "a relabel never changes the uuid recorded on any sample" · RED-owed
  # Binds: a_relabel_after_the_pass_changes_no_stored_uuid (suggested name)
  Given a completed pass that named the "work" samples "u-A"
   When "u-A" is relabelled "home" and the daemon starts again
   Then the samples that name "u-A" still name "u-A"
    And they still sit under "work"
    # The sentinel stops the second start, and no start rewrites a stored label.
    # The label field is `acct`, `src/usage_store.rs:145`. It keeps the label the sample was written under.

Scenario: the sentinel is a file beside the samples file, and the rollup document holds no marker
  # AC: sentinel-is-a-sibling-file · RED-owed
  # Binds: the_sentinel_is_a_sibling_file_and_survives_a_rollup_rewrite (suggested name)
  Given a completed pass
   When the rollup document is rewritten without any key it does not know, as an older binary does
   Then the sentinel is intact
    And a further start attributes nothing
    And the rollup document holds the keys it held before the pass and no others
    # The document has three fields, the watermark and the two tiers, `Rollup`, `src/usage_store.rs:223`.
    # An older binary rewrites it with only the fields it knows, so a marker kept in it would be erased,
    # and a later upgrade would match a later roster against the same samples (PM-21). An older binary never touches a sibling file.
    # The witness does not name the sentinel among its sources, `usage_store`, `src/witness.rs:115`, so its presence cannot make an empty store read as a witness.
```

## Rule 4 — a pass that fails or is interrupted leaves the store safe, and the next start finishes the job

```gherkin
Scenario Outline: a failed pass leaves the samples as they were, and the next start finishes the job
  # AC: failure-leaves-samples-keyless-and-retries · design § 11.3 · RED-owed
  # Binds: a_failed_pass_leaves_the_samples_keyless_and_the_next_start_retries (suggested name)
  Given keyless samples under "work", which "u-A" alone bears
    And <fault>
   When the daemon starts
   Then the samples are keyless as they were
    And no sentinel is written
    And one line is logged, and it passes the issue #15 redaction meter
    And the daemon goes on to start
    And a later start with the fault gone names the "work" samples "u-A" and writes the sentinel

  Examples:
    | fault                                                                |
    | another writer holds the store lock for longer than the store's wait |
    | the atomic rewrite of the samples file fails                         |
    | the samples file cannot be read                                      |
    # The store's wait is `STORE_LOCK_MAX_WAIT`, `src/usage_store.rs:120`. The pass takes the lock `append_sample` and the compaction take, so it contends like them.
    # The third row is not in the item's list. It has the same shape: a read that fails must not spend the one run by reading as "nothing to attribute".
    # `read_samples` returns an error for anything but a missing file, `src/usage_store.rs:449`.
    # "Goes on to start" is design § 11.3: the backfill swallows and logs its own error. The run loop already starts the same way after a failed reconcile, `reconcile_on_start`, `src/daemon/run_loop.rs:606`.
    # A failure of the SENTINEL write after a successful rewrite is not a row. The samples are then already attributed, so "as they were" cannot hold. That is the interrupted case below.
    # The item's failure criterion now names only faults with seams (Red card 2 is settled: the pass takes the loaded roster by value).

Scenario: a pass interrupted after its rewrite and before its sentinel finishes at the next start
  # AC: runs-once-and-survives-a-crash ("A pass interrupted after the rewrite and before the sentinel runs again at the next start") · RED-owed
  # Binds: an_interrupted_pass_finishes_at_the_next_start_with_the_result_of_one_uninterrupted_pass (suggested name)
  Given a roster in which "work" has one bearer, "u-A", and "team" has two
    And a pass that was interrupted after it rewrote the samples file and before it wrote its sentinel
    And so the "work" samples name "u-A", the "team" samples are keyless, and there is no sentinel
   When the daemon starts again with the same roster
   Then no sample changes
    And a sentinel is written
    And the store is what one uninterrupted pass leaves
    # The rewrite is one atomic rename, `write_samples`, `src/usage_store.rs:622`. A crash leaves the file wholly as it was or wholly rewritten, never half attributed.
    # After it, "still-keyless" means the samples that could not be attributed, and an unchanged roster gives the same answer for them.
    # Under a CHANGED roster between the two starts, the second run matches the new roster. That is the limit of Rule 1 and it is not asserted.
    # The repo's precedent for interrupting a write is to split the work into a pure plan and its writes, then drive the writes by hand and stop at the boundary.
    # The test is `a_fault_at_every_write_boundary_loses_no_sample`, `src/usage_store.rs:1460`, over the plan in `plan_roll`, `src/usage_store.rs:557`.
```

## Rule 5 — the pass finishes before the first poll

```gherkin
Scenario: when the first poll is made, the older samples are already attributed
  # AC: runs-before-the-first-poll · RED-owed
  # Binds: the_pass_completes_before_the_first_poll (suggested name)
  Given a store wired to the daemon, holding keyless samples under "work", which "u-A" alone bears
   When the daemon starts and is about to make its first poll
   Then at that moment the "work" samples name "u-A"
    And the sentinel exists
    # Observed from inside a fake poller that reads the store when it is first asked to poll.
    # The final file cannot tell the two orders apart: a pass that ran after the first poll would see that poll's own, already keyed sample, leave it, and write the same bytes.
    # The start-up path is the run loop, `run_loop`, `src/daemon/run_loop.rs:583`: reconcile-on-start, the boot canary, the jittered delay, then the first tick.
    # The delay is `await_startup_delay`, `src/daemon/run_loop.rs:634`. The first poll is `tick`, `src/daemon/run_loop.rs:648`.
    # The item leaves the exact hook to its executor. A test that drives the run loop pins it inside that function, as reconcile-on-start is.

Scenario: a daemon with no store wired runs no pass
  # AC: runs-before-the-first-poll (the injected-path convention, issue #315) · passes today, must keep passing
  # Binds: a_daemon_with_no_usage_store_wired_writes_nothing_at_start (suggested name)
  Given a daemon built with no samples path
   When it starts
   Then no pass runs and no sentinel is written, whatever the roster holds
    # The store is wired only through `with_usage_samples`, `src/daemon.rs:2388`. Without it the collector writes nothing, `record_usage_sample`, `src/daemon.rs:679`.
    # The pass takes the same injected path. It never resolves the real one itself, `usage_samples`, `src/paths.rs:740`,
    # or every hermetic daemon test would write a sentinel into the developer's own support directory.
```

## Rule 6 — nothing that reads the store changes its answer

```gherkin
Scenario: stats and reliability report the same figures before and after the pass
  # AC: readers-unchanged · design D-8 · default "stats-consumes-the-key", see the header · not RED-owed
  # Binds: stats_and_reliability_read_the_store_the_same_before_and_after_the_pass (suggested name)
  Given a seeded store with samples under a one-bearer label and under a shared label
   When stats and reliability read it, then the pass runs, then they read it again
   Then each prints the same output before and after
    And the shared label's accounts are still one series under that label
    # `stats` groups by the stored label, `by_acct`, `src/usage_stats.rs:527`.
    # `reliability` reads the same samples, `read_samples`, `src/reliability.rs:376`, and matches them to swap events on the label, `acct`, `src/reliability.rs:2082`.
    # The pass changes no `acct`, so no figure moves.
    # This pins that the pass moves no figure. It does not say that continuity across a relabel, or separate rows for same-label accounts, are wrong outcomes.
    # Those are the open decision, and whichever item delivers one amends this scenario.

Scenario: the stats wire contract and its version do not move
  # AC: readers-unchanged · REQ-USG-C-003 · AC-USG-002, "BUT NOT a version bump when the shape did not change" · not RED-owed
  # Binds: the_committed_stats_wire_golden_still_matches_the_socket_encoder (existing, unchanged), and a diff check on apps/menubar/ and build/fixtures/
  Given the committed stats wire golden and stats' own schema version
   When this item's change is applied
   Then the golden still matches what the encoder emits, byte for byte
    And the schema version is unchanged and the Swift mirror is not edited
    And nothing under apps/menubar/ or build/fixtures/ changes, so the swift job is not owed
    # The pin is `the_committed_stats_wire_golden_still_matches_the_socket_encoder`, `src/stats.rs:11010`.
    # The version is `JSON_SCHEMA_VERSION`, `src/stats.rs:80`.
```

## Open — three Red cards, one of them settled

🟥 **`backfill-reading-of-ac-usg-001`** — the item's gate. Nothing above is built until the operator answers it.

- *Question.* Does "re-keyed" in AC-USG-001 mean the migration moment, which owes this pass, or the moment a reader switches to the key, under which #1572 is closed as not planned?
- *Worked example.* The operator upgrades to the build that records the uuid. Under the migration reading, the first start afterwards rewrites the samples file once: the "work" samples gain the uuid of the one entry that bears "work", and the "team" samples stay as they are. Under the turnover reading nothing is rewritten. New samples carry the uuid, and the older ones age out of the raw window, `DEFAULT_RAW_WINDOW_SECS`, `src/usage_store.rs:101`. Either way nothing reads the uuid unless `stats-consumes-the-key` is answered yes inside that window (design D-8).
- *Costs.* The migration reading spends a one-way rewrite of a file the witness reads (RK-11, RK-17) on attributions nothing reads. The turnover reading leaves older samples unattributed until they age out, which is cheap, and AC-USG-001's "kept under A's identifier" then needs rewording in the PRD.
- *Recommendation* (design § 14.9.1, non-blocking concern 1). Answer `stats-consumes-the-key` first, and build the pass only if a reader of the key lands in the same window.
- *If unanswered.* #1572 stays unstarted as the last F4 item, and no oracle is filed.
- *Blocks.* The whole item. No scenario's outcome depends on the answer.

**`roster-fault-has-no-referent`** — settled by the scoping run as `backfill-takes-the-loaded-roster` (#1572, its Failure paragraph and its `failure-leaves-samples-keyless-and-retries` criterion): option (a) below. Kept for the record. The item's first failure, "a pass that cannot read the roster", named a fault this design cannot produce.

- *Worked example.* The roster reaches the daemon through `Config::load()?` (`Config`, `src/cli.rs:1444`) and `config.require_roster()?` (`require_roster`, `src/cli.rs:1449`). If either fails, the daemon ends before the run loop, so no pass runs and no sentinel is written. A pass handed the loaded roster by value has no roster it cannot read.
- *Options.* (a) The pass takes the loaded roster by value, the fault is the daemon's existing refusal to start, and the fault row for it is dropped from the item's criterion. (b) The pass reads `config.toml` itself, which makes the fault real but lets it match a roster other than the one the daemon polls.
- *Recommendation.* (a), with the criterion reworded to say so.
- *Settled.* (a). No scenario covers a roster fault, Rule 4's examples carry no roster-fault row, and a pre-authored grader cannot reach it either way.

🟥 **`label-normalization-policy`, as it reaches this pass** — what counts as "the same label" when the pass counts bearers.

- *Worked example.* The roster holds A labelled "Work" and B labelled "work", and samples sit under "Work". By exact comparison, which is what the tree does today, "Work" has one bearer and its samples go to A. If the operator rules that case does not distinguish labels, "Work" and "work" are one label with two bearers and the same samples stay unattributed.
- *Options.* Exact equality (today's rule), or whatever comparison key design D-1 gives the roster once `label-normalization-policy` is decided.
- *Recommendation.* Exact equality for this item. A stored string equals the label of the entry that wrote it exactly, so exact equality is the stricter test of provenance. The key-following pass is the cautious one, and the label-normalization item owns revisiting this pass if it adds a key.
- *If unanswered.* Exact equality. No scenario depends on it, because none uses labels that differ only in case or spacing.
- *Blocks.* Nothing today.

## Oracle — deferred until the reading is recorded

No oracle item is filed yet. The item's first criterion bars writing any code before the reading of "re-keyed" is recorded on #1572, and a pre-authored test is code. Under the turnover reading #1572 is closed as not planned and the oracle would grade a function nobody builds. Once the migration reading is recorded and #1562 has landed, file one oracle item on the pattern of #1451, for the scenarios marked RED-owed. What its filer needs, worked out here so it is not re-derived:

- **Seam.** The pass function does not exist, so a test that names it cannot compile on today's tree, and a test that cannot compile cannot land even `#[ignore]`d. Assert one layer above it, at the daemon's start-up path: drive the run loop over a seeded temporary store, with the hermetic harness that `run_loop(`, `src/daemon.rs:18248`, already uses. Read the store back as raw JSON and list the directory. That is the move #1562's body records for its own grader, one layer above `append_sample_for_poll`. It pins the hook inside the run loop, beside `reconcile_on_start`, and the oracle must say so. A successful first poll appends a sample of its own, so make the fake poll fail, or compare only the seeded lines, before asserting that a file is unchanged.
- **The sentinel's name is a pipeline default.** The oracle finds it by listing the directory before and after: it is the one new regular file other than `usage.lock` (untried).
- **Faults without hooks.** Hold `usage.lock` past the store's wait. Make the rewrite fail with an unwritable directory and `usage.lock` already present, as a non-root user (untried). Make the samples file unreadable. For the interrupted case, seed the state "rewritten, no sentinel". The unreadable roster has no seam until Red card 2 is answered.
- **Landing rule.** A RED oracle lands `#[ignore = "RED oracle: pre-authored ahead of its fix (issue #1572), which un-ignores it"]`, because `ci-ok` cannot merge a failing suite. It keeps at least one non-ignored companion test on the same fixture; the no-store-wired scenario, and the witness outline's rows for a file with nothing parseable to attribute, pass on today's tree and serve. Its executor posts the verbatim RED output on the oracle item. It proves it discriminates by applying the hypothesized fix in a scratch copy (all GREEN) and mutating each guarded invariant (each RED), never by trusting a first red.

## Aspects that are not scenarios

- **`gate-reading-recorded`** (a recorded decision; no seam). The reading of "re-keyed" is recorded on #1572 before any code is written, and under the turnover reading #1572 is closed as not planned. That is Red card 1. Nothing mechanical decides it.
- **The limit of the rule.** The pass attributes a sample to the present sole bearer of its label. A label handed from one account to another inside the raw window may leave a sample with the earlier holder, and the rule cannot see that. No scenario asserts an outcome for it, and the item says to hold the line and not try to be cleverer.
- **The pre-push gates** are the repo's CI jobs the item names: `test`, `msrv` and `deny`. The `swift` job is not owed, because nothing under `apps/menubar/` or `build/fixtures/` changes. They are mechanical and already exist, so this file adds no scenario for them.
