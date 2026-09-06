<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the Swift
XCTest bundle. These scenarios pin each acceptance criterion in scenario form; do not read a
written scenario as a passing test.

Built by issue #1488 (2026-09-06). Each scenario now carries a `# Binds:` line naming the test that
holds it; an unbound scenario is a statement of intent, never evidence. Two are deliberately left
unbound and say so in place — both are statements about what the DIFF does not contain, which no
assertion can carry.
-->

# Feature: the first-sight SLI has a durable readout, and withholds what it could not measure

Issue #1488 · `active-account-observation-continuity` R-4 / R-5 · design § 7 (D-4 / D-4b)

Example Mapping: 🟦 4 rules · 🟩 10 examples · 🟥 0 open

> `PostSwapFirstSightLatency` is pinned as Planguage — p50/p95 over a 7-day window, GOAL
> `p95 <= 2*poll_secs/N`, PAST p50 182 s / p95 436 s, worst observed 638 s. The instrument exists:
> the `ObservationGapEnter` / `ObservationGapExit` render arms in `src/observability.rs` — but it
> is edge-triggered PAST the bound, so it instruments the **breach tail**, not the whole
> distribution, and the `GOAL` half of that Planguage tag has no source (design § 11 OQ-3). But the
> pair reaches **no JSON wire**, so the SLI has no durable readout and nothing moves when a fix lands.
>
> The readout is a `first_sight` block on `ReliabilityWire`, mirroring `BlindEpisodesWire` — the same
> enter/exit-pair census, because it faces the same four pathologies.

## Rule 1 — the percentiles follow the METER's computation, over a censored population

```gherkin
Scenario: the SLI is computed from the qualifying exits only
  Given observation_gap_exit lines in the active --since window
   When the first_sight block is computed
   Then only lines with was_active=true and swapped_away=false contribute percentiles
    And the percentiles are computed as p50 and p95 of elapsed_secs
    And they are NAMED for the censored population they measure, not as a bare p50 / p95
    # Binds: reliability::tests::the_first_sight_percentiles_cover_only_the_qualifying_exits for the
    # filter and the computation, and reliability::tests::the_first_sight_wire_never_publishes_a_bare
    # _percentile_name for the naming Then, which asserts the KEYS as bytes on the wire (breach_p50 /
    # breach_p95 present, no bare p50/p95, and no targets/met pair). The human surface is held to the
    # same by reliability::tests::the_first_sight_render_carries_its_censoring_and_grades_no_goal.
    # That last Then is the K-6 mitigation as an ASSERTION rather than a comment. A bare p50 /
    # p95 field name is the defect; see CENSORED-POPULATION NAMING below.
    # This follows the METER's COMPUTATION (p50/p95 of elapsed_secs over the window) but is not the
    # METER verbatim: the METER specifies no line filter, and the was_active / swapped_away filters
    # here are this design's, not section 6's. Following the computation is also exactly what
    # leaves the GOAL ungraded.
    # observation_gap_enter fires only when elapsed > 2*poll_secs/N (strictly; the comment at its
    # writer says so), so this population is LEFT-CENSORED at the bound: every sample breaches it.
    # p50 is therefore the median of BREACHES, and p95 <= GOAL is unreachable while n > 0 -- while
    # at n = 0 the next rule withholds the figure. The GOAL-met state is not representable.
    # Mind the factor of two. With T = 2*poll_secs/N: the ENTRY edge is T, GOAL is p95 <= T, and
    # FAIL is any occurrence > 2T -- TWICE the entry edge. So filtering the emitted set on
    # elapsed_secs > 2T CANNOT produce a false negative: empty means no FAIL, conclusively. It is an
    # UPPER BOUND and not an exact count -- the entry anchor is observed.max(designated), so a
    # mid-tenure gap on a long-active account passes this same filter while sitting outside section
    # 6's post-swap SCALE. Do not build a FAIL verdict that treats a non-empty result as certain. A
    # detector built at T instead would fire at half the threshold section 6 states. (Section 6's
    # Planguage is itself pipeline-authored and ratification-pending per that PRD's section 11 --
    # it is the stated requirement, not an operator-ratified one.)
    # What is NOT computable is GOAL's p95, which is stated over the WHOLE distribution -- and this
    # set omits everything at or below T. That needs a source recording within-bound first sights;
    # OQ-3 in the solution design's section 11, and it is OPEN.
    #
    # CENSORED-POPULATION NAMING -- a constraint on this work item, not a remark. The wire field
    # names must carry the censoring at the point of use: a bare p50 / p95 invites a reader to
    # compare it against the section 6 GOAL, the one comparison it cannot support. Name them for
    # what they measure (breach_p50 / breach_p95, or equivalent) and keep the human `reliability`
    # render consistent with the wire. This is design section 11 K-6's mitigation -- the highest-
    # rated risk in that register -- and it lives here because this is what an implementer reads.

Scenario: a gap that ended by being parked is not a first sight
  Given an observation_gap_exit line with swapped_away=true
   When the block is computed
   Then it is excluded from the percentiles
    And it is counted separately
    # Binds: reliability::tests::a_gap_that_ended_by_parking_leaves_the_percentile_subject_empty for
    # the exclusion and the separate count, and reliability::tests::a_parked_gap_cannot_trip_the_fail
    # _detector for the second consumer — the FAIL detector applies the SAME exclusion, so a 9000s
    # parked gap is not a FAIL occurrence. One decision in two places, pinned so they cannot drift.
    # Such a gap ended by the account being parked, not by being observed. Folding it in would
    # flatter the metric — it would look like an observation that never happened.
```

## Rule 2 — an empty subject withholds the figure rather than reporting zero

