# Feature: the sample store writes a window the account lacks as `null`, and every reader copes without a fabricated statistic

Issue #1598 · PRD REQ-SMP-B-001 / REQ-SMP-B-002 / REQ-SMP-C-001 / REQ-SMP-Q-001 (§ 10.7; D1 and D6 in § 8; § 11.4) · design § 5 B-7, § 4.3, § 4.4, § 11 Cap-4, § 12 AD-10 / AD-11 / AD-12, § 14 D12 / D13

Example Mapping: 🟦 5 rules · 🟩 18 examples · 🟥 0 open

> Since 2026-10-30 a verified Ultra account's usage reply carries `"five_hour":null` beside a present
> weekly window. Once the parser (#1596) and the reading model (#1597) serve such a reading with its
> session window `None`, the daemon's sample writer `append_sample_for_poll` (`src/daemon.rs:709-734`)
> must decide what it writes for that axis, and the store at `fd8640b` cannot write the truth:
> `Sample.session` is `f64` (`src/usage_store.rs:146`), a line carrying `"session":null` is skipped
> by `read_samples` (`src/usage_store.rs:447-465`) and erased by the next `compact_and_roll`
> (`read_samples` … `write_samples` at `src/usage_store.rs:533-542`), and an hour holding only such
> samples would fold `max_of(&[])` to `f64::NEG_INFINITY` (`src/usage_store.rs:805-807`), which
> `serde_json` 1.0.150 writes to the rollup as `null`, after which every `read_rollup`
> (`src/usage_store.rs:473-484`) hard-errors with `UsageRollupMalformed` and both compaction and
> `stats` fail until the file is repaired by hand. This item makes `Sample.session` and
> `Sample.weekly` `Option<f64>`, written as JSON `null` with the key always present, read and retained
> as such, and folded into per-axis `Option` statistics that are omitted when no reading in the
> bucket carried the axis. It lands in tranche 1's pull request right after the compile unit, by the
> scoping pipeline's choice (design § 4.4): `append_sample_for_poll` passes `reading.usage.session` into
> `Sample::new` at `src/daemon.rs:722-723` (`reading.usage.session`, `reading.usage.weekly`), so #1597's
> lift forces a choice at the writer; the compile unit makes it by keeping #1613's sample skip, re-keyed
> on either fraction being `None`, and this item replaces that skip with the `null` line. Its
> acceptance is graded at the store's own seams: `serde_json` on `Sample`, `compact_and_roll` over a
> temp store, `read_rollup`.
>
> The `# Tier` line under each scenario is this stub's binding line, in the form of
> `docs/specs/parser-optional-quota-windows.feature.md`. Tier A names a grader the executor may not
> edit and this item set already pins by name: a test #1598 § 6 (T1–T11) or AC-16 names verbatim, together
> with the RED #1598 records for it at `fd8640b` or the mutant record one of its acceptance criteria
> owes. None of #1591's four oracles and none of #1592's seven pins reaches the store, so no scenario
> here binds to them. Every "RED at `fd8640b`" below is read off the store's source at that revision,
> never off a run: the runs are AC-13's to carry in the pull request body. Tier B names what is still
> unbound and the item that should bind it.
>
> D6 (`null` on the versionless sample line), D12 (`Sample.weekly` optional), D13 (the per-axis
> `Option` tier representation), the omitted-key reader (design AD-10 against PRD § 11.4) and the
> serialization refusal of a non-finite `Some` are open, each built for its recorded default with the
> alternative stated in #1598; a scenario that depends on one says so in its name. Written headless at
> Stage 3.5 on 2026-11-01: every rule, tier and name here is ratification-pending.

## Rule 1 — a window the account lacks is written as JSON `null`: the key always present, never omitted, never `0`; a non-finite `Some` is refused at the only writer

```gherkin
Scenario: a weekly-only line re-encodes with its session key written as null (D6 default)
  # Tier A — graded by usage_store::tests::a_weekly_only_line_round_trips_with_the_session_key_written_as_null (#1598 T3; RED at fd8640b: the decode step is `Err`, `f64` refuses `null`; mutant, AC-1's record: `skip_serializing_if = "Option::is_none"` on `session` fails the key-set assertion) · covers AC-1, AC-3 (the key set)
  Given the raw line `{"ts":0,"provider":"claude","acct":"work","session":null,"weekly":0.1}`
   When it is decoded as a `Sample` and encoded again with `serde_json::to_string`
   Then the encoded text contains `"session":null`
    And the encoded object's keys, sorted, are exactly `["acct", "provider", "session", "ts", "weekly"]`
    And decoding the encoded text again equals the first decode
    But NOT `"session":0` or `"session":0.0` — byte-identical to an idle window the tree already holds
    But NOT the `session` key omitted — #1580's planned exact-key-set oracle (outside #1590, unlanded
        at `fd8640b`) REDs on omission, and `Sample`'s four optional siblings are the only fields that
        skip their key (`skip_serializing_if`, `src/usage_store.rs:149-163`)
    # No `skip_serializing_if` on either fraction field (#1598 Specification § 1). #1562 adds
    # `account_uuid` to `Sample` concurrently; whichever lands second rebases, and this item asserts
    # the five keys it knows.

Scenario: a successful poll of a weekly-only account appends one line carrying "session":null beside its weekly reading
  # Tier A — graded by daemon::tests::a_weekly_only_reading_is_sampled_with_its_session_written_as_null (#1598 T9, beside collector_writes_one_redacted_sample_per_successful_poll at `src/daemon.rs:19674`; no RED at fd8640b, where it does not compile (`Usage.session` is `f64`); RED at dispatch: against the post-compile-unit origin/main it compiles and fails the one-sample assertion, because the re-keyed #1613 skip writes no line, recorded per #1598 AC-13; mutant, AC-1's record: an early `return` from `append_sample_for_poll` on a `None` session reading writes no line and fails the one-sample assertion) · covers AC-1
  Given a served reading for the account labelled `work` whose session window is `None` and whose
        weekly window is `Some(0.90)` with a weekly reset stamp, severity `Some("critical")`
   When `append_sample_for_poll` runs for it at epoch 1_700_000_000
   Then exactly one line is appended, and it decodes with `ts` 1_700_000_000, `acct` `work`, a session
        reading of `None`, a weekly reading of `Some(0.90)`, the weekly reset stamp, no session reset
        stamp, severity `critical` and `spend` `None`
    And the line's text carries `"session":null` with the key present
    But NOT `"session":0.0`
    But NOT no line at all — gap honesty reserves "nothing" for a failed poll
        (`let Ok(reading) = polled else { return }`, `src/daemon.rs:715-717`), and a failed poll still
        writes nothing
    # The `Option` copies through from `reading.usage.session` at `src/daemon.rs:722-723` once
    # `Sample::new` takes `Option<f64>`; a served reading carries at least one window under D1's
    # default (a), so a line with both axes `null` never arises there.

Scenario: the #1613 sample skip is gone — a reading with no session window writes one line, not a gap
  # Tier A — graded by daemon::tests::collector_writes_a_null_session_line_for_a_reading_whose_session_window_was_absent (#1598 AC-16: #1613's collector_records_nothing_for_a_reading_whose_session_window_was_absent, rewritten and renamed; no RED at fd8640b, where neither #1613's test nor its skip exists; RED at dispatch: against the post-compile-unit origin/main the rewritten test compiles and fails its one-line assertion, because the skip re-keyed on an absent fraction writes no line, recorded per #1598 AC-13) · covers AC-16
  Given the reading #1613's test builds, with its session window `None`
   When `append_sample_for_poll` runs for it
   Then exactly one line is appended, and its text carries `"session":null` with the key present
    And `append_sample_for_poll` holds no early return keyed on an absent window — neither #1613's
        flag nor the compile unit's `None` re-key
    And `git grep -n -i -E '1613|hot-?fix' -- src` prints nothing
    But NOT the gap #1613 recorded on purpose — a stopgap until a `Sample` could say `null`
    # The pull request body names #1613, never a `src` comment, or AC-16's grep would print it. T9
    # grades the same line from a reading built beside collector_writes_one_redacted_sample_per_successful_poll;
    # this scenario grades that the hot-fix's skip and its text are gone.

Scenario Outline: a non-finite Some fraction is refused at serialization, so null on disk has one meaning (serialization-refusal default)
  # Tier A — graded by reliability::tests::the_sample_store_cannot_carry_a_non_finite_session_reading (#1598 T6, the near-end loop; RED at fd8640b: `serde_json::to_string` of such a `Sample` is `Ok` — the canary's own `.expect("a Sample always serializes")` — so the `is_err()` assertion FAILS, recorded per AC-13 in T6's first scratch run; mutant, AC-6's record: `serialize_with` dropped, so `Some(f64::INFINITY)` serializes as `"session":null` and the assertion fails) · covers AC-6 (the serialization arm)
  Given a `Sample` for `work` whose session reading is `Some(<value>)`, <shape>
   When it is serialized with `serde_json::to_string`
   Then the result is `Err`, carrying the hint `a usage value was not a finite number`
    But NOT `Ok` with `"session":null` — which would read back as "no window" for a measurement that
        was not finite

Examples:
  | shape               | value             |
  | a positive infinity | f64::INFINITY     |
  | a NaN               | f64::NAN          |
  | a negative infinity | f64::NEG_INFINITY |
  # The hint is `serialize_err()`'s (`src/usage_store.rs:839-841`), whose doc "reachable only for a
  # non-finite float" becomes true as written: `serde_json` 1.0.150's `serialize_f64` writes `null`
  # for `Nan | Infinite` rather than erroring, so the refusal has to live in the field serializer
  # `serialize_finite_fraction` (#1598 Specification § 1). The alternative the item states —
  # accept `null` for a non-finite `Some` and move the encode loop to assert `None` — makes `null` on
  # disk ambiguous; a ruling for it changes this scenario and nothing else.
```

## Rule 2 — a line carrying `null` on either axis, or omitting a fraction key, decodes with that axis `None`, never `0.0`; the non-finite spellings stay refused

```gherkin
Scenario: a weekly-only line decodes with its session reading absent
  # Tier A — graded by usage_store::tests::a_weekly_only_line_decodes_with_its_session_reading_absent (#1598 T1; RED at fd8640b: `serde_json::from_str::<Sample>` is `Err`, `f64` refuses `null`) · covers AC-2
  Given the raw line `{"ts":0,"provider":"claude","acct":"work","session":null,"weekly":0.1}`
   When it is decoded as a `Sample`
   Then the decode is `Ok`
    And the session reading, read as `Option<f64>`, is `None`
    And the weekly reading, read as `Option<f64>`, is `Some(0.1)`
    But NOT `Some(0.0)` for the session axis
    But NOT the line skipped — `read_samples` (`src/usage_store.rs:447-465`) keeps every record
        `from_slice` accepts and skips the rest in silence, which is how a healthy poll's line is lost
        today
    # The type-stable read is `optional_fraction(decoded.session)`, #1598 § 6's verbatim copy of
    # #1591's test helper: it compiles while the field is `f64` (`From<T> for Option<T>`) and once it
    # is `Option<f64>`, and passes `clippy -D warnings` on both sides, which is what lets T1–T4 and
    # T8a run against `origin/main` for their RED record. Never `Option::<f64>::from(decoded.session)`
    # or `decoded.session.into()` at the call site: after the lift each is an identity conversion,
    # which fails `clippy::useless_conversion` under `-D warnings`.

Scenario: a session-only line decodes with its weekly reading absent (D12 default)
  # Tier A — graded by usage_store::tests::a_session_only_line_decodes_with_its_weekly_reading_absent (#1598 T2; RED at fd8640b, the same refusal on the weekly axis) · covers AC-2
  Given the raw line `{"ts":0,"provider":"claude","acct":"work","session":0.42,"weekly":null}`
   When it is decoded as a `Sample`
   Then the decode is `Ok`
    And the session reading is `Some(0.42)` and the weekly reading is `None`
    But NOT `Some(0.0)` for the weekly axis
    # D12 (design AD-10, author-chosen): the weekly axis takes the same shape because a weekly-less
    # served reading — #1596's session-only case, Oracle D — leaves the writer the same three choices
    # on that axis. A ruling against D12 makes the session-only line unrepresentable: this scenario
    # and T2 are dropped and nothing else here changes.

Scenario: a line omitting the session key reads as no session reading (AD-10 default)
  # Tier A — graded by usage_store::tests::a_line_omitting_the_session_key_reads_as_no_session_reading (#1598 T4, written form; RED at fd8640b: a `missing field` error, the field has no default) · covers AC-8
  Given the raw line `{"ts":0,"provider":"claude","acct":"work","weekly":0.1}`
   When it is decoded as a `Sample`
   Then the decode is `Ok` with a session reading of `None` and a weekly reading of `Some(0.1)`
    But NOT `Err` — PRD § 11.4's row said "still rejected"; #1598 follows design AD-10's
        `#[serde(default)]`, recorded ratification-pending, and under a ruling of "reject" T4 asserts
        `is_err()` and a custom deserializer refuses the omitted key
    # Every writer in this crate serializes a `Sample`, so the key is always present; rejecting an
    # omitted key would drop a line in silence — the loss class this item ends — to defend against a
    # writer that does not exist. `serde`'s derive already treats a missing `Option` field as `None`
    # (`serde` 1.0.228, `private/de.rs` lines 22-23, at the registry path `Cargo.lock` pins); the
    # attribute is kept for symmetry with the four
    # optional siblings.

Scenario: the canary's null arm moves — "session":null decodes as no session reading, and the control line still decodes
  # Tier A — graded by reliability::tests::the_sample_store_cannot_carry_a_non_finite_session_reading (#1598 T6, the moved `"null"` arm; RED at fd8640b: `from_str` of that line `is_err()` in the loop at `src/reliability.rs:7414-7423`) · covers AC-6 (the null arm)
  Given the control line `{"ts":0,"provider":"claude","acct":"work","session":0.42,"weekly":0.1}`
        decodes to a session reading of `Some(0.42)`
    And the same line with `null` in place of `0.42`
   When the second line is decoded as a `Sample`
   Then the decode is `Ok` and its session reading, read as `Option<f64>`, is `None`
    But NOT `is_err()` — the arm the canary asserted at `fd8640b`, where `null` on disk could only be a
        decode failure
    But NOT the canary deleted — REQ-SMP-C-001's BUT NOT; AC-6 reads
        `git diff origin/main -- src/reliability.rs` for the test present under its name
    # The positive control stays first and is kept, its `session` comparison read through `optional_fraction`
    # (a `.session` compared with `0.42` does not compile once the field is `Option<f64>`): every other assertion in that loop is an
    # `is_err()`, so a misspelled field name would make all of them "fail to decode" for a reason that
    # has nothing to do with `session`. The doc comment is rewritten to say `null` on disk means "no
    # window" and a non-finite reading is refused before it can be written.

Scenario Outline: the five non-finite spellings stay refused at decode
  # Tier B — pending: these arms are committed and green at fd8640b (`src/reliability.rs:7414-7423`) and #1598 records no mutant for them. What they pin is `serde_json`'s own policy — no infinity literal, `NumberOutOfRange` for a decimal that overflows `f64` — so the only in-crate regression is a lenient custom deserializer or the canary's deletion, and the grader is AC-6's presence check of the test under its name rather than an independent RED; no further test is owed, and this stub records the arms as unbound rather than promoting a presence check to Tier A · covers AC-6 (the decode arms)
  Given the raw line `{"ts":0,"provider":"claude","acct":"work","session":<spelling>,"weekly":0.1}`
   When it is decoded as a `Sample`
   Then the decode is `Err`
    But NOT `Some(f64::INFINITY)`, `Some(f64::NAN)` or a reading saturated to the `f64` range

Examples:
  | spelling  |
  | Infinity  |
  | -Infinity |
  | NaN       |
  | 1e400     |
  | -1e400    |
  # `serde_json` rejects a string where a number is expected and rejects an overflowing decimal rather
  # than saturating it, so no byte sequence in the store decodes to a non-finite reading — the premise
  # `fold_landings`' `is_finite` guard (`src/reliability.rs:2098-2099`) rests on, which becomes
  # `is_some_and(f64::is_finite)` in the reliability rule's own move (same pull request, not this item).

Scenario Outline: a Sample round-trips with a null reading on either axis, and the four optional keys stay absent when unset
  # Tier A — graded by usage_store::tests::sample_round_trips_with_and_without_optionals (#1598 T5, extended; no RED at fd8640b — it does not compile against origin/main, where `Sample::new` takes `f64`, and T1 and T3 carry the session half; mutant, AC-7's record: `skip_serializing_if = "Option::is_none"` on `weekly` fails the `"weekly":null` assertion, the weekly key's only grader) · covers AC-7
  Given `Sample::new(1_700_000_000, "claude", "work", <session>, <weekly>)` with no optional set
   When it is encoded with `serde_json::to_string` and decoded again
   Then the decode equals the original
    And the encoded text contains <null key>
    And none of `session_resets_at`, `weekly_resets_at`, `severity`, `spend` appears in the text
    But NOT `Some(0.0)` substituted on the absent axis, in the test or in the helper that builds it

Examples:
  | session   | weekly    | null key        |
  | None      | Some(0.2) | "session":null  |
  | Some(0.1) | None      | "weekly":null   |
  # The four `sample` test helpers — `fn sample` at `src/usage_store.rs:853` and `src/reliability.rs:3858`,
  # `fn sample` at `src/stats.rs:5539` and `src/usage_stats.rs:1279` — wrap a present fraction in `Some(..)`
  # at the helper, never an absent one as `Some(0.0)`. The other test sites the lift stops compiling follow the
  # same rule: `golden_samples()` in `src/reliability.rs`, the literal at `src/usage_store.rs:1256`, the `dated`
  # helper, a literal and the corpus loader in `src/usage_stats.rs` (`:2662`, `:2924`, `:3201`), and the raw reads
  # at `src/daemon.rs` (#1598 § 6's ledger, AC-11). AC-9's grep over the lines the pull request
  # adds to `src/usage_store.rs` and `src/reliability.rs`, for `unwrap_or(0.0)`, `unwrap_or_default()`
  # and `Some(0.0)`, prints nothing; the two whole-file hits at `fd8640b` — the spend value
  # `.with_spend(Some(0.0))` and an IEEE negative-zero comment in `src/reliability.rs` — predate it.
```

## Rule 3 — compaction retains a line carrying `null`, rewritten with its key; only an older binary loses it, and that loss is D6's residual, never a pass

```gherkin
Scenario: compaction keeps every in-window weekly-only line, rewritten with its "session":null key
  # Tier A — graded by usage_store::tests::compaction_retains_weekly_only_lines_and_leaves_the_rollup_readable (#1598 T8a, retention half; RED at fd8640b: `read_samples` drops all nine `null` lines, the raw file is EMPTY after compaction, and "exactly three lines" fails) · covers AC-3
  Given a temp store whose raw file holds these eleven lines, in this order, each ending in a newline:
    """
    {"ts":14695200,"provider":"claude","acct":"work","session":null,"weekly":0.6}
    {"ts":14695800,"provider":"claude","acct":"work","session":null,"weekly":0.62}
    {"ts":14696400,"provider":"claude","acct":"work","session":null,"weekly":0.64}
    {"ts":14697000,"provider":"claude","acct":"work","session":null,"weekly":0.66}
    {"ts":14781600,"provider":"claude","acct":"work","session":null,"weekly":0.7}
    {"ts":14782200,"provider":"claude","acct":"work","session":null,"weekly":0.72}
    {"ts":14782800,"provider":"claude","acct":"work","session":0.3,"weekly":0.74}
    {"ts":14783400,"provider":"claude","acct":"work","session":0.5,"weekly":0.76}
    {"ts":17107200,"provider":"claude","acct":"work","session":null,"weekly":0.8}
    {"ts":17107800,"provider":"claude","acct":"work","session":null,"weekly":0.81}
    {"ts":17108400,"provider":"claude","acct":"work","session":null,"weekly":0.82}
    """
    And `now` is `200 * DAY_SECS` and the policy is `RetentionPolicy::default()` — raw window 14 d,
        hourly 90 d, daily lifetime, poll cadence 300 s (`RetentionPolicy`, `src/usage_store.rs:305-315`)
   When `compact_and_roll` runs once
   Then the raw file holds exactly three lines, `ts` 17107200, 17107800 and 17108400 in that order
    And each, parsed as a JSON object, has `session` equal to JSON `null` with the key present,
        `weekly` as written, and the exact key set `["acct", "provider", "session", "ts", "weekly"]`
    And `read_samples` returns those three
    But NOT a line rewritten without its `session` key
    But NOT retention proven only on a line with a numeric session
    # `plan_roll` (`src/usage_store.rs:555-614`) partitions by `ts` alone — a sample rolls only once
    # its whole day is older than `now - 14 d` — and `write_samples` (`:620-627`) re-serializes every
    # retained `Sample` through the field serializer. 14695200 is `170 * DAY_SECS + 2 * HOUR_SECS`,
    # 14781600 is `171 * DAY_SECS + 2 * HOUR_SECS`, 17107200 is `now - 2 * DAY_SECS`: the cohorts
    # `retention_bounds_raw_window_and_rolls_into_tiers_lifetime_reads_daily` (`src/usage_store.rs:1095`)
    # already uses. This is REQ-SMP-Q-001 `SampleStoreContinuity` on the shipped binary: GOAL 0 lines
    # lost between append and the next compaction, FAIL any.

Scenario: the version-skew asymmetry, as an fd8640b binary reads a store this binary wrote (D6 default)
  # Tier B — pending: the silent half IS the RED record AC-13 owes — T1's `Err` and T8a's empty raw file are an fd8640b reader's behaviour on `null` lines; the loud half, an fd8640b `read_rollup` over a rollup whose bucket omits `session` returning `UsageRollupMalformed`, is the scratch-only `read_rollup` test #1598 AC-13 owes; the deployment rule that makes both unreachable is block B-12's (#1604). Both records belong to #1598 AC-13 · covers AC-14 (the behaviour the module doc states)
  Given a store written by this item's binary: raw lines carrying `"session":null`, and a rollup in
        which every bucket whose readings carried no session reading omits its `session` member
   When the binary at `fd8640b` reads that store
   Then `read_samples` skips every `null` line in silence, and its next `compact_and_roll` rewrites the
        raw file without them — the silent loss
    And `read_rollup` fails loudly with `UsageRollupMalformed` at the first bucket missing `session`
        (`read_rollup`, `src/usage_store.rs:473-484`: a present-but-unparseable rollup is a hard error,
        never a silent reset)
    But NOT either outcome counted as a pass of REQ-SMP-Q-001 — loss by an older binary is D6's
        residual, recorded in PRD § 12, never counted
    # A newer binary reading an older store: every line and every bucket carries both axes, so
    # nothing changes. Loud is the better failure. The rule that makes both unreachable — daemon and
    # offline verbs are one binary, replaced together before the first `null` line — is owned by the
    # deployment item #1604; the module doc states it in one line and cites #1604 (AC-14). It does not
    # point at the daemon-replacement runbook, which carries no such rule until #1603 lands in
    # tranche 2.
```

## Rule 4 — a tier bucket's per-axis statistic is over the readings that carried that axis, omitted when none did; the rollup stays finite and readable; `count` and `coverage` stay the bucket's whole sample count

```gherkin
Scenario: an hour and a day holding only session-less samples fold to a finite, readable rollup
  # Tier A — graded by usage_store::tests::compaction_retains_weekly_only_lines_and_leaves_the_rollup_readable (#1598 T8a, rollup half; green at fd8640b only because the lines are dropped — its evidence is AC-4's mutant record: with the emptiness guard removed so an all-absent axis folds `max_of(&[])`, the rollup is written with `"max":null` and T8a FAILS at `read_rollup`) · covers AC-4
  Given the store and the roll of the retention scenario above — eight aged lines, four of them (day
        14688000, hour 14695200) with no session reading
   When the rollup is read back
   Then `read_rollup` is `Ok`
    And the rollup file's text contains none of `null`, `inf`, `NaN`
    And the daily tier holds `day_start` 14688000 and 14774400, the hourly tier holds `hour_start`
        14695200 and 14781600, and `rolled_through_ts` is 14783400
    But NOT `-∞` or `NaN` folded for the session axis — `max_of(&[])` is
        `f64::NEG_INFINITY` (`src/usage_store.rs:805-807`), which `serde_json` 1.0.150 writes as `null`,
        so the compaction WRITE succeeds and the NEXT `read_rollup` fails: `compact_and_roll`
        (`read_rollup` at `src/usage_store.rs:534`) and `stats` (`StoreData::read`, `src/stats.rs:212-218`)
        both run it first, and every later compaction and every `stats` run fails until the file is
        repaired by hand
    But NOT a test that expects the compaction to "fail the write" — the failure is read-side, and the
        test must catch that shape
    # The PRD's and the design's earlier text said the write itself would fail; #1598 corrects the
    # mechanism against `serde_json` 1.0.150's `ser.rs` lines 169-180 at the registry path
    # `Cargo.lock` pins (`serialize_f64` matches `Nan | Infinite` and calls `write_null`; only map
    # keys error). The intent — no `-∞`, `NaN` or
    # `null` statistic ever reaches the rollup — is unchanged.

Scenario: a bucket whose readings carried no session reading omits its session statistic and keeps its whole-bucket count (D13 default)
  # Tier A — graded by usage_store::tests::a_bucket_whose_readings_carry_no_session_window_omits_the_session_statistic (#1598 T8b; cannot compile against fd8640b, green by construction; its evidence is AC-4's mutant, under which `read_rollup` fails before T8b's typed reads. A zero-fabricating guard — `Some(HourStat { max: 0.0, mean: 0.0 })` for an all-absent axis — would pass T8a and fail only T8b, so that row is worth adding to AC-4's record) · covers AC-4, AC-5
  Given the rollup of the scenario above
   When the day bucket 14688000 and the hour bucket 14695200 are read
   Then the day bucket has `count` 4, `coverage` 4/288, `session` `None`, and `weekly` `Some` with
        `max` 0.66, `mean` 0.63, `p95` 0.66 and `cap_hits` 0
    And the hour bucket has `count` 4, `session` `None`, and `weekly` `Some` with `max` 0.66 and
        `mean` 0.63
    And neither bucket's serialized form carries a `session` member
        (`#[serde(default, skip_serializing_if = "Option::is_none")]` on `Option<HourStat>` /
        `Option<DayStat>`)
    But NOT `session: { max: -inf, mean: 0.0 }`, a `p95` of `0.0` or a `cap_hits` of `0` written for an
        axis no reading carried
    But NOT `count` or `coverage` reduced to a per-axis count
    # Fractions compare within 1e-9, as the neighbouring tests do. `p95` by nearest rank is the
    # largest value for n ≤ 20 at p = 0.95 (`ceil(0.95·4) = 4`, `src/percentile.rs:21-31`); 288 is
    # `DAY_SECS / 300`, `expected_per_day` at `src/usage_store.rs:789-792`. D13 (design AD-11,
    # author-chosen): the alternatives it names are `HourStat` / `DayStat` with `Option` fields
    # inside (same skew, uglier) and a per-axis sample count beside the statistic (more honest,
    # deferred); a ruling for another representation replaces this scenario's shape and leaves the
    # finite-rollup behaviour of the scenario above in force.

Scenario: a mixed bucket folds each axis over exactly the readings that carried it
  # Tier A — graded by usage_store::tests::a_bucket_whose_readings_carry_no_session_window_omits_the_session_statistic (#1598 T8b; mutant, AC-4's second record: the session axis collected through `map(|s| s.session.unwrap_or(0.0))` in place of `filter_map` leaves T8a green and fails T8b — the all-absent buckets fold `Some(max 0.0, mean 0.0)` where `None` is asserted, and the mixed hour's session `mean` folds to 0.2, not 0.4: the fabrication AC-9 forbids, which only T8b catches) · covers AC-4, AC-9
  Given the rollup of the finite-rollup scenario above
   When the day bucket 14774400 and the hour bucket 14781600 are read — four samples each, two of which
        carry a session reading (0.3 and 0.5)
   Then the day bucket has `count` 4, `session` `Some` with `max` 0.5, `mean` 0.4, `p95` 0.5 and
        `cap_hits` 0, and `weekly` `Some` with `max` 0.76, `mean` 0.73, `p95` 0.76 and `cap_hits` 0
    And the hour bucket has `count` 4, `session` `Some` with `max` 0.5 and `mean` 0.4, and `weekly`
        `Some` with `max` 0.76 and `mean` 0.73
    But NOT a session `mean` of 0.2 — the two absent readings counted as `0.0`
    But NOT a session `count` of 2 — `count` is the bucket's sample count; the per-axis count that
        would make a same-key merge exact is the follow-up design AD-11 defers, not this item's
    # The fold collects each axis with `filter_map` and builds a statistic only when the vector is
    # non-empty (#1598 Specification § 5), so `max_of`'s "non-empty slice" doc becomes true by
    # construction and `mean_of`'s and `p95_of`'s `0.0`-for-empty arms stay as dead defence.

Scenario: a rollup written at fd8640b reads back unchanged
  # Tier A — graded by usage_store::tests::a_rollup_written_before_the_lift_reads_back_with_both_axes_present (#1598 T10, beside rollup_rewrite_leaves_no_temp_and_round_trips at `src/usage_store.rs:918`), with the mutant record #1598 AC-13 owes for it: serialize the lifted per-axis field under a new key, so a rollup written at `origin/main` no longer reads its `session` statistic and T10 FAILS; green at fd8640b by construction; it guards the first read every upgraded machine performs, which a shape slip would turn into `UsageRollupMalformed` on the lifetime daily tier) · covers AC-5
  Given a rollup JSON as the `fd8640b` binary writes it: `rolled_through_ts`, one hour bucket and one
        day bucket, each carrying both a `session` and a `weekly` statistic
   When `read_rollup` decodes it
   Then the decode is `Ok`, each bucket's `session` and `weekly` read `Some` with the written values,
        and `rolled_through_ts` is unchanged
    And encoding it again keeps both members present
    But NOT `UsageRollupMalformed` — the loud failure is reserved for the reverse skew, an older
        binary over a newer rollup

Scenario Outline: a defensive same-key merge never fabricates an absent axis
  # Tier A — graded by usage_store::tests::a_same_key_merge_keeps_an_absent_axis_absent (#1598 T11, beside weighted_mean_pools_by_count at `src/usage_store.rs:1358`), with the mutant record #1598 AC-13 owes for it: the same-key merge fills an absent axis with a zero statistic, and T11 FAILS; it does not compile against origin/main, where the helpers take `&HourStat` / `&DayStat`; low priority: the path is unreachable under roll-once-per-whole-day and reached only through `upsert_hour` / `upsert_day` on a duplicate key) · covers AC-5
  Given an existing bucket whose session statistic is <existing> and an incoming same-key bucket
        whose session statistic is <incoming>
   When the two are merged
   Then the merged session statistic is <merged>
    But NOT a `None` side weighted in as a mean of `0.0`

Examples:
  | existing                                 | incoming                                 | merged                                                          |
  | None                                     | None                                     | None                                                            |
  | Some(max 0.5, mean 0.4), bucket count 2  | None                                     | Some(max 0.5, mean 0.4) — the Some side, unchanged              |
  | Some(max 0.5, mean 0.4), bucket count 2  | Some(max 0.7, mean 0.6), bucket count 2  | Some(max 0.7, mean 0.5) — today's merge, weighted by the counts |
  # Weighting by the two BUCKET counts is exact only when every sample in both buckets carried the
  # axis, which is the case on the only path that reaches it; the `total == 0.0` arm of
  # `weighted_mean` (`src/usage_store.rs:777-785`) stays as dead defence, its doc saying so.
  # `merge_day_stat` (`src/usage_store.rs:768-775`) takes the same arms, with `p95` the max of the
  # two and `cap_hits` additive.
```

## Rule 5 — the records move with the shape: the module doc says "every dimension the account has" and states the version-skew residual

```gherkin
Scenario: the module doc stops asserting both quota dimensions and states the version-skew residual
  # Tier B — pending: documentation. The mechanically-decidable half is #1598 AC-10's token sweep with its canary — `git grep -c -i -E 'per-dimension strict-early-fire' -- src docs/adr README.md CONTRIBUTING.md` prints `src/swap.rs` on both trees, while `git grep -n -i 'both quota dimensions' -- src/usage_store.rs` prints `src/usage_store.rs` at fd8640b and nothing on the pull request — and `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` for the intra-doc links; the wording is the merge judge's. Nothing further is owed · covers AC-10, AC-14
  Given the module doc at `src/usage_store.rs:39-44`, heading `# Both quota dimensions, never one worst-case scalar`,
        whose paragraph says the store "projects neither window to a single blended number, because the
        two limits are independent and a reader may care about either"
   When this item lands
   Then the heading reads `# Every dimension the account has, never one worst-case scalar` — or the
        operator's rewording — and the paragraph keeps its point and loses its premise: the raw line
        carries an axis the account lacks as `null`, the key always written, never a fabricated `0`;
        a tier bucket omits that axis's statistic when none of its readings carried it; an older
        binary skips such a raw line in silence and erases it on compaction, and refuses such a rollup
        loudly
    And it states in one line the rule that makes both unreachable — daemon and offline verbs are one
        binary, replaced together before the first `null` line is written — citing issue #1604 as its
        owner, and does not cite the daemon-replacement runbook as carrying it
    And AC-10's canary prints the same line on `origin/main` and on the pull request, the corrected
        token returns nothing, and every line of AC-10's token sweep (23 at `fd8640b`) carries one
        disposition on the pull request — corrected in-band, propagated and owned elsewhere, or not
        restating the posture
    But NOT the "no blended scalar" point lost — only its premise moves
    But NOT a sweep keyed on citations to `usage_store` or to the ADRs — it reaches none of the
        `src/usage_store.rs` lines this sweep corrects, `:39` included, because none of them contains either
        word at `fd8640b`; that is the delta the token sweep exists for
    # The `Sample` doc's "four optional fields are omitted from the JSON entirely" (`:130-131`) gains
    # its complement — the two fraction keys are always present, `null` for a window the account lacks;
    # the field docs at `:144-147`, the bucket and statistic docs at `:232`, `:246`, `:255`, `:273`,
    # `:826`, `CAP_FRACTION` at `:106`, `weighted_mean` at `:778`, `max_of` at `:804` and `mean_of` at
    # `:809-810` move with it. The reading model's field docs (`session window consumed` at `src/usage.rs:88`,
    # `:90`, `:119`) and the `usage_stats` module doc (`BOTH quota dimensions` at `src/usage_stats.rs:17`)
    # are owned by #1596, #1597 and the `stats` item (block B-8); ADR-0033's "both lines are config"
    # (`docs/adr/0033-weekly-saturated-session-low-stays-unknown.md:118`) stays true and is #1593's.
```

## Coverage

Each acceptance criterion of #1598, with the scenarios that cover it. A criterion no scenario covers
is named as such, with what grades it instead.

| AC | Scenarios | What grades it |
|---|---|---|
| AC-1 — a weekly-only sample carries `"session":null`, the key written | Rule 1 "a weekly-only line re-encodes with its session key written as null"; Rule 1 "a successful poll of a weekly-only account appends one line…" | T3 with AC-1's first mutant record; T9 with its second, and with its AC-13 RED against the dispatch-time `origin/main` |
| AC-2 — a line carrying `null` on either axis decodes | Rule 2 "a weekly-only line decodes…"; Rule 2 "a session-only line decodes…" | T1, T2 |
| AC-3 — compaction retains such lines | Rule 3 "compaction keeps every in-window weekly-only line…"; Rule 1's key-set clause | T8a (retention half) |
| AC-4 — an all-absent axis folds to a finite, readable rollup with no statistic on that axis | Rule 4 "an hour and a day holding only session-less samples fold to a finite, readable rollup"; Rule 4 "a bucket whose readings carried no session reading omits…"; Rule 4 "a mixed bucket folds each axis over exactly the readings that carried it" | T8a (rollup half) with AC-4's first mutant record; T8b with its second, the fabricating fold |
| AC-5 — the tiers' representation is the per-axis `Option` (D13) | Rule 4 "a bucket whose readings carried no session reading omits…" (the omitted member); Rule 4 "a rollup written at fd8640b reads back unchanged"; Rule 4 "a defensive same-key merge…" | T8b; T10 for the backward read; T11 for the merge arms; the doc comments by `cargo doc` and review |
| AC-6 — the non-finite guard moves with the shape and is not deleted | Rule 2 "the canary's null arm moves…"; Rule 2 "the five non-finite spellings stay refused at decode" (Tier B); Rule 1 "a non-finite Some fraction is refused at serialization" | T6's moved `null` arm; the committed decode arms under AC-6's presence check; T6's near-end arm with its AC-13 RED run and AC-6's mutant record. T7 (`landing_refuses_a_non_finite_parked_reading_rather_than_fabricating_one`, `src/reliability.rs:7444`) stays green under the reliability rule's own move and has no scenario here |
| AC-7 — the round-trip test gains the `null` case for both axes | Rule 2 "a Sample round-trips with a null reading on either axis…" | T5, extended, with AC-7's mutant record for the weekly key |
| AC-8 — a line omitting the `session` key reads as no session reading | Rule 2 "a line omitting the session key reads as no session reading" | T4, written form |
| AC-9 — never a fabricated window value in the store or its tests | Rule 4 "a mixed bucket folds…" (the `mean` 0.2 BUT NOT); Rule 2's round-trip outline (the helper clause) | the `git grep` over the added lines for `unwrap_or(0.0)`, `unwrap_or_default()` and `Some(0.0)` is a review signal, not the gate; review reads every wrapper |
| AC-10 — the module doc's premise is corrected, with the token sweep | Rule 5 "the module doc stops asserting both quota dimensions…" (Tier B) | the canary grep, `cargo doc`, the merge judge |
| AC-11 — this item's share of the diff is bounded | none | `git diff --stat origin/main`, review; the pull request body names which commit carries the `stats` / reliability reader moves |
| AC-12 — gates | none | the five `test` steps, `msrv`, both `deny` checks, in CI; the ignored count equal to `origin/main`'s |
| AC-13 — the RED record exists | none | the pull request body's verbatim `FAILED` output of T1, T2, T3, T4, T8a, the canary's moved `null` arm and its serialization arm, T9 and the rewritten #1613 test against `origin/main`, each `running 1 test`, and of the scratch-only `read_rollup` test (the loud half of AC-14); eight mutation records (T3, T9, T8a, T8b, T6, T5, T10, T11); the line saying which option was built for D13, the omitted-key reader and the serialization guard; it is what turns every "RED at `fd8640b`" above from a reading of the source into a run |
| AC-14 — the version-skew residual is written where a reader of the store will see it | Rule 5 "the module doc stops asserting both quota dimensions…"; Rule 3 "the version-skew asymmetry…" (Tier B, the behaviour) | the source text and `cargo doc`; the loud half's record is AC-13's scratch-only `read_rollup` test |
| AC-15 — commit convention and landing order | none | review; the merge order on `main` (this item's pull request after the compile-unit pull request that carries the reading-model commit, never before it) |
| AC-16 — the #1613 sample skip is replaced | Rule 1 "the #1613 sample skip is gone…" | the renamed test `collector_writes_a_null_session_line_for_a_reading_whose_session_window_was_absent`, with its AC-13 RED against the dispatch-time `origin/main`; the `git grep -n -i -E '1613\|hot-?fix' -- src` check, recorded beside the canary AC-16 names and the non-empty run on the dispatch-time `origin/main`; review |
