---
type: scope-brief
date: 2026-09-01
workflow: /scope
source: docs/requirements/active-account-observation-continuity.md
status: final
---

# Scope Brief: Active-Account Observation Continuity

## What happened

An account reached its ceiling and the daemon never saw it. The operator switched by hand.

Forensics found the daemon had swapped *into* that account on a 185-second-stale reading of `0.71`,
then not observed it again for **638 s**. The next reading was `1.00`, recorded in the same second as
the manual switch. Across that window it polled the account it had just parked **five times** and the
newly-active one **zero times**. Of 24 corpus entries into a full reading, 23 arrived from 0.93–0.99;
this is the only one that arrived from 0.71.

## What it actually was

Not a swap failure. **The swap-away trigger never received an input to fire on** — it evaluated
correctly, against a reading 638 s stale.

`Daemon::next_poll_index` rebuilds the poll schedule only at a cycle wrap. `record_swap` and
`adopt_manual_swap` each set `state.active` and invalidate **six** other pieces of active-derived
state — and neither touches the schedule. So after a mid-cycle swap the interleave keeps polling the
previous active until the cycle turns over.

Measured against `ADR-0012` § Decision item 4's `2·poll_secs / N` — 75 s at the shipped
configuration — that is an **8.5× overrun**. And the sharper finding: **the ADR reasons entirely
about a static active and never considers a mid-cycle change of active.** The guarantee was not
merely violated; it was never derived for the case that failed.

## Tracked items — the ratified eight

| # | Item | State |
|---|---|---|
| **#1451** | Pre-author the mid-cycle change-of-active oracle, **RED before any fix** | READY |
| **#1452** | Invalidate the poll schedule on every change of active — **the fix** | READY, after #1451 |
| **#1453** | Make an unobserved active visible; grade operator-initiated swaps | READY |
| **#1454** | Amend `ADR-0012` — the guarantee was never derived for this case | READY |
| **#1455** | The starvation bound depends on a cooldown a restart clears (couples to #1356) | READY |
| **#1456** | Bound target-reading staleness in ranking | **BLOCKED on `{T}`** |
| **#1457** | Surface an at-limit active without the panel (wire bump 1.14 → 1.15) | **BLOCKED on `{D}`** |
| **#1458** | Expose the thresholds as tunables | **BLOCKED on #1456/#1457** |

Five ready, three blocked on a named blocker. Nothing was added to or dropped from the ratified
selection.

## The fix is three lines at each of two sites

It is not a new pattern — it is a **missing member of an existing invalidation set**, and the
precedent sits three lines away in one of the two files being edited (`reconcile_roster`:
`poll_schedule.clear(); poll_pos = 0`). That precedent never covered the swap case because its stated
reason is *index validity*, and a swap does not move indices.

Clearing rebuilds on the **next** tick, and `build_poll_schedule` puts the active at index 0 — so
first sight lands at **~37.5 s**, half the bound and the tightest a rate-neutral change can be.

**Rate neutrality is structural, not measured**: `next_subinterval` divides by `rotation_len()`,
which counts distinct accounts and never reads `poll_schedule`. Four named existing locks are the
gate, and must pass **unmodified**.

## Two corrections found by reading code rather than the report

1. **PRD R-9 withdrawn.** The landing-watch disarm flagged as a compounding defect is deliberate
   (`#613`: a parked-account watch must not fire against a now-active account). Once active, an
   overshoot belongs to the active-side detector — which is what the observation hole disabled. It
   collapses into R-1. **This removed work.** Recorded rather than deleted, because the finding was
   sourced correctly and interpreted wrongly — the mechanism was read without its rationale.
2. **#1356 coupling.** #1452's peer-starvation bound rests on the swap cooldown; #1356 reports that a
   restart clears the cooldown entirely. That also makes "restart it and see" destructive to the
   state under test for anything in this area. Now #1455.

## Open, and deliberately not decided here

`{T}` (target-reading staleness bound) and `{D}` (surfacing latency) are **operator-owned**. A
silently-picked threshold would wear a requirement's authority it does not have.

`{T}`'s *shape* is settled even though its value is not: **fresh-enough-or-repoll-first, never hard
exclusion.** With five of eight accounts already unavailable at the incident instant, a hard
staleness filter can collapse the viable set to `NoViableTarget` — turning a latency defect into an
availability one.

## One coverage gap, surfaced rather than resolved

**PRD R-8 is not covered by any filed item.** A failed poll assigns `last_reading = result.ok()`,
which **nulls** the slot, and `pick_target_with_reason_ranked`'s `filter_map` then drops the account
from candidacy entirely. On the incident night this hid an account that was **actually at 0%** — the
very account the operator manually switched to.

It is a real, separate defect. It is **not in the ratified eight**, so it was not filed: an explicit
bounded selection binds the remainder, and a whole un-selected work item is not something to add by
interpretation. Recommended as a ninth item; awaiting a decision.

A second candidate, lower value: hoisting all six invalidations behind one `set_active()` helper.
Better end state, but a two-file restructure the ratified appetite does not cover.

## Explicitly out of scope

The capacity strand — synchronized fleet exhaustion, `all_exhausted` holds, the `late=true` swap — is
**#726**. On the incident night it produced a real 68-minute event that the daemon **detected
correctly and reported correctly** and could not act on. A different failure with a different cause;
fusing it here would make a scheduling defect inherit an undecidable provisioning question.

Also out: retuning the reactive thresholds, lowering `poll_secs` / `near_limit_poll_secs`
(`ADR-0012` § Decision item 1 rejects it, and **#1309** exists to measure whether poll cadence is a
material 429 driver first), and `V_PEAK` recalibration.

## Artifacts

- PRD — `docs/requirements/active-account-observation-continuity.md`
- Solution design — `docs/design/active-account-observation-continuity-solution-design.md`
- Design brief — `docs/briefs/2026-09-01-design-active-account-observation-continuity.md`