```gherkin
Scenario: a window with no change of active
  Given the --since window contains no qualifying observation_gap_exit line
   When the block is computed
   Then the percentiles are withheld
    And they are not reported as 0
    And the sample count n is published beside them
    # Binds: reliability::tests::an_empty_window_withholds_the_first_sight_percentiles, which checks
    # all three surfaces — the struct, the human render ("none in view"), and the JSON wire (null,
    # not 0). It also pins the FAIL bound as ungraded rather than passing: with no entry in view the
    # daemon's own threshold is unobservable to this offline reader, so Some(0) would assert a PASS
    # the log cannot support. reliability::tests::an_unobservable_bound_leaves_the_fail_criterion
    # _ungraded_not_passing isolates that half.
    # p95 = 0 over zero samples asserts PERFECT latency where nothing was measured. This is the
    # discriminator against RefreshTokenLossWire's plain-count shape, whose own doc comment says a
    # zero there IS a real reading. Here it is not.

Scenario: a thin window is legible as thin
  Given the window contains very few qualifying lines
   When the block is rendered
   Then n is visible beside the percentiles
    # Binds: reliability::tests::json_render_is_stable_schema_13, whose pinned document carries "n"
    # as the FIRST key of the first_sight block, immediately above breach_p50 — so a consumer cannot
    # read a percentile without having read its denominator. The human render's "qualifying exits:
    # n=1 of 2 in view" line is pinned by the three committed cli-render goldens.
    # The denominator is published, not implied — a reader must be able to see the figure is thin
    # rather than discovering it later.
```

## Rule 3 — the four census pathologies are counted, not silently dropped

```gherkin
Scenario: a --since cutoff or log rotation severed the pair
  Given an observation_gap_exit whose matching enter is not in view
   Then it is counted as an exit-without-enter
    And entry and exit counts visibly need not balance
    # Binds: reliability::tests::the_first_sight_census_counts_all_four_pair_pathologies, and
    # reliability::tests::the_window_bounds_the_first_sight_readout_and_discloses_the_pair_it_severs
    # for the --since cutoff creating one for real. The severed exit still contributes its latency:
    # one line carries the whole gap, so a cutoff costs no percentile sample.

Scenario: a daemon restart lost the in-memory anchor
  Given an entry superseded by a later entry for the same account
   Then it is counted as anchor-lost, apart from never-recovered
    # Binds: reliability::tests::the_first_sight_census_counts_all_four_pair_pathologies, which
    # asserts n_anchor_lost == 1 AND n_never_recovered == 1 over one fixture — the supersession must
    # not also land in the worst tail, which is the whole reason the two counts are separate.
    # The anchor is in-memory (observation_gap in src/daemon.rs), so a restart severs the
    # episode. Counting it as a recovery, or as a worst-case tail, would both be wrong.
    # NOTE: #1486's event=daemon_build line makes restart boundaries visible in the log for the
    # first time. Independent items; neither blocks the other; they compose here.

Scenario: an episode still open at the horizon
  Given an entry with no exit by the end of the window
   Then it is counted as never-recovered
    # Binds: reliability::tests::the_first_sight_census_counts_all_four_pair_pathologies.
    # A gap that never closed is the WORST case, not a missing sample. It must not vanish.

Scenario: an unparseable line
  Given a line with an unreadable ts, acct, or elapsed_secs
   Then it is counted as malformed
    # Binds: reliability::tests::the_first_sight_census_counts_all_four_pair_pathologies, over both
    # halves — an exit with no elapsed_secs and an entry with no threshold_secs. Neither becomes a
    # placed line, so n_entered / n_exited stay counts of PLACED lines and the drop is disclosed.
    # A parse failure that is silently skipped makes the corpus partial without saying so.
```

## Rule 4 — one wire moves, and only one

```gherkin
Scenario: the reliability wire bumps, and nothing else does
  Given the first_sight block is added
   Then JSON_SCHEMA_VERSION in src/reliability.rs goes from 12 to 13
    And STATUS_SCHEMA_VERSION is unchanged
    And no status or watch golden is regenerated
    And no Swift fixture is swept and no Swift file is edited
    # Binds: the bump itself is held by reliability::tests::json_render_is_stable_schema_13 and
    # reliability::tests::the_json_wire_carries_the_loss_block_under_schema_13, plus the issue #913
    # coupling test that ties RELIABILITY_USAGE's advertised schema: to the constant. The three
    # NEGATIVE clauses bind to NO assertion, deliberately and unavoidably: they are statements about
    # what the diff does NOT contain, and no test can witness the absence of an edit. The evidence
    # is the diff — src/reliability.rs, src/cli.rs, the three reliability cli-renders, and the two
    # documents; no apps/menubar path, no build/fixtures/wire-*.json.
    # This repo has FOUR independent schema wires. reliability has no Swift surface at all —
    # WireModel.swift mirrors StatsWire, not this one. Regenerating build/fixtures/wire-*.json or
    # grepping apps/menubar/Tests/Fixtures.swift means you are on the wrong wire.

Scenario: the usage-sample store is NOT repaired by this
  Given record_usage_sample remains inside the poll_idx guard
   Then the sample store still cannot see a never-attempted poll
    # Binds: NOTHING, deliberately — this is a scope bound, not a behaviour this item adds. It is
    # carried in the code where a reader meets it (the FirstSight type doc), in the § 6b row, and in
    # the PR body, because the harm is a reader trusting the WRONG surface rather than a defect a
    # test could catch.
    # Stated as a scenario because it is the misreading most likely to cause harm: a reader who
    # assumes both surfaces were fixed will trust the wrong one. This item repairs the EVENT LOG
    # readout only.
```
