# Feature: the parser serves every quota window a reply carries, and reports an absent window as `None`

Issue #1596 · PRD REQ-RPL-B-001 / REQ-QW-B-001 / REQ-RPL-C-001 / REQ-RPL-B-002 / REQ-RPL-B-003 / REQ-RPL-B-004 / REQ-REC-B-003 / REQ-REC-C-001 (§ 10.1, § 10.2, § 10.10; D1 and D2 in § 8) · design § 5 B-1, § 4.1, § 4.4, § 11 Cap-1, § 12 AD-1 / AD-2 / AD-3 / AD-19

Example Mapping: 🟦 5 rules · 🟩 15 examples · 🟥 0 open

> Since 2026-10-30 a verified Ultra account's `GET /api/oauth/usage` reply carries `"five_hour":null`
> beside a present `seven_day` window. `parse_usage` (`src/usage.rs:503-537`) ends each of its two
> `dimension` lookups in `?`, so the session miss is `Error::UsageParse("no session (five_hour)
> dimension")` before the weekly window is read; every poll of such an account is a failed poll, no
> sample is written, selection drops the account, and `next_swap` reports a healthy fleet out of
> capacity. This item makes `UsageReport.session` and `UsageReport.weekly` `Option<f64>`, serves
> every window a reply carries, and reports a window the reply does not carry as `None` — never a
> fabricated `0`. It lands in one pull request with the reading-model lift (#1597, design block B-2)
> and the decision-arms item (block B-3): the crate compiles only with all three. Its acceptance is
> graded at `parse_usage` alone.
>
> The `# Tier` line under each scenario is this stub's binding line (the `# Binds:` form of
> `docs/specs/first-sight-readout.feature.md`). Tier A names a grader the executor may not
> reinterpret: a test committed before dispatch and left unedited, or a test the issue names verbatim
> with its expected values, which the executor writes from that name and those values — here one of
> #1591's four oracles (landed `#[ignore]`d, RED-proven before any fix), one of #1592's eight tests
> (T1–T8, T8 the byte pin), a test #1596 names verbatim together with its RED at `fd8640b` or its
> mutant row in #1596 § 6, or a committed test #1596 forbids editing. Every "RED at `fd8640b`" below is
> read
> off `parse_usage` at that revision, never off a run: the runs are #1591's to post at its landing and
> AC-14's to carry. Tier B names what is still unbound.
> D1 and D2 are the operator's open decisions; every scenario below is written for the recorded
> default and says so in its name. Every rule, tier and name here is ratification-pending.

## Rule 1 — a window the reply does not carry is `None` on every one of its fields, never a fabricated `0`

