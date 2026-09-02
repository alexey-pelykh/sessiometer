---
title: Active-Account Observation Continuity — Solution Design
scope: daemon-active-account-observation
created: 2026-09-01
status: draft
source: docs/requirements/active-account-observation-continuity.md
---

# Solution Design: Active-Account Observation Continuity

## 1. Goals and Drivers

Close the post-swap observation hole: after the daemon's active account changes mid-cycle, the poll
schedule keeps polling the account it just parked, so the newly-active account goes unobserved for up
to a full schedule sweep. Measured once at **638 s** against `ADR-0012` Decision 4's `2·poll_secs / N`
guarantee — **75 s** at the live configuration, an 8.5× overrun.

Three goals, in dependency order:

1. **Re-point the schedule on every change of active** (PRD R-1, R-2, R-3) — rate-neutrally.
2. **Make the gap observable** (PRD R-4, R-5) — today a never-attempted poll emits nothing at all, so
   a landed fix would move no existing readout and be unfalsifiable.
3. **Pre-author the regression oracle and prove it RED** against the pre-fix tree (PRD R-6).

## 2. Constraints

| # | Constraint | Source | Consequence for this design |
|---|---|---|---|
| C-1 | **Rate-neutral.** Reorder only — never add a request, timer, or concurrent poller | `ADR-0012` Decision 3, restated verbatim in `build_poll_schedule`'s doc comment | The fix may touch only the schedule **vector** and cursor. `rotation_len()` — the tick divisor — is untouchable |
| C-2 | Do not lower `poll_secs` or `near_limit_poll_secs` | `ADR-0012` Decision 1 + Alternative 1; open **#1309** exists to measure whether poll cadence is a material 429 driver *first* | The cadence levers are off the table; this is a scheduling-order fix |
| C-3 | The stagger + interleave locks must stay green, **unmodified** | Existing suite, named in § 11 | They are the rate-neutrality gate. A lock that needed *editing* to accommodate this change would itself be evidence the change was not rate-neutral |
| C-4 | The oracle must be **RED against the pre-fix tree** | PRD R-6 | A detector that does not redden against the corpse falsifies the diagnosis rather than confirming it |
| C-5 | `{T}` (target-reading staleness bound) and `{D}` (surfacing latency) stay **UNSET** | PRD § 4, operator-owned | Design the *shape* that accepts each; do not pick a value |
| C-6 | Mechanism-only, multi-provider-neutral framing on every public artifact | `REQ-LAT-SUR-001`, quoted verbatim in the PRD § 2.4 | No provider names, no HQ pointers, no usage-ceiling rationale in tracked items |
| C-7 | The operator's daemon is **live** and depended upon | Session constraint | Read-only probes only. Every decisive mutating probe is **recorded as a proposed next step**, never run — see § 14 |

## 3. Context and Scope

**In scope**: the daemon's poll-schedule lifecycle (`src/daemon.rs`), the two active-changing command
paths (`src/daemon/commands.rs`), the never-looked-at instrument, the regression oracle, and — sized
separately — the surfacing half's wire contract.

**Out of scope, and why** (restating the PRD § 1b boundary so this document stands alone): the
capacity strand — synchronized fleet exhaustion, `all_exhausted` holds, the `late=true` swap — is
**#726**. On the incident night it produced a real 68-minute event that the daemon **detected
correctly and reported correctly** and could not act on. It is a different failure with a different
cause. `V_PEAK_SESSION_PCT_PER_MIN` recalibration is real (`reliability` self-flags `[RECALIBRATE]`)
and is a separate lever.

## 4. Solution Strategy

**The fix is not a new pattern. It is a missing member of an existing, carefully-maintained
invalidation set, at the two sites that already maintain it.**

That framing is the design's centre, and it is what keeps the change small. Both active-changing
sites already invalidate *six* other pieces of active-derived state, each with a cited rationale:

```
record_swap (src/daemon.rs)                 sets state.active, then invalidates:
  parked_landing[target]  = None            #613 — a prior park's window must not fire on a now-active account
  last_good               = None            #450 — the departing active's anchor must not outlive the swap
  last_swap               = Some(...)        #10  — arms the cooldown
  last_blind_preempt_swap = None            #479 — the narration is stale once active changes
  signaled_retry_after_reserve/_walk = false #582 — a swap ENDS the episode the holds were reported against
  canonical_watch.commit(...)                     — re-baselines the watch
  poll_schedule / poll_pos                  <-- MISSING. Derived from `active`, never invalidated.
```

