# Feature: the sample store writes a window the account lacks as `null`, and every reader copes without a fabricated statistic

Issue #1598 · PRD REQ-SMP-B-001 / REQ-SMP-B-002 / REQ-SMP-C-001 / REQ-SMP-Q-001 (§ 10.7; D1 and D6 in § 8; § 11.4) · design § 5 B-7, § 4.3, § 4.4, § 11 Cap-4, § 12 AD-10 / AD-11 / AD-12, § 14 D12 / D13

Example Mapping: 🟦 6 rules · 🟩 34 examples · 🟥 0 open

> Since 2026-10-30 a verified Ultra account's usage reply carries `"five_hour":null` beside a present
> weekly window. Once the parser (#1596) and the reading model (#1597) serve such a reading with its
> session window `None`, the daemon's sample writer `append_sample_for_poll` (`src/daemon.rs:718-754`)
> must decide what it writes for that axis, and the store at `fd8640b` cannot write the truth:
> `Sample.session` is `f64` (`src/usage_store.rs:148`), a line carrying `"session":null` is skipped
> by `read_samples` (`src/usage_store.rs:449-467`) and erased by the next `compact_and_roll`
> (`read_samples` … `write_samples` at `src/usage_store.rs:535-544`), and an hour holding only such
> samples would fold `max_of(&[])` to `f64::NEG_INFINITY` (`src/usage_store.rs:807-809`), which
> `serde_json` 1.0.150 writes to the rollup as `null`, after which every `read_rollup`
> (`src/usage_store.rs:475-486`) hard-errors with `UsageRollupMalformed` and both compaction and
> `stats` fail until the file is repaired by hand. This item makes `Sample.session` and
> `Sample.weekly` `Option<f64>`, written as JSON `null` with the key always present, read and retained
> as such, and folded into per-axis `Option` statistics that are omitted when no reading in the
> bucket carried the axis. It lands in tranche 1's pull request right after the compile unit, by the
> scoping pipeline's choice (design § 4.4): `append_sample_for_poll` passes `reading.usage.session` into
> `Sample::new` at `src/daemon.rs:742-743` (`reading.usage.session`, `reading.usage.weekly`), so #1597's
> lift forces a choice at the writer; the compile unit makes it by keeping #1613's sample skip, re-keyed
> on either fraction being `None`, and this item replaces that skip with the `null` line. Its
> acceptance is graded at the store's own seams: `serde_json` on `Sample`, `compact_and_roll` over a
> temp store, `read_rollup`.
>
> The `# Tier` line under each scenario is this stub's binding line, in the form of
> `docs/specs/parser-optional-quota-windows.feature.md`. Tier A names a grader the executor may not
> reinterpret: a test committed before dispatch and left unedited, or a test the issue names verbatim
> with its expected values, which the executor writes from that name and those values — here a test
> #1598 names verbatim in § 6 (T1–T11), § 9 (T12–T17), AC-16 or the "As absorbed" lines of AC-24, AC-25
> and AC-26, together with the RED #1598 records for it at `fd8640b`
> or the mutant record one of its acceptance criteria owes. None of #1591's four oracles and none of
> #1592's eight tests reaches the store, so no scenario
> here binds to them. Every "RED at `fd8640b`" below is read off the store's source at that revision,
> never off a run: the runs are AC-13's to carry in the pull request body. Tier B names what is still
> unbound and the item that should bind it.
>
> D6 (`null` on the versionless sample line) and D13 (the per-axis `Option` tier representation) were
> ratified by the council ruling of 2026-11-02 (a comment on #1598): D6 as the default, amended with a
> rollup fence #1598 builds (`usage-rollup.json` becomes `{"format":2,"rollup":{…}}`, which every
> pre-#1598 build fails to read, so an older daemon's compaction stalls instead of erasing) and with a
> deployment rule that names the older daemon as the binary that erases; D13 as the default, amended
> with a per-axis `samples: Option<u32>` on each present tier statistic (#1610 absorbed). The fence's
> conditions C1 to C5 and C7 take their scenarios under Rule 6, with C1's committed-fixture arm in
> Rule 4's T10 scenario; C6, the operator text, is the module doc's under Rule 5 and the runbook item
> #1603's; the per-axis `samples` count takes its scenarios under Rule 4, with its module-doc clause
> and its count-posture sweep (AC-27) under Rule 5 and its pins in Rule 6's T17; and `CLAUDE.md`'s
> naming of the store's generation sits under Rule 5. Every scenario holds under both rulings, and a
> scenario added or amended under a ruling carries that ruling in its name. D12
> (`Sample.weekly` optional), the omitted-key reader (design AD-10 against PRD § 11.4) and the
> serialization refusal of a non-finite `Some` are open, each built for its recorded default with the
> alternative stated in #1598; a scenario that depends on one says so in its name. Every rule, tier
> and name here is ratification-pending.

## Rule 1 — a window the account lacks is written as JSON `null`: the key always present, never omitted, never `0`; a non-finite `Some` is refused at the only writer

```gherkin
Scenario: a weekly-only line re-encodes with its session key written as null (D6 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_weekly_only_line_round_trips_with_the_session_key_written_as_null (#1598 T3;
  # RED at fd8640b: the decode step is `Err`, `f64` refuses `null`; mutant, AC-1's record:
  # `skip_serializing_if = "Option::is_none"` on `session` fails the key-set assertion) · covers AC-1,
  # AC-3 (the key set)
  Given the raw line `{"ts":0,"provider":"claude","acct":"work","session":null,"weekly":0.1}`
   When it is decoded as a `Sample` and encoded again with `serde_json::to_string`
   Then the encoded text contains `"session":null`
    And the encoded object's keys, sorted, are exactly `["acct", "provider", "session", "ts", "weekly"]`
    And decoding the encoded text again equals the first decode
    But NOT `"session":0` or `"session":0.0` — byte-identical to an idle window the tree already holds
    But NOT the `session` key omitted — #1580's planned exact-key-set oracle (outside #1590, unlanded
        at `fd8640b`) REDs on omission, and `Sample`'s four optional siblings are the only fields that
        skip their key (`skip_serializing_if`, `src/usage_store.rs:151-165`)
    # No `skip_serializing_if` on either fraction field (#1598 Specification § 1). #1562 adds
    # `account_uuid` to `Sample` concurrently; whichever lands second rebases, and this item asserts
    # the five keys it knows.

Scenario: a successful poll of a weekly-only account appends one line carrying "session":null beside its weekly reading
  # Tier A — graded by daemon::tests::a_weekly_only_reading_is_sampled_with_its_session_written_as_null
  # (#1598 T9, beside collector_writes_one_redacted_sample_per_successful_poll at `src/daemon.rs:19897`;
  # no RED at fd8640b, where it does not compile (`Usage.session` is `f64`); RED at dispatch: against
  # the post-compile-unit origin/main it compiles and fails the one-sample assertion, because the
  # re-keyed #1613 skip writes no line, recorded per #1598 AC-13; mutant, AC-1's record: an early
  # `return` from `append_sample_for_poll` on a `None` session reading writes no line and fails the
  # one-sample assertion) · covers AC-1
  Given a served reading for the account labelled `work` whose session window is `None` and whose
        weekly window is `Some(0.90)` with a weekly reset stamp, severity `Some("critical")`
   When `append_sample_for_poll` runs for it at epoch 1_700_000_000
   Then exactly one line is appended, and it decodes with `ts` 1_700_000_000, `acct` `work`, a session
        reading of `None`, a weekly reading of `Some(0.90)`, the weekly reset stamp, no session reset
        stamp, severity `critical` and `spend` `None`
    And the line's text carries `"session":null` with the key present
    But NOT `"session":0.0`
    But NOT no line at all — gap honesty reserves "nothing" for a failed poll
        (`let Ok(reading) = polled else { return }`, `src/daemon.rs:724-726`), and a failed poll still
        writes nothing
    # The `Option` copies through from `reading.usage.session` at `src/daemon.rs:742-743` once
    # `Sample::new` takes `Option<f64>`; a served reading carries at least one window under D1's
    # default (a), so a line with both axes `null` never arises there.

Scenario: the #1613 sample skip is gone — a reading with no session window writes one line, not a gap
  # Tier A — graded by
  # daemon::tests::collector_writes_a_null_session_line_for_a_reading_whose_session_window_was_absent
  # (#1598 AC-16: #1613's collector_records_nothing_for_a_reading_whose_session_window_was_absent,
  # rewritten and renamed; no RED at fd8640b, where neither #1613's test nor its skip exists; RED at
  # dispatch: against the post-compile-unit origin/main the rewritten test compiles and fails its
  # one-line assertion, because the skip re-keyed on an absent fraction writes no line, recorded per
  # #1598 AC-13) · covers AC-16
  Given the reading #1613's test builds, with its session window `None`
   When `append_sample_for_poll` runs for it
   Then exactly one line is appended, and its text carries `"session":null` with the key present
    And `append_sample_for_poll` holds no early return keyed on an absent window — neither #1613's
        flag nor the compile unit's `None` re-key
    And `git grep -n -i -E '1613|hot-?fix' -- src` prints nothing
    But NOT the gap #1613 recorded on purpose — a stopgap until a `Sample` could say `null`
    # The pull request body names #1613, never a `src` comment, or AC-16's grep would print it. T9
    # grades the same line from a reading built beside
    # collector_writes_one_redacted_sample_per_successful_poll;
    # this scenario grades that the hot-fix's skip and its text are gone.

Scenario Outline: a non-finite Some fraction is refused at serialization, so null on disk has one meaning (serialization-refusal default)
  # Tier A — graded by reliability::tests::the_sample_store_cannot_carry_a_non_finite_session_reading
  # (#1598 T6, the near-end loop; RED at fd8640b: `serde_json::to_string` of such a `Sample` is `Ok` —
  # the canary's own `.expect("a Sample always serializes")` — so the `is_err()` assertion FAILS,
  # recorded per AC-13 in T6's first scratch run; mutant, AC-6's record: `serialize_with` dropped, so
  # `Some(f64::INFINITY)` serializes as `"session":null` and the assertion fails) · covers AC-6 (the
  # serialization arm)
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
  # The hint is `serialize_err()`'s (`src/usage_store.rs:841-843`), whose doc "reachable only for a
  # non-finite float" becomes true as written: `serde_json` 1.0.150's `serialize_f64` writes `null`
  # for `Nan | Infinite` rather than erroring, so the refusal has to live in the field serializer
  # `serialize_finite_fraction` (#1598 Specification § 1). The alternative the item states —
  # accept `null` for a non-finite `Some` and move the encode loop to assert `None` — makes `null` on
  # disk ambiguous; a ruling for it changes this scenario and nothing else.
```

## Rule 2 — a line carrying `null` on either axis, or omitting a fraction key, decodes with that axis `None`, never `0.0`; the non-finite spellings stay refused

```gherkin
Scenario: a weekly-only line decodes with its session reading absent
  # Tier A — graded by usage_store::tests::a_weekly_only_line_decodes_with_its_session_reading_absent
  # (#1598 T1; RED at fd8640b: `serde_json::from_str::<Sample>` is `Err`, `f64` refuses `null`) · covers
  # AC-2
  Given the raw line `{"ts":0,"provider":"claude","acct":"work","session":null,"weekly":0.1}`
   When it is decoded as a `Sample`
   Then the decode is `Ok`
    And the session reading, read as `Option<f64>`, is `None`
    And the weekly reading, read as `Option<f64>`, is `Some(0.1)`
    But NOT `Some(0.0)` for the session axis
    But NOT the line skipped — `read_samples` (`src/usage_store.rs:449-467`) keeps every record
        `from_slice` accepts and skips the rest in silence, which is how a healthy poll's line is lost
        today
    # The type-stable read is `optional_fraction(decoded.session)`, #1598 § 6's verbatim copy of
    # #1591's test helper: it compiles while the field is `f64` (`From<T> for Option<T>`) and once it
    # is `Option<f64>`, and passes `clippy -D warnings` on both sides, which is what lets T1–T4 and
    # T8a run against `origin/main` for their RED record. Never `Option::<f64>::from(decoded.session)`
    # or `decoded.session.into()` at the call site: after the lift each is an identity conversion,
    # which fails `clippy::useless_conversion` under `-D warnings`.

Scenario: a session-only line decodes with its weekly reading absent (D12 default)
  # Tier A — graded by usage_store::tests::a_session_only_line_decodes_with_its_weekly_reading_absent
  # (#1598 T2; RED at fd8640b, the same refusal on the weekly axis) · covers AC-2
  Given the raw line `{"ts":0,"provider":"claude","acct":"work","session":0.42,"weekly":null}`
   When it is decoded as a `Sample`
   Then the decode is `Ok`
    And the session reading is `Some(0.42)` and the weekly reading is `None`
    But NOT `Some(0.0)` for the weekly axis
    # D12 (design AD-10, author-chosen): the weekly axis takes the same shape because a weekly-less
    # served reading — #1596's session-only case, Oracle D — leaves the writer the same three choices
    # on that axis. A ruling against D12 makes the session-only line unrepresentable: this scenario
    # and T2 are dropped, T5's weekly-null row goes with them, and the weekly axis of the fold, the
    # same-key merge and the readers loses its `None` arm; the session axis is untouched.

Scenario: a line omitting the session key reads as no session reading (AD-10 default)
  # Tier A — graded by usage_store::tests::a_line_omitting_the_session_key_reads_as_no_session_reading
  # (#1598 T4, written form; RED at fd8640b: a `missing field` error, the field has no default) · covers
  # AC-8
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
  # Tier A — graded by reliability::tests::the_sample_store_cannot_carry_a_non_finite_session_reading
  # (#1598 T6, the moved `"null"` arm; RED at fd8640b: `from_str` of that line
  # `is_err()` in the loop at `src/reliability.rs:7414-7423`) · covers AC-6 (the null arm)
  Given the control line `{"ts":0,"provider":"claude","acct":"work","session":0.42,"weekly":0.1}`
        decodes to a session reading of `Some(0.42)`
    And the same line with `null` in place of `0.42`
   When the second line is decoded as a `Sample`
   Then the decode is `Ok` and its session reading, read as `Option<f64>`, is `None`
    But NOT `is_err()` — the arm the canary asserted at `fd8640b`, where `null` on disk could only be a
        decode failure
    But NOT the canary deleted — REQ-SMP-C-001's BUT NOT; AC-6 reads
        `git diff origin/main -- src/reliability.rs` for the test present under its name
    # The positive control stays first and is kept, its `session` comparison read through
    # `optional_fraction`
    # (a `.session` compared with `0.42` does not compile once the field is `Option<f64>`): every other
    # assertion in that loop is an
    # `is_err()`, so a misspelled field name would make all of them "fail to decode" for a reason that
    # has nothing to do with `session`. The doc comment is rewritten to say `null` on disk means "no
    # window" and a non-finite reading is refused before it can be written.

Scenario Outline: the five non-finite spellings stay refused at decode
  # Tier B — pending: these arms are committed and green at fd8640b
  # (`src/reliability.rs:7414-7423`) and #1598 records no mutant for them. What they pin is `serde_json`'s
  # own policy — no infinity literal, `NumberOutOfRange` for a decimal that overflows `f64` — so the
  # only in-crate regression is a lenient custom deserializer or the canary's deletion, and the grader
  # is AC-6's presence check of the test under its name rather than an independent RED; no further test
  # is owed, and this stub records the arms as unbound rather than promoting a presence check to Tier A
  # · covers AC-6 (the decode arms)
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

Scenario Outline: a Sample round-trips with a null reading on either axis, and the four optional keys stay absent when unset (D12 default)
  # Tier A — graded by usage_store::tests::sample_round_trips_with_and_without_optionals (#1598 T5,
  # extended; no RED at fd8640b — it does not compile against origin/main, where `Sample::new` takes
  # `f64`, and T1 and T3 carry the session half; mutant, AC-7's record:
  # `skip_serializing_if = "Option::is_none"` on `weekly` fails the `"weekly":null` assertion, the
  # weekly key's only grader) · covers AC-7
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
  # The four `sample` test helpers —
  # `fn sample` at `src/usage_store.rs:855` and `src/reliability.rs:3858`,
  # `fn sample` at `src/stats.rs:5539` and `src/usage_stats.rs:1279` — wrap a present fraction in
  # `Some(..)`
  # at the helper, never an absent one as `Some(0.0)`. The other test sites the lift stops compiling
  # follow the
  # same rule: `golden_samples()` in `src/reliability.rs`, the literal at `src/usage_store.rs:1258`, the
  # `dated`
  # helper, a literal and the corpus loader in `src/usage_stats.rs` (`:2662`, `:2924`, `:3201`), and the
  # raw reads
  # at `src/daemon.rs` (#1598 § 6's ledger, AC-11). AC-9's grep over the lines the pull request
  # adds to `src/usage_store.rs` and `src/reliability.rs`, for `unwrap_or(0.0)`, `unwrap_or_default()`
  # and `Some(0.0)`, prints nothing; the two whole-file hits at `fd8640b` — the spend value
  # `.with_spend(Some(0.0))` and an IEEE negative-zero comment in `src/reliability.rs` — predate it.
```

## Rule 3 — compaction retains a line carrying `null`, rewritten with its key; only an older daemon loses it, the rollup fence stalls that daemon, and the loss is D6's residual, never a pass

```gherkin
Scenario: compaction keeps every in-window weekly-only line, rewritten with its "session":null key (D6 as ruled)
  # Tier A — graded by
  # usage_store::tests::compaction_retains_weekly_only_lines_and_leaves_the_rollup_readable (#1598 T8a,
  # retention half; RED at fd8640b: `read_samples` drops all nine `null` lines, the raw file is EMPTY
  # after compaction, and "exactly three lines" fails) · covers AC-3
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
        hourly 90 d, daily lifetime, poll cadence 300 s
        (`RetentionPolicy`, `src/usage_store.rs:307-317`)
   When `compact_and_roll` runs once
   Then the raw file holds exactly three lines, `ts` 17107200, 17107800 and 17108400 in that order
    And each, parsed as a JSON object, has `session` equal to JSON `null` with the key present,
        `weekly` as written, and the exact key set `["acct", "provider", "session", "ts", "weekly"]`
    And `read_samples` returns those three
    But NOT a line rewritten without its `session` key
    But NOT retention proven only on a line with a numeric session
    # `plan_roll` (`src/usage_store.rs:557-616`) partitions by `ts` alone — a sample rolls only once
    # its whole day is older than `now - 14 d` — and `write_samples` (`:620-627`) re-serializes every
    # retained `Sample` through the field serializer. 14695200 is `170 * DAY_SECS + 2 * HOUR_SECS` and
    # 17107200 is `now - 2 * DAY_SECS`, two of the three cohorts
    # `retention_bounds_raw_window_and_rolls_into_tiers_lifetime_reads_daily` (`src/usage_store.rs:1097`)
    # already uses (its third is `80 * DAY_SECS + 5 * HOUR_SECS`); 14781600 is
    # `171 * DAY_SECS + 2 * HOUR_SECS`, a second aged day this scenario adds so that a day with numeric
    # samples sits beside a day without. This is REQ-SMP-Q-001 `SampleStoreContinuity` on the shipped
    # binary: GOAL 0 lines
    # lost between append and the next compaction, FAIL any.

Scenario: the version-skew asymmetry, as an fd8640b build meets a store this binary wrote (D6 as ruled)
  # Tier B — pending: the council ruling of 2026-11-02 corrected this scenario's shape — one older
  # compaction pass cannot both erase raw lines and fail `read_rollup`, because `compact_and_roll`
  # returns at `read_rollup` before it writes anything. The silent half — `read_samples` dropping `null`
  # lines, T1's `Err` and T8a's empty raw file — IS the RED record AC-13 owes, an fd8640b reader's
  # behaviour on `null` lines; the loud half is the fence's, recorded twice under AC-13: the
  # scratch-only run that `.expect`s `read_rollup` to be `Ok` over
  # `{"format":2,"rollup":{"rolled_through_ts":0,"hourly":[],"daily":[]}}` and panics at `origin/main`
  # with `UsageRollupMalformed`, and T14 (Rule 6), which decodes the rollup this build writes into
  # frozen copies of the fd8640b tier types and requires `Err`; the deployment rule that keeps an older
  # daemon off the store is block B-12's (#1604, REQ-DEP-C-001). Both records belong to #1598 AC-13 ·
  # covers AC-14 (the behaviour the module doc states)
  Given a store written by this item's binary: raw lines carrying `"session":null`, and the fenced
        rollup `{"format":2,"rollup":{…}}` its first compaction wrote
   When the build at `fd8640b` runs against that store
   Then its `compact_and_roll` reads the raw file and then fails at `read_rollup`
        (`read_rollup` at `src/usage_store.rs:475-486`: a present-but-unparseable rollup is a hard
        error, never a silent reset), before it rewrites either file — the stall; the fd8640b daemon
        discards that error (`if let Ok(summary) =
        compact_and_roll(…)` in `stats_events_for_poll`, `src/daemon.rs:894`) and stops compacting
        without a word, while its `stats` fails loudly with `UsageRollupMalformed`
    And its `reliability` reads the raw file through `read_samples`, which skips every `null` line in
        silence, so its report leaves them out — it deletes nothing
    And only a rollup the older build can decode — none after the first fenced write, unless the file
        is deleted or moved by hand, or lines were appended before a failed first pass — lets its
        `compact_and_roll` go on to rewrite the raw file without the `null` lines its `read_samples`
        skipped: the silent loss, D6's accepted residual
    But NOT either outcome counted as a pass of REQ-SMP-Q-001 — loss by an older daemon is D6's
        residual, recorded in PRD § 12, never counted
    But NOT an older `stats` or `reliability` named as the binary that erases — both only read
    # A newer binary reading an older store: every line and every bucket carries both axes, and
    # `read_rollup` reads a file with no top-level `format` key as the legacy format 1, so nothing
    # changes. Loud is the better failure. The rule that keeps an older daemon off the store from its
    # first `null` line, by any start path, is owned by the deployment item #1604 and stated in
    # REQ-DEP-C-001; the module doc states it in one line, says the store files are never deleted or
    # moved, quotes the older build's "malformed usage rollup" message as expected after #1598 (the
    # ruling's C6) and cites #1604 (AC-14). It does not point at the daemon-replacement runbook, which
    # carries no such rule until #1603 lands in tranche 2.
```

## Rule 4 — a tier bucket's per-axis statistic is over the readings that carried that axis, omitted when none did, and records how many it is over (`samples`); the rollup stays finite and readable; `count` and `coverage` stay the bucket's whole sample count

```gherkin
Scenario: an hour and a day holding only session-less samples fold to a finite, readable rollup
  # Tier A — graded by
  # usage_store::tests::compaction_retains_weekly_only_lines_and_leaves_the_rollup_readable (#1598 T8a,
  # rollup half; RED at fd8640b: `read_samples` drops the nine `null` lines, so only day 14774400's two
  # numeric samples roll — the daily bucket 14688000 and the hourly bucket 14695200 never exist and the
  # tier assertions below fail; its second record is AC-4's mutant: with the emptiness guard removed so
  # an all-absent axis folds `max_of(&[])`, the rollup is written with `"max":null` and T8a FAILS at
  # `read_rollup`) · covers AC-4
  Given the store and the roll of the retention scenario above — eight aged lines, four of them (day
        14688000, hour 14695200) with no session reading
   When the rollup is read back
   Then `read_rollup` is `Ok`
    And the rollup file's text contains none of `null`, `inf`, `NaN`
    And the daily tier holds `day_start` 14688000 and 14774400, the hourly tier holds `hour_start`
        14695200 and 14781600, and `rolled_through_ts` is 14783400
    But NOT `-∞` or `NaN` folded for the session axis — `max_of(&[])` is
        `f64::NEG_INFINITY` (`src/usage_store.rs:807-809`), which `serde_json` 1.0.150 writes as `null`,
        so the compaction WRITE succeeds and the NEXT `read_rollup` fails: `compact_and_roll`
        (`read_rollup` at `src/usage_store.rs:536`) and `stats`
        (`StoreData::read`, `src/stats.rs:212-218`)
        both run it before anything is written — `compact_and_roll` before it rewrites either file —
        and every later compaction and every `stats` run fails until the file is repaired by hand
    But NOT a test that expects the compaction to "fail the write" — the failure is read-side, and the
        test must catch that shape
    # The PRD's and the design's earlier text said the write itself would fail; #1598 corrects the
    # mechanism against `serde_json` 1.0.150's `ser.rs` lines 169-180 at the registry path
    # `Cargo.lock` pins (`serialize_f64` matches `Nan | Infinite` and calls `write_null`; only map
    # keys error). The intent — no `-∞`, `NaN` or
    # `null` statistic ever reaches the rollup — is unchanged.

Scenario: a bucket whose readings carried no session reading omits its session statistic and keeps its whole-bucket count (D13 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_bucket_whose_readings_carry_no_session_window_omits_the_session_statistic
  # (#1598 T8b; cannot compile against fd8640b, green by construction; its evidence is AC-4's mutant,
  # under which `read_rollup` fails before T8b's typed reads. A zero-fabricating guard —
  # `Some(HourStat { max: 0.0, mean: 0.0 })` for an all-absent axis — would pass T8a and fail only T8b,
  # so that row is worth adding to AC-4's record) · covers AC-4, AC-5
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
    # largest value for n ≤ 19 at p = 0.95 (`ceil(0.95·4) = 4`; at n = 20 the rank is 19 —
    # `percentile` at `src/percentile.rs:21-31`); 288 is
    # `DAY_SECS / 300`, `expected_per_day` at `src/usage_store.rs:791-794`. D13 (design AD-11) was
    # ratified by the council ruling of 2026-11-02 with one amendment: every present `HourStat` /
    # `DayStat` also records `samples: Option<u32>`, the readings it covers, set at fold time (#1610
    # absorbed into #1598). Its values here are asserted by "each present statistic records how many
    # of its bucket's readings it is over…" below. The ruling rejected `Option` fields inside
    # `HourStat` / `DayStat` (half-filled statistics, an ambiguous `cap_hits`) and an explicit `null`
    # in buckets (two spellings of no reading), and keeps the finite-rollup behaviour of the scenario
    # above in force.

Scenario: a mixed bucket folds each axis over exactly the readings that carried it (D13 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_bucket_whose_readings_carry_no_session_window_omits_the_session_statistic
  # (#1598 T8b; mutant, AC-4's second record: the session axis collected through
  # `map(|s| s.session.unwrap_or(0.0))` in place of `filter_map` leaves T8a green and fails T8b — the
  # all-absent buckets fold `Some(max 0.0, mean 0.0)` where `None` is asserted, and the mixed hour's
  # session `mean` folds to 0.2, not 0.4: the fabrication AC-9 forbids, which only T8b catches) · covers
  # AC-4, AC-9
  Given the rollup of the finite-rollup scenario above
   When the day bucket 14774400 and the hour bucket 14781600 are read — four samples each, two of which
        carry a session reading (0.3 and 0.5)
   Then the day bucket has `count` 4, `session` `Some` with `max` 0.5, `mean` 0.4, `p95` 0.5 and
        `cap_hits` 0, and `weekly` `Some` with `max` 0.76, `mean` 0.73, `p95` 0.76 and `cap_hits` 0
    And the hour bucket has `count` 4, `session` `Some` with `max` 0.5 and `mean` 0.4, and `weekly`
        `Some` with `max` 0.76 and `mean` 0.73
    But NOT a session `mean` of 0.2 — the two absent readings counted as `0.0`
    But NOT a session `count` of 2 — `count` is the bucket's sample count; the per-axis count that
        makes a same-key merge exact is the `samples` field the ruling adds beside each present
        statistic (#1610, absorbed into #1598), asserted for these buckets — session `Some(2)`, weekly
        `Some(4)` — by "each present statistic records how many of its bucket's readings it is over…"
        below
    # The fold collects each axis with `filter_map` and builds a statistic only when the vector is
    # non-empty (#1598 Specification § 5), so `max_of`'s "non-empty slice" doc becomes true by
    # construction and `mean_of`'s and `p95_of`'s `0.0`-for-empty arms stay as dead defence.

Scenario: a rollup written at fd8640b reads back unchanged (D6 and D13 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_rollup_written_before_the_lift_reads_back_with_both_axes_present (#1598 T10,
  # beside rollup_rewrite_leaves_no_temp_and_round_trips at `src/usage_store.rs:920`), with the mutant
  # record #1598 AC-13 owes for it: serialize the lifted per-axis field under a new key, so a rollup
  # written at `origin/main` no longer reads its `session` statistic and T10 FAILS; and AC-19's record:
  # removing the legacy arm of `read_rollup`'s dispatch, so a file with no `format` key is refused,
  # fails T10 at its read (and T15 at its pass); it does not compile against origin/main (`samples`,
  # `Option` tiers), where it would be green by construction; it guards the first read every upgraded
  # machine performs, which a shape slip would turn into `UsageRollupMalformed` on the lifetime daily
  # tier) · covers AC-5, AC-17 (the committed `fd8640b` fixture C1 names), AC-25 (shape one)
  Given a rollup JSON as the `fd8640b` binary writes it: `rolled_through_ts`, one hour bucket and one
        day bucket, each carrying both a `session` and a `weekly` statistic
   When `read_rollup` decodes it
   Then the decode is `Ok`, each bucket's `session` and `weekly` read `Some` with the written values,
        and `rolled_through_ts` is unchanged
    And every statistic's `samples` reads `None` — not recorded, never the bucket's `count` and never `0`
    And encoding it again keeps both members present
    And the decoded `Rollup`, encoded again with `serde_json::to_string`, carries no `samples` key
    And that re-encoded text, parsed as a `Value`, equals the input parsed as a `Value` — no count
        gained, no field lost
    But NOT `UsageRollupMalformed` — the loud failure is reserved for the reverse skew, an older
        `stats` over a fenced rollup (an older daemon meets the same error and discards it in silence)
    # The literal is the one quoted in Rule 6's "a legacy rollup survives its first fenced compaction…":
    # C1's committed `fd8640b` fixture, an inline literal in the test module and never a file under
    # `build/fixtures/**` (#1598 T10). It is AC-25's shape one; shape two takes its own scenario below.

Scenario Outline: a defensive same-key merge never fabricates an absent axis (D13 as ruled)
  # Tier A — graded by usage_store::tests::a_same_key_merge_keeps_an_absent_axis_absent (#1598 T11,
  # beside weighted_mean_pools_by_count at `src/usage_store.rs:1360`), with the mutant record #1598
  # AC-13 owes for it: the same-key merge fills an absent axis with a zero statistic, and T11 FAILS; it
  # does not compile against origin/main, where the helpers take `&HourStat` / `&DayStat`; low priority:
  # the path is unreachable under roll-once-per-whole-day and reached only through `upsert_hour` /
  # `upsert_day` on a duplicate key) · covers AC-5, AC-26 (the not-recorded arm)
  Given an existing bucket whose session statistic is <existing> and an incoming same-key bucket
        whose session statistic is <incoming>
   When the two are merged by <helper>
   Then the merged session statistic is <merged>
    And the merged statistic's `samples` is `None` — neither operand recorded one
    But NOT a `None` side weighted in as a mean of `0.0`

Examples:
  | helper            | existing                                                     | incoming                                                     | merged                                                                               |
  | `merge_hour_stat` | None                                                         | None                                                         | None                                                                                 |
  | `merge_hour_stat` | Some(max 0.5, mean 0.4), bucket count 2                      | None                                                         | Some(max 0.5, mean 0.4) — the Some side, unchanged                                   |
  | `merge_hour_stat` | None                                                         | Some(max 0.5, mean 0.4), bucket count 2                      | Some(max 0.5, mean 0.4) — the Some side, unchanged                                   |
  | `merge_hour_stat` | Some(max 0.5, mean 0.4), bucket count 2                      | Some(max 0.7, mean 0.6), bucket count 2                      | Some(max 0.7, mean 0.5) — today's merge, weighted by the counts                      |
  | `merge_day_stat`  | None                                                         | None                                                         | None                                                                                 |
  | `merge_day_stat`  | Some(max 0.5, mean 0.4, p95 0.5, cap_hits 0), bucket count 2 | None                                                         | Some(max 0.5, mean 0.4, p95 0.5, cap_hits 0) — the Some side, unchanged              |
  | `merge_day_stat`  | None                                                         | Some(max 0.5, mean 0.4, p95 0.5, cap_hits 0), bucket count 2 | Some(max 0.5, mean 0.4, p95 0.5, cap_hits 0) — the Some side, unchanged              |
  | `merge_day_stat`  | Some(max 0.5, mean 0.4, p95 0.5, cap_hits 0), bucket count 2 | Some(max 1.2, mean 0.6, p95 1.2, cap_hits 1), bucket count 2 | Some(max 1.2, mean 0.5, p95 1.2, cap_hits 1) — today's merge, weighted by the counts |
  # Every operand here records no count (`samples: None`), so the merge takes the arm that weights by
  # the two bucket counts, as `fd8640b` does, and the merged statistic records no count. When both
  # operands record a count the merge weights by those instead ("a same-key merge weights each axis
  # by its recorded counts…" below). Both weights are at least 1 here, so the `total == 0.0` arm
  # of `weighted_mean` (`src/usage_store.rs:779-787`) stays as dead defence.
  # `merge_day_stat` (`src/usage_store.rs:770-777`) takes the same arms, with `p95` the max of the
  # two and `cap_hits` additive.

Scenario: each present statistic records how many of its bucket's readings it is over, and an omitted axis records none (D13 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_bucket_whose_readings_carry_no_session_window_omits_the_session_statistic
  # (#1598 T8b, its `samples` assertions — AC-23 as absorbed; no RED at fd8640b: it does not compile
  # against origin/main, where the field does not exist, so it can only fail to compile there; mutants,
  # AC-23's records: taking the per-axis count from `hour_samples.len()` / `day_samples.len()` instead
  # of the filtered vector fails the session assertions, 4 where 2 is expected; writing a count of `0`
  # for an absent axis fails the absent-axis assertions) · covers AC-23, AC-5
  Given the rollup of "an hour and a day holding only session-less samples fold to a finite, readable
        rollup" above — Rule 3's eleven lines, compacted once at `now = 200 * DAY_SECS` with
        `RetentionPolicy::default()`
   When the buckets are read back through `read_rollup`
   Then day 14688000 has `count` 4, `coverage` 4/288, no session statistic, and weekly `samples` `Some(4)`
    And hour 14695200 has `count` 4, no session statistic, and weekly `samples` `Some(4)`
    And day 14774400 has `count` 4, `coverage` 4/288, session `samples` `Some(2)` and weekly `samples`
        `Some(4)`
    And hour 14781600 has `count` 4, session `samples` `Some(2)` and weekly `samples` `Some(4)`
    And every other statistic value is the one "a bucket whose readings carried no session reading
        omits…" and "a mixed bucket folds each axis…" above state — day 14774400 session
        `{max 0.5, mean 0.4, p95 0.5, cap_hits 0}` and weekly `{max 0.76, mean 0.73, p95 0.76, cap_hits 0}`,
        hour 14781600 session `{max 0.5, mean 0.4}` and weekly `{max 0.76, mean 0.73}`
    But NOT `count` or `coverage` reduced to a per-axis count — `count` stays the bucket's whole sample
        count and `coverage` its ratio to the expected count; the per-axis count is `samples`
    But NOT a count written for an axis no sample carried, not even `0` — an omitted statistic and a
        count of zero say the same thing, and the format must not carry both
    # #1610's own fixture (its eight lines under AC-23) differs from Rule 3's aged lines only in its
    # `acct` labels, `ultra` and `pro`, which the roster-wide fold never reads, so the `samples` values
    # are AC-23's over the same readings. A recorded count is at least 1 and at most the bucket's
    # `count`; the fold sets it to the length of the per-axis vector it already builds (#1598 § 5).

Scenario Outline: the count invariant holds at both tiers for both axes, whether no reading, one or all four carry the axis (D13 as ruled, D12 default)
  # Tier A — graded by usage_store::tests::count_invariants_hold_for_every_folded_bucket (#1598 AC-24
  # as absorbed, a new test beside T8b; no RED at fd8640b: the field does not exist at the base tree,
  # so the test can only fail to compile there; mutant, AC-24's record: counting a sample whose reading
  # is absent makes the exactly-one case record 4 and fail) · covers AC-24, AC-5
  Given one hour of four samples, inside a day wholly older than the raw window at
        `now = 200 * DAY_SECS`, in which the <axis> axis is carried by <carriers> and the other axis by
        all four
   When `compact_and_roll` runs once and the hour bucket and the day bucket are read back
   Then at both tiers the <axis> statistic is <statistic>
    And the bucket's `count` is 4 at both tiers
    But NOT a statistic present without a recorded count, or a count present without a statistic — the
        fold records a count exactly when it builds a statistic (#1598 Specification § 5)
    But NOT a recorded count below 1 or above the bucket's `count` — a statistic exists only over at
        least one reading, and no axis is carried by more samples than the bucket holds

Examples:
  | axis    | carriers    | statistic                                                   |
  | session | none        | absent, with no count                                       |
  | session | exactly one | present, `samples` `Some(1)`, its `max` equal to its `mean` |
  | session | all four    | present, `samples` `Some(4)`, equal to `count`              |
  | weekly  | none        | absent, with no count                                       |
  | weekly  | exactly one | present, `samples` `Some(1)`, its `max` equal to its `mean` |
  | weekly  | all four    | present, `samples` `Some(4)`, equal to `count`              |
  # The invariant, from #1610 (AC-24): for every bucket the fold produces and each of its axes, a
  # statistic is present exactly when a recorded count is present; that count is at least 1, at most
  # the bucket's `count`, and equal to the number of the bucket's samples whose reading on that axis is
  # present.

Scenario: a rollup written without the count, with an axis omitted in one bucket and a statistic over fewer readings than its bucket in another, reads every statistic as not recorded (D13 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_rollup_without_the_count_reads_every_statistic_as_not_recorded (#1598 AC-25
  # as absorbed, shape two, a new test beside T10; a compatibility guard, and no RED owed: the base
  # tree refuses this file (hour 0 omits `session`, which the base `HourBucket` requires) and the
  # assertions name `samples`, so the test compiles only after the change; the mutation records are
  # the evidence — AC-25's: reading a missing count as the bucket's `count`, and separately as
  # `Some(0)`, each fail) · covers AC-25, AC-5
  Given a rollup file holding
    """
    {"rolled_through_ts":9,"hourly":[{"hour_start":0,"count":4,"weekly":{"max":0.76,"mean":0.73}},{"hour_start":3600,"count":4,"session":{"max":0.5,"mean":0.4},"weekly":{"max":0.76,"mean":0.73}}],"daily":[]}
    """
   When `read_rollup` decodes it and the decoded `Rollup` is encoded again with `serde_json::to_string`
   Then the decode is `Ok`
    And hour 0 has no session statistic and its weekly `samples` is `None`
    And hour 3600's session statistic reads `max` 0.5, `mean` 0.4 and `samples` `None`
    And every statistic's `samples` is `None` — not recorded, a state the type keeps apart from every
        number
    And the re-encoded text, parsed as a `Value`, equals the input parsed as a `Value` — no count gained,
        no field lost
    But NOT a missing count read as the bucket's `count` — hour 3600 would then claim four samples
        behind a mean over two
    But NOT a missing count read as `0` — the store never writes an unknown as a number
    But NOT a legacy bucket refused — a bucket without a count is the shape every upgraded store
        holds, and refusing it would stop compaction and `stats` at their first read
    # No released build writes this shape: #1598 writes the format-2 envelope, with the count, from its
    # first compaction. Neither this shape nor T10's has a `format` key, so both read as the legacy
    # format 1; "re-encoding" is `serde_json::to_string` of the decoded `Rollup`, not `write_rollup`,
    # whose output adds the envelope. Shape one, the `fd8640b` rollup, is T10's: "a rollup written at
    # fd8640b reads back unchanged" above.

Scenario Outline: a same-key merge weights each axis by its recorded counts when both operands carry one (D13 as ruled)
  # Tier A — graded by usage_store::tests::a_same_key_merge_weights_each_axis_by_its_recorded_count
  # (#1598 AC-26 as absorbed, a new test beside T11, which keeps the not-recorded arm; no RED at
  # fd8640b: a new signature, which does not compile against origin/main; mutant, AC-26's record:
  # weighting by the bucket counts when both counts are recorded fails the first two rows, 0.6 against
  # 2/3) · covers AC-26, AC-5
  Given an existing bucket of `count` 4 whose session statistic is <a> and an incoming same-key bucket
        of `count` 4 whose session statistic is <b>
   When the two are merged by <helper>
   Then the merged session statistic is <merged>
    And through `upsert_hour` / `upsert_day` over two hand-built same-key buckets — the seam this step
        is graded at, since the two helpers take no bucket — the merged bucket's `count` is the sum of
        the two, and a day bucket's `coverage` is recomputed from it; an `HourBucket` has no `coverage`
    But NOT the bucket counts used as weights when both operands recorded a count — 0.6 where the
        pooled mean over 6 is 2/3

Examples:
  | helper            | a                                                 | b                                                        | merged                                                                              |
  | `merge_hour_stat` | `{max 0.5, mean 0.4}` over 2                      | `{max 0.9, mean 0.8}` over 4                             | `{max 0.9, mean 2/3}` over 6 — not the 0.6 the bucket counts give                   |
  | `merge_day_stat`  | `{max 0.5, mean 0.4, p95 0.5, cap_hits 0}` over 2 | `{max 1.2, mean 0.8, p95 1.2, cap_hits 1}` over 4        | `{max 1.2, mean 2/3, p95 1.2, cap_hits 1}` over 6                                   |
  | `merge_hour_stat` | `{max 0.5, mean 0.4}` over 2                      | `{max 0.9, mean 0.8}`, not recorded                      | `{max 0.9, mean 0.6}`, not recorded — weighted by the bucket counts, in both orders |
  | `merge_day_stat`  | `{max 0.5, mean 0.4, p95 0.5, cap_hits 0}` over 2 | `{max 1.2, mean 0.8, p95 1.2, cap_hits 1}`, not recorded | `{max 1.2, mean 0.6, p95 1.2, cap_hits 1}`, not recorded, in both orders            |
  | `merge_hour_stat` | `{max 0.5, mean 0.4}` over 2                      | absent                                                   | the present side unchanged, count included, in both orders                          |
  | `merge_day_stat`  | `{max 0.5, mean 0.4, p95 0.5, cap_hits 0}` over 2 | absent                                                   | the present side unchanged, count included, in both orders                          |
  | `merge_hour_stat` | absent                                            | absent                                                   | absent                                                                              |
  # The pooled mean is `(m_a·n_a + m_b·n_b) / (n_a + n_b)`: `(0.4·2 + 0.8·4) / 6 = 2/3`, where the two
  # bucket counts give `(0.4·4 + 0.8·4) / 8 = 0.6`. `max` stays the larger of the two, `p95` the larger
  # of the two, `cap_hits` the sum, and the merged statistic records `n_a + n_b` (#1598 Specification
  # § 5, the same-key merge). The path stays defensive: a same-key bucket is unreachable under
  # roll-once-per-whole-day, and reached only through `upsert_hour` / `upsert_day` on a duplicate key.
```

## Rule 5 — the records move with the shape: the module doc says "every dimension the account has", says what `samples` means, and states the store's format and the version-skew residual; the count posture is corrected wherever it was restated; and `CLAUDE.md` names the store's generation

```gherkin
Scenario: the module doc stops asserting both quota dimensions and states the version-skew residual
  # Tier B — pending: documentation. The mechanically-decidable half is #1598 AC-10's token sweep with
  # its canary —
  # `git grep -c -i -E 'per-dimension strict-early-fire' -- src docs/adr README.md CONTRIBUTING.md`
  # prints `src/swap.rs` on both trees, while
  # `git grep -n -i 'both quota dimensions' -- src/usage_store.rs` prints `src/usage_store.rs` at
  # fd8640b and nothing on the pull request — and
  # `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` for the intra-doc links;
  # the wording is the merge judge's. Nothing further is owed · covers AC-10, AC-14
  Given the module doc at
        `src/usage_store.rs:41-46`, heading `# Both quota dimensions, never one worst-case scalar`,
        whose paragraph says the store "projects neither window to a single blended number, because the
        two limits are independent and a reader may care about either"
   When this item lands
   Then the heading reads `# Every dimension the account has, never one worst-case scalar` — or the
        operator's rewording — and the paragraph keeps its point and loses its premise: the raw line
        carries an axis the account lacks as `null`, the key always written, never a fabricated `0`;
        a tier bucket omits that axis's statistic when none of its readings carried it; an older
        `stats` refuses the fenced rollup loudly, an older daemon meets the same refusal and stops
        compacting in silence, and only an older daemon over a rollup it can decode skips such a raw
        line and erases it on compaction
    And it states in one line the rule that keeps that loss unreachable — no pre-#1598 build runs as
        the daemon against the store from its first `null` line, by any start path — says the store
        files are never deleted or moved, quotes the older build's "malformed usage rollup" message as
        expected after #1598 (the ruling's C6), cites issue #1604 as the rule's owner, and does not
        cite the daemon-replacement runbook as carrying it
    And AC-10's canary prints the same line on `origin/main` and on the pull request, the corrected
        token returns nothing, and every line of AC-10's token sweep (23 at `fd8640b`) carries one
        disposition on the pull request — corrected in-band, propagated and owned elsewhere, or not
        restating the posture
    But NOT the "no blended scalar" point lost — only its premise moves
    But NOT a sweep keyed on citations to `usage_store` or to the ADRs — it reaches none of the
        `src/usage_store.rs` lines this sweep corrects, `:39` included, because none of them contains
        either
        word at `fd8640b`; that is the delta the token sweep exists for
    # The `Sample` doc's "four optional fields are omitted from the JSON entirely" (`:130-131`) gains
    # its complement — the two fraction keys are always present, `null` for a window the account lacks;
    # the field docs at `:144-147`, the bucket and statistic docs at `:232`, `:246`, `:255`, `:273`,
    # `:826`, `CAP_FRACTION` at `:106`, `weighted_mean` at `:778`, `max_of` at `:804` and `mean_of` at
    # `:809-810` move with it. The reading model's field docs
    # (`session window consumed` at `src/usage.rs:88`,
    # `:90`, `:119`) and the `usage_stats` module doc
    # (`BOTH quota dimensions` at `src/usage_stats.rs:17`)
    # are owned by #1596, #1597 and the `stats` item (block B-8); ADR-0033's "both lines are config"
    # (`docs/adr/0033-weekly-saturated-session-low-stays-unknown.md:118`) stays true and is #1593's.

Scenario: the module doc states the store's format, what an older and a newer build do with it, and what `samples` means (D6 and D13 as ruled)
  # Tier B — pending: documentation, graded through AC-14 by the source text,
  # `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` and the merge judge; the
  # two greps below decide the citation half; no test binds it · covers AC-14, AC-28
  Given the module doc of `src/usage_store.rs`
   When #1598 lands
   Then a section headed `# Store format and version skew` — or the operator's rewording — states: from
        this build's first compaction `usage-rollup.json` is `{"format":2,"rollup":{…}}`; a file with no
        `format` key is the legacy format 1 and reads as before; a `format` above this build's own is
        refused as written by a newer sessiometer; this build prints that refusal, and any other
        compaction failure, to its stderr, which reaches `daemon.err.log` only under a CLI-installed
        agent; on a store this build has compacted, an older daemon stops compacting without a word and
        erases nothing, an older `stats` fails with "malformed usage rollup", which is expected after
        this change (read the file's `format` key before treating it as damage), and an older
        `reliability` leaves `null` lines out of its report and deletes nothing; `usage-samples.jsonl`
        and `usage-rollup.json` are never deleted or moved; the four residuals the ruling accepts —
        lines appended before the first fenced write, a rollup deleted or moved by hand, an older
        `reliability`'s report, and every pre-#1598 build's `stats` and compaction on a machine that
        has run this build; and, in one line, that no older build runs as the daemon against a store
        holding a `null` line, citing issue #1604
    And the paragraph under `# Every dimension the account has, never one worst-case scalar` says a
        present statistic records how many of the bucket's samples it covers (`samples`), and one
        folded before that field existed reads as not recorded, never as the bucket's `count` and never
        as `0`
    And on the head `git grep -c '#1604' -- src/usage_store.rs` prints a non-zero count while
        `git grep -n 'replacing-the-running-daemon' -- src/usage_store.rs` prints nothing; the canary
        through the same command, `git grep -c 'fn read_rollup' -- src/usage_store.rs`, prints
        `src/usage_store.rs:1` on `origin/main` and on the head, so the empty result is an absence
    But NOT a sentence saying an older binary reads the count and drops it at its next rollup rewrite —
        AC-28's original wording, whose premise the fence made false: inside the envelope an older build
        refuses the whole rollup before it rewrites anything, so it never reads the count and never
        drops it
    But NOT the daemon-replacement runbook cited as carrying the rule — it carries none until #1603
        lands in tranche 2
    # The scenario above carries AC-10's heading move and token sweep, and asserts the fence's
    # version-skew clauses as well; this one carries the fence's section and the `samples` sentence of
    # the same module doc (#1598 Specification § 7). AC-28, as absorbed, is graded through AC-14, and
    # its original sentence is not written; the `samples` sentence is graded with it, and the doc
    # comments AC-5 names are AC-5's, graded by `cargo doc` and review.

Scenario: the count posture is corrected everywhere it was restated, found by its own vocabulary (D13 as ruled)
  # Tier B — pending: documentation, graded by the query, its canary, review and the doc build
  # (`cargo doc`, #1598 AC-27's seam); no RED is owed and no test binds it · covers AC-27
  Given the query, run exactly and with no pathspec:
        `git grep -n -i -E 'count-weighted|weighted_mean|fell in this|exactly mergeable|per-axis (sample )?count|bucket-wide|bucket counts?|whole sample count'`
   When it runs on `origin/main` and on the pull request's head
   Then as its canary it returns `src/usage_store.rs` with a non-zero count on both trees — the
        `weighted_mean` function and its `weighted_mean_pools_by_count` test match the `weighted_mean`
        alternative and survive #1598 unchanged; a zero for that file means the query is broken, not
        that the repository is clean
    And the pull request body reports the cardinality and the executed query verbatim
    And every returned line carries one disposition: corrected in-band · propagated as a tracked item
        carrying the corrected fact verbatim · recorded as not restating the posture, with the reason ·
        recorded as frozen by construction (a scoping-time record that must not be rewritten), with the
        reason
    And if the head adds vocabulary the token set misses, the set is widened, the delta (hits that name
        the vocabulary and cite neither AD-11 nor D13) re-run and the widened set recorded
    But NOT a sweep keyed on citations to AD-11 or D13 — `src/usage_store.rs` names the vocabulary and
        cites neither, which is why a citation-keyed sweep would miss it
    # The corrected proposition: "an axis statistic in a tier bucket records the number of samples it
    # is over; `count` is the bucket's whole sample count; a merge or a pooled read weights by the
    # recorded count". This stub returns under the query as well, and each of its returned lines takes
    # a disposition like any other.

Scenario: `CLAUDE.md` § Schema versions names the store's generation, its home and its bump rule (D6 as ruled)
  # Tier B — pending: documentation, graded by AC-30's greps and their canary; no RED is owed and no
  # test binds it · covers AC-30
  Given `CLAUDE.md` on the pull request's head
   When AC-30's commands run
   Then `git grep -n 'ROLLUP_FORMAT' -- CLAUDE.md` prints at least one line on the head and nothing,
        exit status 1, on `origin/main`
    And `awk '/^### Schema versions/,/^### The design mock/' CLAUDE.md | grep -c 'ROLLUP_FORMAT'` prints
        1 or more, so the name sits inside that section
    And `git grep -c 'the_written_store_shape_is_pinned_to_its_generation' -- CLAUDE.md src/usage_store.rs`
        prints a count for both files on the head
    And the canary through the same command, `git grep -c 'FORMAT_VERSION' -- CLAUDE.md`, prints a
        non-zero count on both trees (`CLAUDE.md:1` at `40d0e81`), so the section's existing constant
        is found before the new one is looked for
    And the paragraph names `src/usage_store.rs`, `usage-rollup.json` and the rule that a change to the
        written shape of either store file bumps the constant — `ROLLUP_FORMAT` is the store's on-disk
        generation, written as the top-level `format` key of `usage-rollup.json`, and a file with no
        key is generation 1
    But NOT the constant documented only in `src/usage_store.rs` — `CLAUDE.md` § Schema versions is
        where a contributor looks for what a shape change bumps
    But NOT a `src/<file>.rs:NNN` citation in the paragraph — no citation gate inspects it
    # The paragraph lands in #1598's pull request, right after the `FORMAT_VERSION` paragraph of
    # `CLAUDE.md`'s "Schema versions are four independent wires", in that section's voice (#1598
    # Specification § 10); this stub only specifies the scenario that grades it.
```

## Rule 6 — the rollup fence: every rollup this build writes is the `{"format":2,"rollup":{…}}` envelope, read by explicit dispatch on `format`; every older build refuses it before rewriting either file, a legacy rollup survives its first fenced compaction bucket for bucket, a newer generation is refused and left byte-identical, the written shape is pinned to its generation, and a stalled daemon says so

```gherkin
Scenario: every rollup this build writes is the format-2 envelope, by `write_rollup` alone and by `compact_and_roll` (D6 as ruled)
  # Tier A — graded by usage_store::tests::every_rollup_write_is_the_format_2_envelope (#1598 T12, the
  # ruling's C1; RED at fd8640b: `origin/main` writes the bare rollup, whose top-level keys are the three
  # inner ones, so the `["format", "rollup"]` key-set assertion fails) · covers AC-17
  Given `write_rollup` of `Rollup::default()` to a temp path
    And a temp store holding Rule 3's eleven raw lines, after one `compact_and_roll` at
        `now = 200 * DAY_SECS` with `RetentionPolicy::default()`
   When each rollup file is read and parsed as a `serde_json::Value`
   Then each is an object whose keys, sorted, are exactly `["format", "rollup"]`
    And its `format` is `2`
    And its `rollup` object's keys, sorted, are exactly `["daily", "hourly", "rolled_through_ts"]`
    And `read_rollup` reads back what was written
    But NOT a flat `"format":2` key beside the unchanged fields — an older build decodes it, so it fences
        nothing (measured by the council at the crate's locked `serde` 1.0.228 and `serde_json` 1.0.150)
    But NOT a path that writes the bare rollup — `write_rollup` is the only rollup writer, so the envelope
        is the only shape this build writes
    # `write_rollup` serializes `RollupFileOut { format: ROLLUP_FORMAT, rollup }` through its existing
    # atomic, mode-preserving write, and nothing else in it changes; `ROLLUP_FORMAT` is `2` (#1598
    # Specification § 9).

Scenario Outline: a corrupt envelope is reported at its own position, and a `format` that is not a number is refused (D6 as ruled)
  # Tier A — graded by usage_store::tests::a_corrupt_envelope_is_reported_at_its_own_position (#1598
  # T13, the ruling's C1; RED at fd8640b: the second row — the old reader ignores the unknown `format`
  # key and reads the file `Ok`; the first row is green there by another route, a missing-field error
  # at the end of the object) · covers AC-17
  Given a rollup file holding <file>
   When `read_rollup` reads it
   Then the result is <result>
    But NOT the second file read as the legacy shape — a `format` that is not a number is refused,
        never read as format 1
    But NOT a corrupt envelope reported at line 0, column 0 — what `#[serde(untagged)]` reports, "data
        did not match any variant of untagged enum", losing the field's own position

Examples:
  | file                                                                     | result                                                                                                             |
  | `{"format":2,"rollup":{"rolled_through_ts":"x","hourly":[],"daily":[]}}` | `Err(Error::UsageRollupMalformed { line: 1, column })` with `column` above 0 — never `untagged`'s line 0, column 0 |
  | `{"format":"2","rolled_through_ts":0,"hourly":[],"daily":[]}`            | `Err(Error::UsageRollupMalformed { .. })`                                                                          |
  # The probe's `format` is a `u32`, so a `format` that is not a number fails inside `serde` with its
  # own line and column, which `read_rollup` reports as `UsageRollupMalformed`, as it does today (#1598
  # Specification § 9).

Scenario: the dispatch on `format` is explicit, never `#[serde(untagged)]` (D6 as ruled)
  # Tier B — pending: a property of the source text, graded by AC-17's grep pair; no test binds it. The
  # grep prints nothing on `origin/main` too (measured at `40d0e81`), so the pair records absence on
  # both trees, never a RED · covers AC-17
  Given `src/usage_store.rs` on the pull request's head
   When `git grep -n 'untagged' -- src/usage_store.rs` runs
   Then it prints nothing and exits 1
    And the grep reads every line of the file, its test module included, so neither T13's messages
        and comments nor the docs may name the rejected attribute
    And the canary through the same command, `git grep -c 'fn read_rollup' -- src/usage_store.rs`,
        prints `src/usage_store.rs:1` on `origin/main` and on the head
    But NOT an empty result read as a pass without its canary — `git grep` prints nothing on no match
    # Under `#[serde(untagged)]` a corrupt envelope reports `data did not match any variant of untagged
    # enum` at line 0, column 0, and a `"format":3` file is accepted unless code checks the number; an
    # explicit `match` on the probe's `format` keeps the field's own error and position and gives a
    # higher number its own arm (#1598 Specification § 9).

Scenario: a rollup this build writes is refused by frozen copies of the fd8640b tier types, even when no bucket omits an axis (D6 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_rollup_this_build_writes_is_refused_by_the_fd8640b_tier_types (#1598 T14, the
  # ruling's C2; RED at fd8640b: both files decode into those types there, which are that build's own,
  # so the `Err` assertion fails; mutant, AC-18's record: `write_rollup` writing the bare rollup again,
  # the envelope dropped, fails T14 for both rollups — the silent loss of the fence the ratchet exists
  # to catch) · covers AC-18
  Given a nested test module `fd8640b_tiers` holding `Rollup`, `HourBucket`, `HourStat`, `DayBucket` and
        `DayStat`, copied field for field as `fd8640b` declares them — the five types from `Rollup` to
        `DayStat` at `src/usage_store.rs:223-287` at `40d0e81`, identical to `fd8640b`'s, whose
        `git show fd8640b:src/usage_store.rs` is the reference — deriving `Deserialize` only, the module
        `#[allow(dead_code)]` (its fields are decoded, never read), under the comment "Frozen at
        `fd8640b`: the tier types every pre-#1598 build decodes. Never edit: issue #1598's fence is
        graded against them."
    And two rollups this build writes: (a) the one `compact_and_roll` writes over an empty temp store;
        (b) the one it writes at `now = 200 * DAY_SECS` over a store whose aged cohort carries both axes
        on every line, `{"ts":14695200,"provider":"claude","acct":"work","session":0.3,"weekly":0.6}`
        and `{"ts":14695800,"provider":"claude","acct":"work","session":0.5,"weekly":0.62}`, so no
        bucket omits an axis
   When each file's bytes are decoded with `serde_json::from_slice::<fd8640b_tiers::Rollup>`
   Then each decode is `Err`, its message containing `missing field`
    But NOT the frozen types edited to track a later shape — edited types would decode the new shape,
        and the test would stop proving that an older build refuses it
    But NOT the test removed or `#[ignore]`d by a later commit — the ruling: "The test stays in every
        later commit"
    # Rollup (b) is there because D13's omitted axis alone fences only a bucket in which no sample
    # carried that axis — the lifted rollup without an envelope still decodes into the old types when
    # every bucket carries both axes (measured by the council) — so a bucket carrying both axes must be
    # refused by the envelope itself, which fails with ``missing field `rolled_through_ts` `` whatever
    # its buckets hold.

Scenario: a legacy rollup survives its first fenced compaction bucket for bucket, and comes out as format 2 (D6 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_legacy_rollup_survives_its_first_fenced_compaction_bucket_for_bucket (#1598
  # T15, the ruling's C3; RED at fd8640b: the `format` half — the pass writes the bare rollup there; the
  # preservation half is green there; mutant, AC-19's record: removing the legacy arm of `read_rollup`'s
  # dispatch, so a file with no `format` key is refused, fails T15 at the pass and T10 at its read) ·
  # covers AC-19
  Given a temp store whose raw file is empty and whose rollup file holds this literal, as `fd8640b`'s
        `write_rollup` emits it (#1598 T10's body, C1's committed `fd8640b` fixture):
    """
    {"rolled_through_ts":9,"hourly":[{"hour_start":0,"count":2,"session":{"max":0.5,"mean":0.4},"weekly":{"max":0.8,"mean":0.7}}],"daily":[{"day_start":0,"count":2,"coverage":0.5,"session":{"max":0.5,"mean":0.4,"p95":0.5,"cap_hits":0},"weekly":{"max":0.8,"mean":0.7,"p95":0.8,"cap_hits":0}}]}
    """
   When one `compact_and_roll` runs at `now = DAY_SECS` with `RetentionPolicy::default()`, so nothing
        ages out and no bucket leaves the hourly window
   Then the pass is `Ok`
    And `read_rollup` afterwards equals `read_rollup` of the literal before it — `rolled_through_ts`, and
        the `hourly` and `daily` tiers bucket for bucket
    And the file, parsed as a `Value`, now has `format` `2`
    But NOT a bucket dropped or altered by the rewrite — that first pass rewrites the lifetime daily tier
        (`write_rollup(rollup_path, &rollup)` in `compact_and_roll`, `src/usage_store.rs:543` at
        `40d0e81`)
    # The literal is T10's: Rule 4's "a rollup written at fd8640b reads back unchanged" reads it through
    # `read_rollup` alone, and here it goes through one fenced pass. The criterion exists because that
    # pass rewrites the lifetime daily tier, which cannot be rebuilt once its raw samples have aged out.

Scenario: a store a newer generation wrote is refused with its own error, and both files are left byte-identical (D6 as ruled)
  # Tier A — graded by
  # usage_store::tests::a_newer_store_format_is_refused_and_both_files_are_left_byte_identical (#1598
  # T16, the ruling's C4; RED at fd8640b: the old reader reports `UsageRollupMalformed` (missing
  # `rolled_through_ts`), so the not-malformed assertion fails; the byte-identical half is green there)
  # · covers AC-20
  Given a temp store whose rollup file is
        `{"format":3,"rollup":{"rolled_through_ts":9,"hourly":[],"daily":[]}}` and whose raw file holds
        Rule 3's lines at `ts` 14695200 (aged) and 17107200 (inside the raw window), both files' bytes
        read first
   When `compact_and_roll` runs at `now = 200 * DAY_SECS`, and then `read_rollup`
   Then both are `Err`, and neither is `UsageRollupMalformed`
        (`!matches!(err, Error::UsageRollupMalformed { .. })`)
    And each error's `to_string()` contains `newer sessiometer` and `must not be deleted`
    And both files' bytes are unchanged
    And `every_error_variant_enumerates_every_declared_variant` (`src/daemon.rs` tests) stays green,
        with the new variant listed in `every_error_variant()` in declaration order
    But NOT a newer store reported as malformed — which invites deleting it
    But NOT a newer store reset or rewritten — the newer build's data would be lost; the refusal
        leaves both files for that build
    # The variant's home and shape: `UsageRollupNewerFormat { found, supported }`, declared right after
    # `UsageRollupMalformed` (`src/error.rs:1136` at `40d0e81`), its `Display` carrying two integers
    # and nothing else (the redaction meter renders it), its exit code the generic `1`, as
    # `UsageRollupMalformed`'s is. Its text may be reworded; T16 reads `newer sessiometer` and
    # `must not be deleted` in it, so both phrases stay (#1598 Specification § 9).

Scenario: the written store shape is pinned to its generation, and a shape change without a bump turns the pin red (D6 and D13 as ruled)
  # Tier A — graded by usage_store::tests::the_written_store_shape_is_pinned_to_its_generation (#1598
  # T17, the ruling's C5; no RED at fd8640b: it does not compile against origin/main, where
  # `Sample::new` takes `f64` and `ROLLUP_FORMAT` does not exist; mutant, AC-21's record:
  # `#[serde(rename = "n")]` on `HourStat`'s `samples` without a bump of `ROLLUP_FORMAT` fails T17 at
  # its rollup pin) · covers AC-21
  Given three canonical values: `Sample::new(1_700_000_000, "claude", "work", Some(0.25), Some(0.5))`
        with resets `(Some(1_700_018_000), Some(1_700_600_000))`, severity `Some("warning")` and spend
        `Some(1.5)`; `Sample::new(1_700_000_000, "claude", "work", None, Some(0.5))` with resets
        `(None, Some(1_700_600_000))`; and a `Rollup` with `rolled_through_ts` 7200, one `HourBucket`
        (`hour_start` 3600, `count` 4, session `max` 0.5, `mean` 0.25, `samples` 2; weekly `max` 0.75,
        `mean` 0.5, `samples` 4) and one `DayBucket` (`day_start` 0, `count` 4, `coverage` 0.5, no
        session; weekly `max` 1.0, `mean` 0.5, `p95` 1.0, `cap_hits` 1, `samples` 4)
   When each sample is serialized with `serde_json::to_string` and the rollup is written by `write_rollup`
   Then the three texts equal their pins in a `GENERATION_2_SHAPE` constant declared beside the test,
        under a never-edit comment:
    """
    {"ts":1700000000,"provider":"claude","acct":"work","session":0.25,"weekly":0.5,"session_resets_at":1700018000,"weekly_resets_at":1700600000,"severity":"warning","spend":1.5}
    {"ts":1700000000,"provider":"claude","acct":"work","session":null,"weekly":0.5,"weekly_resets_at":1700600000}
    {"format":2,"rollup":{"rolled_through_ts":7200,"hourly":[{"hour_start":3600,"count":4,"session":{"max":0.5,"mean":0.25,"samples":2},"weekly":{"max":0.75,"mean":0.5,"samples":4}}],"daily":[{"day_start":0,"count":4,"coverage":0.5,"weekly":{"max":1.0,"mean":0.5,"p95":1.0,"cap_hits":1,"samples":4}}]}}
    """
    And `ROLLUP_FORMAT` equals the newest pinned generation, 2
    But NOT a generation-2 pin edited in place to absorb a shape change — a bump adds a generation-3 pin
        and never edits generation 2's
    # The field order is #1598 Specification § 5's: `samples` after `mean` in `HourStat` and after
    # `cap_hits` in `DayStat`. `ROLLUP_FORMAT` is bumped with any change to the written shape of either
    # store file, and T17 forces the bump; #1562's `account_uuid` on `Sample` is the next such change in
    # view (#1598 Specification § 9).

Scenario: only compaction rewrites the raw file, and it reads the generation first (D6 as ruled)
  # Tier B — pending: a property of the source's shape, graded by AC-21's grep compared base to head,
  # by the two greps keyed on the raw file's path, compared the same way, and the doc sentence by
  # review; no test binds it · covers AC-21
  Given `src` on the pull request's head
   When `git grep -n 'write_samples(' -- src` runs
   Then it prints the same three call sites as on `origin/main`: at `40d0e81`, the definition
        `fn write_samples` (`src/usage_store.rs:622`), the call in `compact_and_roll` (`:544`) after its
        `read_rollup` (`:536`), and the crash-ordering test's `write_samples` call (`:1495`), in
        `a_fault_at_every_write_boundary_loses_no_sample` after its own `read_rollup`
    And `write_samples`' doc states the rule: its one caller is `compact_and_roll`, after `read_rollup`
        has checked the generation, and any new caller checks it first, or it rewrites a store that a
        newer generation wrote
    And the raw file's routes and writers are listed by its path, not by one function's name:
        `git grep -n -E 'paths::usage_samples\(\)|samples_path: &' -- src` prints every route to the
        raw file's path and every function that takes it — sixteen lines at `40d0e81`, doc comments
        included, among them `append_sample`, `read_samples`, `compact_and_roll` and `write_samples` —
        and `git grep -n -E '(write_private_file|create_private_file|fs::write|File::create)\(&?samples_path' -- src`
        prints every write primitive handed it by that name — four lines at `40d0e81`:
        `append_sample`'s append, `write_samples`' rewrite and two tests that append a torn line
    And on the head each prints the lines it prints on `origin/main`, line numbers aside, and every
        line it adds is a reader, an append, or a rewrite that review shows to run after `read_rollup`
    But NOT a new caller of `write_samples` that has not read the rollup — it would rewrite a raw file
        that a newer generation wrote, with no generation check before it
    But NOT a rewrite of the raw file that bypasses `write_samples` — `paths::write_private_file` on the
        raw path from a new function leaves the `write_samples(` grep at three lines and passes T12 to
        T17; only the path-keyed greps list it
    # `compact_and_roll` already reads the rollup, and so checks the generation, before it rewrites
    # either file; the ruling's C5 is that order, kept by the doc sentence and the grep (#1598
    # Specification § 9).

Scenario: a daemon whose compaction fails says so on its stderr, and emits no event for it (D6 as ruled)
  # Tier B — pending: libtest captures stderr, so no test reads the line; graded by AC-22's grep pair,
  # the positive grep for the line #1598 § 9 writes out beside its canary, the green existing
  # `stats_events_for_poll` tests and the diff · covers AC-22
  Given `stats_events_for_poll` at a due roll, its `compact_and_roll` returning `Err`
   When the poll's store maintenance runs
   Then one line goes to the daemon's stderr in the form of `append_sample_for_poll`'s
        `eprintln!("sessiometer: usage-sample write skipped: {err}")` — the line #1598 § 9 writes out
        is `sessiometer: usage-store compaction skipped: {err}` — carrying only the store error's
        `Display`, which holds no secret
    And the `Ok` arm emits `usage_rollup` exactly as before
    And `git grep -n 'if let Ok(summary) = compact_and_roll' -- src` prints one line on `origin/main` —
        the `if let Ok(summary) = compact_and_roll(…)` at `src/daemon.rs:894` at `40d0e81` — and nothing,
        exit status 1, on the head
    And `git grep -n 'usage-store compaction skipped' -- src/daemon.rs` prints one line on the head, the
        `Err` arm's `eprintln!`, and nothing, exit status 1, on `origin/main`; its canary through the
        same command, `git grep -c 'usage-sample write skipped' -- src/daemon.rs`, prints a non-zero
        count on both trees (three at `40d0e81`), so the empty result on `origin/main` is an absence,
        never a broken query
    And the existing `stats_events_for_poll` tests in `src/daemon.rs` stay green — the error arm adds no
        event
    But NOT a new event kind for the stall — the event log is outside #1598
    But NOT the error swallowed in another arm — a stall without a word is the failure C7 exists to
        end
    # Where the line goes: to a file only under a CLI-installed agent, whose plist routes stderr to
    # `daemon.err.log`; the app bundle's agent plist routes stderr to no file, so for the app-registered
    # agent a stall shows only as `stats unavailable` on the socket verb and through #1604's read of the
    # rollup's `format` key. `eprintln!` stays the ruling's C7 mechanism (#1598 Specification § 9).
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
| AC-5 — the tiers' representation is the per-axis `Option`, and each present statistic records its sample count | Rule 4 "a bucket whose readings carried no session reading omits…" (the omitted member); Rule 4 "a rollup written at fd8640b reads back unchanged"; Rule 4 "a defensive same-key merge…"; Rule 4 "each present statistic records how many of its bucket's readings it is over…"; Rule 4 "the count invariant holds at both tiers for both axes…"; Rule 4 "a rollup written without the count…"; Rule 4 "a same-key merge weights each axis by its recorded counts…" | T8b; T10 for the backward read, every statistic's `samples` `None`; T11 for the merge arms; the tests of AC-23 to AC-26 for the count; the doc comments by `cargo doc` and review |
| AC-6 — the non-finite guard moves with the shape and is not deleted | Rule 2 "the canary's null arm moves…"; Rule 2 "the five non-finite spellings stay refused at decode" (Tier B); Rule 1 "a non-finite Some fraction is refused at serialization" | T6's moved `null` arm; the committed decode arms under AC-6's presence check; T6's near-end arm with its AC-13 RED run and AC-6's mutant record. T7 (`landing_refuses_a_non_finite_parked_reading_rather_than_fabricating_one`, `src/reliability.rs:7444`) stays green under the reliability rule's own move and has no scenario here |
| AC-7 — the round-trip test gains the `null` case for both axes | Rule 2 "a Sample round-trips with a null reading on either axis…" | T5, extended, with AC-7's mutant record for the weekly key |
| AC-8 — a line omitting the `session` key reads as no session reading | Rule 2 "a line omitting the session key reads as no session reading" | T4, written form |
| AC-9 — never a fabricated window value in the store or its tests | Rule 4 "a mixed bucket folds…" (the `mean` 0.2 BUT NOT); Rule 2's round-trip outline (the helper clause) | the `git grep` over the added lines for `unwrap_or(0.0)`, `unwrap_or_default()` and `Some(0.0)` is a review signal, not the gate; review reads every wrapper |
| AC-10 — the module doc's premise is corrected, with the token sweep | Rule 5 "the module doc stops asserting both quota dimensions…" (Tier B) | the canary grep, `cargo doc`, the merge judge |
| AC-11 — this item's share of the diff is bounded | none | `git diff --stat origin/main`, review; the pull request body names which commit carries the `stats` / reliability reader moves |
| AC-12 — gates | none | the five `test` steps, `msrv`, both `deny` checks, in CI; the ignored count equal to `origin/main`'s |
| AC-13 — the RED record exists | none | the pull request body's verbatim `FAILED` output of T1, T2, T3, T4 (written form), T8a, the canary's moved `null` arm and its serialization arm, T9, the rewritten #1613 test, and T12, T13 (its non-numeric `format` arm), T14, T15 and T16, each run by exact name against the dispatch-time `origin/main` and each printing `running 1 test`; the `FAILED` output of the scratch-only `read_rollup` test over the format-2 envelope `{"format":2,"rollup":{"rolled_through_ts":0,"hourly":[],"daily":[]}}`, which panics at `origin/main` with `UsageRollupMalformed` (the loud half of AC-14); the mutation records of AC-1 (T3, T9), AC-4 (T8a, T8b), AC-6 (T6), AC-7 (T5), AC-5 (T10, T11), AC-18 (T14), AC-19 (T15, T10), AC-21 (T17) and AC-23 to AC-26; the line naming the option built for the omitted-key reader and for the serialization guard; it is what turns every "RED at `fd8640b`" above from a reading of the source into a run |
| AC-14 — the version-skew facts, and the rule that backs the fence, are written where a reader of the store will see them | Rule 5 "the module doc stops asserting both quota dimensions…"; Rule 5 "the module doc states the store's format…"; Rule 3 "the version-skew asymmetry…" (Tier B, the behaviour) | the source text and `cargo doc`; the loud half's records are the scratch-only `read_rollup` run and T14, under AC-13 |
| AC-15 — commit convention and landing order | none | review; the merge order on `main` (this item's pull request after the compile-unit pull request that carries the reading-model commit, never before it) |
| AC-16 — the #1613 sample skip is replaced | Rule 1 "the #1613 sample skip is gone…" | the renamed test `collector_writes_a_null_session_line_for_a_reading_whose_session_window_was_absent`, with its AC-13 RED against the dispatch-time `origin/main`; the `git grep -n -i -E '1613\|hot-?fix' -- src` check, recorded beside the canary AC-16 names and the non-empty run on the dispatch-time `origin/main`; review |
| AC-17 — the rollup is written in one shape and read by explicit dispatch | Rule 6 "every rollup this build writes is the format-2 envelope…"; Rule 6 "a corrupt envelope is reported at its own position…"; Rule 6 "the dispatch on `format` is explicit…" (Tier B); Rule 4 "a rollup written at fd8640b reads back unchanged" (the committed fixture) | T12 and T13 with their AC-13 RED runs; T10; the `untagged` grep beside its canary |
| AC-18 — a ratchet holds the fence against every older build | Rule 6 "a rollup this build writes is refused by frozen copies of the fd8640b tier types…" | T14 with its AC-13 RED run and AC-18's mutant record |
| AC-19 — the first fenced compaction preserves a legacy rollup | Rule 6 "a legacy rollup survives its first fenced compaction…" | T15 (its `format` half RED per AC-13); AC-19's mutant record on T15 and T10 |
| AC-20 — a newer store is refused with its own error and left untouched | Rule 6 "a store a newer generation wrote is refused…" | T16 with its AC-13 RED run; `every_error_variant_enumerates_every_declared_variant` green with the new variant |
| AC-21 — one gate checks the generation before any rewrite, and the written shape is pinned to it | Rule 6 "the written store shape is pinned to its generation…"; Rule 6 "only compaction rewrites the raw file…" (Tier B) | T17 with AC-21's mutant record; the `write_samples(` grep, base against head; the two greps keyed on the raw file's path, base against head; review of `write_samples`' doc |
| AC-22 — a stalled daemon says so | Rule 6 "a daemon whose compaction fails says so on its stderr…" (Tier B) | the `if let Ok(summary) = compact_and_roll` grep pair; the `usage-store compaction skipped` grep beside its `usage-sample write skipped` canary; the existing `stats_events_for_poll` tests green; the diff |
| AC-23 (from #1610) `fold-records-each-axis-count` | Rule 4 "each present statistic records how many of its bucket's readings it is over…" | T8b's `samples` assertions with AC-23's two mutant records |
| AC-24 (from #1610) `count-invariants-hold-for-every-folded-bucket` | Rule 4 "the count invariant holds at both tiers for both axes…" | `count_invariants_hold_for_every_folded_bucket` with AC-24's mutant record |
| AC-25 (from #1610) `buckets-written-before-the-field-read-as-not-recorded` | Rule 4 "a rollup written at fd8640b reads back unchanged" (shape one); Rule 4 "a rollup written without the count…" (shape two) | T10; `a_rollup_without_the_count_reads_every_statistic_as_not_recorded`; AC-25's two mutant records |
| AC-26 (from #1610) `same-key-merge-weights-each-axis-by-its-recorded-count` | Rule 4 "a same-key merge weights each axis by its recorded counts…"; Rule 4 "a defensive same-key merge never fabricates an absent axis" (the not-recorded arm) | `a_same_key_merge_weights_each_axis_by_its_recorded_count` with AC-26's mutant record; T11 |
| AC-27 (from #1610) `corrected-posture-is-stated-everywhere-it-was-restated` | Rule 5 "the count posture is corrected everywhere it was restated…" (Tier B) | the query and its canary, base and head; the cardinality and one disposition per line in the pull request body; review and `cargo doc` |
| AC-28 (from #1610) `version-skew-is-written-where-a-reader-will-see-it` | Rule 5 "the module doc states the store's format…" (Tier B, its first `But NOT` and its `samples` clause) | graded through AC-14, as #1598 absorbs it: the source text, `cargo doc`, the merge judge |
| AC-29 (from #1610) `gates-are-green` | none | the commands AC-29 lists, in its order with flags verbatim: the five `test` steps, the `msrv` build and test, `./scripts/check-no-security-framework.sh` and `cargo deny check advisories sources licenses`; never `--include-ignored` |
| AC-30 — `CLAUDE.md` § Schema versions names the rollup's generation | Rule 5 "`CLAUDE.md` § Schema versions names the store's generation…" (Tier B) | AC-30's greps and its `FORMAT_VERSION` canary, base and head |
