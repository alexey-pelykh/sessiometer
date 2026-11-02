# Feature: the five raw-sample readers exclude a reading with no session window from every session figure and keep it in every weekly one

Issue #1600 · PRD REQ-STS-B-001 / REQ-STS-C-001 / REQ-RDG-C-001 (§ 10.8, § 10.3; D5 and D8 in § 8, D12 in design § 14; § 11.3, § 11.4; MET-T-003 in § 4) · design § 5 B-8 (first bullet), § 4.2, § 4.4, § 4.5, § 11 Cap-5, § 12 AD-4 / AD-13 / AD-17 / OAD-3, § 14 R-D4

Example Mapping: 🟦 5 rules · 🟩 17 examples · 🟥 0 open

> Since 2026-10-30 a verified Ultra account's usage reply carries `"five_hour":null` beside a present
> weekly window. Once the parser (#1596), the reading model (#1597) and the sample store (#1598)
> serve such an account, `usage-samples.jsonl` carries lines with `"session":null` and
> `Sample.session` is `Option<f64>`. Five readers of the raw samples in the offline verbs read that
> field as a number at `fd8640b` and stop compiling the moment #1598 lands: the per-account
> aggregate `aggregate_with_roster` (`src/usage_stats.rs:514`, the two series at `:543-544`), the
> all-high census's `high_windows` (`src/usage_stats.rs:865`, `let high = s.session >=
> high_threshold;` at `:874`) under `all_high` (`:727`), the capacity-hold census's
> `blocked_windows` (`src/usage_stats.rs:1046`, `boundary.session_blocked(s.session)` at `:1055`)
> feeding `relief_of` (`:1094`), the velocity replay `account_velocity` (`src/stats.rs:1074`, the two
> series at `:1096-1097`) and the post-swap landing fold `fold_landings` (`src/reliability.rs:2057`,
> the peak filter at `:2098`). Each takes the **exclude** move of design § 4.2 for a sample whose
> `session` is `None`: the sample is left out of every session figure (mean / peak / p95,
> `cap_hits`, `time_at_cap_secs`, the session velocity and runway, the post-swap landing peak) and
> out of the all-high census's set, and kept in every weekly figure, in `seen` / `coverage`, in the
> contribution share and in the capacity-hold census, where a session window that does not exist
> never blocks. The default a hurried fix reaches for — `unwrap_or(0.0)` — would count a weekly-only
> account as a `0 %` session reading in the census, as a never-at-cap account in the cap counts, as
> a flat session velocity and as a `0 %` landing: a fabricated value on three measurement surfaces,
> the class `MET-T-003` counts and `REQ-RDG-C-001` forbids at every site, tests included (design
> AD-4). The item lands as the last commits of #1598's pull request — it is what makes that pull
> request compile across the crate — and leaves the `stats` wire's shape untouched: `stats --json`
> still carries a `session` block for such an account, holding the zero summary an empty series
> produces, until the sibling wire item #1601 replaces that block with an omission under
> `JSON_SCHEMA_VERSION` 2 (design OAD-3 / D8: under either answer this item's shape is the same).
>
> The `# Tier` line under each scenario is this stub's binding line, in the form of
> `docs/specs/sample-store-session-null.feature.md`. Tier A names a grader the executor may not
> edit and this item set already pins by name: a test #1600 § 7 names verbatim (T1–T15, or a
> committed test its must-stay-green list names) together with the records its acceptance
> criteria owe. Two records back each new test, both carried in the pull-request body. The RED
> record: every `Sample` such a test reads, the present readings too, is built from a JSON line (a
> typed `Sample { session: None, .. }` does not compile at `fd8640b`, where `session` is `f64`),
> so on the unchanged tree a `null` line fails to deserialise inside the test's helper and the test
> fails at setup — run by exact name against `origin/main` with only the test additions applied,
> printing `running 1 test` and a `FAILED` line that comes from its setup; that reading comes from
> the derive of `Sample` (`src/usage_store.rs:132-148`) and has not been run, so the executor's
> record is what confirms it. The mutant record: the reader's `filter_map` / `is_some_and` replaced
> by the fabricating `unwrap_or(0.0)` (or the mutant the scenario names), the test run by name on
> the final tree, the red recorded. The two answer different questions: the setup failure shows the
> state was unrepresentable before #1598, the mutant shows the test can tell a fabricated zero from
> an exclusion. A test with no discriminating mutant (T2, and the zero-block half of T8) is a
> canary of the documented interim: it gets the RED record and says it has no mutant. The
> committed `a_rostered_account_with_no_samples_stays_in_the_census_instead_of_vanishing` is
> already green and gets the mutant record only. None of #1591's four oracles and none of
> #1592's seven pins reaches a sample reader, so no scenario here binds to them. Tier B names what
> is still unbound and the item that should bind it; where a committed test already pins the
> behaviour and no further test is owed, the line says so rather than promoting a presence check to
> Tier A.
>
> D5 (`census-excludes-absent-window`), D12 (`Sample.weekly` optional, applied symmetrically) and
> D8 (whether #1601 rides in tranche 1) are open, each built for its recorded default with the
> alternative stated in #1600; a scenario that depends on one says so in its name. Under D5's
> alternative — hold the census UNKNOWN whenever such an account is on the roster — Rule 2's three
> D5 scenarios invert (the weekly-only account stays in the set and empties the intersection) and
> nothing else here changes. Written headless at Stage 3.5 on 2026-11-01: every rule, tier and
> name here is ratification-pending.

## Rule 1 — `aggregate_with_roster` computes each axis over the readings that carry it; `seen`, `coverage` and the contribution share keep every sample; an axis no reading carries yields the finite zero summary as a documented interim, never as a measurement

```gherkin
Scenario: a sample with no session reading is excluded from the session figures and kept in the weekly ones
  # Tier A — graded by usage_stats::tests::a_sample_with_no_session_reading_is_excluded_from_the_session_figures_and_kept_in_the_weekly_ones (#1600 T1; RED record at `fd8640b` is the setup failure — the `null` JSON line does not deserialise there; mutant, AC-1's record: `unwrap_or(0.0)` in place of the `filter_map` reads the two absent samples as `0 %` and the session mean becomes 0.3, not 0.9 → red) · covers AC-1, AC-8
  Given one account with three in-period samples at the poll cadence
    And their session readings are `None`, `Some(0.9)`, `None` and their weekly readings `Some(0.3)`, `Some(0.5)`, `Some(0.7)`
    And `session_cap` is 0.8
   When `aggregate_with_roster` runs over the period
   Then the account's session summary is `DimStats { mean: 0.9, peak: 0.9, p95: 0.9 }` — over the one present session reading
    And its weekly mean is 0.5 — over all three weekly readings
    And `seen == 3` and `coverage` counts all three samples, because a weekly-only sample is an observation of the account
    And `cap_hits == 1` and `time_at_cap_secs` is that one sample's validity window — both counts are over the present session readings at or above the cap
    But NOT a session mean of 0.3, the `unwrap_or(0.0)` reading of the two absent samples as `0 %`

Scenario: an account none of whose readings carries a session reading holds the interim zero summary until the wire omits it
  # Tier B — pending: #1600 § 7 names usage_stats::tests::an_account_with_no_session_reading_holds_the_interim_zero_summary_until_the_wire_omits_it (T2) verbatim and records no mutant for it, and none is expressible — over an all-`None` series the fabricating `unwrap_or(0.0)` and the `filter_map` fold both yield `DimStats::ZERO`, `cap_hits == 0` and `time_at_cap_secs == 0` — so T2 is a canary of the documented interim shape (AC-7), not of the move; it gets the RED record and no mutant record. The grader of the interim's honesty is AC-7's five doc comments and #1601 AC-1's test, whose mutant (`Some(DimStats::ZERO)` in the empty arm → red) is the discriminating one; T2 moves with #1601 · covers AC-7
  Given one account whose in-period samples all carry `session: None` beside present weekly readings
   When `aggregate_with_roster` runs
   Then its session summary is `DimStats::ZERO` — the finite zero `dim_stats(&[])` produces (`src/usage_stats.rs:1242`), reachable now for a real account
    And `cap_hits == 0` and `time_at_cap_secs == 0`
    And its weekly summary is over its weekly readings
    And the doc comments of `dim_stats`, `DimStats::ZERO`, `AccountStats.session`, `AccountStats.cap_hits` and `AccountStats.weekly` say that zero summary and those `0` counts are an interim representation, not a measurement, and name #1601 as what replaces them with an omission under `JSON_SCHEMA_VERSION` 2
    And `DimStats::ZERO`'s doc no longer says it is "never produced from actual samples", and `AccountStats.weekly` says the same for an account with no weekly reading (D12)
    And the test's own doc names #1601 as what moves it
    # The contribution-only branch (`src/usage_stats.rs:572-589`, an account active for some observations but never itself sampled) keeps its `DimStats::ZERO` readings unchanged: "active time known, utilisation not" was already gap-honest, and `contribution_share_credits_an_active_account_with_no_readings_of_its_own` (`src/usage_stats.rs:1616`) keeps pinning it.

Scenario: the stats wire and table carry the interim for such an account at this item's head, and no golden moves
  # Tier A — graded by stats::tests::an_account_with_no_session_reading_wires_the_interim_zero_block_and_no_velocity_under_schema_1 (#1600 T8, T2's wire half plus the interim's table face; RED record at `fd8640b` is the setup failure — the `null` JSON line does not deserialise there; the zero `session` block, `"band":"idle"` and the two `0` counts have no discriminating mutant, because over an all-`None` series the `filter_map` fold and `unwrap_or(0.0)` serialise the same bytes, so that half is a canary of the interim that #1601 AC-2 retires; the no-`velocity` step and the table step's elided `velocity` column are red under T6's mutant, AC-4's record) with the two committed byte pins stats::tests::json_wire_is_byte_stable_vs_158_159 (`src/stats.rs:10207`) and stats::tests::the_committed_stats_wire_golden_still_matches_the_socket_encoder (`src/stats.rs:11010`) and AC-9's `git status --porcelain -- build/fixtures apps/menubar` for the golden and schema steps; the table steps are T8's table half, whose `underused` / `0/0` cells are a canary #1601's W10 moves · covers AC-9, AC-7
  Given a report holding one account whose in-period readings carry no session reading, with the velocity overlay applied
   When the report is serialised for `stats --json` under `JSON_SCHEMA_VERSION` 1
   Then that account's entry carries `"session":{"mean":0.0,"peak":0.0,"p95":0.0}`, `"band":"idle"` (`Band::of(0.0)`, `src/stats.rs`), `"cap_hits":0` and `"time_at_cap_secs":0` — the interim this item keeps and documents as not a measurement
    And the entry carries no `velocity` object, because `velocity_wire` still returns early on an unknown session rate (`src/stats.rs:3073`) — a gate that hides the account's weekly velocity and is #1601's to move
    And the document's `schema` is still `1`; `JSON_SCHEMA_VERSION` in `src/stats.rs` (`:80`), `src/log.rs` and `src/reliability.rs` (`:264`) are unchanged
    And `build/fixtures/wire-stats-basic.json` is byte-identical to `fd8640b`'s after `cargo test --verbose`
    And the terminal table holds no `velocity` or `runway` column — both cells are `—` for this lone account, whose session rate and runway are unknown (`velocity_cell` at `src/stats.rs:2534`, `runway_cell` at `:2545`), and `render_account_table`'s empty-column pass elides a droppable column that is uniformly `—` (`:3772-3777`)
    And the account's row reads `underused` in its `signal` cell and `0/0` in its `session` cell — the zero summary's face (`signal_cell`, `session_cell`), which #1601's W10 turns into `—`
```

## Rule 2 — the two censuses: a structurally absent session window is not an unobserved reading — the all-high census excludes it (D5), the capacity-hold census keeps the sample as covering and never session-blocked

```gherkin
Scenario: the all-high census is over the accounts that have a session window (D5 default)
  # Tier A — graded by usage_stats::tests::the_all_high_census_is_over_the_accounts_that_have_a_session_window (#1600 T3; mutants, AC-2's record: `unwrap_or(0.0)` keeps `a` in the set at `0 %`, the high intersection empties and `all_high_episodes == 0` → red; the other wrong move, `a` kept in the set contributing no window, empties the covering intersection and `all_high_covered_secs == 0` → red; for the `contribution_share` step, a weekly-only sample left out of the in-period set the contribution counts read leaves `a`'s share at `0` → red, which a single-account test such as T1 cannot see because the denominator shrinks with the numerator) · covers AC-1, AC-2, AC-8
  Given a roster of two accounts `a` and `b`
    And `a` is sampled across the whole period with every session reading `None`
    And `b` is sampled across the whole period at `Some(0.98)`
    And both accounts are read at the same instants, and no swap occurs in the period
    And `high_threshold` is 0.95
   When the census runs with that roster
   Then `all_high_episodes == 1`
    And `all_high_covered_secs` equals `b`'s covered seconds — the census set is the roster minus every account none of whose in-period samples carries a session reading
    And the `all_high` doc comment quotes D5's sentence — "A structurally absent session window is excluded from the session census — it is not an unobserved reading, and the census is over the accounts that have the window" — and names `docs/adr/0033-weekly-saturated-session-low-stays-unknown.md`, amended by #1593, as its record — checked by AC-2's `git grep -c 'structurally absent session window' -- src/usage_stats.rs`, which prints nothing at `fd8640b`
    And `a`'s `contribution_share` equals `b`'s, both `0.5` — a weekly-only sample is an observation of the account, so it counts toward the contribution share exactly as any sample does
    But NOT `a` counted as a `0 %` session reading that empties the high intersection
    But NOT `a` kept in the set with no window, which would empty the covering intersection and read UNMEASURABLE
    But NOT `a`'s share read as `0` because its weekly-only samples were left out of the in-period set the contribution counts read

Scenario: a mixed-period account is counted only over the instants it had a session window (D5 default)
  # Tier A — graded by usage_stats::tests::a_mixed_period_account_is_counted_only_over_the_instants_it_had_a_session_window (#1600 T4; mutant, AC-2's record: `unwrap_or(0.0)` makes the first half covered-but-low, so `all_high_covered_secs` becomes the whole period → red) · covers AC-2, AC-8
  Given a roster of `a` and `b`, with `b` at `Some(0.98)` throughout
    And `a` reads `None` for the first half of the period and `Some(0.98)` for the second
   When the census runs
   Then `all_high_covered_secs` is the second half only — a sample whose session is `None` pushes no covering window and no high window, so `a`'s absent instants are uncovered on the session axis and nothing is interpolated across them
    And `all_high_episodes == 1`, over that second half
    And `prop_all_high_time_never_exceeds_the_jointly_covered_time` (`src/usage_stats.rs:2544`) still holds: the high list stays a subset of the covering one per account

Scenario: a rostered account with no samples at all still empties the intersection — unobserved is not no-window
  # Tier A — graded by usage_stats::tests::a_rostered_account_with_no_samples_stays_in_the_census_instead_of_vanishing (committed and green at fd8640b, `src/usage_stats.rs:1816`, named in #1600 § 7's must-stay-green list; mutant, AC-2's record: the careless-D5 set — `all_high`'s census built as the accounts with at least one present session reading — drops the unsampled `b` along with the session-less ones, so its configured-roster `all_high_episodes == 0` reads `1` → red). The three-account combination below (a weekly-only account beside the unsampled one) has no pin of its own and is T3's natural extra row; no new test is owed · covers AC-2
  Given a roster of `a`, `b` and `c`
    And `a` is weekly-only (every session `None`) throughout, `b` is at `Some(0.98)` throughout, and `c` is absent from the period's samples
   When the census runs with that roster
   Then `all_high_covered_secs == 0` — UNMEASURABLE, which the table renders as its `—` sentinel (`src/stats.rs`)
    And `all_high_episodes == 0`
    And `a` leaves the set by membership before the intersection loop, never by a skip inside it, so `c`'s empty place still empties the covering intersection and `all_high`'s comment "A rostered account absent from the period's samples covers NOTHING — it stays in the intersection and empties it" stays true
    But NOT `c` dropped from the set the way `a` is: no samples is "unobserved", not "no window"

Scenario: without a known roster the fallback set is the sampled accounts minus those with no session reading (D5 default)
  # Tier A — graded by stats::tests::without_a_known_roster_the_all_high_census_is_over_the_sampled_accounts_that_have_a_session_window (#1600 T9, T3's `roster == None` twin, placed in `stats::tests` because `census_over_roster` is the `Report` field `build_report` sets; RED record at `fd8640b` is the setup failure; mutants, AC-2's record: T3's two — `unwrap_or(0.0)` keeps `a` in the sampled set at `0 %` and `all_high_episodes == 0`; `a` kept in the set with no window makes `all_high_covered_secs == 0` → red) · covers AC-2
  Given no roster is known, and the period's samples carry `a` (every session `None`) and `b` (`Some(0.98)` throughout)
   When the census runs with `roster == None`
   Then the census set is `{b}` and `census_over_roster == false` — its wire value and meaning for a roster-less read are unchanged
    And `all_high_episodes == 1` and `all_high_covered_secs` equals `b`'s covered seconds

Scenario: a weekly-only sample over the weekly line is a weekly hold relieved at its weekly reset, never a session hold
  # Tier A — graded by usage_stats::tests::a_weekly_only_sample_over_the_weekly_line_is_a_weekly_hold_relieved_at_its_weekly_reset (#1600 T5; mutant, AC-3's record: `unwrap_or(1.0)` reads a session over the line; `relief_of` then needs `session_resets_at`, which is `None`, so relief is `None`, the hold falls back to the cadence horizon and its end differs from the reset → red; the same mutant also blocks the under-line sample on a fabricated session, and `capacity_holds_session` becomes 1) · covers AC-3, AC-8
  Given a viability boundary is supplied (at defaults the session line is 0.80 and the weekly line 0.97 — `ViabilityBoundary` at `src/swap.rs:270-280`)
    And one weekly-only sample reads `session: None`, `weekly: Some(0.97)`, carrying `weekly_resets_at` later than its cadence horizon
    And a later weekly-only sample, at the reset, reads `weekly` under the line
    # Placed at the reset so the first reading's window runs to its reset instead of being superseded by the next reading (`next.min(anchored_hi)`, `src/usage_stats.rs:1062`).
   When the capacity-hold census runs
   Then exactly one hold is counted, with cause `Weekly`, ending at the weekly reset — reset-anchored, as `blocked_windows` anchors any blocked reading
    And `capacity_holds_session == 0` — `session_blocked` reads `false` for a session that does not exist (`s.session.is_some_and(|v| boundary.session_blocked(v))`), so `relief_of`'s rule is unchanged and the hold's cause is the daemon's own `spare_relief` outcome for the same reading
    And the under-line sample is covering and not blocked
    But NOT `session_blocked` true from a fabricated session reading
    # The call site resolves the option: `ViabilityBoundary::session_blocked` keeps its `f64` parameter, because its definition (`src/swap.rs:301`) and its tests (`src/swap.rs:1269-1277`) sit outside AC-10's three files and its one production caller is `src/usage_stats.rs:1055`.

Scenario: a weekly-only account that is viable is somewhere to land, so the capacity census keeps it in its set
  # Tier A — graded by usage_stats::tests::a_viable_weekly_only_account_is_somewhere_to_land_so_the_capacity_census_keeps_it (#1600 T10; RED record at `fd8640b` is the setup failure; mutants, AC-3's record: `a` filtered out of the capacity set, as D5 filters it out of the session census, leaves `b` alone and `capacity_holds == 1`; `a`'s weekly-only reading pushing no covering window makes `capacity_hold_covered_secs == 0` → red) · covers AC-3
  Given a roster of `a` and `b` and a viability boundary
    And `a` is weekly-only (every session `None`) and under the weekly line throughout
    And `b` is at or over its session line throughout, carrying its session reset
   When the capacity-hold census runs
   Then `capacity_holds == 0` — `a` was viable, so the daemon had somewhere to land
    And `capacity_hold_covered_secs` is the jointly covered period — a weekly-only sample says whether the account was viable at its instant, so it covers
    But NOT `a` dropped from the capacity set the way D5 drops it from the session census: the two censuses take opposite moves on purpose
```

## Rule 3 — `account_velocity` replays each axis over the readings that carry it, and states a runway only from a present latest reading

```gherkin
Scenario: a weekly-only account has no session velocity and a weekly one
  # Tier A — graded by stats::tests::a_weekly_only_account_has_no_session_velocity_and_a_weekly_one (#1600 T6; mutant, AC-4's record: `unwrap_or(0.0)` yields `session_rate == Some(0.0)`, a MEASURED FLAT by the wire's own doc → red) · covers AC-4, AC-8
  Given an account whose in-window samples all carry `session: None`
    And its weekly readings climb below the weekly trigger at a plausible rate over more than `MIN_VELOCITY_SAMPLES` (2, `src/stats.rs:865`) intervals
    And its latest reading is within `stale_after_secs` of the window end — a weekly-only sample is a current reading of the account, so the rows and the staleness test are unchanged
   When `account_velocity` runs
   Then `session_rate == None` and `session_runway_secs == None`
    And `weekly_rate`, `weekly_runway_secs` and `weekly_headroom` are all `Some`
    But NOT `session_rate == Some(0.0)`: a flat is a measured rate, and nothing was measured

Scenario: a mixed session series replays its EMA over the present readings only
  # Tier A — graded by stats::tests::a_mixed_session_series_replays_its_ema_over_the_present_readings_only (#1600 T11; RED record at `fd8640b` is the setup failure; mutant, AC-4's record: `unwrap_or(0.0)` reads each fabricated `0.0` as a drop that re-seeds the EMA, so `session_rate` becomes ≈ `6.667e-4` instead of `2.5e-4` frac/s and the runway `600` instead of `1600` s → red) · covers AC-4
  Given an account whose in-window session readings are `None`, `Some(0.10)`, `None`, `Some(0.20)`, `None`, `Some(0.30)`, `Some(0.40)` at a 300 s cadence, weekly readings present throughout
   When `account_velocity` runs
   Then `session_rate` is the `replay_velocity_ema` of the four present points over their real elapsed times — 600 s, 600 s and 300 s — with three intervals blended
    And `session_runway_secs` is from the present latest reading `Some(0.40)`
    But NOT an EMA reset at each fabricated `0.0`, which `replay_velocity_ema` reads as a window drop

Scenario: an account whose session window vanished mid-window has a session rate but no session runway
  # Tier A — graded by stats::tests::an_account_whose_session_window_vanished_mid_window_has_a_session_rate_but_no_session_runway (#1600 T12; RED record at `fd8640b` is the setup failure; mutant, AC-4's record: `last.session.unwrap_or(0.0)` computes a runway from a fabricated `0 %` latest reading, `Some(2400)` → red) · covers AC-4
  Given an account whose in-window session readings are `Some(0.10)`, `Some(0.20)`, `Some(0.30)` and then `None` at the cadence, weekly readings present throughout
   When `account_velocity` runs
   Then `session_rate` is `Some` — the EMA over the three present readings, two intervals blended
    And `session_runway_secs == None` — a runway runs from the latest reading, and the latest reading carries no session value
    And the weekly figures are computed exactly as before
    But NOT a session runway from a fabricated `0 %` latest reading
```

## Rule 4 — `fold_landings` treats an absent reading as an unmeasured anchor; no `reliability` rule, percentile, breach class or wire field moves

```gherkin
Scenario Outline: a parked window's landing is the peak of its present session readings, and unmeasured when it has none
  # Tier A — graded by reliability::tests::a_parked_window_with_no_session_reading_is_an_unmeasured_landing (#1600 T7; mutant, AC-5's record: `unwrap_or(0.0)` measures the all-absent anchor at `0 %` → red) · covers AC-5, AC-8
  Given a `reason=session` swap anchor for a parked account, not capacity-held
    And the account's samples strictly after the swap and within `LANDING_WINDOW_SECS` (15 minutes, `src/reliability.rs:194`) carry session readings <readings>
   When `compute_landing` folds the anchors
   Then `n_measured` is <measured> and `n_unmeasured` is <unmeasured>
    And the landing is <landing>
    But NOT a `0 %` landing for an anchor whose window held no session reading — "no reading of the parked account in view" is the branch that already exists for an unmeasured anchor

  Examples:
    | readings             | measured | unmeasured | landing                                                              |
    | `None`, `None`       | 0        | 1          | none — the anchor is unmeasured, a coverage gap reported honestly    |
    | `None`, `Some(0.42)` | 1        | 0          | 42 % — the peak over the present readings only                       |

Scenario: the non-finite guard stays ahead of the clamp, applied to the present readings
  # Tier B — pending: #1600 § 7 names reliability::tests::landing_refuses_a_non_finite_parked_reading_rather_than_fabricating_one (`src/reliability.rs:7444`) as a committed canary that must stay green; it is green at fd8640b, its own doc records the measurement behind it, and #1600 records no mutant for the reordered guard. No further test is owed: the graders are that canary under its name and reliability::tests::landing_keeps_the_finite_peak_when_a_non_finite_reading_shares_the_window (`src/reliability.rs:7485`) for the mixed window; this stub records the arm as bound to committed pins rather than promoting a presence check to Tier A · covers AC-5, AC-9
  Given a parked window whose present session readings include a non-finite value beside a finite one
   When the fold runs with the `filter_map` ahead of the `is_finite` filter
   Then the non-finite reading is dropped before the clamp and the finite peak lands, exactly as at fd8640b
    And a window holding only non-finite readings is unmeasured — never a `0` or `255` landing from the saturating cast
    And `JSON_SCHEMA_VERSION` at `src/reliability.rs:264` is unchanged from the merge base with `origin/main` (14 after #1602's compile-unit pull request; 13 at `fd8640b`), and no percentile, breach class or wire field moves
```

## Rule 5 — CONSTRAINT: never a numeric default on either axis, in the diff or its tests; the records say "every dimension the account has"; the diff is bounded to the three reader files and lands in #1598's pull request

```gherkin
Scenario Outline: a reading with no weekly window takes the same move on the weekly axis (D12 default)
  # Tier A — graded by usage_stats::tests::a_sample_with_no_weekly_reading_is_excluded_from_the_weekly_figures_and_kept_in_the_session_ones (the `aggregate_with_roster` row), usage_stats::tests::a_session_only_sample_over_the_session_line_is_a_session_hold_relieved_at_its_session_reset (the `blocked_windows` row) and stats::tests::a_session_only_account_has_no_weekly_velocity_and_a_session_one (the `account_velocity` row) (#1600 T13–T15, the `weekly: None` twins of T1, T5 and T6; RED record at `fd8640b` is the setup failure; mutants, AC-1's, AC-3's and AC-4's records: `unwrap_or(0.0)` on the weekly series → weekly mean 0.3 → red; `unwrap_or(1.0)` on the weekly read — `0.0` sits under every line and cannot discriminate — blocks on a fabricated weekly whose reset is `None`, so two cadence-bounded `Weekly` holds replace one reset-anchored `Session` hold → red; `unwrap_or(0.0)` → `weekly_rate == Some(0.0)`, a measured flat → red). #1601 AC-13's weekly-symmetry test grades only the aggregate's `Option` shape after that item · covers AC-1, AC-3, AC-4, AC-8
  Given a sample whose `weekly` is `None` and whose `session` is present
   When <reader> runs over it
   Then <session-side> is taken from the session reading, as for any sample
    And <weekly-side>
    But NOT the weekly reading read as `0.0`

  Examples:
    | reader                  | session-side                                                               | weekly-side                                                                                                              |
    | `aggregate_with_roster` | the session summary, `cap_hits`, `time_at_cap_secs`, `seen` and `coverage` | the weekly summary is over the present weekly readings only — the interim `DimStats::ZERO` when none in the period carries the axis |
    | `blocked_windows`       | `session_blocked` from the session reading; the sample is covering         | `weekly_blocked` reads `false` — a window the account lacks never blocks it; `relief_of` is unchanged                      |
    | `account_velocity`      | `session_rate` and `session_runway_secs`                                   | `weekly_rate`, `weekly_runway_secs` and `weekly_headroom` are `None`                                                      |

Scenario Outline: the four two-window-premise comments say "every dimension the account has"
  # Tier B — pending: documentation. The mechanically-decidable half is AC-6's named check — #1600 § 8's query verbatim, `git grep -n -i -E 'both quota dimensions|both dimensions|both windows|both of its judgements' -- src/usage_stats.rs src/stats.rs src/reliability.rs`, which returns 7 lines (6 sites) at `fd8640b` and at the branch head exactly 3: the `including both of its judgements` line of `relief_of`'s doc (it names the two judgements, not the quota dimensions) and the two test comments `Both windows report` and `both windows are covering either way` (`src/usage_stats.rs:1875` and `:2109` at `fd8640b`, matched on text because the numbers shift), each dispositioned as not restating the posture, any other hit being a site this item missed; zero lines would mean the query did not run, so the three hits are their own canary at the branch head, and the narrower `both quota dimensions|both dimensions` returns zero lines there — and `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items`; the wording is the merge judge's. Nothing further is owed · covers AC-6
  Given the comment at <site> reads <today> at fd8640b
   When this item lands
   Then it states the posture "every dimension the account has" — <becomes>
    And the pull-request body carries #1600 § 8's query verbatim with its cardinality (7 lines, 6 sites at fd8640b; re-run at the branch head), the per-site disposition (4 corrected, 2 do not restate) and the canary hit `src/usage_store.rs:39` at `fd8640b` (#1598's commit corrects that hit, so at the branch head the three remaining hits are their own canary)
    # Two of these spell "both quota dimensions", which #1593's ADR sweep regex (`both dimensions|both windows`) does not match — the reason they are routed here rather than left to that sweep.

  Examples:
    | site                      | today                                                                                 | becomes                                                                                                                                                              |
    | `src/usage_stats.rs:17`   | "mean / peak / p95 utilisation for BOTH quota dimensions (session + weekly)"          | per quota dimension, each over the readings that carry that dimension — an account with no session window contributes to the weekly figures and to no session figure |
    | `src/usage_stats.rs:1090` | "an account blocked on BOTH dimensions returns only once the LATER window clears"     | an account blocked on every dimension it has returns only once the LATER window clears; a dimension the account lacks never blocks it                                |
    | `src/stats.rs`        | "One bound for both dimensions is a wrong answer even where the arithmetic is right." | one bound shared across dimensions is a wrong answer even where the arithmetic is right; each dimension an account's readings carry takes its own                   |
    | `src/stats.rs:1060`       | "Both dimensions' rates come from [`replay_velocity_ema`] (the #539 recipe);"         | each dimension's rate — for every dimension the account's readings carry; an absent dimension has no rate — comes from `replay_velocity_ema`                        |

Scenario: no numeric default on a sample's window read anywhere in the diff, tests included
  # Tier B — pending: the mechanically-decidable half is AC-8's named check — `git diff <#1598's commit> -- src/usage_stats.rs src/stats.rs src/reliability.rs | grep -n -E '^\+.*((unwrap_or(_else|_default)?|map_or(_else)?)\(|or\(Some\()'`, which lists the added lines that spell a default or a fold-with-default through those calls or `.or(Some(..))` (`unwrap_or(1.0)`, `unwrap_or_else(|| 0.0)` and `.or(Some(0.0))` included; a `match … None => 0.0` or an `if let … else { 0.0 }` is not a spelling it sees), each hit listed in the pull-request body with its disposition and none of them on a `Sample.session` / `Sample.weekly` read; the diff base is #1598's commit, because #1598's own `Some(..)` wrap edits sit in the same three files and `origin/main` would put its lines in this check — and the per-reader mutants above are what prove each `filter_map` / `is_some_and` load-bearing; the `Some(0.0)`-wrapper half (design premortem P-11, R-D4) is the merge judge's read of every wrapper. By design (AD-4) no source-lint test exists, so nothing further is owed · covers AC-8, AC-10
  Given this item's commits, diffed against #1598's commit
   When the added lines of `src/usage_stats.rs`, `src/stats.rs` and `src/reliability.rs` are read
   Then no read of `Sample.session` or `Sample.weekly` resolves the absent state with a default of any kind — `unwrap_or`, `unwrap_or_else`, `unwrap_or_default`, `map_or` or `map_or_else`, whatever the number
    And the pull-request body lists each hit of AC-8's grep with its disposition
    And no test asserts the absent state by wrapping an old literal in `Some(0.0)` — every § 7 test constructs at least one sample with an absent window — `session: None`, or `weekly: None` in T13–T15 — and asserts on that account's figures
    And the `Some(..)` wrap of the `sample` helpers and `Sample::new` literals carrying real readings is #1598's commit (its AC-11), not this item's
    And the commits, against `origin/main`, touch nothing outside those three files, sit after #1598's commit in #1598's pull request, which merges after the compile-unit pull request carrying #1597, and that pull request's head passes `cargo test --verbose` — the earlier commits alone need not
```

## Coverage

Each acceptance criterion of #1600, with the scenarios that cover it. A criterion no scenario covers
is named as such, with what grades it instead.

| AC | Scenarios | What grades it |
|---|---|---|
| AC-1 — session figures exclude a reading with no session window; weekly figures keep it | Rule 1 "a sample with no session reading is excluded…"; Rule 2 "the all-high census is over the accounts…" (its `contribution_share` step); Rule 5 outline (D12 default), the `aggregate_with_roster` row | T1 and its weekly twin T13 with AC-1's mutant record, and T3's `contribution_share` step with its own mutant (Tier A) |
| AC-2 — the all-high census is over the accounts that have the window (D5) | Rule 2 "the all-high census is over the accounts…", "a mixed-period account…", "a rostered account with no samples…", "without a known roster…" | T3, T4 and the roster-less twin T9 with AC-2's mutant records (Tier A); the committed `a_rostered_account_with_no_samples_stays_in_the_census_instead_of_vanishing` for the unsampled clause, under AC-2's careless-D5 mutant record (Tier A); the `all_high` and `census_over_roster` doc sentences by AC-2's two `git grep -c 'structurally absent session window'` checks |
| AC-3 — a weekly-only sample is never session-blocked, and the capacity census keeps it | Rule 2 "a weekly-only sample over the weekly line…", "a weekly-only account that is viable…"; Rule 5 outline, the `blocked_windows` row | T5, the set-membership test T10 and the weekly twin T14 with AC-3's mutant records (Tier A) |
| AC-4 — velocity is per axis over the readings that carry it | Rule 3, all three; Rule 5 outline, the `account_velocity` row | T6, the mixed-series T11, the vanished-window T12 and the weekly twin T15 with AC-4's mutant records (Tier A) |
| AC-5 — an absent reading is an unmeasured landing | Rule 4 outline (two rows); Rule 4 "the non-finite guard stays…" | T7 with AC-5's mutant record (Tier A); the committed canaries at `src/reliability.rs` and `:7485`; the constant at `:264` by `git diff` |
| AC-6 — the four comments say "every dimension the account has", and the sweep is recorded | Rule 5 outline "the four two-window-premise comments…" | the § 8 grep (7 lines, 6 sites at `fd8640b`; 3 lines at the branch head, which are their own canary), `cargo doc`, the merge judge; the pull-request body's disposition table |
| AC-7 — the interim zero is documented where it arises, with the sibling's number | Rule 1 "an account none of whose readings carries…"; Rule 1 "the stats wire and table carry the interim…" | T2 as the canary of the shape (Tier B, no discriminating mutant exists) and T8 as its wire half (Tier A); the five doc comments by `cargo doc` and the judge; #1601 AC-1 moves T2 to assert `None` (#1601 § 1: "its T2 moves to assert `None`") and its AC-2 retires T8 |
| AC-8 — no numeric default anywhere in the diff, tests included | every `But NOT` clause above; Rule 5 "no numeric default on a sample's window read…" | the per-reader mutants of T1, T3, T4, T5, T6, T7 and the weekly twins T13–T15; AC-8's grep as the signal, each hit listed in the pull-request body with its disposition; the judge's read of every wrapper |
| AC-9 — no wire byte and no golden moves | Rule 1 "the stats wire and table carry the interim…"; Rule 4 "the non-finite guard stays…" (the reliability constant) | `json_wire_is_byte_stable_vs_158_159`, `the_committed_stats_wire_golden_still_matches_the_socket_encoder`, and T8 for a weekly-only entry under `schema` 1 (Tier A); `git status --porcelain -- build/fixtures apps/menubar` empty after `cargo test --verbose`; the three constants by `git diff` |
| AC-10 — this item's share of the diff is bounded, and it lands in #1598's pull request | Rule 5 "no numeric default…" (its last step) | `git diff --stat origin/main`; the pull request's commit order; review, which includes the two comments at `src/reliability.rs` and `:7431` that cite `fold_landings`' old guard spelling (§ 5) |
| AC-11 — gates | none | the five `test` steps, `msrv`, both `deny` checks, in CI, each command and last line in the pull-request body; never `cargo test -- --include-ignored` |