`adopt_manual_swap` (`src/daemon/commands.rs`) carries the same set and the same omission.

And the precedent for the missing member already exists, three lines, in the *same file* as one of
the two sites — `reconcile_roster` (`src/daemon/commands.rs`):

```rust
// The schedule held OLD roster indices; clear it so `next_poll_index` rebuilds a
// fresh one (the active interleaved before each enabled non-quarantined peer,
// #366) at the next cycle start.
self.state.poll_schedule.clear();
self.state.poll_pos = 0;
```

**Why the precedent did not already cover the swap case.** Its stated reason is *index validity* —
"the schedule held OLD roster indices". A swap does not move indices, so the reason does not reach
it. The active-change case was never in view. This is the same shape as the gap in `ADR-0012` itself
(§ 12): the mechanism was derived for one case and silently assumed for the other.

### 4.1 Why clearing is sufficient — and immediate

`next_poll_index` rebuilds under `if self.state.poll_pos >= self.state.poll_schedule.len()`. After a
clear, that reads `0 >= 0` → **true**, so the rebuild happens on the very next tick, not at some
later boundary. And `build_poll_schedule` emits `[active, p₁, active, p₂, …]` — the active is at
index 0. So the next tick polls the newly-active account.

**Resulting first-sight latency: one tick = `poll_secs / N` ≈ 37.5 s**, which is *half* the `{N}` =
`2·poll_secs / N` = 75 s bound R-1 requires. The fix does not merely meet the guarantee; it lands
at the per-source floor, which is the tightest any rate-neutral change can be.

### 4.2 Why it is rate-neutral by construction, not by measurement

This is the load-bearing argument for C-1, and it is structural rather than empirical:

- `next_subinterval()` computes the tick spacing as `next_poll_interval() / rotation_len()`.
- `rotation_len()` counts **distinct rotation accounts** — it reads `roster` and `state.accounts`,
  and **never reads `poll_schedule`**.
- Therefore the schedule vector's length and contents cannot influence tick spacing at all.

The fix changes only *which index* a tick polls. It changes no tick's timing, adds no tick, and adds
no poller. The `#366` doc comment already states this invariant for the interleave; the fix inherits
it unchanged. **The `#80`/`#366` stagger locks are the gate that proves it** (§ 11), and they are
expected to stay green *without modification* — a stagger lock that needed editing to accommodate
this change would itself be evidence the change was not rate-neutral.

### 4.3 The instrument (R-4) needs no new state

`state.accounts[i].last_reading_at` **already exists** — `note_blind_episode` reads it to anchor a
blind episode. So the never-looked-at instrument is a derivation over state the daemon already keeps:

> Each tick, for the resolved active account, compare `now − last_reading_at` against
> `2·poll_secs / N`. On the **crossing** edge, emit once; on re-observation, close the episode.

Three properties this shape buys, each answering a specific way the existing detectors fail here:

| Property | Why it matters |
|---|---|
| Evaluated **outside** the `if let Some(i) = poll_idx { … }` block | Every existing detector — `record_usage_sample`, `maintain_stats_store`, `note_blind_episode`, `note_exhausted_poll` — sits *inside* it, so a never-attempted poll emits nothing. This is the whole defect |
| Keyed on **elapsed time**, not on a poll result | `note_blind_episode`'s entry arm is `(None, Err(_))` — it needs a poll to have *run and failed*. A poll that never ran produces no `Err` |
| **Edge-triggered**, mirroring `blind_anchor` | An every-tick emission would flood the log. The existing episode pattern (enter/exit with an anchor) is the house shape and is reused rather than reinvented |

The tick-filter is a second, narrower blindness the same instrument covers for free:
`poll_idx.filter(|&i| !account_backing_off(i) && !exhausted_slow_polling(i, active))` means an
account can be *scheduled* and still not polled. An elapsed-time instrument sees that too; a
schedule-position instrument would not.

## 5. Building Blocks

