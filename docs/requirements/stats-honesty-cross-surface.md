---
title: Stats Honesty — Coverage Gating, Runway Degeneracy, and Cross-Surface Parity
scope: stats-roster-aggregates
created: 2026-08-04
status: draft
dor_status: passed-with-findings
source: session-context investigation (`/investigate` 2026-08-03), re-grounded at cb3eaca
parent-requirements: private HQ, REQ-STA-* family (prd-stats) — not dereferenceable from here; see § 0
formulation: {}
features:
  census-coverage-gate: {stage: requirements, tracks: {}}
  runway-degeneracy: {stage: requirements, tracks: {}}
  runway-representativeness: {stage: requirements, tracks: {}}
  capacity-holds-on-panel: {stage: requirements, tracks: {}}
  runway-on-panel: {stage: requirements, tracks: {}}   # added 2026-08-04 by operator scope amendment (R-17)
  census-window-anchoring: {stage: requirements, tracks: {}}   # added 2026-08-04 by second operator scope amendment (R-18, R-19)
  stats-parity-coverage: {stage: requirements, tracks: {}}
  gap-state-reference: {stage: requirements, tracks: {}}
artifacts:
  design-doc: docs/design/stats-honesty-cross-surface-solution-design.md
  requirements-brief: docs/briefs/2026-08-04-requirements-stats-honesty-cross-surface.md
  design-brief: docs/briefs/2026-08-04-design-stats-honesty-cross-surface.md
  scope-brief: docs/briefs/2026-08-04-scope-stats-honesty-cross-surface.md
---

# PRD — Stats Honesty: Coverage Gating, Runway Degeneracy, and Cross-Surface Parity

> **Provenance warning, read before acting.** This PRD was authored by an AI pipeline (`/scope` Stage 1)
> from an investigation the operator triggered and two verbatim defect reports they filed. It is a
> **remediation PRD**, not a product PRD: most of what it asks for is already ratified upstream in
> `hq/strategy/prd-stats.md` and is simply not enforced on every surface. Every requirement carries
> `Origin` + `Ratification`. Two requirements — **R-12** and **R-13** — are pipeline-authored and were
> **decided, not surfaced**, with the reasoning recorded in § 12. One genuinely operator-owned question
> remains open and is named there.

## 0. Why this PRD exists in the code repo, and what owns what

The product requirements for `stats` live in the private HQ: `hq/strategy/prd-stats.md`, IDs
`REQ-STA-{B,C,CFG,I,Q,SUR}-NNN` (21 of them). That is the **parent**. This document does not restate,
amend, or compete with it.

This PRD exists because a keystone requirement in that parent is **satisfied on one surface and
violated on another, with nothing in CI able to tell** — and because one shipped figure
(`fleet-runway`) has **no parent requirement at all**. It is therefore a *conformance-and-coverage*
PRD, matching the house pattern already set by the two remediation PRDs beside it
(`panel-presentation-reference-coverage.md`, `menubar-accessibility-reachability.md`).

**Cross-boundary caution.** `hq/` is a **separate private repo**. Every REQ-STA-* citation below
carries its claim **quoted in-band** so this document remains readable and actionable by anyone who
cannot reach the HQ — an executor in a firewalled subprocess, a fresh clone, a future reader after
the sibling checkout moves. The `parent-requirements:` frontmatter path is **provenance, not a
dereferenceable dependency**.

## 1. Problem Statement

**Current state.** On a week in which the operator's entire six-account fleet was saturated and
*no account was usable for days*, every roster-level figure the tool reports was wrong, and each was
wrong in the direction of **calm**:

| Surface | Reported | Reality |
|---|---|---|
| Menubar Stats tab | `All accounts ≥95% at once — 0 episodes (0s) · swaps 39 · last 7 days` | The census was **unmeasurable** — `all_high_covered_secs == 0`. Not one instant in 7 days had all six accounts jointly observed. |
| `stats` CLI | `accounts last ~648427` (days) | Combined burn was ≈ 0 and the quotient **saturated**. A live capture the same session read `runway_secs: 9223372036854775807` — `i64::MAX`, 106,751,991,167,300 days. |
| Menubar Stats tab | *(capacity holds not shown at all)* | `capacity_holds = 6`, `capacity_hold_covered_secs = 386747`, held **≥ 10h22m** — the one metric that actually answers "why can no account be used", visible only in the CLI. |

**Affected user.** The sole operator, who is also the sole developer. There is no research to run: the
operator reading the screen *is* the acceptance test, and it already failed — twice, in their words
("wtf", "that's wrong").

**Why these are one problem, not three.** Each figure has a **precondition for being meaningful** — a
coverage denominator, or a rate large enough to divide by — and in each case the code *computes and
carries that precondition* but some render does not consult it. The failure is uniform: **a degraded
input rendered as a confident measurement.**

**Why now — the failure is correlated, not random.** These surfaces degrade *precisely when the
operator most needs them*. The daemon polls only the **active** credential each tick, so idle accounts
go stale; when the whole fleet is saturated and swapping is blocked, joint coverage collapses and burn
rates decay toward zero. The conditions that break the metrics **are** the conditions the metrics exist
to report. A tool whose readings fail exactly on the bad day is worse than one with no readings.

**Root cause, and why it is not "two bugs".** `hq/strategy/prd-stats.md:121` already ratifies the
governing rule as a **keystone**:

> **REQ-STA-B-008** `[keystone]` — **Gap honesty.** A period with no samples for an account
> (daemon-down window, throttled/cleared poll) SHALL be treated as **UNKNOWN — never zero, never
> exhausted, never healthy**. Aggregates SHALL carry a **coverage** figure (samples-seen ÷
> samples-expected) per account/period; low-coverage periods SHALL be annotated; charts SHALL render
> gaps as **breaks**, never as a drop to 0%. *(Correctness AND firewall: a daemon-down week must not
> read as "underused → cancel".)*

The Rust producer restates it at the field, twice, in the imperative:

