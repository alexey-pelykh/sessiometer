# Feature: `stats` says "no session window" on its wire, in its Swift mirror and in its table — never a zero reading

Issue #1601 · PRD REQ-STS-B-001 / REQ-STS-C-001 (§ 10.8; D8 in § 8; § 11.3; § 15.6; MET-T-003 in § 4) · design § 5 B-8, § 4.3, § 4.4, § 8, § 9a, § 11 Cap-5, § 12 AD-13 / OAD-3, § 14 DP-4 / R-D4 (D15 among its open questions)

Example Mapping: 🟦 5 rules · 🟩 28 examples · 🟥 0 open

> Since 2026-10-30 a verified Ultra account's `GET /api/oauth/usage` reply carries `"five_hour":null`
> beside a present `seven_day` window. Once the parser (#1596), the reading model (#1597), the sample
> store (#1598) and the five sample readers (#1600) land, `stats` computes a weekly-only account's
> figures over the readings it has — and its wire and table still have to say something in the session
> slots. Today they can only say a number: `dim_stats(&[])` is the finite zero summary
> (`src/usage_stats.rs`) and `Band::of(0.0)` is `Idle` (`src/stats.rs`), so `stats --json`
> carries `"session":{"mean":0.0,"peak":0.0,"p95":0.0}`, `"band":"idle"`, `"cap_hits":0` and
> `"time_at_cap_secs":0`, and the table prints `0/0/0` under `underused` (`signal_cell` collapses
> `Band::Idle` to that word) — a fabricated window value on a
> third surface (PRD MET-T-003: zero surfaces). This item makes `AccountStats.session`, `cap_hits` and
> `time_at_cap_secs` `Option`s, omits the four keys on `AccountWire` under `JSON_SCHEMA_VERSION` 2,
> regenerates `build/fixtures/wire-stats-basic.json` once, moves the Swift `StatsWire` mirror and its
> fixtures in lockstep, leaves the panel's Stats tab a non-inventing interim until #1594 renders what
> D15 classifies, and reuses the table's own `—` UNKNOWN sentinel, pinned by two new render goldens. It
> lands after #1600 (which lands in #1598's pull request), in one pull request, under either D8 answer.
>
> The `# Tier` line under each scenario is this stub's binding line (the `# Binds:` form of
> `docs/specs/first-sight-readout.feature.md`). The work class is deterministic output (a byte-pinned
> wire, a Swift decoder lockstep, two text goldens), so the B → A promotion is pulled forward to now:
> Tier A names a grader the executor may not reinterpret: a test committed before dispatch and left
> unedited, or a test the issue names verbatim with its expected values, which the executor writes
> from that name and those values. None of #1591's four oracles and none of #1592's eight tests reads
> the `stats` wire — they grade `parse_usage` and `dimension` — so Tier A here rests on (a) the tests
> #1601 § 6 names verbatim, each with its expected values in the body: W1–W11 for the aggregate, the
> wire and the table, and W12–W17 for the sparkline, the two text faces, the trend / heatmap picks and
> the weekly symmetry; and on (b) a test committed at `fd8640b` that the item leaves unedited, carried
> as a ratification-pending reading. A scenario still Tier B is graded by a read, a sweep or the merge
> judge and owes no test;
> every "RED" below is a reading of the tree at `fd8640b` (`origin/main`, 2026-11-01), never a run.
> D8 / OAD-3, the weekly symmetry of AC-13 (design D12) and D15 are open decisions; each scenario
> they reach is written for the recorded default and says so in its name. One contradiction inside
> the body is resolved here, ratification-pending: § 3 adds a weekly-only account to
> `statsPreviewFixtureJSON`, which seeds the committed `stats` panel frame
> (`apps/menubar/Sources/PanelRenderHarness.swift:301-303`), while AC-5 and AC-10 require zero panel
> drift and no `Panel-Goldens-Rebaselined:` trailer — so the fixture moves to `"schema":2` only
> (Rule 3), and the interim is graded by a unit test (Rule 4). Every rule, tier and name here is
> ratification-pending.

## Rule 1 — absence is carried as `None` from the aggregate, never reconstructed at the wire — and the weekly axis takes the same shape (AC-13 default)

```gherkin
Scenario: an account whose readings carry no session reading aggregates to None on every session figure and to a weekly summary
  # Tier A — graded by `an_account_with_no_session_reading_aggregates_to_none_never_a_zero_summary`
  # (`usage_stats::tests`; #1601 § 6 W1, its `spare` account), a row this item set pins by name with its
  # values, ratification-pending; AC-1's mutant `Some(DimStats::ZERO)` in the session empty arm makes it
  # FAIL, and #1600's T2 moves to the same `None` assertions · covers AC-1
  Given one account with three in-period samples whose `session` is `None` on each and whose `weekly` is
        `Some(0.3)`, `Some(0.5)`, `Some(0.7)` — the shape #1598 writes and #1600 reads
   When `aggregate_with_roster` (`src/usage_stats.rs:514`) runs over them
   Then `session == None`, `cap_hits == None` and `time_at_cap_secs == None`
    And `weekly == Some(DimStats { mean: 0.5, peak: 0.7, p95: 0.7 })`
    And `seen == 3`, `coverage` counts all three and `contribution_share` is as today — a weekly-only
        sample is an observation of the account
    But NOT `Some(DimStats::ZERO)` (`src/usage_stats.rs:1232`) and NOT `Some(0)` — the finite zero
        summary `dim_stats(&[])` returns (`src/usage_stats.rs:1242`) is a measurement nobody made

Scenario: a present session reading keeps every figure Some, computed over the present subset
  # Tier A — graded by `an_account_with_no_session_reading_aggregates_to_none_never_a_zero_summary`
  # (#1601 § 6 W1): its `mixed` account carries T1's values wrapped in `Some`, and its `work` account,
  # every reading present and under the cap, pins `cap_hits == Some(0)`, a present zero and never `None`
  # · covers AC-1
  Given the same account with `session` = `None`, `Some(0.9)`, `None` and `session_cap` 0.8
    And, as the contrast fixture, W1's `work` account, every reading present and under the cap
   When the aggregate runs
   Then `session == Some(DimStats { mean: 0.9, peak: 0.9, p95: 0.9 })`, `cap_hits == Some(1)` and
        `time_at_cap_secs == Some(<that one sample's validity window>)`
    And an account every one of whose samples carries a session reading aggregates to exactly today's
        values, each wrapped in `Some`

Scenario: the contribution-only account stores None, not the zero summary
  # Tier A — graded by `an_account_with_no_session_reading_aggregates_to_none_never_a_zero_summary`
  # (#1601 § 6 W1), its `dark` account, the active credential for every in-period reading and never
  # sampled, with AC-1's mutant in the contribution-only branch
  # (`src/usage_stats.rs:579-587`); the committed `contribution_share_credits_an_active_account_with_no_readings_of_its_own` asserts today's `DimStats::ZERO`
  # there (`:1634-1638`) and moves to `None` with this item · covers AC-1
  Given an account that was the active (swapped-in) credential for part of the period but was never
        itself sampled in it — today's `or_insert_with` branch (`src/usage_stats.rs:579`)
   When the aggregate runs
   Then its entry carries `seen == 0`, `coverage == 0.0`, its `contribution_share`, and `None` on both
        axes and both cap counts — the active time is known, its utilisation is not, which `None` says
        and `ZERO` never did
    And `DimStats::ZERO` keeps only the uses that remain, or is removed with its doc comment

Scenario: a reading whose weekly window is absent aggregates to a None weekly summary (AC-13 default; design D12)
  # Tier A — graded by `a_weekly_less_reading_aggregates_to_none_on_the_weekly_axis`
  # (`usage_stats::tests`; #1601 § 6 W2) for the aggregate, with the mutant `Some(DimStats::ZERO)` in
  # the weekly empty arm; the wire and table clauses below by
  # `a_weekly_less_entry_omits_the_weekly_key_and_prints_the_unknown_sentinel_in_its_weekly_cells`
  # (#1601 § 6 W16, mutant `a.weekly.unwrap_or(DimStats::ZERO)` in `account_wire`), and the Swift clause
  # by `MenubarTests/StatsTests/testAWeeklyLessStatsRowRendersTheTransitionalNAAndDropsTheWeeklyClause`
  # (W17, mutant: an absent `weekly` defaulted to `0` in `statsWeeklyPeak`) — both, like W2, written
  # only under AC-13's default. Under the operator's recorded overrule, `weekly` stays required and this
  # scenario is replaced by the `AccountWire.weekly` doc sentence AC-13 names · covers AC-13
  Given one account whose samples carry `weekly == None` and `session == Some(0.42)` — the account class
        the operator named as the future case, with no observable instance today
   When the aggregate runs
   Then `weekly == None` while `session`, `cap_hits` and `time_at_cap_secs` are `Some`
    And on the wire the `weekly` key is omitted for that entry, the Swift mirror decodes
        `weekly == nil`,
        and the table's `weekly m/p/p95` and `weekly` cells print `—`
    And the panel's `Weekly pk` cell reads the transitional `n/a` and the spoken summary drops its
        weekly
        clause — the D15 interim of Rule 4, ratification-pending
    But NOT a second `JSON_SCHEMA_VERSION` bump when that account class appears — the symmetry rides
        this one
```

## Rule 2 — the wire omits exactly the four session-keyed keys for such an account, keeps every other key, un-gates the velocity object, and moves `schema` 1 → 2 once, announced

```gherkin
Scenario: a weekly-only account's entry omits session, band, cap_hits and time_at_cap_secs and keeps the rest
  # Tier A — graded by
  # `the_stats_wire_omits_the_four_session_keys_for_an_account_with_no_session_window` (`stats::tests`;
  # #1601 § 6 W3), which pins `spare`'s compact bytes with the four keys omitted and `work`'s fd8640b
  # bytes under `"schema":2`; the committed byte pins compare against literals the executor regenerates,
  # so they are not independent here · covers AC-2
  Given a report with two summary accounts — `work`, every sample carrying both windows, and `spare`,
        every sample carrying no session reading (labels only; never an email or a token)
   When `stats_wire` (`src/stats.rs:2977`) is serialised compact (the socket frame) and pretty
        (`stats --json`)
   Then `spare`'s entry has no `session`, `band`, `cap_hits` or `time_at_cap_secs` key
    And `spare`'s entry keeps `seen`, `coverage`, `coverage_class`, `weekly` and `contribution_share` —
        a
        present entry with its weekly block, distinguishable by bytes from an account absent from the
        figures
    And `work`'s entry is byte-identical to today's, key order included
    And the document's `schema` is `2`
    But NOT `"session":{"mean":0.0,"peak":0.0,"p95":0.0}`, `"band":"idle"`, `"cap_hits":0` or
        `"time_at_cap_secs":0` — today's bytes for such an account (`account_wire`, `src/stats.rs:3051`)
    But NOT a sibling marker beside always-present zeros, and NOT `null` in place of an omitted key

Scenario Outline: the velocity object is emitted when either axis has a rate, with a nullable session rate
  # Tier A — graded by `the_velocity_object_rides_a_weekly_only_rate_with_a_null_session_rate`
  # (`stats::tests`; #1601 § 6 W4), one exact compact object per Examples row; RED at fd8640b on the
  # second row, where `let session = v.session_rate?;` (`src/stats.rs:3073`) drops the object · covers
  # AC-2
  Given a summary account whose `AccountVelocity` has `session_rate` <session> and `weekly_rate`
        <weekly>
   When `account_wire` (`src/stats.rs:3051`) attaches its velocity
   Then the `velocity` key is <object>
    And when present, `session_pct_per_min` is <session field> and `weekly_pct_per_min` is <weekly
        field>
    And each runway follows its own axis — `session_runway_secs` is `null` whenever the session rate is
        absent

  Examples:
    | session    | weekly     | object  | session field | weekly field |
    | Some(rate) | Some(rate) | present | a number      | a number     |
    | None       | Some(rate) | present | null          | a number     |
    | Some(rate) | None       | present | a number      | null         |
    | None       | None       | omitted | —             | —            |

Scenario: the version moves once, both byte pins are regenerated once by their emitters, and the golden carries the omission
  # Tier A — graded by
  # `the_committed_stats_golden_moves_to_schema_2_keeps_work_and_adds_an_entry_without_session_keys`
  # (`stats::tests`; #1601 § 6 W5), the baseline-integrity read of `WIRE_STATS_GOLDEN`: `"schema":2`,
  # `work`'s fd8640b entry bytes twice, and the second entry's key set exactly {`seen`, `coverage`,
  # `coverage_class`, `weekly`, `contribution_share`}; the two committed pins still go green only after
  # regeneration, both their sides the executor's · covers AC-3, AC-12
  Given `JSON_SCHEMA_VERSION` is `1` (`src/stats.rs:80`) and `build/fixtures/wire-stats-basic.json`
        carries one
        account, `work`, with all four session keys
   When this item lands
   Then `JSON_SCHEMA_VERSION` reads `2` and its doc comment says why: four existing keys became
        omittable,
        which a required-field decoder cannot absorb — breaking by the constant's own rule ("Bumped only
        on a breaking change"), unlike the additive precedents (#159, #160, #543, #804) that stayed at
        1,
        and by the `log` wire's own precedent for an honest bump (`src/log.rs`)
    And `wire_golden_report` (`src/stats.rs:7244`) gains a second account whose readings carry no
        session
        reading, so the regenerated golden pins the omission bytes under the cross-language guard, not
        only the digit
    And exactly `cargo test -- --ignored emit_wire_stats_golden_fixture` (`src/stats.rs:10991`)
        regenerates
        the golden once; `WIRE_GOLDEN` (`src/stats.rs:10100`) is updated to the pretty document; the
        `config_unreadable` assertion (`src/stats.rs`) asserts `2` with its message reworded
    And `src/log.rs`'s constant stays `2` (`:167`) and `src/reliability.rs`'s stays at `14`, where the
        compile-unit pull request moved it from `13` (#1602 § 3; `:264` at fd8640b) — the three
        offline wires version separately
    And the pull request body announces the bump and lists every consumer touched (the PRD's BUT NOT)
    But NOT a golden hand-edited, and NOT `cargo test -- --include-ignored` (it re-seals the frozen
        migration fixture)

Scenario: the vocabulary is stated where each absence is read
  # Tier B — pending: a doc build
  # (`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items`) and the merge judge's
  # read; no test grades prose, so no test is owed — the judge reads the sites named below against this
  # scenario, and runs AC-16's `git grep` with its control · covers AC-7, AC-16
  Given the doc comments of `AccountWire.session`, `.band`, `.cap_hits` and `.time_at_cap_secs`
        (`src/stats.rs`), the `—` sentinel's doc (`src/stats.rs`) `VelocityWire`'s docs
        (`src/stats.rs:2770-2797`), `AccountWire.velocity`'s doc (`src/stats.rs:2761-2765`) and
        `velocity_wire`'s doc
        (`src/stats.rs`)
   When this item lands
   Then each omitted key's doc states what its absence means: "omitted when the account's readings in
        this
        window carry no session reading — the account has no five-hour session window (or none was
        observed
        on it); never emitted as `0`"
    And the `—` doc says the table spends the same UNKNOWN token on "no session window", and that only
        the
        wire distinguishes the two absences (the omitted key against a present entry)
    And the velocity docs replace "Always a real number — the object is present only when it is known"
        with
        the un-gated rule and the nullable session rate, and the `AccountWire.velocity` and
        `velocity_wire` docs,
        which say the object is present only with a known session velocity, state the same rule
    And the five interim-representation docs that #1600's AC-7 plants (`dim_stats`, `DimStats::ZERO`,
        `AccountStats.session`, `.cap_hits`, `.weekly`) are rewritten, or removed with their items, so
        `git grep -n -E 'interim representation' -- src` prints nothing at the head — and prints at
        least one hit per site on #1600's merged head, the control (AC-16)
    But NOT a sentence that calls the omitted key `null`, or that calls the table's `—` "no window" — on
        the
        table it means UNKNOWN, "I could not see", on both regimes

Scenario: no site still states that the current stats wire is schema:1
  # Tier B — pending: AC-8 is a `git grep` read with its canary
  # (`build/fixtures/wire-stats-basic.json:1` matches at fd8640b) and the pull request body's per-site
  # disposition; no test grades a sweep, so no test is owed — the merge judge re-runs the query · covers
  # AC-8
  Given the body's query, verbatim:
        `git grep -n -I -E '"schema": ?1[,} ]|schema, 1[,)]|schema:1[^0-9]' -- . | grep -v -E 'wire-status|wire-watch|"minor"|STATUS_SCHEMA|statusB|watch'`
        → 26 hits at fd8640b, 25 of them `stats` sites (the other, `src/reliability.rs`, is that wire's
        own history)
   When the query is re-run at the branch head
   Then no hit states or assumes that the current `stats` wire is `schema:1`: the golden and the seven
        Swift
        sites are regenerated or moved to 2; the module doc (`src/stats.rs`), the section header
        (`:2623`)
        and the test comments are corrected to 2 or to a digit-free "the versioned wire contract"; the
        four
        precedent notes (`:2640`, `:2660`, `:2701`, `:2764`) read "additive, no bump (the #159/#160
        precedent)"
    And the four sites outside code (`docs/adr/0035-at-cap-time-stays-cadence-bounded.md:54`,
        `docs/design/credential-grant-survival-solution-design.md:187`,
        `docs/specs/stats-census-coverage-gate.feature.md:65`,
        `docs/specs/first-sight-readout.feature.md:233`)
        are read by hand — neither query returns a `docs/` hit (both were run over `docs` at fd8640b and
        print
        nothing) — and stay as frozen or still-true records; the pull request body owes them no per-site
        disposition
    And the body carries the query, the cardinality at fd8640b and at the head, the dispositions and the
        canary hit

Scenario: ADR-0035 stands — the keys' presence moves, the at-cap rule and its discriminator decision do not
  # Tier B — pending: `git diff --stat origin/main -- docs/adr` (empty) and the doc read are review
  # reads; the committed
  # `the_two_at_water_figures_diverge_by_an_order_of_magnitude_on_one_reading_set` (`src/usage_stats.rs:1691`)
  # pins the cadence-bounded rule but reads `time_at_cap_secs` as a value and is executor-adapted to the
  # `Option`, so it is not independent here; no further test is owed — the judge reads the doc paragraph
  # · covers AC-9
  Given `AccountWire.time_at_cap_secs`'s doc (`src/stats.rs`) records why no rule-discriminator key
        was added and names `docs/adr/0035-at-cap-time-stays-cadence-bounded.md`
   When this item lands
   Then no discriminator key is added, `time_at_cap_secs` stays cadence-bounded, and the ADR file is
        unedited
    And the doc keeps every ADR-0035 sentence and gains one: `JSON_SCHEMA_VERSION` moved to 2 in this
        change
        for omittability, not for the discriminator
    But NOT ADR-0035 amended — #1593 owns the four in-place amendments and this item owes none
```

## Rule 3 — the Swift mirror decodes the omission in the same bundle: `nil` for an omitted key, a throw for a present-but-invalid one, every `"schema":1` site moved, one pull request (D8 default)

```gherkin
Scenario: a schema-2 frame whose weekly-only entry omits the four keys decodes with nil in each
  # Tier A — graded by
  # `MenubarTests/StatsTests/testDecodesAWeeklyOnlyEntryWithTheFourSessionKeysOmitted` (#1601 § 6 W6),
  # RED read off fd8640b — `let session: StatsDim` is required
  # (`apps/menubar/Sources/WireModel.swift:1010`) and `decodeStatsReply` throws on a missing required
  # field (`:1169`); the run is AC-4's to record with its executed-test count · covers AC-4
  Given the regenerated `Fixtures.statsBasic` — `"schema":2`, `work` with all four keys, `spare` without
        them
   When `decodeStatsReply` reads it
   Then it returns `.ok(wire)` with `spare`'s `session == nil`, `band == nil`, `capHits == nil`,
        `timeAtCapSecs == nil`
    And `work`'s entry decodes to today's values (`session.mean == 0.5`, `capHits == 1`,
        `band == .high`)
    And `wire.schema == 2`
    But NOT a thrown error, and NOT a defaulted `StatsDim(mean: 0, peak: 0, p95: 0)` or `capHits == 0`

Scenario: a present band with an unknown value still throws
  # Tier A — graded by the committed `MenubarTests/StatsTests/testUnknownBandIsADecodeError`
  # (`apps/menubar/Tests/StatsTests.swift:167`), which rewrites `work`'s `"band":"high"` to
  # `"band":"nova"` in `Fixtures.statsBasic` and asserts a throw; it compiles and runs unedited once
  # `band` is `StatsBand?` decoded with `decodeIfPresent`, because a present key with an unknown raw
  # value still throws — a ratification-pending category (b) reading, as the sibling stubs carry it ·
  # covers AC-4
  Given `Fixtures.statsBasic` with `work`'s `"band":"high"` replaced by `"band":"nova"`
   When `decodeStatsReply` reads it
   Then it throws — a drifted daemon degrades loudly, exactly as today
    But NOT `band == nil` — optional means "key absent", never "value unrecognised"

Scenario: a present but malformed session block still throws
  # Tier A — graded by `MenubarTests/StatsTests/testAMalformedPresentSessionBlockStillThrows` (#1601 § 6
  # W7), green at fd8640b, with the mutant that decodes the key through `try?` (swallowing the error
  # into `nil`) → FAILS · covers AC-4
  Given `Fixtures.statsBasic` with `work`'s `"session":{"mean":0.5,"peak":0.9,"p95":0.85}` replaced by
        `"session":{"mean":"x"}`
   When `decodeStatsReply` reads it
   Then it throws
    But NOT `session == nil` — `decodeIfPresent` reads absence as `nil` and a present wrong shape as an
        error

Scenario: the Swift golden fixture follows the regenerated golden byte for byte
  # Tier A — graded by the committed `MenubarTests/WireGoldenTests/testStatsFixtureMatchesRustGolden`
  # (`apps/menubar/Tests/WireGoldenTests.swift:114`), which the item leaves unedited and which fails
  # structurally after the regeneration until `Fixtures.statsBasic` follows — the `stats` wire's
  # fixture-integrity pin, read as the sibling stubs read `WireGoldenTests` (ratification-pending
  # category (b)) · covers AC-4, AC-12
  Given the golden regenerated with the second account
   When the `swift` job's test bundle runs
   Then `Fixtures.statsBasic` (`apps/menubar/Tests/Fixtures.swift:367-368`) equals
        `build/fixtures/wire-stats-basic.json` byte for byte — pasted verbatim, one long line, as today
    And the executed-test count is read, never `TEST SUCCEEDED` alone
    But NOT a fixture edited by hand to a shape the emitter did not write

Scenario: every schema:1 site under apps/menubar moves to 2 or is recorded as deliberately pinned
  # Tier A — graded by `MenubarTests/StatsTests/testTheConfigUnreadableFixtureIsTheGoldenPlusItsOneKey`
  # (#1601 § 6 W8), asserting `Fixtures.statsConfigUnreadable` equals `Fixtures.statsBasic` with
  # `,"config_unreadable":"…"` inserted before the final `}`, so the two cannot drift; AC-4's two greps
  # stay the review read for the seven sites · covers AC-4
  Given the seven Swift sites at fd8640b: `Fixtures.swift:367-368` and `:385-386`;
        `StatsTests.swift:56`, `:134`,
        `:745`, `:922`; `PanelStatsModel.swift:203` (`statsPreviewFixtureJSON`)
   When this item lands
   Then `statsBasic` is the regenerated golden; `statsConfigUnreadable` is that plus the
        `config_unreadable`
        key, its doc's derivation kept; the three `StatsTests` assertions and the `:745` input literal
        read `2`, the `:134` message reworded
        ("an additive field, so no bump of its own — the version is 2 for the omittable keys")
    And `statsPreviewFixtureJSON` reads `"schema":2` with its three accounts unchanged
    And both greps return nothing, or only sites the pull request body lists as deliberately pinned with
        a reason
    But NOT a fourth, weekly-only account added to `statsPreviewFixtureJSON`: that fixture seeds the
        committed
        `stats` panel frame
        (`PanelRenderFixture(name: "stats", … statsWire: PanelStatsModel.loadedPreviewFixture)`,
        `apps/menubar/Sources/PanelRenderHarness.swift:301-303`), so a fourth card would move a
        committed
        panel golden and owe the re-baseline AC-5 and AC-10 rule out — § 3's "also gains one weekly-only
        account" is read here as the contradiction to drop, ratification-pending; Rule 4's unit test
        grades
        the interim instead

Scenario: the reshape, the mirror, the fixtures and the goldens land in one pull request under either D8 answer (D8 default: follow, as #1590 places this item and #1604 reads a missing answer)
  # Tier B — pending: the pull request's composition is a read (`gh pr view <N> --json files,body`), and
  # no test grades a delivery boundary; the mechanism it protects is itself tested — the committed
  # `testStatsFixtureMatchesRustGolden` fails on any split that regenerates the golden without the
  # fixture — so no further test is owed · covers AC-11
  Given the daemon and the app ship as one bundle, and an un-updated app's `decodeStatsReply` throws on
        a frame
        omitting `session` (`apps/menubar/Sources/WireModel.swift:1010`, `:1169`) — losing the whole
        Stats
        tab, not one row
   When this item lands
   Then the Rust reshape, the Swift optionals, both Swift fixtures, the regenerated wire golden and the
        two
        render goldens are in one pull request
    And its body records which D8 answer was ratified: ride first (tranche 1, right after #1600's pull
        request)
        or follow (tranche 2), the tracker's default, recorded as "follow — no recorded answer" when the
        operator
        has not answered (the merge does not wait for an answer) — the item's shape is identical under
        both;
        the design recommends ride first
        (OAD-3), to be ratified with D8 as one question
    And the fleet's deployment item of #1590 rebuilds the bundle after it
    But NOT the daemon half merged ahead of the app half, and NOT the app half ahead of the daemon's
        golden
```

## Rule 4 — the panel's Stats tab decodes such a row without crashing and invents nothing: a non-inventing interim until #1594 renders what D15 classifies (D15 default)

```gherkin
Scenario: a decoded row with no session dimension renders the transitional n/a, no signal pill, and a weekly-only spoken summary (D15 default interim)
  # Tier A — graded by
  # `MenubarTests/StatsTests/testAWeeklyOnlyStatsRowRendersTheTransitionalNAAndSpeaksOnlyItsWeeklyPeak`
  # (#1601 § 6 W9), over `statsSessionMeanPeak`, `statsWeeklyPeak`, `statsSignal` and the Cap-hits value
  # and spoken summary lifted out of `StatStripRow`, with two mutant rows: (i) default the absent
  # dimension to `0` → the cell reads `0 / 0%` → FAILS; (ii) draw a pill for a `nil` band → FAILS ·
  # covers AC-5
  Given a decoded `StatsAccountStats` with `session == nil`, `band == nil`, `capHits == nil` and
        `weekly.peak == 0.9`
   When the Stats row renders (`apps/menubar/Sources/StatusPanelStats.swift:197-199`, `:216-224`)
   Then the `Session m/pk` cell and the `Cap-hits` cell read `n/a` — the token the panel's own row
        already
        spends on an unknown session reading (`apps/menubar/Sources/StatusPanelFormat.swift:865`), under
        the
        same D10 transition
    And no signal pill is drawn — an omission, not a token
    And the spoken summary is "<handle>[, active]. Weekly peak 90 percent." — the signal, session and
        cap-hit
        clauses dropped
    And the `Weekly pk` cell carries `90%`
    But NOT `0 / 0%`, `0`, an `underused` pill, "Session mean 0 percent" or "0 cap hits"
    But NOT the `—` observed-absence token, an empty cell or a new register row — those pre-empt D0 /
        D15
        (`apps/menubar/design/README.md:1289-1291`); #1594 replaces this interim

Scenario: the row's sparkline draws no line for an account with a no-session bucket, never a floor line (D15 default interim)
  # Tier A — graded by
  # `MenubarTests/StatsTests/testSparkSeriesIsEmptyForAnAccountWithANoSessionBucketNeverAFloorLine`
  # (#1601 § 6 W12), over `StatusPanelFormat.sparkSeries`
  # (`apps/menubar/Sources/StatusPanelFormat.swift:2613-2615`), with two mutants: the floor-line pick
  # `?.session?.peak ?? 0` → FAILS on `spare` and `mixed`, and the rule without its `seen > 0` clause
  # (`session == nil` alone) → FAILS on `dark`; the default is #1601 § 3's, ratification-pending ·
  # covers AC-5
  Given three buckets: `work` (session peak 0.9) in buckets 0 and 2, `spare` (no session reading) in
        buckets 0
        and 2, `mixed` with a present session in bucket 0 and none in bucket 1, and `dark` — a healthy
        account —
        with session peak 0.9 in buckets 0 and 2 and, in bucket 1, a contribution-only entry (`seen` 0,
        no session
        reading: it was active in that bucket and never sampled in it)
   When `sparkSeries` picks each handle's series
   Then `spare`'s and `mixed`'s series are empty, so `sparkPoints` returns no point and the `Sparkline`
        view's
        `points.count >= 2` guard (`apps/menubar/Sources/StatusPanelStats.swift:282`) draws nothing
    And `work`'s series is `[0.9, 0.0, 0.9]` — a bucket with no entry for the handle keeps today's floor
    And `dark`'s series is `[0.9, 0.0, 0.9]` — a `seen == 0` entry keeps today's floor like an absent
        entry, so a
        healthy account that was merely unsampled in one bucket keeps its line (the `seen > 0` key of
        #1601 § 3,
        ratification-pending); only a sampled entry without a session reading empties the series
    And the `sparkSeries` doc and the `Sparkline` comment (`StatusPanelStats.swift:275-281`) say that an
        account
        with no session window draws no line
    But NOT a flat floor line with an area fill under the `n/a` cells — a fabricated zero
    But NOT per-bucket gaps drawn as a broken path — rendering work #1594 owns under D15

Scenario: a row with every dimension present renders exactly as today
  # Tier A — graded by the committed `MenubarTests/StatsTests/testStatsNumericCells`
  # (`apps/menubar/Tests/StatsTests.swift:312`: `statsSessionMeanPeak(work) == "50 / 90%"`,
  # `statsWeeklyPeak(work) == "40%"`) and `testSignalCollapsesTheBandLikeTheCLI` (`:277`), which compile
  # unedited against the optionals (Swift promotes a non-optional argument) and fail if the present arm
  # regresses — a ratification-pending category (b) reading · covers AC-5
  Given `work` decoded from the regenerated `statsBasic`
   When its row renders
   Then `Session m/pk` reads `50 / 90%` and `Weekly pk` `40%` — the two cells the committed test
        asserts — and the band collapses to `saturated` as today
    And the `Cap-hits` cell reads `1` and the spoken summary carries all four clauses as today — read
        by W9's row test (#1601 § 6), since no committed test asserts either

Scenario: the panel shows zero drift under the armed golden gate and owes no panel re-baseline
  # Tier A — graded by the committed, armed
  # `MenubarTests/PanelGoldenParityTests/testEveryRenderMatchesItsCommittedGolden` and
  # `testEachFreshRenderIsNearestToItsOwnGolden` under `TEST_RUNNER_SESSIOMETER_PANEL_GOLDEN_GATE=1`,
  # read off the `[panel-goldens] max drift …` line with none skipped; they grade that no authored
  # golden moved, never the unauthored interim state · covers AC-5, AC-10, AC-12
  Given no committed panel golden carries a weekly-only account, and `statsPreviewFixtureJSON` keeps its
        three accounts
   When the armed run executes both comparisons
   Then the drift line is within the ceiling and `Executed 2 tests` with none skipped
    And no `Panel-Goldens-Rebaselined:` trailer is owed, and the panel builds nothing against
        `apps/menubar/design/menubar-preview.html` for this state — the mock authors none, and the pull
        request body says so
    But NOT a green read off `TEST SUCCEEDED` with `2 tests skipped` — an unarmed run compared nothing
```

## Rule 5 — the CLI `stats` human views show such an account as a gap, never a zero: the table's own `—` UNKNOWN sentinel in every session-keyed cell, blank buckets in the trend and the heatmap, no rank in the `lowest utilisation` line, a bare gauge row — one vocabulary, pinned by two new render goldens

```gherkin
Scenario: the piped table prints — in the signal, session, caps, t@cap, velocity and runway cells of a weekly-only account
  # Tier A — graded by
  # `every_session_keyed_cell_prints_the_unknown_sentinel_for_an_account_with_no_session_window`
  # (`stats::tests`; #1601 § 6 W10), direct cells over `spare` in the shape of the committed
  # `signal_cell(&stat(0, …)) == "—"` assertion (`src/stats.rs:4975`), RED read off the tree after #1600
  # (`session_cell` prints `0/0`, `col_caps` prints `0`, `col_time_at_cap` prints `0s`); the new golden
  # `stats-weekly-only-piped` stays the merge judge's read · covers AC-6
  Given a report with `work` (both windows) and `spare` (no session reading) rendered piped — `cols`
        `None`,
        the wide numeric column set (`piped_columns`, `src/stats.rs:3637`), zero ANSI
   When `render_human` runs
   Then `spare`'s row reads `—` in `signal` (`signal_cell`, `src/stats.rs:2498-2504`), `session m/p/p95`
        (`triple`, `:2156`), `caps` (`col_caps`, `:3508`), `t@cap` (`col_time_at_cap`, `:3519`),
        `velocity`
        and `runway` (already `—`, `:2534`, `:2545`)
    And `spare`'s `cov`, `weekly m/p/p95` and `share` cells carry its figures
    And `work`'s row is byte-identical to today's
    But NOT `0/0/0`, `0`, `0s` or `underused` — the `0`-shaped values the cells print today over
        `dim_stats(&[])`

Scenario: the wide colour table prints — with no SGR in the session and signal cells of such an account
  # Tier A — graded by
  # `every_session_keyed_cell_prints_the_unknown_sentinel_for_an_account_with_no_session_window` (#1601
  # § 6 W10), its no-SGR rows: `signal_sgr` (`src/stats.rs:2510`) and `col_session_compact`'s colour
  # closure (`:3474`; today `Some(Band::of(r.stats.session.peak).sgr())` at `:3481`) return `None` for a
  # `None` session, with the mutant that passes a default `Band` → an `idle` tint under a `—` → FAILS;
  # the new golden `stats-weekly-only-wide` stays the merge judge's read · covers AC-6
  Given the same report rendered on a wide TTY with colour (`tty_columns`, `src/stats.rs:3657`)
   When `render_human` runs
   Then `spare`'s compact `session` cell (`session_cell`, `:2524`) and `signal` cell read `—` wrapped in
        no SGR
    And `spare`'s `weekly` cell carries its weekly peak with its weekly tint, as today
    But NOT a `—` tinted by `Band::Idle`'s SGR — the tint classifies a reading nobody made

Scenario: a roster that is entirely weekly-only keeps the caps and t@cap columns and elides velocity and runway
  # Tier A — graded by
  # `an_entirely_weekly_only_roster_keeps_caps_and_t_at_cap_and_elides_velocity_and_runway`
  # (`stats::tests`; #1601 § 6 W11), which pins the piped header to
  # `account · signal · cov · session m/p/p95 · weekly m/p/p95 · caps · t@cap · share`; the outcome is
  # read off `render_account_table`'s retain at fd8640b
  # (`src/stats.rs:3772-3777`: a column elides only when `priority.is_some()` and every cell is the gap;
  # `col_caps` and `col_time_at_cap` carry `priority: None`, `:3508-3530`), ratification-pending ·
  # covers AC-6
  Given a report whose every account carries no session reading, rendered piped
   When `render_human` runs
   Then the `signal`, `session m/p/p95`, `caps` and `t@cap` columns stay, every cell `—` (keep-columns
        are never
        elided; the session triple is the floor)
    And the droppable `velocity` and `runway` columns elide, as they do today on a fleet with no
        computable rate
    And the weekly columns carry figures
    But NOT a table that drops `caps` or `t@cap` — or the executor pins the opposite in the golden and
        records why

Scenario: the trend sparkline and the heatmap leave a bucket blank when the account carried no session reading in it
  # Tier A — graded by
  # `a_bucket_with_no_session_reading_is_a_gap_in_the_trend_and_heatmap_picks_never_a_zero_bar`
  # (`stats::tests`; #1601 § 6 W15) at the seam `account_series` with `session_peak` /
  # `session_mean` (`src/stats.rs:3273-3294`), RED read off the tree after #1600 (`Some(0.0)` for each
  # bucket `spare` is in), with the mutant `Some(a.session.map_or(0.0, |d| d.peak))` → FAILS · covers
  # AC-6
  Given buckets where `spare` (no session reading) is present in two of three, and `mixed` carries a
        session
        reading in one bucket and none in another
   When `account_series` picks the session peak (the table's `trend` cell, `src/stats.rs:3687`) and the
        session
        mean (the heatmap, `:3907`)
   Then every bucket of `spare` is `None`, and `mixed`'s no-session bucket is `None` beside its present
        one
    And `spare`'s rendered trend is a run of break glyphs (` `), exactly as an unobserved bucket's
    But NOT `Some(0.0)` — the lowest ramp glyph, a zero bar for a reading nobody made

Scenario: the lowest-utilisation line ranks only accounts with a session reading
  # Tier A — graded by `the_lowest_utilisation_line_ranks_only_accounts_with_a_session_reading`
  # (`stats::tests`; #1601 § 6 W13), RED read off the tree after #1600 (the zero summary ranks `spare`
  # lowest at `0%`), with the mutant `a.session.map_or(0.0, |d| d.mean)` as the ranking key → FAILS; the
  # default is #1601 § 4's, ratification-pending · covers AC-14
  Given a report with `work` (session mean 0.5) and `spare` (no session reading), and no velocity
        overlay
    And, as the contrast fixture, a report holding only `spare`
   When `render_summary` (`src/stats.rs:2267`) renders the roster block
   Then it reads exactly `  lowest utilisation: work (session mean 50%)`
    And a report holding only `spare` prints no `lowest utilisation` line — with no velocity overlay, an
        empty block
    But NOT `lowest utilisation: spare (session mean 0%)` — PRD risk P-10, the fleet read as
        session-idle
    But NOT `spare` ranked with `—` as its mean — an unknown is not a low

Scenario: the session-distribution gauge keeps the account's row with a bare track and the — sentinel
  # Tier A — graded by
  # `the_session_gauge_draws_a_bare_track_and_the_unknown_sentinel_for_an_account_with_no_session_window`
  # (`stats::tests`; #1601 § 6 W14), exact lines at width 80 with the ASCII track, RED read off the tree
  # after #1600 (`x` in cell 0, `0% · 0% · 0%`), with the mutant `unwrap_or(DimStats::ZERO)` → FAILS;
  # the default is #1601 § 4's, ratification-pending · covers AC-15
  Given the same report
   When `render_percentiles` (`src/stats.rs:3928`) draws the `session distribution — mean · p95 · peak`
        block
   Then `work`'s row is byte-identical to today's:
        `work   [--------------------m------------P-x----]  50% · 85% · 90%`
    And `spare`'s row is `spare  [----------------------------------------]  —` — no `m`, `P` or `x`
        marker
    And the gauge width stays the widest printed trailer's, so `—` does not widen it
    But NOT a gauge at `0% · 0% · 0%`, and NOT the row omitted while the table and the heatmap show the
        account

Scenario: a render case without a golden, or a golden without a case, fails the committed comparator
  # Tier A — graded by the committed
  # `the_committed_stats_goldens_still_match_the_render` (`src/stats.rs:11776`) through
  # `render_golden::assert_matches_goldens` (`src/render_golden.rs`, unedited: equal cardinality, every
  # golden claimed by a case, every case in the goldens table) and by the committed canary
  # `the_stats_golden_gate_rejects_a_corrupted_render` (`:11783`); only the two const lists grow — a
  # ratification-pending category (b) reading · covers AC-6
  Given `cases()` (`src/stats.rs:11639`) gains `stats-weekly-only-piped` and `stats-weekly-only-wide`
        (names the
        body suggests)
   When the comparator runs before the two goldens are emitted and listed in
        `GOLDENS` (`src/stats.rs:11743-11761`)
   Then it FAILS on the cardinality assertion first — `19 cases vs 17 committed goldens — the two lists
        have drifted apart` — before any per-case check, so the message names no case
    And once both are emitted and listed, it passes over nineteen cases against nineteen goldens
    But NOT a pass over a partial case set — cardinality zero and a size mismatch are automatic failures

Scenario: the two new goldens are emitted once, the seventeen existing goldens are byte-identical, and the trailer is carried
  # Tier B — pending: the new goldens' bytes are executor-emitted (the judge's read by construction, as
  # the sibling stubs read theirs); `git diff --stat origin/main -- build/fixtures/cli-renders` listing
  # exactly two added files is a review read; `scripts/check-cli-golden-rebaseline.sh` grades the
  # trailer in CI. No further test is owed beyond the cell test of this rule's first scenario · covers
  # AC-6, AC-10
  Given the seventeen committed `build/fixtures/cli-renders/stats-*.txt`, none holding a weekly-only
        account
   When exactly `cargo test -- --ignored emit_cli_render_goldens_stats` (`src/stats.rs:11771`) runs once
        on the
        changed tree
   Then `build/fixtures/cli-renders/stats-weekly-only-piped.txt` and `stats-weekly-only-wide.txt` exist
        with
        `spare`'s `—` row
    And the seventeen existing files are byte-identical to `origin/main`
    And the commit carries `CLI-Goldens-Rebaselined: <the two new cases and why>`, contiguous with the
        `Co-Authored-By:` block, and neither `Gate-Change-Acknowledged:` nor
        `Panel-Goldens-Rebaselined:`
    But NOT `cargo test -- --include-ignored`, and NOT a golden hand-edited
```

## Coverage

Each acceptance criterion of #1601, with the scenarios that cover it. A criterion no scenario covers
is named as such, with what grades it instead.

| AC | Scenarios | What grades it |
|---|---|---|
| AC-1 — absence is carried as `None` from the aggregate | Rule 1: "an account whose readings carry no session reading aggregates to None…"; "a present session reading keeps every figure Some…"; "the contribution-only account stores None…" | Tier A throughout — `an_account_with_no_session_reading_aggregates_to_none_never_a_zero_summary` (#1601 § 6 W1) with its `Some(DimStats::ZERO)` mutant; #1600's T2 moved, and the committed `contribution_share_credits_an_active_account_with_no_readings_of_its_own` with it |
| AC-2 — the wire omits exactly the four keys and nothing else | Rule 2: "a weekly-only account's entry omits session, band, cap_hits and time_at_cap_secs…"; the velocity outline | Tier A — `the_stats_wire_omits_the_four_session_keys_for_an_account_with_no_session_window` (#1601 § 6 W3) and the four-row `the_velocity_object_rides_a_weekly_only_rate_with_a_null_session_rate` (W4) |
| AC-3 — the version moved once, both byte pins regenerated once | Rule 2: "the version moves once, both byte pins are regenerated once…" | Tier A — `the_committed_stats_golden_moves_to_schema_2_keeps_work_and_adds_an_entry_without_session_keys` (#1601 § 6 W5); the committed pins are green after regeneration but both their sides are the executor's |
| AC-4 — the Swift mirror decodes the omission; the lockstep sweep reads zero stale sites | Rule 3: "a schema-2 frame … decodes with nil in each"; "a present band with an unknown value still throws"; "a present but malformed session block still throws"; "the Swift golden fixture follows the regenerated golden…"; "every schema:1 site under apps/menubar moves…" | `testUnknownBandIsADecodeError` and `testStatsFixtureMatchesRustGolden` (Tier A, committed); `testDecodesAWeeklyOnlyEntryWithTheFourSessionKeysOmitted`, `testAMalformedPresentSessionBlockStillThrows` and `testTheConfigUnreadableFixtureIsTheGoldenPlusItsOneKey` (Tier A, #1601 § 6 W6–W8); the two greps are review reads |
| AC-5 — the Stats tab does not crash and invents nothing | Rule 4, all four | `testAWeeklyOnlyStatsRowRendersTheTransitionalNAAndSpeaksOnlyItsWeeklyPeak` (Tier A, #1601 § 6 W9); `testSparkSeriesIsEmptyForAnAccountWithANoSessionBucketNeverAFloorLine` for the sparkline (Tier A, W12); `testStatsNumericCells` + `testSignalCollapsesTheBandLikeTheCLI` for the present arm (Tier A); the armed `PanelGoldenParityTests` (Tier A) |
| AC-6 — the table prints `—` in every session-keyed cell, pinned by goldens | Rule 5: the five table scenarios and the trend / heatmap one | `every_session_keyed_cell_prints_the_unknown_sentinel_for_an_account_with_no_session_window` and `an_entirely_weekly_only_roster_keeps_caps_and_t_at_cap_and_elides_velocity_and_runway` (Tier A, #1601 § 6 W10, W11); `a_bucket_with_no_session_reading_is_a_gap_in_the_trend_and_heatmap_picks_never_a_zero_bar` for the trend and the heatmap (Tier A, W15); the committed comparator and its canary (Tier A) for the case/golden pairing; the new goldens' bytes are the judge's read |
| AC-7 — the vocabulary is stated | Rule 2: "the vocabulary is stated where each absence is read" | the doc build and the merge judge — no test |
| AC-8 — the moved contract is propagated by its token | Rule 2: "no site still states that the current stats wire is schema:1" | the sweep grep with its canary and the pull request body's dispositions — no test |
| AC-9 — ADR-0035 stands | Rule 2: "ADR-0035 stands — the keys' presence moves…" | `git diff --stat -- docs/adr` and the doc read; the committed divergence test is executor-adapted |
| AC-10 — the diff is bounded and the gates pass | Rule 4 "the panel shows zero drift…" and Rule 5 "the two new goldens are emitted once…" (the trailer clauses only); otherwise none | `git diff --stat origin/main`; the five `test` steps, `msrv`, both `deny` checks, `./scripts/check-menubar-zero-egress.sh`, `xcodebuild test` read off its executed-test count, the armed `panel-goldens` run; the three trailer gates in CI |
| AC-11 — one delivery under either D8 answer | Rule 3: "the reshape, the mirror, the fixtures and the goldens land in one pull request…" | a read of the pull request; the committed `testStatsFixtureMatchesRustGolden` fails on a split |
| AC-12 — build-reference conformance | Rule 2 "the version moves once…"; Rule 3 "the Swift golden fixture follows…"; Rule 4 "the panel shows zero drift…" | `testStatsFixtureMatchesRustGolden` (Tier A); W5's baseline read of the regenerated golden (Tier A, #1601 § 6); the Rust pin after regeneration; the armed panel gate; the pull request body's statement that nothing is built against the mock |
| AC-13 — the weekly symmetry, or its recorded overrule | Rule 1: "a reading whose weekly window is absent aggregates to a None weekly summary (AC-13 default; design D12)" | `a_weekly_less_reading_aggregates_to_none_on_the_weekly_axis` (Tier A, #1601 § 6 W2) for the aggregate; `a_weekly_less_entry_omits_the_weekly_key_and_prints_the_unknown_sentinel_in_its_weekly_cells` (Tier A, W16) for the wire and the table; `testAWeeklyLessStatsRowRendersTheTransitionalNAAndDropsTheWeeklyClause` (Tier A, W17) for the Swift mirror and its interim; under the overrule, the `AccountWire.weekly` doc sentence is the judge's read |
| AC-14 — the `lowest utilisation` line ranks only accounts with a session reading | Rule 5: "the lowest-utilisation line ranks only accounts with a session reading" | `the_lowest_utilisation_line_ranks_only_accounts_with_a_session_reading` (Tier A, #1601 § 6 W13) with its ranking-key mutant |
| AC-15 — the session-distribution gauge shows a gap, never a zero gauge | Rule 5: "the session-distribution gauge keeps the account's row…" | `the_session_gauge_draws_a_bare_track_and_the_unknown_sentinel_for_an_account_with_no_session_window` (Tier A, #1601 § 6 W14) with its `unwrap_or(DimStats::ZERO)` mutant |
| AC-16 — the interim docs #1600 plants are retired | Rule 2: "the vocabulary is stated where each absence is read" | the `git grep` of AC-16 at the head with its control on #1600's merged head, and the merge judge's read — no test |