| # | Change | Site | Size | Depends on |
|---|---|---|---|---|
| **B-1** | Invalidate the schedule on autonomous swap | `Daemon::record_swap`, `src/daemon.rs` | 3 lines + comment | — |
| **B-2** | Invalidate the schedule on operator-adopted swap, guarded on `next_active != prev_active` | `Daemon::adopt_manual_swap`, `src/daemon/commands.rs` | 3 lines + comment | — |
| **B-3** | Never-looked-at instrument: edge-triggered active-observation-gap episode | `src/daemon.rs` tick body + `src/observability.rs` | ~40 lines | — |
| **B-4** | Regression oracle: mid-cycle change of active, every path | `src/daemon.rs` tests | ~120 lines | B-1, B-2 (must be RED before them) |
| **B-5** | `ADR-0012` amendment or superseding record | `docs/adr/` | doc | B-1..B-4 |
| **B-6** | Surfacing: wire field + panel + notification | `snapshot.rs`, Swift | **sized separately** — § 8 | B-3 |
| **B-7** | Target-reading staleness in ranking | `selection.rs` | **blocked on `{T}`** | § 14 |

**B-2's guard is not cosmetic.** `adopt_manual_swap` re-resolves the active from the canonical item
and may receive a duplicate or same-account notification; it already guards its `last_good` reset on
`next_active != prev_active` for exactly this reason. The schedule invalidation must sit inside the
same `if let Ok(canonical)` block and carry the same guard — a locked keychain leaves `active`
unresolved, and invalidating a schedule for an active that did not change restarts the peer sweep
for nothing.

## 6. Runtime View

**Before (the incident, reconstructed):**

```
t+0     schedule rebuilt for active=A   ->  [A, p1, A, p2, A, p3, ... ]   pos=0
t+0     tick polls A
...     swap A -> B at mid-cycle          state.active = B
        (schedule still holds A at every even index; pos untouched)
t+n     tick polls A          <-- the account just PARKED
t+n+1   tick polls p_k
t+n+2   tick polls A          <-- again
...     x5 in the observed window, and B ZERO times
t+638s  cycle finally wraps; schedule rebuilt for B; B polled -> reading 1.00
```

**After:**

```
...     swap A -> B                       state.active = B
                                          poll_schedule.clear(); poll_pos = 0
t+1tick rebuild -> [B, p1, B, p2, ...]     pos=0
t+1tick tick polls B          <-- first sight at one sub-interval = poll_secs/N
```

## 7. Interface Contracts

**The core fix crosses no wire.** `poll_schedule` and `poll_pos` are daemon-internal runtime state,
absent from `StatusResponse` and from every `watch` frame. B-1, B-2 and B-4 therefore need **no**
`STATUS_SCHEMA_VERSION` bump, **no** golden regeneration, and **no** Swift edit. This is why the core
half is shippable ahead of the surfacing half, and it is the mechanical fact the ratified appetite
rests on.

**B-3's instrument is log-only** in its first increment — an event in the daemon's own event log,
consumed by `sessiometer log` and the SLI readouts. Also no wire bump.

## 8. The surfacing half — sized separately

Per the ratified appetite, the surfacing half (PRD R-5, R-10) is costed here and scoped as its own
item rather than folded into the core.

**Cost, itemized.** `STATUS_SCHEMA_VERSION` is currently **1.14** (`src/daemon/snapshot.rs:538-541`).
Surfacing an observation-gap state on the panel requires a field on `StatusResponse`, which is a
**minor bump to 1.15** and obligates:

| Obligation | Command / file |
|---|---|
| Regenerate the five status/watch wire goldens | `cargo test -- --ignored emit_wire_golden_fixtures` |
| Update **current-minor** Swift fixtures only | `grep -n '"minor":14' apps/menubar/Tests/Fixtures.swift` — that grep **is** the set to sweep |
| Update `WireDecoderTests.swift`, **both spellings** | `grep -nE 'minor: 14\|"minor":14'` — the assertions use the spaced form; an inline JSON literal one minor *ahead* stops being "future" on the bump |
| Possibly add a **new pinned fixture at the outgoing minor** | so the backward tolerance 1.14 proved keeps being tested |
| Leave version-pinned compat fixtures alone | anything not at the current minor is deliberately pinned |
| No production Swift edit | `WireModel.swift` reads only the **major** |

