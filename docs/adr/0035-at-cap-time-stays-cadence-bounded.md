---
type: architecture-decision-record
number: 34
title: "`time_at_cap_secs` stays cadence-bounded; the asymmetry with the census is stated on the surface"
date: 2026-08-11
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0035: `time_at_cap_secs` stays cadence-bounded; the asymmetry with the census is stated on the surface

## Status

**Accepted** — 2026-08-11. Settles the three-way choice issue **#1098** raised against **#1030**'s
census anchoring: *should the per-account at-cap time be anchored too, documented as different, or
left alone?* Companion to **ADR-0033**, which settled the other open question #1030 left behind.

## Context

`stats` prints two durations that are computed at the **same water, over the same readings**, and
measures them under **different validity rules**.

`params_from` (`src/stats.rs`) passes `Config::swap_threshold` twice — once as the session cap, once
as the census threshold — so in production the cap and the all-accounts-high water are one value.
Both figures then select the same readings. But:

- `time_at_cap_secs` sums each at-cap reading's **cadence-bounded** forward window
  (`validity_windows`): `[ts, min(next_ts, ts + stale_after))`.
- `all_high_secs` runs each high reading out to its own **carried session reset** (`high_windows`,
  issue #1030), because session utilisation only climbs within a window, so a reading at/above the
  water stays true until the reset.

At the poll sparsity #1030 exists for — an out-of-rotation saturated peer polled at
`exhausted_poll_secs` against a `poll_secs` staleness horizon — the two land an order of magnitude
apart. One account, 24 hourly readings at `session = 0.95`, each carrying a 5 h session reset, over
a 24 h window: **7 200 s** against **86 400 s**. That is asserted rather than recalled, by
`the_two_at_water_figures_diverge_by_an_order_of_magnitude_on_one_reading_set`
(`src/usage_stats.rs`), so the figures move with the behaviour instead of going stale here.

Both are honest under their own rule; neither invents time. The defect #1098 reports is that
`stats` renders `t@cap` in the accounts table and `all-accounts-high` in the roster line beneath it,
and **nothing on the page says the two were measured differently**. Widening `validity_windows` was
deliberately out of #1030's scope, which was the right call — and what it left behind is a visible
cross-field contradiction.

## Decision

**`time_at_cap_secs` stays cadence-bounded. The asymmetry is made legible where a reader meets it:
on the rendered surface and in the wire's field docs.**

`crate::stats`'s `validity_rule_line` states both rules above the roster block of the numeric view —
the one surface that renders both figures — and `AccountWire::time_at_cap_secs` /
`RosterWire::all_high_secs` carry the same statement for a machine reader. No aggregate moves, no
wire key is added, and `JSON_SCHEMA_VERSION` does not change.

### Why anchoring does not close the complaint

**It relocates the contradiction rather than removing it, and moves it somewhere with no
reconciling figure.** `time_at_cap_secs` renders as `t@cap`, in the same table ROW as `cov` —
`coverage`, which is `seen ÷ expected` against the poll cadence. That is a **sample-count** ratio,
not a time-coverage one, so it cannot follow an anchor. On the fixture above, an anchored at-cap
time would be 86 400 s — the whole window — beside a `cov` of **8 %**. Same shape of defect, one
row tighter, and this time with nothing on the page to reconcile the pair.

The census can be anchored honestly precisely because it does not have that problem: it carries
`all_high_covered_secs`, measured over the **same** anchored windows, which `roster_line` renders as
`all in view N% of the window`. Numerator and denominator move together. Giving the per-account row
an equivalent denominator means a new always-present key on `AccountWire` — which reshapes
`StatsWire`, moves `JSON_SCHEMA_VERSION`, and drags the hand-maintained Swift `StatsWire` mirror and
its fixtures with it.

**So the surface statement is required under every option and the anchoring is not.** Even a fully
anchored `time_at_cap_secs` leaves a reader unable to tell why `t@cap` and `cov` disagree. Whatever
else is done, the render has to say which rule produced which figure — which makes that the part
worth doing, and the aggregate change the part to justify separately.

### What real data says

The frozen 48 h corpus (`build/fixtures/capacity-replay-corpus.tsv`) witnesses the divergence, and
cannot witness its worst case. At the 0.80 water — the only one this corpus reaches at all — two of
its six accounts record cap hits, and anchoring would move their at-cap time by:

| account | readings | coverage | cadence-bounded | anchored | factor |
|---|---|---|---|---|---|
| `a1` | 346 | 60.1 % | 20 217 s | 26 015 s | 1.29× |
| `a2` | 870 | 100 % | 3 889 s | 15 343 s | 3.94× |

Neither is sparsely polled, so neither shows the order-of-magnitude gap — and neither would render a
contradiction against its own `cov` cell if anchored. The accounts that ARE sparsely polled on this
corpus (`a4`–`a6`, coverage ~7.6 %) are weekly-saturated and session-**low**, peaking below 0.16, so
they never reach the water and their at-cap time is zero under both rules. That is the same
structural reason ADR-0033 records for this corpus reporting an UNKNOWN census, and the same reason
#1030's own gate is synthetic. `on_the_replay_corpus_anchoring_at_cap_time_would_move_it_by_a_factor_of_a_few`
pins the BAND those factors sit in — anchoring widens the at-cap span, by less than an order of
magnitude, on exactly these two accounts — and fails if the corpus ever starts showing the worst
case, so this section cannot quietly understate the evidence. **The cells themselves are measured,
not asserted**: no test in `src/` names 1.29, 3.94, 20 217, 26 015, 3 889 or 15 343, so a change to
`validity_windows` or `high_windows` that moved a factor while keeping it inside the band would
leave this table stale and green. The synthetic `(7_200, 86_400)` gate is what an aggregate move
would most likely red first.

## Alternatives considered

1. **Document the asymmetry on the surface** (issue #1098's option 1) — **chosen**.
   - **Pros**: closes the reported defect exactly where it is reported, at the cost of one rendered
     line; no aggregate moves, so every pinned `time_at_cap_secs` expectation and every #158
     consumer is untouched; and it is the part that is required under every option anyway.
   - **Cons**: the two figures still differ, and a reader who does not read the line still compares
     them. Documentation cannot make two rules into one.
   - **Why chosen**: it is the necessary half of every option, and the only half whose blast radius
     is the render.

2. **Anchor `time_at_cap_secs` on the same "only an at-cap reading, only its session expiry" rule**
   (option 2).
   - **Pros**: the most consistent-looking answer, and #1030's argument does transfer verbatim — a
     reading at/above the water is a statement that stays true until the reset, whichever threshold
     named it.
   - **Cons**: it does not close the complaint, it relocates it into the same table row as `cov`,
     where no denominator reconciles the pair (see above). It also moves a separately-ratified
     metric for every account, re-deriving every pinned expectation and every #158 consumer, to buy
     an agreement the surface would still have to explain.
   - **Why rejected**: the defect is that the surface does not state its rules. Changing which pair
     of figures disagrees does not state them.

3. **Anchor `time_at_cap_secs` AND give the per-account row its own anchored covered-seconds**, so
   the numerator and denominator move together the way the census's do.
   - **Pros**: the only version of alternative 2 that is actually honest — it is what makes the
     census's anchoring defensible, applied one level down.
   - **Cons**: a new always-present `AccountWire` key, hence a `JSON_SCHEMA_VERSION` bump, the
     hand-maintained Swift `StatsWire` mirror, its fixtures, and a second coverage notion beside
     `coverage` on a surface that already has one. That is a separately-ratifiable change with its
     own consumers, not a reconciliation of two existing figures.
   - **Why rejected**: on scope, not on merit. This is the alternative worth writing down — it is
     the one a future reader will re-derive, and the one to revisit if the per-account row ever
     needs a time-coverage denominator for its own reasons.

4. **Leave as-is and record why** (option 3), on the ground that the two answer different questions.
   - **Pros**: true as far as it goes, and cheapest of all — the field doc already recorded the
     divergence.
   - **Cons**: that field doc is what the issue was filed against. It is reachable only by reading
     both window functions side by side; the operator holding a `stats` render has no path to it.
   - **Why rejected**: a divergence documented only where the reader is not is not documented.

## Consequences

### Positive

- **Both figures keep their meaning.** `time_at_cap_secs` stays the gap-honest, cadence-bounded
  figure the module's own gap-honesty note describes it as, and the census keeps the anchoring
  #1030 ratified. Nothing was widened to make two numbers agree.
- **The rules reach the reader.** The numeric render states them above the block that carries both,
  and the wire states them on the two fields.
- **No pin was re-derived and no schema moved.** Every `time_at_cap_secs` expectation, the `#158`
  consumers, the Swift `StatsWire` mirror and its fixtures are untouched.
- **The rejected alternative is priced.** The `cov` collision, the wire cost, and the corpus factors
  are recorded, so the next person to have this idea can skip the measurement.

### Negative / trade-offs

- **The divergence survives.** Two durations at one water still differ, and the fix is a sentence.
  A reader who skips it is where they were. Accepted: the alternative that removes the divergence
  introduces a tighter one, and the alternative that removes it honestly is a schema change.
- **The note renders on every numeric view**, including the many where neither figure is
  interesting. That is deliberate — it states a rule, not a reading, so it must not appear to come
  and go with the data — but it is a permanent line on a surface that is otherwise all figures.
- **The chart view says nothing.** It renders the census without `t@cap`, so it has no pair to
  reconcile and is deliberately left unannotated. If a `t@cap` column is ever added there, the note
  is owed there too; `the_chart_render_states_no_at_cap_rule_because_it_shows_no_at_cap_column`
  fails if that column appears without it.