```gherkin
Scenario: the captured null-session reply is served from its weekly window
  # Tier A — graded by
  # usage::tests::oracle_a_the_captured_null_session_reply_parses_with_its_weekly_window (#1591; RED at
  # fd8640b) · covers AC-1, AC-2
  Given the captured quota fields of a verified Ultra account's reply, the constant
        `CAPTURED_NULL_SESSION_QUOTA_FIELDS`: `"five_hour":null`; a `seven_day` object with
        `"utilization":90.0`, `"resets_at":"2026-11-06T22:59:59.916061+00:00"` and no `severity`;
        a two-entry `limits[]` holding only weekly kinds, `weekly_all` active at percent 90 and
        `weekly_scoped` inactive at percent 12
   When `parse_usage` runs over it
   Then the parse is `Ok`
    And the weekly fraction is within 1e-9 of 0.90
    And `weekly_resets_at` is `"2026-11-06T22:59:59.916061+00:00"`
    And `weekly_severity` is `None`
    But NOT `Err(UsageParse)` — the tree at `fd8640b` returns "no session (five_hour) dimension"
        before it reads the weekly window
    But NOT a weekly fraction of 0.12, the inactive `weekly_scoped` entry's
    # The weekly severity is `None` on purpose. `dimension` reads the top-level `seven_day` object
    # first, and that object carries no `severity`, so the active `weekly_all` entry's "critical" is
    # never reached. That provenance gap is a separate, pre-existing defect (#1590 and #1592's T4
    # record it); pinning today's `None` keeps the two defects from being conflated by this fix.
    # It is filed as #1606. If #1606 has landed before this item's branch step, this line reads
    # `Some("critical")`, as #1592's T4 then expects (#1596 AC-2).

Scenario: the captured reply reports no session window, never a fraction
  # Tier A — graded by
  # usage::tests::oracle_b_the_captured_reply_reports_no_session_window_never_a_fraction (#1591; RED at
  # fd8640b and against the fabricating mutant) · covers AC-2, AC-8
  Given the same captured quota fields
   When `parse_usage` runs over them
   Then the parse is `Ok`
    And the session fraction is `None`
    And `session_resets_at` is `None`
    And `session_severity` is `None`
    But NOT `Some(0.0)` or any fraction
    But NOT a reset stamp or a severity for a window that is not there
    # The mutant that fills a missing session window with 0.0 passes Oracle A and fails here, which
    # is why Oracle A alone does not grade the fix. The three `None`s together are invariant I-1 on
    # this body: `dimension` returns fraction, stamp and severity as one tuple, so an absent window
    # has none of the three.

Scenario: a reset stamp or a severity is present only with its fraction (invariant I-1)
  # Tier A — graded by usage::tests::a_window_stamp_or_severity_is_present_only_with_its_fraction (N3,
  # #1596 § 4b; green at fd8640b; its four mutant rows of #1596 § 6 — (i) the weekly stamp routed into
  # the session slot of the composed report, (iii) the session stamp into the weekly slot, (iv) the
  # session severity into the weekly slot, (v) the weekly severity into the session slot — are run on
  # the branch at landing and AC-14 carries each FAILED line) · covers AC-7
  Given every body in a fixed list: the five fixture bodies of #1591 and #1592, the session-only
        `limits[]` body of
        Rule 2, `{}`, and one two-window body such as the one in
        `parses_the_limits_array_shape_with_percent`
    And that list holds `SESSION_LIMITS_ONLY_WITH_STAMP_AND_SEVERITY`, a session-only body whose
        session window carries a reset stamp and a severity — the body on which a session stamp or
        severity routed into a weekly slot (#1596 § 6 (iii) and (iv)) fails the weekly axis
   When `parse_usage` runs over each
   Then for every `Ok` report, a `Some` session stamp or severity implies a `Some` session fraction
    And the same holds on the weekly axis
    # Green at fd8640b by construction: every served window has a fraction. Kept because a later
    # consumer may read a stamp without re-checking its fraction (design AD-3); it detects a parser
    # that spreads the lookup tuple over the wrong window's fields.

Scenario: the parser's contract states the rule
  # Tier B — pending: documentation — no test grades prose; the intra-doc link is decided by
  # `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` (named here, not
  # authored), the wording by the fresh-context merge judge · covers AC-10, AC-15
  Given the sentence "A window that cannot be found is a hard
        [`Error::UsageParse`]" at `src/usage.rs:473-474`
    And the two "tolerant of" bullets above it, naming `utilization` and
        `percent` (`src/usage.rs:467-471`)
   When the item lands
   Then that sentence is gone, the bullets stay, and the comment states that each window is
        independently optional, that a window the reply does not carry is reported as `None` (not
        applicable), never a fabricated `0`, and that a reply carrying no window at all takes D1's
        recorded answer
    And the intra-doc link to `Error::UsageParse` still resolves under the doc gate
    And the neighbouring "both dimensions" comments are corrected in the same commit:
        `UsageReport`'s (`src/usage.rs:149-157`), `to_usage`'s (`src/usage.rs:185-191`),
        `PolledReading`'s `usage` field (`src/usage.rs:124-126`) and
        `UsageParse`'s (`src/error.rs:556-560`)
    And the comment lands in the commit that moves the test of Rule 2, never before it
    # REQ-REC-B-003, routed here from #1593 (design block B-10). The ADR half is #1593's and merges
    # first — REQ-REC-C-001: no commit reverses a pinned behaviour without its record (AC-15).
```

## Rule 2 — every window is independently optional, and a present window is read as its `Some` value