**Field shape**: follow the established omit-when-healthy precedent (`canonical_scrub`,
`systemic_refresh_failure`'s 1.10→1.11 bump) — an additive `Option`, `#[serde(default)]`, omitted
entirely when healthy, so a healthy frame's bytes are byte-for-byte unchanged.

**R-5's readout half is independent of the bump and should not wait on it.** The overshoot SLI filters
to `reason=session`, and a manual swap records `session_pct=0` **by design**
(`src/observability.rs`: *"a manual/forced swap is not session-triggered"*). So the operator's own
rescues — the defining event of this incident class — are graded by nothing. Widening that readout to
grade operator-initiated swaps is a log/SLI change with no wire surface, and it is the **only** thing
that makes the core fix falsifiable. It belongs with the core, not with the panel.

## 9. Crosscutting Concepts

### Security
N/A. No credential path, no new external surface, no change to peer authentication or credential
storage. The instrument records elapsed seconds and a roster index, never a token, email, or path.

### Observability
The centre of gravity, and covered in § 4.3 and § 8. One additional obligation: the new event must
carry enough to compute § 13's `PostSwapFirstSightLatency` without a second source — the elapsed gap
and whether the account was active — so the SLI is derivable from the event log alone.

### Error Handling
The instrument must be **fail-open**: it observes and reports, and can never delay, block, or alter
the poll it instruments. A failure to emit is swallowed, exactly as `record_usage_sample` swallows a
store error today ("off the swap-decision path, so a sampling failure never perturbs the loop").

### Master Test Plan

**Risk surface (ACC), scoped to this design:**

| Cap | Component | Capability under test | Risk |
|---|---|---|---|
| Cap-1.1 | `next_poll_index` / `build_poll_schedule` | After a change of active by **any** path, the new active is polled within `2·poll_secs/N` | **HIGH** — the defect itself |
| Cap-1.2 | `next_subinterval` / `rotation_len` | Tick spacing, aggregate rate and the per-source floor are unchanged | **HIGH** — C-1 |
| Cap-1.3 | `record_swap`, `adopt_manual_swap` | Every active-changing path invalidates; a same-account adopt does **not** | MEDIUM |
| Cap-1.4 | instrument | An observation gap emits **without any poll having run** | **HIGH** — nothing observes this today |
| Cap-1.5 | schedule lifecycle | Peer re-observation stays bounded under back-to-back swaps at the cooldown floor | MEDIUM — § 14 R-2 |

**Pyramid**: entirely at the hermetic unit level. The daemon already has a `FakeDaemon` + `FakeClock`
+ `FakeRosterPoller` harness (`tunables(…)`, `warmed_tick(…)`), which is exactly the shape
`REQ-LAT-Q-001` mandates. No integration or E2E tier is warranted, and none is proposed.

**The oracle (B-4), and why the existing lock does not cover it.** `REQ-LAT-Q-001` and its
implementation (`#367`) lock a burn crossing **95→100 % within a single poll interval**. This
incident is `0.71 → 1.00` across a **638 s observation hole** — a different shape — and the existing
lock additionally breaks at the first swap. So it cannot fail on this defect, and its passing says
nothing about it.

The new oracle must:

1. Drive a **mid-cycle** change of active — not at a cycle boundary, which is the case that already
   works and would pass pre-fix.
2. Cover **both** paths: `record_swap` (autonomous) and `adopt_manual_swap` (operator-adopted).
   Cap-1.3's negative case — a same-account adopt must **not** invalidate — belongs here too.
3. Assert on **elapsed time to first observation of the new active**, not on schedule contents. A
   structural assertion ("the schedule now contains B") passes on a schedule that is never consumed.
4. Assert the **complement**: tick spacing and poll count over the window are unchanged. Without this
   the oracle would pass on a fix that added a poller.
5. **Be demonstrated RED against the pre-fix tree, with the failing output recorded**, before any of
   B-1/B-2 lands (C-4).

**Its template already exists.** `reconcile_roster_resets_the_stale_poll_schedule`
(`src/daemon/commands.rs:2789`) is the same assertion for the roster-reconcile case — it seeds
`poll_schedule = vec![0, 1]` and `poll_pos = 1`, then asserts both are reset. The new oracle is that
test's swap-shaped sibling, with one deliberate difference: it must assert on **elapsed time to first
observation**, not on the reset itself. Asserting the reset would pass on a schedule that is cleared
and then never consumed, and would not measure the thing the requirement bounds.

**Authored as a `deterministic-output` oracle**: the assertions are specified here, at design time,
so the executor does not bind its own. That is not ceremony — a change that edits both the
implementation and the assertions grading it has no independent gate for that change.

## 10. Architecture Decisions

| # | Decision | Alternatives considered | Why |
|---|---|---|---|
| **D-1** | Invalidate by `clear()` + `poll_pos = 0`, mirroring `reconcile_roster` verbatim | (a) Rebuild in place preserving peer progress; (b) hoist invalidation into a single `set_active()` helper both sites call | (a) needs new per-cycle peer-progress state for a benefit `ADR-0012` already deems unnecessary — *"peers are only swap targets, ranked by weekly reset, so relaxing their cadence is fine"*. (b) is the better end state and is **explicitly deferred**: it is a wider refactor of six invalidations across two files, and folding it in would trade a 3-line, precedent-matching fix for a restructure the ratified appetite does not cover. Recorded as a follow-up, not silently dropped |
| **D-2** | The instrument keys on elapsed time against `last_reading_at`, not on schedule position | Track "ticks since the active was scheduled" | Elapsed time also catches the `poll_idx.filter(…)` case, where an account is scheduled and still not polled. Schedule position does not. It also needs no new state |
| **D-3** | Widen the overshoot readout to grade operator-initiated swaps **with the core**, not with the panel | Ship it with the surfacing half | Without it no readout moves when the fix lands, so the core would be unfalsifiable. It has no wire surface, so nothing forces it to wait |
| **D-4** | **PRD R-9 is WITHDRAWN** — see § 12 | Keep it as a `should` | Grounding showed the behaviour it describes is deliberate and correct |
| **D-5** | `{T}` and `{D}` stay unset; B-7 is blocked rather than estimated | Pick defensible defaults | C-5. A silently-picked threshold would wear a requirement's authority |

## 11. Quality Requirements

**The gate is four named, existing tests** — pinned here so the claim is checkable rather than
gestural. All must pass **without modification**:

| Test | File | What it pins |
|---|---|---|
| `the_sub_interval_spreads_a_cycle_across_the_rotation` | `src/daemon.rs:12304` | Tick spacing = `poll_secs / N`. The rate-neutrality lock proper |
| `the_poll_schedule_interleaves_the_active_before_each_peer_and_wraps` | `src/daemon.rs:12244` | The `[active, p₁, active, p₂, …]` shape and its wrap |
| `the_poll_schedule_interleaves_before_each_peer_and_handles_degenerate_rosters` | `src/daemon.rs:12273` | Degenerate rosters — no active, or an active with no peers |
| `near_limit_fast_poll_caps_the_active_sub_interval_in_band_only` | `src/daemon.rs:12497` | The `#540` cap applies in-band only; steady-state cadence stays flat |


| Attribute | Target | Gate |
|---|---|---|
| Post-swap first-sight latency | p95 ≤ `2·poll_secs / N` (75 s at live config). Design lands at `poll_secs / N` ≈ 37.5 s | Cap-1.1 + § 13 SLI |
| Rate neutrality | Aggregate `/usage` requests per hour: **delta == 0** | The four named locks below, **unmodified** |
| Instrument overhead | Zero additional requests; fail-open | Cap-1.4 + code review |
| Oracle validity | RED pre-fix, GREEN post-fix, with the pre-fix failure recorded | C-4 |

## 12. Risks, Open Questions, and one correction

### Correction — PRD R-9 is withdrawn

The PRD listed, as compounding mechanism (b), that `record_swap` clears the incoming account's
`parked_landing`, disarming the landing watch so a later ceiling crossing could not trip
`note_landing_overshoot`. **Reading the site shows the behaviour is deliberate, reasoned, and
correct**:

```rust
// Issue #613: the account going ACTIVE cannot be a parked-landing subject — disarm any landing
// watch on it here (the shared swap path), so a prior park's stale window can't fire against an
// account that is now active again.
```

A landing watch is a *parked*-account mechanism. Once the account is active again, an overshoot is
the **active-side** detector's job — and the active-side detector is precisely what the 638 s hole
disabled. **R-9 therefore collapses into R-1 and is withdrawn as a separate requirement.** This
removes work rather than adding it, and it corrects a finding the investigation propagated as a
defect. The PRD is updated to match.

### Feasibility

| Component | Verdict | Note |
|---|---|---|
| B-1, B-2 | **FEASIBLE** | Precedent exists three lines away, in one of the two files being edited |
| B-3 | **FEASIBLE** | `last_reading_at` already exists; the episode/anchor pattern is the house shape |
| B-4 | **FEASIBLE** | The hermetic harness already exists and is the shape `REQ-LAT-Q-001` mandates |
| B-5 | **FEASIBLE** | Documentation |
| B-6 | **FEASIBLE-WITH-SPIKE** | Cost is enumerated (§ 8), but `{D}` is unset |
| B-7 | **UNCERTAIN — BLOCKED** | `{T}` unset, and assumption A-6 is 🔴. Not proposed for this scope |

### Risk register

| # | Risk | L×I | Priority | Mitigation |
|---|---|---|---|---|
| R-1 | The fix lands on one path and a third active-changing site is missed later | 2×3=6 | MEDIUM | Cap-1.3 drives **every** path; the enumeration of write sites in § 5 is exhaustive at HEAD (`grep` of every `poll_schedule` / `poll_pos` write returns exactly three production sites) |
| R-2 | **Peer starvation under swap churn.** `clear()` restarts the peer sweep; the default cooldown is **60 s** (`DEFAULT_COOLDOWN_SECS`, `src/config.rs:102`) while a full sweep spans `2·poll_secs·(N−1)/N` ≈ 525 s. Repeated swaps could keep restarting it | 2×2=4 | MEDIUM | Partly pre-ratified: `ADR-0012` states *"peers are only swap targets, ranked by weekly reset, so relaxing their cadence is fine."* **But the second-order effect is not pre-ratified**: staler peer readings feed `pick_target` ranking, coupling this to the unset `{T}`. Mitigated by Cap-1.5 asserting peer re-observation stays bounded under back-to-back swaps at the cooldown floor. **And the bound itself is conditional**: #1356 reports that a daemon restart clears the cooldown entirely (`DecisionState` is `Default`-constructed, so `last_swap` returns `None`), so across a restart this argument does not hold. Tracked as its own coordination item |
| R-3 | The oracle passes pre-fix, i.e. the diagnosis is wrong | 1×3=3 | LOW | C-4 makes the RED demonstration a gate, not a formality. A pre-fix pass **falsifies the diagnosis** and sends this back to analysis rather than being explained away |
| R-4 | The instrument lands but the readout still cannot see the operator's rescues | 2×3=6 | MEDIUM | D-3 pulls the readout widening into the core half for exactly this reason |
| R-5 | A genuinely-free target stays invisible because a 429 nulls `last_reading` (PRD R-8) | 3×2=6 | MEDIUM | **Explicitly deferred, not mitigated, and NOT YET FILED.** `self.state.accounts[i].last_reading = result.ok();` is universal, so a 429 nulls the slot and `pick_target_with_reason_ranked`'s `filter_map` drops the account. On the incident night this hid the account the operator manually switched to — an account **actually at 0%**. A real, separate defect. It falls **outside the ratified scope selection**, so it was not filed by interpretation; it is surfaced as a recommended ninth item awaiting a decision. The core fix does not depend on it — it routes correctly to the best *visible* target, strictly better than today |

**No unmitigated HIGH risks.** R-2 and R-5 are MEDIUM and carry explicit dispositions rather than
silent acceptance.

### Open questions

- **`{T}` — target-reading staleness bound.** Load-bearing for B-7 only, which is not in this scope.
  The *shape* is settled and constrains any future value: **fresh-enough-or-repoll-first, never hard
  exclusion.** With five of eight accounts weekly-exhausted at the incident instant, a hard staleness
  filter can collapse the viable set to `NoViableTarget` — converting a latency defect into an
  availability one (PRD A-6, 🔴).
- **`{D}` — surfacing latency.** Load-bearing for B-6 only, which is sized separately here and not
  proposed for the core.

Neither blocks the core. Both are recorded on `should` requirements.

## 13. Success Criteria and SLI

```
TAG:    PostSwapFirstSightLatency
SCALE:  seconds from a change of active designation to the first completed observation of the new active
METER:  p50 / p95 over a 7-day window, from the daemon's own event log (B-3's episode events)
PAST:   p50 182 s, p95 436 s (measured 2026-09-01; worst observed 638 s)
GOAL:   p95 <= 2*poll_secs / N            (75 s at poll_secs=300, N=8)
DESIGN: ~poll_secs / N                    (37.5 s — one sub-interval, the per-source floor)
FAIL:   any single occurrence > 2 * (2*poll_secs / N)
```

## 14. Proposed next steps requiring a mutating probe — NOT RUN

Recorded per C-7. Each would be decisive and each is left for the operator to authorize:

1. **Observe a live swap end-to-end** with the instrument in place, to confirm the measured
   first-sight latency matches the design's ~37.5 s. Requires restarting the daemon onto a build
   carrying B-1..B-3.
2. **Drive a manual swap** to exercise B-2's path against the live daemon.

Neither is needed to land the core fix — the hermetic oracle (B-4) is the gate. They are
confirmation, not evidence the design lacks.

## 15. Forward Coverage — PRD requirement to design element

| PRD Req | Design element | ACC | Status |
|---|---|---|---|
| R-1 | § 4.1, B-1, B-2 | Cap-1.1 | covered |
| R-2 | § 4.2, B-1, B-2 | Cap-1.2 | covered |
| R-3 | B-1 + B-2 (both paths), § 5 site enumeration | Cap-1.3 | covered |
| R-4 | § 4.3, B-3 | Cap-1.4 | covered |
| R-5 | D-3, § 8 | Cap-1.4 | covered |
| R-6 | § 9 Master Test Plan, B-4 | Cap-1.1..1.5 | covered |
| R-7 | B-7 | — | **deferred** — `{T}` unset (§ 12); tracked as its own item |
| R-8 | § 12 risk R-5 | — | **UNCOVERED** — outside the ratified selection, not filed. Surfaced as a recommended ninth item; see the scope brief |
| R-9 | § 12 Correction | — | **withdrawn** — the behaviour is deliberate and correct (D-4) |
| R-10 | B-6, § 8 | — | **deferred** — `{D}` unset; surfacing half sized separately |
| R-11 | § 8 field shape | — | conditional on R-7 / R-10 |
| R-12 | B-5 | — | covered |

**One `UNCOVERED` entry: R-8.** Every other requirement is covered, deferred with a tracking
destination, or withdrawn with a reason. R-8 is resolved per the UNCOVERED gate's option (c) —
deferred with explicit tracking — by being named in the scope brief as a recommended ninth item with
its mechanism replicated in-band. It is **not** silently absorbed and **not** filed by
interpretation: the scope selection was an explicit bounded choice, and adding a whole un-selected
work item to it is the operator's call, not the pipeline's.

## 16. Backward Coverage — design element to requirement

| Design element | Traces to | Status |
|---|---|---|
| B-1, B-2 | R-1, R-2, R-3 | traced |
| B-3 | R-4 | traced |
| B-4 | R-6 | traced |
| B-5 | R-12 | traced |
| B-6 | R-10, R-11 | traced |
| B-7 | R-7 | traced |
| D-3 (readout widening pulled into the core) | R-5 | traced |
| Cap-1.5 / risk R-2 (peer-starvation bound) | **net-new** | **ratified net-new** — a consequence introduced *by* D-1, not present in the PRD. Surfaced here rather than absorbed; it constrains the fix rather than expanding scope |

No `PHANTOM` entries.

## 17. Glossary

| Term | Meaning here |
|---|---|
| **N** | `rotation_len()` — the count of DISTINCT rotation accounts. **Not** the schedule length. The tick divisor, and the reason the fix is rate-neutral |
| **Sub-interval / tick** | `poll_secs / N` ≈ 37.5 s at live config. One account polled per tick |
| **Sweep** | One full traversal of the schedule vector, `2·(N−1)` entries ≈ 525 s |
| **`{N}`** | The post-swap first-sight bound: `2·poll_secs / N` = 75 s. `ADR-0012` Decision 4, operator-ratified 2026-09-01 |
| **`{T}` / `{D}`** | Target-reading staleness bound / surfacing latency. **UNSET, operator-owned** |
| **Change of active** | Any transition of `state.active`, by any path. The design's unit of concern — deliberately wider than "a swap" |
