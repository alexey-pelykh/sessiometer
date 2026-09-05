---
title: Active-Account Observation Continuity — Re-Pointing the Poll Schedule on a Change of Active
scope: daemon-active-account-observation
created: 2026-09-01
status: draft
dor_status: passed-with-findings
source: incident on 2026-09-01 (operator report, verbatim in § 1), investigated via `/investigate` at HEAD 7c24a97; forensic evidence is machine-local daemon runtime state (event log + usage sample store), not a committed artifact, so every figure it supports is replicated in-band in § 2
parent-requirements: private HQ, `strategy/prd-swap-latency.md` (REQ-LAT-B-001 / B-002 / CFG-001 / Q-001 / SUR-001) — not dereferenceable from a clone; every statement this PRD relies on is quoted verbatim in-band in § 1 and § 2
appetite: small batch on the core (schedule invalidation + RED oracle + instrument); the surfacing half is sized separately at Stage 2, after the STATUS_SCHEMA_VERSION bump is costed — operator-ratified 2026-09-01
formulation: {technical-architecture: complete, performance-architecture: complete, testing-architecture: complete, api-design: complete}
features:
  schedule-invalidation: {stage: design, tracks: {technical-architecture: complete, testing-architecture: complete}}
  observation-gap-instrument: {stage: design, tracks: {technical-architecture: complete, testing-architecture: complete}}
  regression-oracle: {stage: design, tracks: {testing-architecture: complete}}
  operator-swap-readout: {stage: design, tracks: {technical-architecture: complete}}
  adr-amendment: {stage: design, tracks: {technical-architecture: complete}}
  operator-surfacing: {stage: design, tracks: {api-design: complete}}
  target-staleness: {stage: blocked, tracks: {}}
artifacts:
  design-doc: docs/design/active-account-observation-continuity-solution-design.md
  design-brief: docs/briefs/2026-09-01-design-active-account-observation-continuity.md
---

# PRD — Active-Account Observation Continuity

> **Provenance.** Authored by an AI pipeline (`/scope` Stage 1) from an operator incident report plus
> first-party forensics the operator's own daemon produced. **Three things are operator-ratified and
> are marked as such**: the scope membership (§ 1b), the `{N}` anchor (§ 3, R-1), and the appetite
> (§ 1b). **Everything else is pipeline-authored and ratification-pending** — each requirement carries
> an explicit `Origin` tag, and § 8 records which upstream claims were tested rather than inherited.
> One upstream misattribution was found and corrected during authoring; see § 2.4.

## 0. Why this PRD exists in the code repo, and what owns what

The governing requirement family `REQ-LAT-*` lives in the **private HQ**, which is a repo-root
sibling on the maintainer's machine and **unreachable from any clone**. Per the project's doc-citation
rule a committed document may only point at something a fresh clone has, so this PRD **replicates the
load-bearing upstream statements verbatim in-band** and treats the HQ only as provenance.

Split of ownership:

- The **private HQ** owns the *why* — usage-ceiling headroom, burn rate, and enforcement timing.
  None of that appears here.
- **This PRD** owns the neutral *what*: observation continuity across a change of active, its
  measurable, and its verification. Per `REQ-LAT-SUR-001`, quoted verbatim in § 2.4, public build
  items filed from this PRD carry mechanism-only, provider-neutral framing.

## 1. Problem Statement

**The operator's report, verbatim:**

> the incident that has just occured when oleksii@pelykh.com account jumped to 100% usage w/o us
> noticing so that I had to manually switch

**Current state.** At `2026-09-01T04:51:35Z` the daemon swapped *into* an account on a 185-second-old
reading of `0.71`. It then did not observe that account again for **638 seconds**. The next reading
was `1.00`, recorded in the same second as the operator's manual swap at `04:59:08Z`. Across the
climb the daemon polled the account it had *just parked* five times at active cadence and the
newly-active account **zero** times.

**Affected users.** The operator, and any future multi-account operator: the failure is silent by
construction, so its cost is paid as unplanned manual intervention.

**Why now.** The committed guarantee this violates is `ADR-0012` Decision 4, and the measured gap is
**8.5× that guarantee** (638 s against 75 s at the live configuration).

### Problem framing — what was challenged

The operator's sentence fuses three claims. Each was verified separately, and **two of the three
resolve differently than the framing implies**:

| Claim as stated | Verdict |
|---|---|
| "jumped to 100% usage" | **TRUE**, on the `five_hour` (session) dimension, not `seven_day`. This is the *symptom*, not the fault — reaching a session limit is not itself a defect |
| "w/o us noticing" | **TRUE, and the reason is narrower than "we failed to notice"**: nothing was *detected* because nothing was *looked at*. Detection-vs-surfacing does not arise here, because no detection event ever existed to surface |
| "so that I had to manually switch" | The swap arm did **not** fail, fire late, get blocked, or hit the post-swap tail. **The trigger never received an input to fire on** — it evaluated correctly against a `last_reading` of `0.71` that was 638 s stale |