```gherkin
Scenario: a session-only reply is served with its weekly window absent
  # Tier A — graded by usage::tests::oracle_d_a_session_only_reply_parses_with_its_weekly_window_absent
  # (#1591; RED at fd8640b and against both fabricating mutants) · covers AC-4, AC-8
  Given the body `{"five_hour":{"utilization":42.0,"resets_at":null},"seven_day":null,"limits":[]}`,
        the constant `SESSION_ONLY_QUOTA_FIELDS`
   When `parse_usage` runs over it
   Then the parse is `Ok`
    And the session fraction is within 1e-9 of 0.42
    And `session_resets_at` is `None`, because the body's `resets_at` is JSON `null`
    And the weekly fraction is `None`
    And `weekly_resets_at` and `weekly_severity` are `None`
    But NOT `Err(UsageParse)` — the tree at `fd8640b` returns "no weekly (seven_day) dimension"
    But NOT a weekly fraction of `Some(0.0)`
    # No such reply has been observed. The operator named it as the shape to expect next ("maybe
    # eventually they will make some account w/o weekly limits"); the PRD carries it as ASSUMED.

Scenario: the session-only limits[] body moves from "parse error" to "served", after its record lands
  # Tier A — graded by usage::tests::a_session_only_limits_body_is_served_with_its_weekly_window_absent
  # (the moved test, #1596 § 4c; RED at fd8640b: the body is Err(UsageParse)) · covers AC-4, AC-9, AC-15
  Given the committed test
        `a_missing_dimension_is_a_parse_error_not_a_fabricated_zero` (`src/usage.rs:959-965`)
    And its body `{"limits": [{"kind": "session", "percent": 10}]}`, which it asserts is
        `Err(UsageParse)` today
    And #1593's amendments to ADR-0013, ADR-0017, ADR-0025 and ADR-0033 are merged to `main`
   When the item lands
   Then the test carries the name above and asserts `Ok`, a session fraction within 1e-9 of 0.10,
        a weekly fraction of `None`, and `weekly_resets_at` and `weekly_severity` of `None`
    And its comment reads "a window the reply does not carry is `None`, never a fabricated `0` —
        the weekly window here is absent, not failed"
    But NOT the move landing before #1593 — no commit reverses a pinned behaviour without its record
    # Oracle D covers the top-level-object form of the same reading; this test keeps the `limits[]`
    # form the old test pinned, so both shapes of a session-only reply stay graded.

Scenario Outline: the committed two-window bodies keep parsing, each present window read as its Some value
  # Tier B — pending: the adapted fraction reads of <test> are executor-written, and of the two-window
  # bodies' bytes only the issue #66 capture
  # `parses_the_live_response_shape_utilization_on_a_0_to_100_scale` is pinned (#1592 T8; #1592's T1
  # freezes the five fixture bodies, none of them a row here) — the other rows' bytes rest on AC-9's
  # rule that `git diff origin/main -- src/usage.rs` shows no `-` line inside any `r#"…"#` literal
  # outside the one test #1596 § 7 deletes, a review read · covers AC-9
  Given the committed parse test <test> at `src/usage.rs` line <line>, over a body carrying <shape>
   When the lift of `UsageReport.session` and `UsageReport.weekly` to `Option<f64>` lands
   Then the test is green with <adaptation>
    But NOT a `Some(0.0)` expected value, an `unwrap_or(0.0)`, or an edited fixture body

Examples:
  | test                                                            | line | shape                                  | adaptation                                                                                              |
  | parses_the_limits_array_shape_with_percent                      | 703  | both windows in `limits[]`             | the two fraction reads at lines 711-712 read `Some`                                                     |
  | parse_widening_retains_severity_and_prefers_weekly              | 724  | three `limits[]` bodies                | the fraction read at line 767 reads `Some`                                                              |
  | parses_the_top_level_window_shape_with_utilization              | 773  | both top-level objects                 | lines 780-781 read `Some`                                                                               |
  | parses_the_live_response_shape_utilization_on_a_0_to_100_scale  | 785  | the issue #66 capture, frozen          | lines 801-808 read `Some`; the body is not edited                                                       |
  | parses_kind_aliases_in_the_limits_array                         | 813  | `five_hour` / `seven_day` kinds        | lines 822-823 read `Some`                                                                               |
  | prefers_an_active_limit_entry_over_an_inactive_duplicate        | 827  | a duplicated session kind              | line 838 reads `Some`                                                                                   |
  | falls_back_to_an_inactive_limit_when_no_active_one_exists       | 842  | an inactive `weekly_all` only          | lines 854-855 read `Some`                                                                               |
  | an_over_full_reading_clamps_to_a_full_window                    | 859  | 150 on both windows                    | lines 870-871 become `Some(1.0)`                                                                        |
  | resets_at_tolerates_a_numeric_timestamp                         | 875  | a numeric `resets_at`                  | no change; it reads only the stamps                                                                     |
  | non_json_body_is_a_parse_error                                  | 895  | "not json"                             | no change                                                                                               |
  | a_success_yields_the_reading                                    | 1183 | `limits[]` through the fake transport  | reads `Usage.session` / `Usage.weekly`; adapted in the B-2 item's commit (#1597) of the same pull request |
  # Rows 10 and 13 of #1596 § 4d are the moved test above and the `{}` scenario of Rule 5. The line
  # numbers are the tree at fd8640b (`origin/main`, 2026-10-30).
