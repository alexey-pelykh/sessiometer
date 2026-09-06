<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the Swift
XCTest bundle. These scenarios pin each acceptance criterion in scenario form; do not read a
written scenario as a passing test.

Built by issue #1488 (2026-09-06). Each scenario now carries a `# Binds:` line naming the test that
holds it; an unbound scenario is a statement of intent, never evidence. Exactly ONE scenario is
unbound — "the usage-sample store is NOT repaired by this", a scope bound rather than a behaviour
this item adds — plus the three NEGATIVE clauses inside "the reliability wire bumps, and nothing
else does", which are statements about what the DIFF does not contain and which no assertion can
carry. Those are two different reasons, and an earlier form of this note gave the second one for
both while counting them as two scenarios.
-->

# Feature: the first-sight SLI has a durable readout, and withholds what it could not measure

Issue #1488 · `active-account-observation-continuity` R-4 / R-5 · design § 7 (D-4 / D-4b)

Example Mapping: 🟦 4 rules · 🟩 13 examples · 🟥 0 open

> `PostSwapFirstSightLatency` is pinned as Planguage — p50/p95 over a 7-day window, GOAL
> `p95 <= 2*poll_secs/N`, PAST p50 182 s / p95 436 s, worst observed 638 s. The instrument exists:
> the `ObservationGapEnter` / `ObservationGapExit` render arms in `src/observability.rs` — but it
> is edge-triggered PAST the bound, so it instruments the **breach tail**, not the whole
> distribution, and the `GOAL` half of that Planguage tag has no source (design § 11 OQ-3). Before
> this item the pair reached **no JSON wire**, so the SLI had no durable readout and nothing moved
> when a fix landed. As of schema:13 it does; `docs/design/daemon-diagnostic-integrity-solution-design.md`
> § 7 and `docs/requirements/daemon-diagnostic-integrity.md` still describe the pre-#1488 state.
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
    # FAIL is any occurrence > 2T -- TWICE the entry edge. Since 2T > T, no occurrence that would
    # trip FAIL is missing from the EMITTED set, so the censoring alone cannot lose one. It is an
    # UPPER BOUND and not an exact count -- the entry anchor is observed.max(designated), so a
    # mid-tenure gap on a long-active account passes this same filter while sitting outside section
    # 6's post-swap SCALE. Do not build a FAIL verdict that treats a non-empty result as certain. A
    # detector built at T instead would fire at half the threshold section 6 states.
    # An earlier form of this note went further and said "empty means no FAIL, conclusively". That
    # is FALSE as an unconditional claim, and the two ways it fails are pinned as scenarios under
    # Rule 3: one 2T is applied to every exit while a window can hold several T (the rotation length
    # is the divisor), and three populations sit outside the filter entirely. Take the SMALLEST T in
    # view, and publish whether the zero carries its strong reading rather than asserting it always
    # does. (Section 6's
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

Scenario: a gap whose designation moved is not a first sight of the current one
  Given an observation_gap_exit line with swapped_away=true
   When the block is computed
   Then it is excluded from the percentiles
    And it is counted separately
    # Binds: reliability::tests::a_gap_that_ended_by_parking_leaves_the_percentile_subject_empty for
    # the exclusion and the separate count, and reliability::tests::a_parked_gap_cannot_trip_the_fail
    # _detector for the second consumer — the FAIL detector applies the SAME exclusion, so a 9000s
    # parked gap is not a FAIL occurrence. One decision in two places, pinned so they cannot drift.
    # Read the emitter before reading this exclusion. The daemon pushes NO exit until an
    # observation lands newer than the anchor, so a swapped_away episode DID end in a real
    # observation — this is not "nothing ever looked". What moved is which designation the sample
    # belongs to: the episode opened on the active account and closed after the daemon had swapped
    # away, or after the account left and came back. Nor does folding it in flatter the metric; in
    # the committed fixture it moves p50 from 300 to 420, i.e. the wrong way. The exclusion is about
    # WHICH designation the sample measures, and about nothing else.
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
    # n=1 of 2 in view" line is pinned by TWO of the three committed cli-render goldens (full and
    # windowed); the third, empty-log, takes the withholding branch and pins "none in view (n=0)".
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

Scenario: the window holds more than one entry edge
  Given observation_gap_enter lines carrying different threshold_secs
   When the FAIL bound is derived
   Then it is twice the SMALLEST edge in view
    And the count of distinct edges is published
    # Binds: reliability::tests::the_fail_bound_is_the_smallest_edge_in_view_so_a_mixed_window
    # _cannot_hide_a_fail, whose fixture returns 1 under the smallest edge and 0 under the latest —
    # so it discriminates rather than merely agreeing. Its sibling ..._tracks_a_tightened_edge_too
    # covers the case where smallest and latest coincide, which passes either way.
    # This is not a hand-built input. observation_gap_threshold() divides by the rotation length,
    # which counts enabled, un-quarantined accounts, so quarantining one account of four raises T
    # from 150s to 200s mid-window with no restart and no config edit — and raises it in the
    # under-counting direction, precisely when the fleet is already degraded. One 2T is applied to
    # every subject exit, so grading a tighter episode against a looser bound reports a real FAIL as
    # absent. The minimum can only over-count, which is what an upper bound is for.

Scenario: a zero FAIL count is qualified by what the filter could not see
  Given the block is computed
   When n_over_fail_bound_upper is 0
   Then whether that zero is conclusive is published beside it
    # Binds: reliability::tests::a_zero_fail_count_is_conclusive_only_when_nothing_went_unseen,
    # which asserts each disqualifier ALONE strips the strong reading, on both surfaces.
    # Three populations sit outside the 2T filter and each can hide an occurrence: an unplaceable
    # line never becomes an exit, a still-open gap has no elapsed_secs yet, and a severed exit has
    # no T of its own so the window's bound may exceed the one its episode ran under. Derived once
    # (fail_zero_is_conclusive) rather than restated per surface, so the human render and the wire
    # cannot disagree about a verdict's strength. The committed windowed golden takes the
    # NOT-conclusive branch off its own severed pair.

Scenario: an unreadable boolean flag
  Given an observation_gap_exit line whose was_active or swapped_away is not exactly true or false
   When the line is parsed
   Then it is counted as malformed
    And it contributes no percentile and no FAIL sample
    # Binds: reliability::tests::an_unreadable_flag_is_malformed_and_never_a_population_fact, which
    # truncates a genuinely swapped-away exit by exactly ONE trailing byte and requires the verdict
    # not to flip.
    # swapped_away is the LAST field the emitter writes, so a torn append reaches it first. Under a
    # lenient `== "true"` parse the truncated line read as swapped_away=false — a QUALIFYING first
    # sight — contributing a percentile and a FAIL sample while n_malformed, the field whose whole
    # job is making unreadable lines visible, stayed 0. Silent exclusion is "the safe direction" for
    # the percentiles only; for a detector whose value is that its zero can be conclusive, it is the
    # unsafe one.

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