**The reframe this yields, and the actual problem statement:** *the daemon has no concept of "the
active account has not been looked at."* Every existing blindness detector keys on a poll that **ran
and failed** — `note_blind_episode`'s entry arm is `(None, Err(_))` — and `record_usage_sample`,
`maintain_stats_store` (which emits `usage_gap`), `note_blind_episode` and `note_exhausted_poll` all
sit **inside** the `if let Some(i) = poll_idx` block. **A never-attempted poll produces zero
observability of any kind.**

**Prevention vs solution.** Both are required and they are different requirements. Prevention is
keeping the schedule pointed at the account that matters (R-1, R-2). Solution-side is being able to
*tell* when it is not (R-5) — and without the latter, a landed fix is unfalsifiable, because no
current readout would move (§ 2.3).

## 1b. Boundaries

### Appetite

**Operator-ratified, 2026-09-01.** The core — schedule invalidation, its pre-authored regression
oracle, and the observability that makes it provable — is a **small, tightly-bounded fix**. The
surfacing half is sized **separately**, after § Stage 2 design has costed the `STATUS_SCHEMA_VERSION`
bump. Rationale: the core is rate-neutral by construction and therefore shippable without waiting on
wire work.

**Costing outcome, 2026-09-04 — the precondition above is discharged, and it returned a different
wire.** The ratified sentence stands as written and is not amended here; what follows records what the
costing found. Stage 2 design (`docs/design/daemon-diagnostic-integrity-solution-design.md` § 7, D-4)
concluded the surfacing half belongs on `ReliabilityWire` under `reliability`'s own
`JSON_SCHEMA_VERSION`, so no `STATUS_SCHEMA_VERSION` bump is costed *because none is owed*. § 6b
carries the decision; § 9.6's superseded lockstep instruction is corrected there.

### Out of Scope — with the reason each is excluded