```

## Rule 3 — an unlisted `limits[].kind` neither fails the parse nor is read as a window

```gherkin
Scenario: the weekly-limits-only body is served from weekly_all and skips weekly_scoped
  # Tier A — graded by
  # usage::tests::a_weekly_limits_only_reply_is_served_from_weekly_all_and_skips_weekly_scoped (N2,
  # #1596 § 4b; RED at fd8640b: Err(UsageParse)) · covers AC-3
  Given the captured two-entry `limits[]` alone, with no top-level window object, the constant
        `CAPTURED_WEEKLY_LIMITS_ONLY`: `weekly_all` active at percent 90, severity "critical",
        `resets_at` "2026-11-06T22:59:59.916061+00:00"; `weekly_scoped` inactive at percent 12,
        severity "normal", `resets_at` "2026-11-06T22:59:59.916182+00:00"
   When `parse_usage` runs over it
   Then the parse is `Ok`
    And the session fraction is `None`
    And the weekly fraction is within 1e-9 of 0.90
    And `weekly_resets_at` is `"2026-11-06T22:59:59.916061+00:00"`
    And `weekly_severity` is `Some("critical")`
    But NOT 0.12, "2026-11-06T22:59:59.916182+00:00" or "normal" — the inactive, unlisted entry's
    But NOT a parse error on the unlisted kind
    But NOT a dependency on PR #1537's `max_active_weekly_scoped` semantics (decision D9 stays there)
    # Here the `limits[]` scan is taken, which the captured body's top-level `seven_day` object
    # short-circuits in Rule 1. The weekly severity is therefore "critical" here and `None` there.

Scenario Outline: at the lookup seam, a limits[] holding only weekly kinds yields no session window
  # Tier A — graded by
  # usage::tests::the_captured_null_session_reply_has_no_session_window_at_the_dimension_seam (#1592 T3;
  # its mutant row, deleting the kind filter, is run at #1592's landing) · covers AC-3
  Given the body <body>
   When `dimension` looks the session window up with the alias list `["session", "five_hour"]`
   Then it returns `None`
    But NOT `weekly_all` or `weekly_scoped` read as the session window

Examples:
  | body                                              |
  | CAPTURED_NULL_SESSION_QUOTA_FIELDS                       |
  | CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY |
  | CAPTURED_WEEKLY_LIMITS_ONLY                       |
  # The filter is the `is_some_and` … `continue` at `src/usage.rs:563-566`. The lookup itself is
  # unchanged by this item — `dimension` at `src/usage.rs:548-582` — which is what lets #1592's pins
  # hold on both sides of the fix.

Scenario: at the lookup seam, the weekly reading is weekly_all's, never weekly_scoped's
  # Tier A — graded by usage::tests::the_captured_weekly_limits_read_weekly_all_and_skip_weekly_scoped
  # (#1592 T5; its mutant row, inverting the kind filter, is run at #1592's landing — preferring the
  # inactive entry is ledger row 6's detection, not T5's) · covers AC-3
  Given the body `CAPTURED_WEEKLY_LIMITS_ONLY`
   When `dimension` looks the weekly window up with the alias list
        `["weekly_all", "seven_day", "weekly"]`
   Then it returns the fraction within 1e-9 of 0.90, the stamp "2026-11-06T22:59:59.916061+00:00"
        and the severity "critical", as one tuple
    But NOT 0.12, "2026-11-06T22:59:59.916182+00:00" or "normal"