> `src/usage_stats.rs:288` — *"Surfaces **MUST** consult the denominator and render UNKNOWN rather
> than a bare `0` — an unmeasurable period is not a calm one (issue #804, REQ-STA-B-008)."*
> `src/usage_stats.rs:341` — *"Surfaces **MUST** consult it and render their own gap sentinel,
> **exactly as REQ-STA-B-008 already requires** of the utilisation census."*

The CLI obeys. The panel never received the field: `StatsRoster` in `apps/menubar/Sources/WireModel.swift`
decodes **five** keys and `all_high_covered_secs` is not among them, so
`StatusPanelFormat.statsAggregateText` (`:2218-2224`) renders episodes and duration unconditionally.
The value it needs **is already on the wire** (`RosterWire`, `src/stats.rs:2101`, serialized `:2351`).

So the root cause is **not** that anyone disagreed about honesty. It is that **the honesty rule is
enforced by memory, one surface at a time.** The repo owns a mechanism that would have caught this —
the ADR-0026 cross-surface parity contract (`build/fixtures/cross-surface-severity.json` +
`src/cross_surface.rs`, issue #768): one byte-pinned manifest, two independent conformers, so neither
surface can move alone. It pins daemon-fault ranks, account severity bands and expiry cells, and it
explicitly enumerates three `uncovered_axes` under its own doctrine *"Stating the boundary beats
implying coverage."* **The roster/stats aggregate is in neither list** — not covered, and not declared
uncovered. That gap is the root cause of this defect's whole class, and closing it is the prevention.

**A second, independent gap.** `fleet-runway` appears **nowhere** in `hq/strategy/prd-stats.md`
(verified: zero occurrences). It exists only in `design-stats.md`, which extends gap honesty to it —
*"`signal`/`velocity`/`runway` (any observed-only column) render `—` for `seen==0`"* (`:128`) — but
only for the **no-samples** case. The degenerate-rate case falls in the crack between "no samples"
(covered) and "a computable rate" (assumed to mean a *meaningful* one). A feature with no ratified
requirement had no one to specify its degraded behaviour.

**Severity escalator.** The runway figure does not merely misreport. `design-stats.md:130` requires it
to stay *"a past-rate **descriptor**, not a forecast (clears the D-STA-6 firewall)"*, and two keystones
forbid forecasting outright — **REQ-STA-B-006** (*"SHALL present observed facts only — no
projections/forecasts"*) and **REQ-STA-SUR-001** (*"SHALL NOT forecast"*). A `~648427 days` figure
cannot be read as a description of the past. **It is a keystone breach, not an inaccuracy.**

### Problem framing — what was challenged

| Framing challenged | Verdict |
|---|---|
| "Two unrelated numeric bugs" | **Rejected.** One class: a precondition computed, carried, and not consulted at render. |
| "The panel has a bug" | **Refined.** The panel is *faithful to its build reference* — `menubar-preview.html` depicts only the happy path — so this is also a **Reference Defect** routing to the design owner, not solely an implementation defect. |
| "Fix the two renders" | **Insufficient.** Without the parity-contract extension (R-11), the next metric repeats it; the parity harness exists and simply does not reach here. |
| "Make the runway correct" | **Rejected as the goal.** Making the counted set *representative* means changing the polling model. Making it *honest* is cheap and matches the same doctrine. See § 1b. |
| "A saturating cast is a Rust footgun" | **True but not the defect.** The `> 0.0` guard is the defect; the cast is what turns it from wrong into absurd. Fixing only the cast yields a *credible* lie — see premortem P7. |

## 1b. Boundaries

### Appetite

**Re-sized 2026-08-04 → two to three weeks of focused solo evenings.** Originally one week, on the
premise that every item was a render-side correction over data already on the wire. That premise held
for ten of the eleven items and **does not hold for R-18**: changing the census's validity-window
anchoring is an algorithmic change to a ratified metric, with property-test obligations and an HQ PRD
amendment attached. The re-size is the honest consequence of the operator's second scope amendment —
recorded rather than absorbed, so the appetite is not quietly overrun.

The other ten items keep their original shape: corrective fixes to shipped surfaces over data already
serialized, with no schema version move and no daemon migration (verified — see § 10).

**Circuit-breaker.** Feature `runway-representativeness` (R-5, R-6) is the only genuine rabbit hole:
making the counted set representative requires touching *which accounts the daemon polls*, which is
out of scope. It is **time-boxed**, and its descope target is stated up front: if the representative
computation cannot be reached inside the appetite, ship the **honesty** half — state the counted set,
and refuse when the subset is unrepresentative — and stop. Honesty is the requirement; correctness of
the estimate is not.

### Out of Scope

1. **Changing the daemon's polling model.** Idle accounts go stale because only the active credential
   is polled each tick (`#80`, one account per tick). That is a deliberate footprint decision. This
   PRD works *within* stale-idle-accounts, it does not remove them.
2. **New metrics.** Nothing here adds a measurement. Every figure discussed already exists — R-17
   moves an existing one to a second surface; it does not invent one.
3. **Schema/wire changes.** `all_high_covered_secs`, `capacity_hold_covered_secs`, and the fleet
   block (`runway_secs` / `counted` / `observed`) are all already carried. Any proposal requiring a
   wire change is out of appetite and must be surfaced, not absorbed.
4. **Adding a parent requirement for `fleet-runway`** to `hq/strategy/prd-stats.md`. Still the right
   long-term move, still the HQ's own lifecycle. Recorded in § 11. *(Note: amending the HQ PRD is no
   longer categorically out of scope — see the second amendment below, which requires it for R-18.)*
5. **Retro-fixing charts/sparklines.** REQ-STA-B-008's chart clause ("render gaps as breaks") is
   already satisfied and not in evidence here.

> **Scope amendment 2026-08-04.** "Rendering the fleet runway on the menubar panel" was originally
> excluded here as outside the operator's bounded nine-item selection. It was surfaced as an open
> question (the panel ships a `fleetRunwayWarnSecs` tunable for a figure it never displays) and the
> operator **promoted it into scope** as a tenth item. It is now **R-17**, feature `runway-on-panel`.
> The appetite is unchanged; the added cost is a Stage-2 panel-layout decision, since the roster block
> now carries a third fact alongside the census and capacity holds.

> **Scope amendment 2026-08-04 (second).** Stage-2 grounding established that the all-accounts-high
> census is **structurally unmeasurable whenever the roster is saturated** — the condition it exists to
> report. `DEFAULT_EXHAUSTED_POLL_SECS = 3600` widens the cadence for out-of-rotation exhausted peers
> while `stale_after_secs` defaults to `poll_secs` (300), and `validity_windows` bounds a reading at
> `s.ts + stale_after` — so a saturated peer is "covered" ~8 % of the time and the roster-wide
> intersection collapses to zero. This is exactly the mechanism `hq/strategy/prd-stats.md:104` already
> names *for capacity holds*, and which **REQ-STA-B-010 fixed by anchoring a blocked reading to its own
> carried expiry**. REQ-STA-B-005's census never received that treatment, which is why
> `capacity_hold_covered_secs` read 386747 s over the same window in which `all_high_covered_secs` read 0.
>
> Fixing only the render (R-8) would leave the census honestly, permanently UNKNOWN. The operator
> **extended scope to fix the anchoring** — now **R-18**, feature `census-window-anchoring`, the
> eleventh item. Two consequences: **appetite is re-sized** (below), and **amending
> `hq/strategy/prd-stats.md` becomes required**, not optional — REQ-STA-B-010 ratified expiry-anchoring
> at requirement level for capacity holds, so the symmetric change to the census belongs at the same
> level. R-18 therefore ships with an HQ amendment, not silently under it.

## 2. ORCA Object Model

| Object | Instances | CTAs |
|---|---|---|
| **StatMeasurement** — a roster/fleet figure with a precondition for meaningfulness | all-accounts-high census · capacity holds · fleet runway | Report · Withhold-as-UNKNOWN · Qualify (state coverage / counted set) |
| **StatsSurface** — a render of those measurements | `stats` CLI (`roster_line`) · menubar Stats tab (`statsAggregateText`) | Render · Gate-on-precondition · Degrade |
| **ParityContract** — the byte-pinned manifest and its two conformers | `cross-surface-severity.json` + `src/cross_surface.rs` + the Swift conformer | Pin-case · Conform · Declare-uncovered |
| **BuildReference** — the oracle a surface is built against | `menubar-preview.html` `.agg` frame · `design-stats.md` · `prd-stats.md` | Depict-state · Adjudicate |
| **ScopingRecord** — a tracked record asserting code facts | issue #866 body | Assert · Correct |

**Relationships.** A `StatsSurface` renders a `StatMeasurement` only after consulting its precondition.
A `ParityContract` pins the *pair* of renders for one measurement, or declares the axis uncovered. A
`BuildReference` must depict every state its `StatsSurface` can reach — otherwise a faithful
implementation is still wrong.

## 3. Requirements (EARS)

**Ratification vocabulary.** `n/a` = the requirement is the operator's own words, or traces to an
already-ratified keystone/design decision. `user-ratified (scope membership 2026-08-04)` = the operator
was shown the enriched finding list and chose **"All enriched (~9 items)"**, ratifying these items as
scope. `pending-user` = pipeline-authored beyond that selection; **not yet ratified**.

### StatMeasurement

**R-1** — *When* a stat measurement's coverage denominator is zero, the system **shall** report that
measurement as UNKNOWN on **every** surface that renders it, and **shall not** render a numeric value.
`Origin: user-stated` ("why there are 0s and 0 episodes … wtf").
`Traces to: REQ-STA-B-008 [keystone]; usage_stats.rs:288, :341.` `Ratification: n/a`

**R-2** — *Where* a measurement renders as UNKNOWN, that render **shall** be distinguishable from a
measured zero **and** from a healthy quiet period.
`Origin: user-stated`, sharpened by premortem P1. `Traces to: REQ-STA-B-008 ("low-coverage periods
SHALL be annotated"); REQ-STA-B-005 amendment ("SHALL render — (UNKNOWN), never 0 … a bare 0 is
indistinguishable from a genuinely quiet week and is therefore forbidden").` `Ratification: n/a`
*(The property is ratified. The panel's specific UNKNOWN vocabulary is a Stage-2 design call — see R-13.)*

**R-3** — *When* the fleet runway's combined burn rate is not **meaningfully** distinguishable from
zero, the system **shall** report the runway as UNKNOWN rather than as a duration — and **shall state**
that it is unknown rather than silently omitting the line.
`Origin: user-stated` ("Accounts lasts ~648427 - that's wrong"), omission clause from premortem P2.
`Traces to: NO PARENT REQUIREMENT — fleet-runway is unspecified in prd-stats.md (verified: 0 hits).
design-stats.md:128 covers only seen==0. This is the gap.` `Ratification: n/a`

**R-4** — The system **shall not** emit a fleet-runway figure produced by a saturating or otherwise
lossy numeric conversion.
`Origin: user-stated` (live capture `runway_secs: 9223372036854775807`). `Ratification: n/a`

**R-5** — *Where* the fleet runway is computed from a proper subset of the roster, every surface
reporting it **shall** state the counted set.
`Origin: enrichment-expanded (F3).` `Traces to: counted/observed are already carried on the wire
("counted":2,"observed":6).` **Already satisfied on the CLI** — `render_summary` prints
`({counted} of {observed} counted)` alongside the figure. The open surface is the panel (R-17).
*(Sibling docs call the CLI render `fleet_line`, a name that has never been a Rust item; tracked as
#1105.)* `Ratification: user-ratified (scope membership 2026-08-04)`

**R-20** — Every roster-block fact **shall** be printed in every state, on every surface that reports
it. A fact whose value is UNKNOWN **shall not** be rendered by omitting its line.
`Origin: user-stated 2026-08-04` ("for CLI this line needs to be printed").
**This is the generalisation of R-3's stated-not-omitted clause to the whole roster block**, and it is
load-bearing *because* of the other fixes: `render_summary` then emitted the runway line only when the
figure was finite, so R-3's meaningful-rate floor would have made the line **disappear more often** —
the corrective work would have made the surface quieter rather than more honest, which is premortem P2
exactly. **Since delivered on the CLI (issue #1028)**: `render_summary` no longer gates on a finite
figure, and `fleet_runway_phrase` returns a stated unknown for every runway state that is not a known
figure, so the line is printed in every state of a counted fleet.
`Ratification: n/a (operator-stated)`

**R-21** — User-facing stat strings **shall not** carry implementation vocabulary. The census's
denominator **shall** be stated as the reader-meaningful condition it represents — that the census
could observe the whole roster at one moment — not as the field name `covered`.
`Origin: user-stated 2026-08-04` ("0 covered — covered WHAT?"). Applies to **both** surfaces: the
CLI's `roster_line` then rendered `, {n}% covered`, which had the same defect. **Since delivered on
both surfaces (issue #1029)**: `roster_line` renders `, all in view {n}% of the window` and a test
asserts the annotation no longer contains `% covered`; the panel's `StatusPanelFormat` renders the
same phrasing, and reports a census that could never observe the whole roster as not measurable
rather than as nought. `Ratification: n/a (operator-stated)`

**R-6** — *When* the counted subset excludes every account at or near its weekly ceiling, the system
**shall not** report a runway figure.
`Origin: enrichment-expanded (F3, core).` Rationale: the runway answers "how long until the fleet runs
out"; computing it exclusively from accounts that are *not* running out inverts the question. Observed
live: `counted 1–2 of 6`, the counted account being the idle spare with the large headroom while the
five at weekly 97–100 % were excluded as stale. `Ratification: user-ratified (scope membership)`

**R-7** — The fleet runway **shall** remain a past-rate descriptor, and **shall not** render a figure
whose magnitude can only be read as a forecast.
`Origin: enrichment-expanded.` `Traces to: REQ-STA-B-006 [keystone] ("observed facts only — no
projections/forecasts"); REQ-STA-SUR-001 [keystone] ("SHALL NOT forecast"); design-stats.md:130 ("stays
a past-rate descriptor, not a forecast — clears the D-STA-6 firewall").` `Ratification: n/a`

### StatsSurface

**R-8** — *When* the menubar Stats tab renders the all-accounts-high census, it **shall** gate that
render on `all_high_covered_secs`, as the CLI's `roster_line` does.
`Origin: user-stated.` `Traces to: R-1; the identical #804 → #805 precedent already blessed in-repo for
all_high_threshold — "a fabricated threshold is the very defect #805 exists to end" (WireModel.swift).`
`Ratification: n/a`

**R-9** — The menubar Stats tab **shall** report capacity holds, gated on `capacity_hold_covered_secs`.
`Origin: enrichment-expanded (F4).` `Traces to: REQ-STA-B-010 [roster capacity holds]; design-stats.md:115,
which specifies the roster block as lowest-utilisation + all-accounts-high + swaps + capacity holds +
fleet-runway — the panel renders one of the four.` `Ratification: user-ratified (scope membership)`

**R-17** — The menubar Stats tab **shall** report the fleet runway, subject to R-3 through R-7 exactly
as the CLI is — including the counted set (R-5), the refusals (R-3, R-6), and the no-forecast
constraint (R-7).
`Origin: user-stated (scope amendment 2026-08-04)` — promoted from the § 8 open question after the
panel was found to ship a `fleetRunwayWarnSecs` tunable (`SettingsModel.swift:48`, `ConfigWire.swift:45`,
labelled "Fleet Runway") for a figure it never displays. `Traces to: design-stats.md:115, which
specifies the roster block as lowest-utilisation + all-accounts-high + swaps + capacity holds +
fleet-runway — with R-9 and R-17 the panel reaches three of the four.` `Ratification: n/a (operator-selected)`
**No wire work**: the fleet block is already serialized (`runway_secs` / `counted` / `observed`,
captured live). **Ordering**: R-17 must not land before R-3/R-4 — shipping the runway to a second
surface while it can still emit `i64::MAX` would double the blast radius of the defect this PRD exists
to close.

**R-18** — *Where* a rostered account's reading carries an expiry that bounds how long the reading
remains a valid statement about that account, the all-accounts-high census **shall** anchor that
reading's validity window to the carried expiry rather than to the poll cadence alone.
`Origin: user-stated (scope amendment 2026-08-04)` — after Stage-2 grounding established the census is
structurally blind while the roster is saturated. `Traces to: REQ-STA-B-010's ratified anchoring rule —
"A blocked reading's validity window SHALL be anchored to its own carried expiry … rather than to the
poll cadence. This is what makes the metric measurable at all" — applied symmetrically to REQ-STA-B-005's
census; the in-repo implementation precedent is blocked_windows() vs validity_windows().`
`Ratification: n/a (operator-selected)`

**Asymmetry is intended and benign.** A *saturated* peer carries `session_resets_at`, so its reading
stays valid until that instant — the case that is currently blind. A *low* peer carries no equivalent
guarantee, but it is the account in rotation, polled every `poll_secs`, whose cadence coverage was never
the problem. R-18 therefore extends coverage exactly where coverage is lost, and nowhere else.

> ⚠ **The second clause of that rationale is contradicted by the frozen replay corpus — see #1097.**
> A peer can be session-low *and* polled on the widened exhausted cadence simultaneously, held out
> of rotation by its **weekly** dimension: `a4`/`a5`/`a6` in `build/fixtures/capacity-replay-corpus.tsv`
> carry 44 / 44 / 43 readings across 172 800 s at session peaks 0.03 / 0.15 / 0.00 — low **and**
> ~7.6 % covered. The decision stands on the physics recorded in `high_windows`'s own doc (only a
> high reading carries a guarantee that survives to its reset); the "never blind" justification for
> it does not. Whether that class can carry a session guarantee is #1097, and it is the spec
> author's call. Marked in-band so this section is not read as ratifying the premise, and mirrored
> at `docs/design/stats-honesty-cross-surface-solution-design.md` § D-I and
> `docs/specs/census-validity-anchoring.feature.md` Rule 3.

**R-18 shall not weaken UNKNOWN.** It refines what "covered" means for a reading that carries its own
validity bound; it **shall not** make a genuinely dead daemon read as covered. REQ-STA-B-008's UNKNOWN
survives R-18 intact, exactly as REQ-STA-B-010's anchoring *"refines REQ-STA-B-008's UNKNOWN … it does
not repeal it."* This is the discriminating constraint between R-18 and the rejected
"just widen `stale_after`" alternative, which would have weakened coverage uniformly for every account.

**R-19** — The HQ parent PRD **shall** be amended to carry the census-anchoring rule at requirement
level, in the same form REQ-STA-B-010 carries it for capacity holds.
`Origin: enrichment-expanded — the necessary companion to R-18.` A code change that alters what a
ratified metric measures, without amending the requirement that ratifies it, leaves the parent PRD
stating something the system no longer does. `Ratification: user-ratified (scope amendment 2026-08-04)`

**R-10** — *Where* both surfaces report the same measurement, they **shall** render the same STATE;
per-medium vocabulary may differ.
`Origin: user-stated` via `design-menubar.md` R-2 — *"The panel and the `status` CLI are two renders of
the ONE StatusResponse … a divergence between them is a bug"* — ratified 2026-07-01, re-ratified by
`/council` 2026-07-09 as **STATE-parity, not glyph-parity**. `Ratification: n/a`

### ParityContract

**R-11** — The cross-surface parity manifest **shall** either pin the roster/stats aggregate cases or
list that axis in `uncovered_axes` with a stated reason.
`Origin: enrichment-expanded (F6).` `Traces to: ADR-0026 / issue #768; the manifest's own doctrine
"Stating the boundary beats implying coverage".` Canary-verified absence: 50 hits for `severity`, 0 for
each of `roster` / `all_high` / `capacity_hold` / `swap_count` / `runway`.
`Ratification: user-ratified (scope membership)`

**R-12** — *Where* the manifest pins a measurement, it **shall** include at least one case whose
expected report is UNKNOWN.
`Origin: AI-inferred-expansion (premortem P3).` Rationale: parity is agreement, not correctness — if
both surfaces printed `0 episodes` the contract would pass. The manifest must pin the **honesty**, not
merely the **agreement**. `Ratification: pipeline-decided 2026-08-04` — additive rigor *inside* R-11,
which the operator scoped; reversible; and no information the operator holds bears on it. Recorded
here rather than surfaced, per § 12.

### BuildReference

**R-13** — *Where* the panel can reach a degraded or UNKNOWN state for a metric it renders, the panel's
build reference **shall** depict that state.
`Origin: enrichment-expanded (F8) + premortem P5.` `Traces to: project CLAUDE.md — the design mock is
the panel's build reference; menubar-preview.html .agg frame (lines 943, 1020) depicts only
"3 episodes (1h40m)".` **The absence of a gap-state frame was never adjudicated** — it is not a ratified
decision, and its silence currently reads to an implementer as "render the number unconditionally".
`Ratification: pipeline-decided 2026-08-04` for the **requirement** — it is the lesson the sibling
`panel-presentation-reference-coverage.md` already established, and no operator-held information bears
on whether a reference should cover the states its surface can reach.
**The *depiction* is a separate, genuinely operator-owned design call** — what the panel actually shows
when a census is unmeasurable — and is carried into Stage 2 as a required design input, not resolved here.

### ScopingRecord

**R-14** — Issue #866's body **shall** be corrected to state the actual `StatsRoster` field set.
`Origin: user-stated (F5).` The body asserts `StatsRoster` already carries `allHighCoveredSecs`;
verified false — the token appears in `apps/menubar/` only inside raw fixture JSON strings in
`Tests/Fixtures.swift`, never as a Swift property. An open issue scoping adjacent work rests on it.
`Ratification: user-ratified (scope membership)`

### Cross-cutting — observability & verification

**R-15** — *When* the fleet-runway computation yields a non-finite, saturated, or implausible result,
the system **shall** record it as a fault rather than rendering it.
`Origin: enrichment-expanded (F9).` Note the existing warning is **not** a backstop: `current_fleet_runway()`
feeds the daemon's `fleet_runway_low` check, and a saturated runway is a *KNOWN* value **above** any
threshold — so the warning never fires, and would emit the RECOVERED leave-edge. Latent today only
because `fleet_runway_warn_secs` defaults to `0` (opt-in, off). `Ratification: user-ratified (scope membership)`

**R-16** — Each requirement above **shall** be covered by a regression test that fails against the
pre-fix code and asserts the **semantic** (no numeric value when the precondition fails) rather than a
golden string.
`Origin: enrichment-expanded (F7) + premortem P6.` `Ratification: user-ratified (scope membership)`

## 4. Acceptance Criteria (GWT + BUT NOT)

**AC-1 — Census gap on the panel (R-1, R-2, R-8)**
```
GIVEN a stats payload whose roster carries all_high_covered_secs == 0
WHEN the menubar Stats tab renders the aggregate callout
THEN the census reports UNKNOWN
AND the render is distinguishable from a week measured as genuinely quiet
BUT NOT does it print "0 episodes" or any episode count
BUT NOT does it print a duration such as "(0s)"
BUT NOT does the swap count (swap_count, which has no coverage precondition) disappear with it
```

**AC-2 — Partial census coverage (R-2)**
```
GIVEN all_high_covered_secs > 0 but < the period length
WHEN either surface renders the census
THEN the episode count renders AND the coverage share is stated alongside it
BUT NOT does a partially-covered census render identically to a fully-covered one
```

**AC-3 — Degenerate burn rate (R-3, R-7)**
```
GIVEN a fleet whose combined weekly burn rate is a decayed EMA of ~1e-11
WHEN the fleet runway is computed
THEN the runway is reported as UNKNOWN
AND the reader is told it is unknown
BUT NOT does any surface print a duration (787,037 days at that rate today)
BUT NOT is the line silently omitted, leaving absence to read as "no problem"
BUT NOT does the figure survive by being clamped to a large-but-plausible number (see P7)
```

**AC-4 — Saturation (R-4, R-15)**
```
GIVEN a headroom/rate quotient that exceeds i64::MAX
WHEN the fleet runway is computed
THEN no runway value is emitted
AND the implausible computation is recorded as a fault
BUT NOT does the value saturate to i64::MAX (9223372036854775807, captured live)
BUT NOT does the daemon's fleet_runway_low check treat it as a healthy KNOWN reading
```

**AC-5 — Unrepresentative counted set (R-5, R-6)**
```
GIVEN six rostered accounts, five at weekly 97–100 % and excluded as stale,
      and one idle spare with large headroom as the only counted account
WHEN the fleet runway is computed
THEN no runway figure is reported
BUT NOT is a figure derived from the idle spare alone presented as a fleet runway
BUT NOT does the counted set stay unstated wherever a runway IS legitimately reported
```

**AC-6 — Capacity holds on the panel (R-9)**
```
GIVEN capacity_hold_covered_secs > 0 with capacity_holds = 6 (6 session / 0 weekly), ≥10h22m
WHEN the menubar Stats tab renders the roster block
THEN the capacity holds are reported with their session/weekly cause split and held duration
AND when capacity_hold_covered_secs == 0 they report UNKNOWN, not "0 holds"
BUT NOT does the duration render as exact rather than as the bound REQ-STA-B-011 requires
```

**AC-7 — Parity contract covers the axis (R-11, R-12)**
```
GIVEN the cross-surface manifest
WHEN the roster/stats aggregate axis is considered
THEN it is either pinned with cases, or listed in uncovered_axes with a stated reason
AND if pinned, at least one case expects UNKNOWN
BUT NOT does a pinned case set consist only of happy-path values
BUT NOT can either conformer be satisfied while the other renders a different STATE
```

**AC-8 — Reference depicts the reachable states (R-13)**
```
GIVEN the panel can reach an UNKNOWN state for the census and for capacity holds
WHEN menubar-preview.html is used as the build reference
THEN a frame depicts each reachable degraded state
BUT NOT does the reference depict only the happy path for a metric with a reachable gap state
```

**AC-10 — Fleet runway on the panel (R-17)**
```
GIVEN a stats payload carrying fleet {runway_secs, counted, observed}
WHEN the menubar Stats tab renders the roster block
THEN the fleet runway is reported, with its counted set stated
AND every refusal the CLI makes (degenerate rate, saturation, unrepresentative subset) is
    made identically on the panel — same STATE, per-medium vocabulary permitted
AND the panel's fleetRunwayWarnSecs setting now configures a warning for a figure the panel shows
BUT NOT does the panel render a runway while the CLI would refuse one (or the reverse)
BUT NOT does R-17 ship ahead of R-3/R-4 — a second surface must not inherit a live i64::MAX
BUT NOT does adding a third roster fact push the roster block into a wrapped or truncated layout
```

**AC-11 — Census measurability while saturated (R-18, R-19)**
```
GIVEN a week in which five of six rostered accounts are weekly-saturated and therefore polled
      on the widened exhausted cadence (3600s) against a 300s staleness horizon
WHEN the all-accounts-high census aggregates that week
THEN all_high_covered_secs is non-zero — the census is measurable in the very condition
     it exists to report
AND the HQ parent PRD carries the anchoring rule at requirement level (R-19)
BUT NOT does a genuinely dead daemon (no samples at all) read as covered — UNKNOWN survives
BUT NOT does all_high_secs ever exceed all_high_covered_secs (the existing ⊆ property test
    must still hold under the widened denominator)
BUT NOT is an account's window extended past an expiry it did not itself carry
```

**AC-9 — Regression coverage (R-16)**
```
GIVEN the pre-fix code at cb3eaca
WHEN the new tests run against it
THEN every one of them fails
BUT NOT does any test assert only a golden string that a future rebaseline could silently flip
```

## 5. Quality Attributes (Planguage)

```
TAG:   UnknownStateFidelity
SCALE: fraction of reachable (measurement × failed-precondition) combinations that render
       UNKNOWN rather than a numeric value, across both surfaces
METER: enumerate the reachable combinations from the § 7 state matrix; assert each in test
GOAL:  1.0
NOW:   CLI 2/6 — correct on both coverage-denominator cases (census, capacity holds), wrong on all
       four runway-degeneracy cases (§ 7 rows 8-11). The CLI is the reference implementation for
       COVERAGE gating only; it is not a clean surface overall.
       Panel 0/1 of what it renders today (the census, § 7 row 3), heading to 0/6 as R-9 and R-17
       make the other five states reachable there.
```
```
TAG:   RunwayPlausibility
SCALE: whether any input rate can produce a reported duration that is not a past-rate descriptor
METER: property test over rate ∈ [1e-300, 1e3] and headroom ∈ [0, roster-max]
GOAL:  no input produces a reported figure outside the plausibility bound; degenerate inputs
       produce UNKNOWN
CONSTRAINT: no saturating cast is reachable on the reporting path
NOW:   rate 1e-11 → 787,037 days · rate 1e-300 → i64::MAX
```
```
TAG:   CrossSurfaceStateAgreement
SCALE: fraction of pinned parity cases in which the CLI STATE equals the panel STATE
METER: the two independent conformers over cross-surface-severity.json
GOAL:  1.0
NOW:   1.0 over the covered axes — the defect is COVERAGE (the roster axis is unpinned and
       undeclared), not the agreement rate. Do not read the green as reassurance.
```
```
TAG:   ReferenceStateCoverage
SCALE: fraction of panel-reachable render states depicted in the panel's build reference
METER: enumerate reachable states per metric; check for a mock frame
GOAL:  1.0 for every metric the panel renders
NOW:   census 1/2 (happy path only) · capacity holds 0/2 · fleet runway 0/5 (R-17; not yet
       rendered, not yet depicted — the largest single reference gap this work opens)
```

## 5b. Feature Completeness

| Feature | Requirements | Verdict | Gap |
|---|---|---|---|
| `census-coverage-gate` | R-1, R-2, R-8 | **COMPLETE** | — the field is on the wire, the CLI is the reference implementation, the #804→#805 precedent is exact |
| `runway-degeneracy` | R-3, R-4, R-7, R-15 | **NEAR-COMPLETE** | the meaningful-rate **floor value** is undetermined; see A-3. Requirement is mechanism-free by design |
| `runway-representativeness` | R-5, R-6 | **INCOMPLETE** | "at or near its weekly ceiling" is not yet operationalised, and R-6's interaction with the staleness gate is the P4 risk. **This is the time-boxed feature** |
| `capacity-holds-on-panel` | R-9 | **NEAR-COMPLETE** | wire + CLI reference exist; the panel's *layout* for a second roster fact is undesigned (Stage 2) |
| `runway-on-panel` | R-17 | **NEAR-COMPLETE** | wire + CLI reference exist and the setting already ships. Two gaps: panel layout for a **third** roster fact (shares the Stage-2 layout decision with R-9), and the hard ordering constraint that R-17 must follow R-3/R-4 |
| `census-window-anchoring` | R-18, R-19 | **INCOMPLETE** | the largest item and the only algorithmic one. Open: which readings qualify as carrying a bounding expiry, how the ⊆ invariant (`all_high_secs ≤ all_high_covered_secs`) is preserved under a widened denominator, and the HQ amendment's exact wording. Property tests already exist to extend (`prop_all_high_time_never_exceeds_the_jointly_covered_time`) |
| `stats-parity-coverage` | R-11, R-12 | **NEAR-COMPLETE** | pin-vs-declare is a genuine choice with different costs; Stage 2 decides |
| `gap-state-reference` | R-13 | **INCOMPLETE** | blocked on an operator design call — what the panel *shows* when a census is unmeasurable |

## 6. Success Criteria

**North Star** — *On a total-downtime week, every roster-level figure the operator reads is either
correct or says it does not know.* Nothing in between.

**Leading indicators**
- L1 — `UnknownStateFidelity` reaches 1.0 on the panel (measured in test, per § 5).
- L2 — The roster/stats axis appears in the parity manifest, pinned or declared (binary).
- L3 — Every new test fails against `cb3eaca` (AC-9). A test that passes pre-fix is not a regression test.
- L4 — `ReferenceStateCoverage` = 1.0 for every metric the panel renders.

**Lagging indicators**
- G1 — The next total-downtime week produces **no** operator-filed "that's wrong" report about a
  roster figure. This is the only indicator that matters, and it is the operator's own reading.
- G2 — The next metric added to the roster block ships with its gap state pinned in the manifest
  *at the same time*, without anyone remembering to ask — i.e. the prevention held.

**Decision gates**
- **DG-1** (Stage 2 entry) — R-12 and R-13 ratified or explicitly declined by the operator.
- **DG-2** (`runway-representativeness` circuit-breaker) — at the appetite boundary, if the
  representative computation is not reachable, descope to the honesty half (R-5 + R-6) and stop.
- **DG-3** (before pinning) — confirm that pinning the roster axis in the manifest does not require
  a wire change. If it does, the item exceeds appetite and returns to the operator.

## 7. State Matrix — measurement × precondition × surface

**A snapshot, not a live view.** The CLI and Panel columns below record what each surface did **at
authoring, 2026-08-04**; work since has overtaken parts of **both** columns — row 3's Panel cell, for
one, is the reading issue #1029 was filed to end. Where this table and a requirement block above
disagree, the requirement block is authoritative. Only R-20 and R-21 carry delivery notes, though, so
a row whose authoritative block is some other requirement — row 3's Required cell routes to R-1 and
R-8 — is not disambiguated by that rule and has to be checked against the code. The rows are kept as authored
because the requirements were derived from them; re-auditing them against current code is tracked as
#1105.

Panel column: **✗** = renders a confident value it should not, **∅** = metric absent entirely.
Every ∅ below is now in scope to close — capacity holds via R-9, the runway via **R-17**.

| # | Measurement | Precondition state | CLI at authoring | Panel at authoring | Required (both surfaces) |
|---|---|---|---|---|---|
| 1 | all-accounts-high | fully covered | `N episodes (Xh)` | `N episodes (Xh)` | ✅ unchanged |
| 2 | all-accounts-high | partially covered | `N episodes (Xh, 64% covered)` | `N episodes (Xh)` — share dropped **✗** | state the share (R-2) |
| 3 | all-accounts-high | `covered_secs == 0` | `—` | **`0 episodes (0s)` ✗** | UNKNOWN (R-1, R-8) |
| 4 | capacity holds | covered | `6 (6 session / 0 weekly) · ≥10h22m` | **∅** | reported (R-9) |
| 5 | capacity holds | `covered_secs == 0` | `—` (`capacity_holds_cell` gates) | **∅** | UNKNOWN (R-9) |
| 6 | fleet runway | rate meaningful, all counted | `~Xh` | **∅** | figure + counted set (R-5, R-17) |
| 7 | fleet runway | rate meaningful, subset counted | `~Xh … (1 of 6 counted)` — ✅ **already correct** | **∅** | figure + counted set stated (R-5, R-17) |
| 8 | fleet runway | subset excludes all ceiling-bound accounts | `~Xh` from the idle spare **✗** | **∅** | UNKNOWN (R-6, R-17) |
| 9 | fleet runway | rate ≈ 0 but `> 0.0` | **`~648427 days` ✗** | **∅** | UNKNOWN (R-3, R-7, R-17) |
| 10 | fleet runway | rate exactly `0.0` | line silently omitted **✗** | **∅** | UNKNOWN, **stated** not omitted (R-3, R-17) |
| 11 | fleet runway | quotient exceeds `i64::MAX` | **`i64::MAX` ✗** | **∅** | UNKNOWN + fault recorded (R-4, R-15, R-17) |

Row 10 is the subtle one: the guard then *worked* at exactly zero and the line vanished. Silent
absence is the same misinformation in a quieter register — the operator reads "no problem".

## 8. Assumption Registry

| ID | Assumption | Importance | Evidence | Verdict | Cheapest test | Hedge while open |
|---|---|---|---|---|---|---|
| ~~A-1~~ | ~~Both coverage denominators mean the same thing~~ | **HIGH** | **RESOLVED 2026-08-04 — PARTLY CONFIRMED, and the residue is R-18.** Both censuses use the **same** intersection rule (`intersect(&prev, &covering)`, same short-circuit, same ⊆ invariant). What differs is the **validity-window anchoring**: `all_high()` → `validity_windows()`, cadence-only (`hi = next.min(s.ts + stale_after)`); `capacity_holds()` → `blocked_windows()`, which extends a blocked reading to its **carried expiry** (`anchored_hi = cadence_hi.max(relief_at)`). That single difference produced 0 vs 386747 s over the same window and roster | ~~test~~ | — | Still gate each measurement on **its own** denominator (the rule is shared; the anchoring is not). R-18 closes the anchoring gap; do **not** unify the two helpers as part of it |
| A-2 | `all_high_covered_secs == 0` is a *measured* nil, not a sentinel | HIGH | `usage_stats.rs:299-301` states it explicitly — *"That zero is a measured quantity, not a sentinel — jointly-covered time really was nil"* | **decided** | — | — |
| A-3 | A principled plausibility bound for the runway exists — roster-size × weekly-window, rather than an arbitrary constant | MED | Weekly windows *reset*, so a runway far beyond one window is arguably meaningless already. **Not verified** — `weekly_headroom` may aggregate across accounts in a way that legitimately exceeds one window | **test** | Read the `weekly_headroom` derivation; check whether 6 accounts × 7 d is the natural ceiling | Require *refusal* (R-3), never a clamp. A clamped figure is P7 |
| A-4 | Pinning the roster axis needs no wire change | MED | Both coverage fields verified already serialized (`stats.rs:2351`, `:2352`) | **test** | Attempt the pin; DG-3 gates it | If false, the item exceeds appetite → surface, do not absorb |
| A-5 | The operator wants the panel to *say* UNKNOWN rather than hide the row | MED | Inferred from the complaint's shape ("0 episodes… wtf" is an objection to a **wrong** statement) — but never asked | **surface** | R-13 ratification (DG-1) | Assume state-it-visibly; it is the reversible choice |
| A-6 | The staleness gate is why five accounts are excluded | HIGH | `stats.rs:970,986` + live `counted 1–2 / observed 6` while five sat at weekly 97–100 % | **decided** | — | — |
| A-7 | R-6's refusal will not disable the runway permanently | **HIGH** | **None.** Idle accounts are *always* stale under one-account-per-tick polling, so a naive R-6 may never report again. This is premortem P4 | **test** | Replay a week of real history through the proposed predicate; count how often it reports | Do not ship R-6 without this replay. Descope per DG-2 rather than ship a metric that never fires |

### Premortem (Phase 0, de-anchored — findings the ISO sweep cannot enumerate)

*Six months out, all items shipped, and the operator is misled by the stats surface again. What happened?*

- **P1** → R-2. The panel got its coverage gate, but renders a bare `—` that reads as "all quiet"
  rather than "we could not measure". The operator reads calm where there is blindness. *UNKNOWN must
  be distinguishable from a healthy zero, not merely non-numeric.*
- **P2** → R-3, AC-3. The runway now returns UNKNOWN far more often, and the line simply **vanishes**.
  Absence reads as "no problem". *Silent omission is the same lie, quieter.*
- **P3** → R-12, AC-7. The manifest gained roster cases, both surfaces conform, and both are still
  wrong the same way — **parity locked in the defect**. *Pin the semantics, not the agreement.*
- **P4** → A-7, DG-2. R-6 shipped naively; because idle accounts are always stale, the runway now
  never reports at all. *The fix removed the feature.*
- **P5** → R-13. The mock gained a census gap frame, capacity holds shipped to the panel with **no**
  frame, and the identical "faithful to an incomplete reference" defect recurred one metric over.
- **P6** → R-16, AC-9. A test asserts `—`; a year later someone "improves" the empty render back to
  `0` and rebaselines the golden without reading it. *Assert the semantic; make the manifest the thing
  that fails.*
- **P7** → AC-3, A-3. The saturation guard **clamps** to a plausible-looking large number instead of
  refusing. `i64::MAX` becomes `9999 days` — still a lie, now a **credible** one, and much harder to
  notice. *Clamping is not a fix; refusal is.*

### Resolved operator question (2026-08-04)

The panel exposes a **`fleetRunwayWarnSecs` setting** (`SettingsModel.swift:48`, `ConfigWire.swift:45`,
labelled "Fleet Runway" / "Runway warning (s)") for a figure **the panel never displays** — a warning
threshold tunable for a metric not visible on that surface. Found during Stage-1 grounding, outside the
operator's bounded nine-item selection, so it was surfaced rather than silently absorbed.

**Ruling: promote into scope.** The runway becomes a panel surface — **R-17**, feature
`runway-on-panel`, the tenth item. The setting stops being an orphan, and the panel reaches three of
the four roster-block facts `design-stats.md:115` specifies. Two consequences carried forward: the
Stage-2 panel-layout decision now covers three roster facts rather than two, and **R-17 is ordered
behind R-3/R-4** so the second surface never inherits a live saturating runway.

## 9. Cross-Cutting & Non-Functional Concerns

**9.1 Security** — N/A. No requirement touches credentials, the keychain, the AF_UNIX transport, or
peer auth. All work is downstream of an already-derived local store. Handle redaction in `--json`
(REQ-STA-B-007) is unaffected: no requirement adds a handle to any payload.

**9.2 Compliance & Regulatory** — **Applies.** REQ-STA-SUR-001 `[keystone]` governs public artifacts:
*"SHALL NOT forecast"*, no imperative remedy, symmetric underuse/saturation framing. Every string this
work introduces — UNKNOWN sentinels, capacity-hold copy, fault log lines, issue and commit text — is
in scope for it. R-7 is the requirement that makes the runway fix a *compliance* fix, not merely a
correctness one. Reviewers must read new copy against SUR-001, not only against correctness.

**9.3 Reliability & Observability** — **Applies (R-15).** The existing `fleet_runway_low` warning is
not a backstop: a saturated runway is a KNOWN value above any threshold, so the check silently passes
and would emit the RECOVERED leave-edge. Dormant today only because `fleet_runway_warn_secs` defaults
to `0`. New fault recording must obey the daemon's existing fail-open discipline (REQ-STA-B-001) — a
diagnostic write must never block or alter the swap loop.

**9.4 Performance & Scalability** — Negligible. Aggregation is already a pure synchronous function of
samples + events (REQ-STA-B-005); this work adds comparisons at render time and a decode of two `Int64`
fields already present in the payload. No new I/O, no new allocation of consequence.

**9.5 Operational** — **Applies.** Repo coupling obligations, from the project CLAUDE.md § If you touch X:
panel golden changes require a `Panel-Goldens-Rebaselined:` trailer; CLI golden changes require
`CLI-Goldens-Rebaselined:`; any new Swift file a test compiles against needs an explicit `project.yml`
path entry plus `xcodegen generate`. Byte-pinned `Tests/Fixtures.swift` payloads must be updated in the
same change as any manifest edit. **No daemon restart or data migration is required** — both coverage
fields are already on the wire (§ 10).

**9.6 Lifecycle** — Rollout is per-surface and independently revertible; there is no coordinated
release. The single ordering constraint: **R-11/R-12 (parity coverage) should land *after* the two
render fixes**, so the manifest pins the corrected behaviour rather than the defect (P3). The panel
and CLI must not be corrected in isolation from each other — doing so is precisely how this defect
arose.

## 10. Source Traceability

| Requirement | Source | Kind |
|---|---|---|
| R-1, R-2, R-8 | Operator: *"why there are 0s and 0 episodes of all accounts >=95% … wtf"* + screenshot | user-stated |
| R-3, R-4, R-7 | Operator: *"Accounts lasts ~648427 - that's wrong, we've been waiting for a few days of full downtime"* | user-stated |
| R-1, R-2 | `hq/strategy/prd-stats.md:121` REQ-STA-B-008 `[keystone]`; `:99` REQ-STA-B-005 render-rule amendment | ratified parent |
| R-7 | `prd-stats.md` REQ-STA-B-006 + REQ-STA-SUR-001 `[keystone]`; `design-stats.md:130` | ratified parent |
| R-9 | REQ-STA-B-010; `design-stats.md:115` roster-block composition | ratified parent |
| R-10 | `design-menubar.md` R-2, re-ratified `/council` 2026-07-09 | ratified parent |
| R-11, R-12 | `build/fixtures/cross-surface-severity.json` + `src/cross_surface.rs`, ADR-0026 / #768 | in-repo contract |
| R-13 | `apps/menubar/design/menubar-preview.html` `.agg` frame (943, 1020); project CLAUDE.md | build reference |
| R-5, R-6, R-15 | `src/stats.rs:970, 986, 1095, 1107, 1133`; `src/daemon.rs` `check_fleet_runway_warn` | code forensics |
| R-14 | Issue #866 body vs. `apps/menubar/Sources/WireModel.swift` | contradiction |
| R-16 | Absence of any test asserting the panel's unmeasurable render or a saturated runway; premortem P6 | coverage gap |
| R-17 | Operator scope amendment 2026-08-04, from the orphaned `fleetRunwayWarnSecs` tunable (`SettingsModel.swift:48`, `ConfigWire.swift:45`) vs. `design-stats.md:115` roster-block composition | user-stated |
| R-18 | Second operator scope amendment 2026-08-04. Mechanism: `config.rs` `DEFAULT_EXHAUSTED_POLL_SECS = 3600` vs `AggregateParams::new` (`stale_after_secs` = `poll_secs` = 300) vs `usage_stats.rs` `validity_windows()` (`hi = next.min(s.ts + stale_after)`). Precedent: `blocked_windows()` `anchored_hi = cadence_hi.max(relief_at)`, ratified by REQ-STA-B-010 | code forensics + user-stated |
| R-19 | Necessary companion to R-18 — REQ-STA-B-010 carries the anchoring rule at requirement level for capacity holds; the symmetric census change belongs at the same level | enrichment-expanded |
| R-20 | Operator, 2026-08-04: *"for CLI this line needs to be printed."* Verified against `render_summary`, which then emitted the runway line only when the figure was finite (since delivered — issue #1028) | user-stated |
| R-21 | Operator, 2026-08-04: *"0 covered — covered WHAT?"* Verified against `roster_line`, which then rendered `, {n}% covered` (since delivered — issue #1029), and the proposed panel copy | user-stated |
| All | Live captures 2026-08-03: `all_high_covered_secs: 0`; `fleet: {runway_secs: 9223372036854775807, counted: 2, observed: 6}`; `capacity_holds: 6, capacity_hold_covered_secs: 386747` | self-verifying (A) |

**Verified-not-assumed** (each checked at `cb3eaca`, not carried from memory):
- `all_high_covered_secs` **is** on the Rust wire (`stats.rs:2101`, serialized `:2351`) — the gap is
  Swift-consumer-side only. *(This corrected my own initial hypothesis, which had it missing from the wire.)*
- `capacity_hold*` **is** on the Rust wire (`stats.rs:2107-2122`, serialized `:2352`) — so R-9 is
  decode + render, **not** a schema bump.
- `fleet-runway` has **zero** occurrences in `prd-stats.md`.
- `REQ-STA-B-008` appears in **no** `.md` file in the code repo (canary-verified: the `REQ-STA` prefix
  is absent from all of them) — it lives only in Rust comments here, and is defined in the private HQ.
- The panel renders **no** runway and **no** capacity holds; `fleetRunwayWarnSecs` exists there only as
  a settings tunable.

## 11. Related Work — Generalize, Do Not Duplicate

| Item | Relationship |
|---|---|
| **#804 → #805** (`all_high_threshold` carried on the wire, then consumed by the panel) | **The exact precedent.** Same shape, one field over, already blessed: *"a fabricated threshold is the very defect #805 exists to end."* R-8 should follow its implementation, not invent a new pattern. |
| **#866** (menubar census regime qualifier) | Overlaps R-8 and **contains the false premise R-14 corrects**. Reconcile before scoping either. |
| **#865** (should the census refuse to report under roster fallback) | An open *design* question adjacent to R-1. R-1 does not answer it; do not let it be closed by implication. |
| **#864** (capacity-holds unstated-regime blindness) | Adjacent to R-9. Check for overlap before creating a new item. |
| **ADR-0026 / #768** (cross-surface parity) | R-11 **extends** this contract; it does not create a second one. |
| **REQ-STA-B-011** (durable hold closure) | Constrains R-9: hold duration must render as a **bound**, not an exact figure, until the LEAVE edge is durable. |
| **`hq/strategy/prd-stats.md`** | Should gain a parent requirement for `fleet-runway`. **Out of scope here** (§ 1b.5) — belongs to the HQ's own lifecycle. Recorded so it is not lost. |

## 12. Definition-of-Ready Verdict

| # | Check | Verdict |
|---|---|---|
| 1 | Validated problem statement | **PASS** — § 1, traced to Phase 0 framing; five framings challenged, current state evidenced with live captures |
| 2 | Explicit out-of-scope declarations | **PASS** — § 1b: appetite + six numbered exclusions + a stated circuit-breaker |
| 3 | Success and telemetry metrics | **PASS** — § 6: four leading, two lagging, three decision gates |
| 4 | Cross-cutting & non-functional | **PASS** — § 9.1–9.6 all present; 9.1 is `N/A` with rationale, the rest carry content |
| 5 | Feature completeness verdict | **PASS-WITH-FINDINGS** — § 5b: three INCOMPLETE (`runway-representativeness` blocked on A-7; `gap-state-reference` blocked on an operator design call; `census-window-anchoring`, the algorithmic item added 2026-08-04), all with documented gaps |
| 6 | Requirement provenance | **PASS** — 8 `n/a` (operator's own words or a ratified keystone), 6 `user-ratified (scope membership 2026-08-04)`, 2 `pipeline-decided` with recorded rationale (below). Zero unratified-and-unaccounted. |

**Verdict: PASS-WITH-FINDINGS** — carried by check 5 (three INCOMPLETE features), not by check 6.

> **Amended twice on 2026-08-04, both by operator scope decision, both recorded rather than absorbed.**
> R-17 (runway on the panel) and R-18/R-19 (census window anchoring + its HQ amendment) were added
> after the DoR verdict was first written. The second amendment **re-sized the appetite** from one week
> to two-to-three and **reopened the HQ PRD** as an in-scope artifact — consequences stated in § 1b
> rather than left for a later stage to discover.

### Why R-12 and R-13 were decided rather than surfaced

Both are pipeline-authored, so the reflex is to route them to the operator for ratification. That
reflex was tested and rejected: for each, the question *"what does the operator know that the pipeline
does not?"* has no answer.

- **R-12** — additive rigor inside R-11, which the operator explicitly scoped. Reversible, fail-closed
  in the safe direction, and justified by evidence already in this document (parity is agreement, not
  correctness). Asking would have transferred accountability without transferring any capability.
- **R-13, the requirement** — a reference must cover the states its surface can reach. The sibling
  PRD `panel-presentation-reference-coverage.md` established exactly this; re-asking it treats a
  settled house lesson as an open question.

**What genuinely is the operator's, and is therefore NOT decided here:**

1. **R-13's depiction content** — what the panel shows when a census is unmeasurable. Carried into
   Stage 2 as a required design input (see A-5, whose hedge is "state it visibly", the reversible choice).
2. ~~The orphaned `fleetRunwayWarnSecs` setting~~ — **RESOLVED 2026-08-04.** Asked before Stage 2
   precisely because it bears on the design surface; the operator promoted it into scope as **R-17**
   (§ 8). Scope is now **ten** items, appetite unchanged.