| Excluded | Why |
|---|---|
| The capacity strand — synchronized fleet exhaustion, the `20:20:45Z late=true` swap, `all_exhausted` holds | A **different failure with a different cause**, owned by **#726**. On the incident night it produced a real 68-minute event that the daemon detected correctly, reported correctly, and could not act on. Fusing it here would make a scheduling defect inherit an undecidable provisioning question |
| Retuning `reactive_session_threshold` / `TAIL_MARGIN` / `effective_ceiling` | The reflex `ADR-0024` (#609) already took. It does not close a blindness the trigger cannot see past |
| Lowering `poll_secs` / `near_limit_poll_secs` | `ADR-0012` Decision 1 and Alternative 1 explicitly reject it as the default lever, and **#1309** (OPEN) exists to measure whether poll cadence is a material 429 driver before anyone moves it |
| `V_PEAK_SESSION_PCT_PER_MIN` recalibration | Real and evidenced — `reliability` self-flags `[RECALIBRATE]`, assumed 6.95 %/min against an observed P100 of 13.55 %/min — but a **separate lever** |
| Settling `D-LAT-2` (the `poll_secs` direction keystone) | Deliberately untouched. R-1's measurable is expressed as a **function of** `poll_secs`, so it stays correct under either resolution — see § 2.4 |
| `src/migration.rs` `FORMAT_VERSION`; the three independent `JSON_SCHEMA_VERSION` wires | Unrelated wires. `STATUS_SCHEMA_VERSION` governs `status` and both `watch` frames only |

## 2. Evidence

All figures below are from the operator's own daemon runtime state at HEAD `7c24a97`, replicated
in-band because that state is machine-local and not a committed artifact.

### 2.1 The observation hole — CONFIRMED

Session, weekly, and inter-sample gap, from `usage-samples.jsonl`:

```
04:44:33Z  0.71  0.73  (+72s)
04:45:55Z  0.71  0.73  (+82s)
04:47:18Z  0.71  0.73  (+83s)
04:48:30Z  0.71  0.73  (+72s)   <-- LAST reading before the hole
04:59:08Z  1.00  0.78  (+638s)  <-- SAME SECOND as the manual swap
```

Of **24** corpus entries into `session == 1.00`, **23** arrived from a `0.93`–`0.99` reading. This is
the only one that arrived from `0.71`.

### 2.2 Root cause — CONFIRMED, and its rival FALSIFIED

`Daemon::next_poll_index` rebuilds the schedule via `build_poll_schedule(active)` **only** under
`if self.state.poll_pos >= self.state.poll_schedule.len()`. `Daemon::record_swap` sets
`self.state.active = Some(target_idx)` and touches neither `poll_schedule` nor `poll_pos`;
`Daemon::adopt_manual_swap` does the same. The **only** non-wrap invalidation anywhere in the tree is
`reconcile_roster`, which clears both explicitly.

The rival hypothesis — *a poll silently failed* — is **falsified, not merely disfavoured**:
`usage_velocity` is documented as never emitted across a poll gap, and one fired carrying
`elapsed_secs=637`, which proves two consecutive **successful** polls 637 s apart with nothing
failing between.

**The near-limit tightening could not engage, and the reason is self-sealing.**
`near_limit_fast_poll_engaged` requires the active's observed reading — or its velocity projection —
at or above `session_velocity_min_project_above` (85). The carried reading was `0.71` and the EMA had
decayed during the account's park, so the projection reached ≈ 72.6. **The predicate reads the very
reading the stale schedule prevented refreshing.**

### 2.3 Two independent compounding mechanisms

**(a) The genuinely-free target was invisible.** A peer entered a 709-second rate-limit backoff at
`04:35:58Z` and emerged at `04:53:38Z` reading `session_at_recovery=0` — blind for 1139 s spanning
the `04:51:35Z` decision, and **actually at 0%** throughout. The poll-result assignment
`self.state.accounts[i].last_reading = result.ok();` is universal, so a 429 **nulls the slot
outright**, and `pick_target_with_reason_ranked`'s `filter_map` over `readings: &[Option<Usage>]`
excluded it from candidacy entirely. **The account the operator manually switched to is the account
the daemon could not see.** This is the same root cause in mirror image: the daemon acted on a
stale-low reading of the active and could not see a stale-high one on the peer that was free.

**(b) The landing watch was disarmed by the swap itself.** `record_swap` clears the incoming
account's `parked_landing`. The account was parked with a landing watch at `04:41:58Z`; the
`04:51:35Z` swap back **disarmed it**, so its `04:59:08Z` reading of `1.00` could not trip
`note_landing_overshoot` — inside the 900 s landing window it otherwise would have.

**No existing readout would move if a fix landed.** The overshoot SLI filters to `reason=session`,
and a manual swap records `session_pct=0` **by design** (`src/observability.rs`: *"a manual/forced
swap is not session-triggered"*), so the operator's own rescues — the defining event of this incident
class — are graded by nothing. **This makes an instrument a prerequisite, not a nice-to-have.**

**Target selection has no staleness term at all**, and grounding confirms this is an absence in the
**design corpus**, not merely in the code: no ADR, requirement, spec or doc-comment in the committed
tree *or* the private HQ imposes an age bound on a swap target's reading.

### 2.4 Upstream statements, verbatim — and one correction

`REQ-LAT-B-001` (`[user-stated]`, `must`, 🟢) — the only user-stated member of the family:

> The system SHALL NOT complete a proactive swap **only after** the active account is already
> session-limited (observed `session_pct ≥ 100` / provider-enforced) **when a viable target existed
> ≥ 1 poll-interval earlier**. Measurable: **zero** swaps whose first ≥`session_trigger` observation
> of the active account was already ≥100% while a viable target was available at the prior poll.

`REQ-LAT-B-002` (`[enrichment]`, `must`, 🟡) — **relative, carrying no absolute interval**:

> WHILE the daemon is running, it SHALL **re-observe the ACTIVE account more frequently than it
> round-robins the idle peers**, closing the active-account blind spot — **BUT NOT** by bursting a
> poll of all N accounts at once (SHALL preserve the `#80` per-account sub-interval that avoids the
> 429 burst).

`REQ-LAT-Q-001` (`[enrichment]`, `must`, 🟢):

> A **hermetic test** (fake clock + fake `UsageSource` …) SHALL **reproduce a burn crossing 95→100%
> within a single poll interval** and SHALL **assert the chosen fix (D/F1/F3) prevents a swap at
> `session_pct ≥ 100`** — a regression lock on REQ-LAT-B-001.

`REQ-LAT-SUR-001` (`[enrichment]`, `must`, 🟢):

> Public build items filed from this PRD SHALL use **mechanism-only, multi-provider-neutral** framing
> (no provider names in marketing framing, no HQ pointers) … The reaction-latency **rationale**
> (usage-ceiling / burn-rate / session-limit-enforcement framing) stays in THIS private PRD; the
> public issue carries only the neutral mechanism.

**CORRECTION — a misattribution caught during authoring.** The investigation report that seeded this
PRD stated that *"`REQ-LAT-B-002`'s ratified ~90 s [60,150] s band is the natural anchor for `{N}`."*
Checked against the HQ source, that fuses three requirements and misattributes twice: **B-002 carries
no band at all** (quoted above), and the ~90 s `[60,150]` band belongs to **`REQ-LAT-CFG-001`**, where
it is one side of an **explicitly OPEN keystone**:

> EITHER the `poll_secs` default SHALL be lowered toward the ratified base, OR the shipped
> `poll_secs=300` SHALL be **recorded as an intentional post-ratification change with rationale** …
> ⚠ The **direction** is the open call (D-LAT-2).

tracked as **K1, "⏳ PENDING operator ratification."** Anchoring `{N}` there would import an
unratified decision as settled and would point a fix at lowering `poll_secs` — which `ADR-0012`
Decision 1 rejects and **#1309** exists to warn against. **The operator ratified the alternative
anchor on 2026-09-01**: `ADR-0012` Decision 4, quoted verbatim from the committed tree:

> The active tightens to **`2·poll_secs / N`**, and the 1:1 interleave is the cap. That interval is
> deliberately **2× above** the `poll_secs / N` per-source floor …

At the live configuration (`poll_secs=300`, N=8) that is **75 s**. It is committed, derived from
configuration rather than asserted, is exactly the guarantee the incident violated, and stays correct
under **either** resolution of D-LAT-2.

**A gap in `ADR-0012` itself.** Decision 4 reasons entirely about a **static** active. Nothing in the
ADR considers a **mid-cycle change of active**. The guarantee was therefore not merely violated — it
was **never derived for the case that failed**.

**`REQ-LAT-Q-001` does not cover this incident's shape.** It locks a burn crossing 95→100% *within a
single poll interval*. This incident is `0.71 → 1.00` across a **638 s observation hole**. The
existing lock (#367) is shaped for the other case and additionally breaks at the first swap, so it
cannot fail on this one.

## 3. Object Model (OOUX)

| Object | Core concept | Key attributes | CTAs |
|---|---|---|---|
| `PollSchedule` | The interleave vector plus its position | `schedule: Vec<usize>`, `pos: usize`, `active: Option<usize>` | build, advance, **invalidate** |
| `ActiveAccount` | The account currently serving traffic | index, `last_reading`, `last_reading_at` | designate, observe |
| `SwapTarget` | A candidate the daemon may swap to | `last_reading: Option<Usage>`, `last_reading_at` | rank, filter-for-viability |
| `ObservationGap` | Elapsed time since the active was last *looked at* | `since: Instant`, `elapsed` | measure, **emit** |
| `LandingWatch` | The post-swap overshoot watch (`parked_landing`) | armed-at, window | arm, disarm, trip |

## 4. Requirements (EARS)

`{N}` throughout is **`2·poll_secs / N`**, the `ADR-0012` Decision 4 interval — 75 s at the live
configuration. Expressing it as a function rather than a constant is deliberate: it self-adjusts if
`poll_secs` or roster size changes, and it does not presuppose either resolution of D-LAT-2.

| ID | Origin | Requirement | Priority |
|---|---|---|---|
| **R-1** | `[operator-ratified 2026-09-01]` (anchor); outcome traces to `REQ-LAT-B-001` `[user-stated]` | WHEN the daemon's designated active account changes, the system SHALL observe the newly-designated active account within **`{N}` = `2·poll_secs / N`**. | must |
| **R-2** | `[AI-inferred-expansion]` | WHEN the designated active account changes, the system SHALL invalidate the poll schedule so the next selected poll index is derived from the new active — **BUT NOT** by adding any request, timer, or concurrent poller (the `ADR-0012` Decision 3 rate-neutrality invariant SHALL hold). | must |
| **R-3** | `[AI-inferred-expansion]` | The invalidation in R-2 SHALL apply on **every** path that changes the active account, including the autonomous path and the operator-initiated path. | must |
| **R-4** | `[AI-inferred-expansion]` | WHILE the daemon is running, IF the active account has not been *observed* for longer than `{N}`, THEN the system SHALL emit a durable observability event recording the elapsed gap — **BUT NOT** only when a poll ran and failed. | must |
| **R-5** | `[AI-inferred-expansion]` | The system SHALL provide a readout on which a change in post-swap first-sight latency is visible, **including for operator-initiated swaps** — which today record `session_pct=0` and are excluded from every overshoot SLI. | must |
| **R-6** | `[AI-inferred-expansion]` | A hermetic regression test SHALL reproduce a **change of active mid-cycle** and SHALL assert the newly-designated active is observed within `{N}`; it SHALL be demonstrated **RED against the pre-fix tree** before any fix is accepted. | must |
| **R-7** | `[AI-inferred-expansion]` | WHEN ranking swap targets, the system SHALL account for the **age** of each candidate's reading — **BUT NOT** by hard-excluding stale candidates, which can collapse the viable set to `NoViableTarget`. `{T}` is **UNSET** and is owned by the operator. | should |
| **R-8** | `[AI-inferred-expansion]` | WHEN a usage poll fails such that a candidate's reading is discarded, the system SHALL NOT thereby render that candidate permanently invisible to target selection while it is in fact viable. | should |
| ~~**R-9**~~ | `[AI-inferred-expansion]` | ~~WHEN a swap re-designates an account that was parked with an armed landing watch, the system SHALL NOT silently disarm the overshoot watch such that a subsequent ceiling crossing goes unrecorded.~~ **WITHDRAWN at Stage 2** — the disarm is deliberate and correct (`#613`: a parked-account watch must not fire against a now-active account). Once active, an overshoot is the ACTIVE-side detector's job — which is what the observation hole disabled. **R-9 collapses into R-1.** See the solution design § 12 Correction. | ~~should~~ |
| **R-10** | `[AI-inferred-expansion]` | The system SHALL surface an at-limit active account to the operator without requiring a panel to be opened. `{D}` is **UNSET** and is owned by the operator. | should |
| **R-11** | `[AI-inferred-expansion]` | IF `{N}`, `{T}` or `{D}` becomes operator-configurable, THEN it SHALL travel the existing `[tunables]` surface and its wire mirror, preserving the Swift fixture lockstep. | could |
| **R-12** | `[AI-inferred-expansion]` | `ADR-0012` SHALL be amended or superseded to record that its Decision 4 guarantee was **never derived for a mid-cycle change of active**. | must |

## 5. Acceptance Criteria (GWT + BUT NOT)

**R-1 / R-2 / R-3 — observation continuity**

> **Given** a running daemon with a multi-account roster and a designated active account,
> **When** the active designation changes mid-cycle by any path,
> **Then** the newly-designated active is observed within `2·poll_secs / N`,
> **And** the per-tick spacing, aggregate request rate, and `poll_secs / N` per-source floor are all
> unchanged.
> **BUT NOT** by adding a request, a timer, or a concurrent poller;
> **BUT NOT** by lowering `poll_secs` or `near_limit_poll_secs`;
> **BUT NOT** leaving any active-changing path uncovered.

**R-4 / R-5 — the gap is observable**

> **Given** an active account that has not been observed for longer than `{N}`,
> **When** that threshold is crossed,
> **Then** a durable event records the elapsed gap,
> **And** a readout exists on which post-swap first-sight latency is visible for **operator-initiated**
> swaps as well as autonomous ones.
> **BUT NOT** conditioned on a poll having run and failed;
> **BUT NOT** filtered to `reason=session`, which excludes the operator's own rescues.

**R-6 — the oracle**

> **Given** the tree **before** any fix,
> **When** the regression test drives a mid-cycle change of active,
> **Then** the test **FAILS**.
> **BUT NOT** authored by whoever implements the fix;
> **BUT NOT** accepted as evidence if it passes pre-fix — a detector that does not redden against the
> corpse falsifies the diagnosis rather than confirming it.

**R-7 / R-8 — target selection**

> **Given** a roster in which some candidates carry stale or discarded readings,
> **When** a target is ranked,
> **Then** reading age is accounted for.
> **BUT NOT** by a hard staleness exclusion — with five of eight accounts weekly-exhausted at the
> incident instant, a hard filter can collapse the viable set to `NoViableTarget`, converting a
> latency defect into an availability defect.

## 6. Quality Attributes (Planguage)

```
TAG:    PostSwapFirstSightLatency
SCALE:  seconds from a change of active designation to the first completed observation of the new active
METER:  p50 and p95 over a 7-day window, from the daemon's own event log
PAST:   p50 182 s, p95 436 s (measured 2026-09-01; worst observed 638 s)
GOAL:   p95 <= 2*poll_secs / N   (75 s at poll_secs=300, N=8)
FAIL:   any single occurrence > 2 * (2*poll_secs / N)
```

```
TAG:    PollRateNeutrality
SCALE:  aggregate /usage requests per hour, roster-wide
METER:  the existing #80/#366 stagger locks, run pre- and post-change
PAST:   unchanged is the requirement, not an improvement target
GOAL:   delta == 0     (the fix reorders; it must not add)
FAIL:   any increase
```

## 6b. Feature Completeness

| Feature | Verdict | Gap |
|---|---|---|
| Schedule invalidation on change of active (R-1..R-3) | **COMPLETE** | Mechanism, sites, oracle and rate-neutrality proof all identified |
| Never-looked-at observability (R-4, R-5) | **NEAR-COMPLETE** | The *readout home* is now decided; the METER is **half-satisfiable**, so the row does not move to COMPLETE. **Decided 2026-09-04**: a `first_sight` block on `ReliabilityWire`, `reliability`'s own `JSON_SCHEMA_VERSION` 12 → 13, additive, mirroring `BlindEpisodesWire`'s enter/exit-pair census — see `docs/design/daemon-diagnostic-integrity-solution-design.md` § 7. No `STATUS_SCHEMA_VERSION` bump, no Swift surface, no status/watch golden — but `reliability`'s human render shares its `Report`, so the three `build/fixtures/cli-renders/reliability-*.txt` renders do move and owe `emit_cli_render_goldens` plus a `CLI-Goldens-Rebaselined:` trailer. Scope bound: the **event-log** readout only — `record_usage_sample` stays inside the `poll_idx` guard, so the usage-sample store still cannot see a never-attempted poll. **Why this stays NEAR-COMPLETE:** `observation_gap_enter` fires only when `elapsed > 2*poll_secs / N` (strictly, and deliberately so), so the emitted set is the breach tail — left-censored at the bound. With `T = 2*poll_secs / N`, the entry edge is `T` while § 6's `FAIL` is *any single occurrence > 2T* — twice that edge — so filtering the emitted set on `elapsed_secs > 2T` **cannot produce a false negative**: an empty result conclusively means no `FAIL`. It is an **upper bound** rather than an exact count, because the entry anchor is `observed.max(designated)`, so a mid-tenure observation gap on a long-active account also passes the filter while lying outside § 6's post-swap `SCALE`. It makes the breach tail durable for the first time, which is the value here. What it cannot carry is `GOAL p95 <= T`, a percentile over the **whole** distribution: this set omits everything at or below `T` by construction, so no filtering recovers it and GOAL-met is not a representable state. The daemon's own emitter doc comment in `src/observability.rs` says as much — *"a p50 of first-sight latency needs a source that also records the non-breaches, which no event here is"*. Which source could grade the GOAL half is **OQ-3** in that design's § 11, and it is open |
| Regression oracle (R-6) | **COMPLETE** | Template and RED-state proof are exact |
| Target-selection staleness (R-7, R-8) | **INCOMPLETE** | `{T}` UNSET — operator-owned. Bounding form (`fresh-enough-or-repoll-first`) is specified; the value is not |
| ~~Landing-watch preservation (R-9)~~ | **WITHDRAWN** | Stage 2 grounding showed the behaviour is deliberate and correct; the requirement collapses into R-1. Removes work rather than adding it |
| Operator surfacing (R-10) | **INCOMPLETE** | `{D}` UNSET — operator-owned. Appetite deliberately deferred to Stage 2 (§ 1b) |
| Tunable surface (R-11) | **NEAR-COMPLETE** | Conditional on R-7/R-10 outcomes |
| ADR-0012 amendment (R-12) | **COMPLETE** | The defect in the ADR's own derivation is identified |

## 7. Success Criteria

| # | Criterion | How it is measured | Why this one |
|---|---|---|---|
| S-1 | **Zero recurrences of the incident signature** — an active account reaching its ceiling with a preceding observation gap greater than `2·{N}` | The daemon's own event log, swept over a 30-day window | This is `REQ-LAT-B-001`'s measurable narrowed to the mechanism this PRD owns. It is falsifiable from first-party data with no new instrumentation beyond R-4 |
| S-2 | **p95 post-swap first-sight latency ≤ `{N}`** | § 6 `PostSwapFirstSightLatency`, against the recorded PAST of p50 182 s / p95 436 s | The continuous measure behind S-1's binary. A fix that moves S-1 by luck will not move this |
| S-3 | **The regression oracle exists, and was RED against the pre-fix tree** | The commit that adds it, plus a recorded pre-fix failing run | Without the recorded RED, a passing test is consistent with a test that never could fail (§ 5, R-6) |
| S-4 | **Aggregate request rate is unchanged** | The existing `#80`/`#366` stagger locks, run before and after | The constraint that keeps this clear of the open `#1309` question rather than entangled with it |
| S-5 | **The operator can tell, from a readout, that a manual rescue happened and how late it was** | A readout that grades operator-initiated swaps — which today record `session_pct=0` and are graded by nothing (§ 2.3) | Without it the incident class stays invisible in exactly the case the operator cares about most: the one where they had to intervene |

**Explicitly NOT a success criterion**: any reduction in swap frequency, any change in fleet
headroom, or any improvement to the `all_exhausted` path. Those belong to the capacity strand
(**#726**, § 1b) and would credit this work with an outcome it does not produce.

## 8. Assumption Registry

| # | Assumption | Origin | Confidence | Cheapest test | Signpost that reopens it |
|---|---|---|---|---|---|
| A-1 | Schedule invalidation is sufficient to close the gap | pipeline, from source reading | 🟡 | R-6's oracle, RED pre-fix then GREEN post-fix | Oracle passes pre-fix ⇒ diagnosis is wrong |
| A-2 | The fix is rate-neutral | `ADR-0012` Decision 3, quoted | 🟢 | The #80/#366 stagger locks stay green | Any stagger lock reddens |
| A-3 | Rate-neutrality is what keeps this clear of #1309 | pipeline inference | 🟡 | #1309's own measurement, when it runs | #1309 finds poll cadence material even at constant rate |
| A-4 | No open tracker item already owns this mechanism | lexical sweep, 2026-09-01 | 🟡 | Sweep found zero hits on four identifiers — but GitHub indexes prose, not code identifiers | Any duplicate surfaces |
| A-5 | `{N}` = `2·poll_secs / N` is the right target | **operator-ratified** | 🟢 | — | D-LAT-2 resolves in a way that supersedes it |
| A-6 | Reading age can enter ranking without collapsing viability | pipeline inference | 🔴 | Replay ranking against the incident-night roster | A replay yields `NoViableTarget` |

### Premortem (de-anchored — blind spots the ISO sweep cannot enumerate)

*Assume the fix shipped and the incident recurred. Why?*

1. **A third path changes the active** and was never invalidated → R-3 exists for exactly this; the
   oracle must drive every path, not just the autonomous one.
2. **The schedule re-points but the first poll still lands late** because re-pointing sets position
   without guaranteeing the new active is *next* → R-1 is written as an outcome bound, not as
   "invalidate and hope."
3. **The staleness term collapses the viable set** under fleet exhaustion → A-6, 🔴, and R-7's
   explicit `BUT NOT`.
4. **The instrument lands but still cannot see it**, because manual swaps keep recording
   `session_pct=0` → R-5 names that exclusion explicitly.
5. **The free target stays invisible** because a 429 still nulls `last_reading`, so the fix routes
   correctly to a worse account → R-8.
6. **Surfacing lands but the operator does not have the panel open** → R-10 is written as
   "without requiring a panel to be opened."

## 9. Cross-Cutting & Non-Functional Concerns

**9.1 Security** — N/A. No new credential path, no new external surface, no change to peer
authentication or credential storage.

**9.2 Compliance & Regulatory** — N/A, with one framing constraint that does bind: `REQ-LAT-SUR-001`
(§ 2.4) requires mechanism-only, provider-neutral wording on every public build item filed from this
PRD, and no HQ pointer.

**9.3 Reliability & Observability** — **This is the PRD's centre of gravity, not a side concern.**
R-4 and R-5 exist because the failure is currently *unobservable by construction*: every blindness
detector keys on a poll that ran and failed, and a never-attempted poll emits nothing at all. R-6
additionally requires the regression oracle be demonstrated RED before a fix is accepted.

**9.4 Performance & Scalability** — Rate-neutrality is a hard constraint, not a goal (§ 6,
`PollRateNeutrality`). The fix must reorder rather than add. Roster-size scaling is inherent: `{N}`
is expressed as a function of `poll_secs` and N.

**9.5 Operational** — No migration, no state format change, no restart semantics change for the core
fix. **One operational hazard is recorded rather than exercised**: `DecisionState` is
`Default`-constructed with nothing restored from disk, so a daemon restart sets `last_swap` to `None`
and **clears the swap cooldown entirely**. That makes "restart it and see" actively destructive to
the state under test, and it is adjacent to **#1356**.

**9.6 Lifecycle** — `ADR-0012` owes an amendment or a superseding record (R-12).

**Superseded 2026-09-04 — do not follow the `STATUS_SCHEMA_VERSION` lockstep from this section.** This
section previously instructed that the surfacing half takes a `STATUS_SCHEMA_VERSION` **minor** bump
and therefore owes the five status/watch goldens plus a current-minor Swift fixture sweep. § 6b's
decision record now settles the readout home elsewhere: the surfacing half lands on `ReliabilityWire`
and carries `reliability`'s **own** `JSON_SCHEMA_VERSION`, so `STATUS_SCHEMA_VERSION` does not move,
there is no Swift surface and no status/watch golden. What it *does* owe is the three
`build/fixtures/cli-renders/reliability-*.txt` renders (`cargo test -- --ignored
emit_cli_render_goldens`) and a `CLI-Goldens-Rebaselined:` trailer. Following the superseded
instruction would regenerate five goldens and sweep `apps/menubar/Tests/Fixtures.swift` for a change
that touches neither wire — the cross-wire confusion `CLAUDE.md` § Schema versions calls the single
most common cross-cutting mistake here. Read § 6b, not this paragraph.

## 10. Source Traceability

| Requirement | Source | Reliability |
|---|---|---|
| R-1 (the `{N}` value) | `docs/adr/0012-active-reobservation-via-schedule-interleave.md` Decision 4, quoted verbatim in § 2.4; operator ratification 2026-09-01 | **A** (committed artifact, read directly) + **B** (ratified) |
| R-1 (the outcome it bounds) | `REQ-LAT-B-001`, quoted verbatim in § 2.4 — the family's only `[user-stated]` member | **B** (user-authoritative, upstream) |
| R-2, R-3 | `Daemon::next_poll_index`, `Daemon::record_swap` (`src/daemon.rs`), `Daemon::adopt_manual_swap` (`src/daemon/commands.rs`), and `reconcile_roster` as the precedent — all read directly at HEAD `7c24a97` | **A** |
| R-2's rate-neutrality clause | `ADR-0012` Decision 3; the `#80` / `#366` stagger locks | **A** |
| R-4 | `note_blind_episode`'s `(None, Err(_))` entry arm, and the `if let Some(i) = poll_idx` block enclosing `record_usage_sample` / `maintain_stats_store` / `note_blind_episode` / `note_exhausted_poll` | **A** |
| R-5 | `src/observability.rs`, the overshoot SLI's `reason=session` filter and the doc comment *"a manual/forced swap is not session-triggered"* | **A** |
| R-6 | `REQ-LAT-Q-001`, quoted verbatim in § 2.4 — **and the finding that it does not cover this incident's shape** | **B** upstream; the non-coverage finding is **A** (read against the existing lock, `#367`) |
| R-7, R-8 | `self.state.accounts[i].last_reading = result.ok();` and `pick_target_with_reason_ranked`'s `filter_map` over `readings: &[Option<Usage>]` | **A** |
| R-8's incident instance | The peer's 709 s backoff, `session_at_recovery=0`, blind across the `04:51:35Z` decision | **A** (first-party runtime state) |
| ~~R-9~~ | `record_swap` clearing the incoming account's `parked_landing` — **re-read at Stage 2 WITH its `#613` doc comment, which states the rationale**. The Stage 1 reading saw the mechanism and missed the reason | **A**, but the Stage 1 *interpretation* was wrong — see § 12 below |
| R-10, R-11 | The existing `[tunables]` surface and its daemon-routed `config-set`; the `STATUS_SCHEMA_VERSION` fixture lockstep in the project's own `CLAUDE.md` | **A** |
| R-12 | `ADR-0012` itself — Decision 4 reasons about a static active and never about a mid-cycle change | **A** (absence, established by reading the ADR whole rather than by search) |
| The 638 s figure, the 24-entry census, the five-polls-to-zero asymmetry | The operator's own `usage-samples.jsonl` and event log — machine-local runtime state, replicated in-band in § 2.1 because it is not a committed artifact | **A** |
| The `~90 s [60,150]` band | `REQ-LAT-CFG-001` — **and it is one side of an OPEN keystone (D-LAT-2 / K1), not a target** | **B** upstream, but **explicitly unratified** — see § 2.4 |

### A note on one corrected input

The Investigation Report that seeded this PRD attributed the `~90 s [60,150]` band to
`REQ-LAT-B-002` and called it *ratified*. Checked against the HQ source, **both halves are wrong**:
B-002 carries no band, and the band belongs to CFG-001 where it is an explicitly open keystone
pending operator ratification. Anchoring `{N}` there would have imported an unsettled decision as
settled *and* pointed the fix at lowering `poll_secs` — which `ADR-0012` Decision 1 rejects and
`#1309` exists to warn against. The anchor was moved to `ADR-0012` Decision 4 and ratified on
2026-09-01. Recorded here because the misattribution is exactly the kind that survives review: it
named a real requirement, in the right family, with a real number.

## 11. Requirement Provenance (DoR check 6)

| Class | Requirements | Provenance |
|---|---|---|
| **Traces to an explicit operator statement** | The problem itself (§ 1, quoted verbatim); the outcome R-1 bounds | Operator, 2026-09-01, and `REQ-LAT-B-001` `[user-stated]` upstream |
| **Operator-ratified during this pipeline** | The `{N}` anchor (R-1); the scope membership (§ 1b); the appetite (§ 1b) | Operator, 2026-09-01, each surfaced and answered individually |
| **Derived from committed artifacts** | R-2, R-3, R-12 | `ADR-0012` + the four call sites, read directly |
| **Derived from measured evidence** | R-4, R-5, R-7, R-8 | § 2, first-party daemon runtime state |
| **Premortem-derived, not requested** | The `BUT NOT` clauses on R-2, R-4, R-5 and R-7 | § 8 Premortem. **These constrain rather than expand scope** — and are the class most worth challenging |
| **Deferred to the operator, deliberately unset** | `{T}` (R-7), `{D}` (R-10) | Named as UNSET rather than guessed. A value here would be a silently-chosen threshold wearing a requirement's authority |
| **UNCOVERED — awaiting a scope decision** | R-8 | Falls outside the operator's ratified scope selection, so it was not filed by interpretation. Surfaced as a recommended ninth item in the scope brief |

**Outstanding operator action.** Everything in the two `derived` rows and the premortem row is
**pipeline-authored and ratification-pending**. They elaborate the ratified problem rather than
extending scope — but a reviewer should confirm that reading rather than inherit it, and `dor_status`
stays `pending` until they do. The two UNSET quantities are not blockers for the core fix: R-7 and
R-10 are `should`, and § 1b sizes the surfacing half separately.

## 12. Withdrawals

| Req | Withdrawn at | Why |
|---|---|---|
| **R-9** (landing-watch preservation) | Stage 2 design, 2026-09-01 | The Investigation Report, and this PRD after it, recorded `record_swap` clearing the incoming account's `parked_landing` as a compounding defect. Reading the site *with its doc comment* shows the behaviour is deliberate: *"the account going ACTIVE cannot be a parked-landing subject — disarm any landing watch on it here … so a prior park's stale window can't fire against an account that is now active again"* (`#613`). A landing watch is a **parked**-account mechanism; once the account is active again an overshoot belongs to the ACTIVE-side detector — which is precisely what the 638 s observation hole disabled. **R-9 collapses into R-1.** |

**Why this is recorded rather than deleted.** The Stage 1 finding was sourced correctly (reliability
**A** — the code was read) and interpreted wrongly, because the mechanism was read without its
rationale. That is the failure mode most likely to recur, and a silently-dropped row would leave the
next reader to re-derive the same wrong conclusion from the same true observation.