```

## Rule 4 — `"five_hour":null` and an absent `five_hour` key read identically (D2 default)

```gherkin
Scenario: the absent-key reply is served exactly like the null one (D2 default)
  # Tier A — graded by usage::tests::the_absent_five_hour_key_reply_is_served_exactly_like_the_null_one
  # (N1, #1596 § 4b; RED at fd8640b: both Err(UsageParse), the expect fails) · covers AC-6
  Given `CAPTURED_NULL_SESSION_QUOTA_FIELDS` and
        `CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY`, which
        differ only in the one member `"five_hour":null,`
   When `parse_usage` runs over each
   Then both parses are `Ok` and the two reports are equal (`UsageReport` derives `PartialEq`)
    And in each: the session fraction, `session_resets_at` and `session_severity` are `None`; the
        weekly fraction is within 1e-9 of 0.90; `weekly_resets_at` is
        `"2026-11-06T22:59:59.916061+00:00"`; `weekly_severity` is `None` (`Some("critical")` if
        #1606 has landed first, #1596 § 4b N1)
    But NOT a test over only one of the two bodies
    But NOT an undocumented difference between them
    # Both bodies fall through to the `limits[]` scan today and no code distinguishes them. Under a
    # ruling of *different*, this scenario is replaced by the documented difference and a branch in
    # `parse_usage`; nothing else in this stub changes.

Scenario: the two bodies take the same outcome at both seams (D2 default)
  # Tier A — graded by usage::tests::a_null_five_hour_and_an_absent_five_hour_key_read_identically
  # (#1592 T6; its mutant row, a null-specific branch before the session lookup, is run at #1592's
  # landing) · covers AC-6
  Given the same two bodies
   When `dimension` looks each window up in both, and `parse_usage` runs over both
   Then the two session lookups are equal (both `None`) and the two weekly lookups are equal
    And the two parse outcomes are of one kind and equal: at `fd8640b` both `Err` with the same
        message; after this item both `Ok` with equal reports
    But NOT one `Ok` and one `Err`, in either order
```

## Rule 5 — a reply with no applicable window takes D1's answer, and a served reading carries at least one window

```gherkin
Scenario: a windowless reply is a hard parse error, so the loop skips and never swaps (D1 default)
  # Tier A — graded by usage::tests::oracle_c_a_reply_with_no_window_is_never_two_fabricated_windows
  # (#1591; green at fd8640b by construction under D1 (a), RED against the mutant that serves two 0.0
  # windows) · covers AC-5
  Given the body `{"five_hour":null,"seven_day":null,"limits":[]}`, the constant
        `WINDOWLESS_QUOTA_FIELDS`
   When `parse_usage` runs over it
   Then the outcome is `Err(Error::UsageParse(_))`
    And the error text names both windows and carries no response bytes — "no quota window (neither
        five_hour nor seven_day)" is the suggested wording; no test pins the text, and review reads it
    And `classify_poll` (`src/daemon/classify.rs:55-62`) maps that `Err` to `PollOutcome::Transient`,
        the same skip-never-swap path a rejected reply takes today
    But NOT `Ok` with two fabricated 0.0 windows
    But NOT a fraction of any kind
    # D1 (b), the alternative: `Ok` with both windows `None` — classifies `Live`, passes every
    # viability filter and never swaps away. No instance exists (Ultra keeps the weekly window).
    # Under (b) Oracle C takes its (b) form and the pull request body says which option was built.

Scenario: the empty object through the poll path stays a failed poll (D1 default)
  # Tier A — graded by usage::tests::a_200_with_an_unparseable_body_is_a_parse_error (committed at
  # fd8640b; stands unedited under D1 (a), #1596 § 4d row 13) · covers AC-5, AC-9
  Given a 200 reply whose body is `{}`, through the fake transport
   When the usage source polls
   Then the outcome is `Err(Error::UsageParse(_))` — no window applies, so D1's answer
    But NOT a served reading with both windows `None`, which is D1 (b), under which this test moves
    # `a_200_with_an_unparseable_body_is_a_parse_error` is at `src/usage.rs:1472`. It is a committed
    # grader the executor may not edit under the default, and the one that would fail if the
    # neither-window guard were dropped.

Scenario: a served reading carries at least one window — invariant I-2 (D1 default)
  # Tier A — graded by usage::tests::a_served_reading_carries_at_least_one_window (N4, #1596 § 4b; its
  # invariant half is green at fd8640b vacuously — every `Ok` report there carries both windows — and
  # its exact-`Err`-set half is RED there, where every body but the two-window one is `Err`; its mutant
  # row, #1596 § 6 (ii) — the neither-window guard deleted, D1 (a) only — is run on the branch at
  # landing and AC-14 carries its FAILED line) · covers AC-7
  Given the same fixed list of bodies as Rule 1's invariant scenario
   When `parse_usage` runs over each
   Then every body is either `Err(Error::UsageParse(_))` or `Ok` with a `Some` session fraction or
        a `Some` weekly fraction — the invariant, which holds at fd8640b as well
    And exactly the windowless body and `{}` are the `Err` ones — the half that is RED at fd8640b,
        where every body but the two-window one is `Err`
    # Under D1 (b) this test is not written: the windowless body is then `Ok` with both `None`.
