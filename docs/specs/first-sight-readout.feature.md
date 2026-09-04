<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the Swift
XCTest bundle. These scenarios pin each acceptance criterion in scenario form; do not read a
written scenario as a passing test.

No scenario below names a binding test yet — the item is scoped, not built. Add a *Binds* line to
each as its test lands; an unbound scenario is a statement of intent, never evidence.
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
    # p95 = 0 over zero samples asserts PERFECT latency where nothing was measured. This is the
    # discriminator against RefreshTokenLossWire's plain-count shape, whose own doc comment says a
    # zero there IS a real reading. Here it is not.

Scenario: a thin window is legible as thin
  Given the window contains very few qualifying lines
   When the block is rendered
   Then n is visible beside the percentiles
    # The denominator is published, not implied — a reader must be able to see the figure is thin
    # rather than discovering it later.
```

## Rule 3 — the four census pathologies are counted, not silently dropped

```gherkin
Scenario: a --since cutoff or log rotation severed the pair
  Given an observation_gap_exit whose matching enter is not in view
   Then it is counted as an exit-without-enter
    And entry and exit counts visibly need not balance

Scenario: a daemon restart lost the in-memory anchor
  Given an entry superseded by a later entry for the same account
   Then it is counted as anchor-lost, apart from never-recovered
    # The anchor is in-memory (observation_gap in src/daemon.rs), so a restart severs the
    # episode. Counting it as a recovery, or as a worst-case tail, would both be wrong.
    # NOTE: #1486's event=daemon_build line makes restart boundaries visible in the log for the
    # first time. Independent items; neither blocks the other; they compose here.

Scenario: an episode still open at the horizon
  Given an entry with no exit by the end of the window
   Then it is counted as never-recovered
    # A gap that never closed is the WORST case, not a missing sample. It must not vanish.

Scenario: an unparseable line
  Given a line with an unreadable ts, acct, or elapsed_secs
   Then it is counted as malformed
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
    # This repo has FOUR independent schema wires. reliability has no Swift surface at all —
    # WireModel.swift mirrors StatsWire, not this one. Regenerating build/fixtures/wire-*.json or
    # grepping apps/menubar/Tests/Fixtures.swift means you are on the wrong wire.

Scenario: the usage-sample store is NOT repaired by this
  Given record_usage_sample remains inside the poll_idx guard
   Then the sample store still cannot see a never-attempted poll
    # Stated as a scenario because it is the misreading most likely to cause harm: a reader who
    # assumes both surfaces were fixed will trust the wrong one. This item repairs the EVENT LOG
    # readout only.
```