```

## Coverage

Each acceptance criterion of #1596, with the scenarios that cover it. A criterion no scenario covers
is named as such, with what grades it instead.

| AC | Scenarios | What grades it |
|---|---|---|
| AC-1 — Oracles A–D un-ignored, green, unedited | the four Tier A oracle scenarios: Rule 1 "the captured null-session reply is served from its weekly window" and "the captured reply reports no session window, never a fraction"; Rule 2 "a session-only reply is served with its weekly window absent"; Rule 5 "a windowless reply is a hard parse error…" | the oracles themselves; the un-ignore and the "no other line inside the oracles, their constants, the helper `optional_fraction` or the companion" clause are read off `cargo test usage::tests::oracle_` printing `running 4 tests` (the bare filter `oracle_` also matches #1591's companion and prints `running 5 tests`) and the diff against `origin/main` — no scenario states them |
| AC-2 — the live null-session reply is served | Rule 1, the two oracle scenarios | Oracle A, Oracle B |
| AC-3 — an unlisted kind stays inert | Rule 3, all three | N2; #1592 T3, T5 |
| AC-4 — a session-only reply is served | Rule 2, "a session-only reply is served with its weekly window absent"; "the session-only limits[] body moves…" | Oracle D; the moved test |
| AC-5 — no applicable window takes D1's answer | Rule 5, "a windowless reply is a hard parse error…"; "the empty object through the poll path stays a failed poll" | Oracle C; `a_200_with_an_unparseable_body_is_a_parse_error`; the error text by review |
| AC-6 — null and absent key read identically | Rule 4, both | N1; #1592 T6 |
| AC-7 — invariants I-1 and I-2 pinned | Rule 1 "a reset stamp or a severity is present only with its fraction"; Rule 5 "a served reading carries at least one window" | N3, N4 — both Tier A: named, green today, each against its mutant rows of #1596 § 6 — (i), (iii), (iv) and (v) for N3, (ii) for N4 — which AC-14 carries |
| AC-8 — never a fabricated window value | Rule 1 "the captured reply reports no session window, never a fraction"; Rule 2 "a session-only reply is served with its weekly window absent" | Oracle B, Oracle D; the grep for `unwrap_or(0.0)`, `unwrap_or_default()` and `Some(0.0)` over the lines this item's own commits (subject carrying `(issue #1596)`) add under `src/`, not over the pull request head, is a secondary review signal, no scenario |
| AC-9 — the ledger is intact | Rule 2 outline (Tier B); Rule 2 "the session-only limits[] body moves…" (row 10); Rule 5 "the empty object…" (row 13) | `cargo test usage::tests`; the fixture-body freeze by review of the diff — only the five fixture bodies (#1592 T1) and the issue #66 capture (#1592 T8) carry a byte pin |
| AC-10 — the parser's contract rewritten | Rule 1 "the parser's contract states the rule" (Tier B) | the doc build for the link; the merge judge for the wording |
| AC-11 — propagation sweep | none | the `git grep` with its canary and the pull request body's per-line disposition |
| AC-12 — the diff is bounded | none | `git diff --stat origin/main`, review |
| AC-13 — gates | none | the five `test` steps, `msrv`, both `deny` checks, in CI |
| AC-14 — the RED record exists | none | the pull request body's verbatim `FAILED` output; it is what turns the "RED at fd8640b" of N1, N2 and the moved test from a reading of `parse_usage` into a run |
| AC-15 — the record precedes the moved test | Rule 1 "the parser's contract states the rule" (ordering clause); Rule 2 "the session-only limits[] body moves…" | the merge order on `main` |
| AC-16 — commit convention | none | review |
| AC-17 — the #1613 hot-fix retired | none | the two word-bounded greps AC-17 names (`git grep -nwE` for the two flag names, and `git grep -nwiE` for `1613` or `hot-fix`, both across `src/`), each printing nothing on the pull request head and read only beside its positive control, the same command against `origin/main` once #1613 has merged, printing at least one line; Oracle B, which supersedes #1613's deleted AC-1 test; and #1613's tests as #1596 § 7 rewrites or re-keys them |
| AC-18 — #1592's pins unedited | Rule 3 "at the lookup seam, the weekly reading is weekly_all's…" (T5) and the T3 scenario; Rule 4 "the two bodies take the same outcome at both seams" (T6) | the extraction-and-`git diff --no-index` loop AC-18 gives, printing nothing over all eight of #1592's tests against the pull request's base; it is what makes T3, T5 and T6 graders the executor may not edit |
