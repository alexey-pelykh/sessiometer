---
type: design-brief
date: 2026-09-01
source: docs/design/active-account-observation-continuity-solution-design.md
workflow: /design-solution
status: draft
---

# Design Brief: Active-Account Observation Continuity

## Problem

When the daemon's active account changes mid-cycle, the poll schedule keeps polling the account it
just parked. The newly-active account goes unobserved for up to a full schedule sweep — measured once
at **638 s** against `ADR-0012` Decision 4's `2·poll_secs / N` guarantee of **75 s**, an 8.5× overrun.
During that window the daemon polled the parked account five times and the new one zero times. The
account climbed 0.71 → 1.00 unseen, and the operator had to intervene by hand.

## Key Decisions

1. **Mirror `reconcile_roster`'s invalidation verbatim** — `poll_schedule.clear(); poll_pos = 0` at
   both active-changing sites — because this is not a new pattern. `record_swap` and
   `adopt_manual_swap` already invalidate *six* other pieces of active-derived state, each with a
   cited issue rationale; the poll schedule is the one member that was missed. The precedent sits
   three lines away in one of the two files being edited.
2. **Clearing is sufficient *and* immediate** — because `next_poll_index` rebuilds under
   `poll_pos >= poll_schedule.len()`, which a clear makes true at once, and `build_poll_schedule`
   puts the active at index 0. First-sight latency lands at **one tick ≈ 37.5 s**, half the 75 s the
   requirement asks for, and the tightest a rate-neutral change can be.
3. **Rate neutrality is structural, not measured** — `next_subinterval` divides by `rotation_len()`,
   which counts distinct accounts and **never reads `poll_schedule`**. The fix cannot affect tick
   timing. The `#80`/`#366` stagger locks are the gate, and are expected to pass **unmodified**; a
   lock that needed editing would itself be evidence the change was not rate-neutral.
4. **The instrument keys on elapsed time, not on schedule position** — because it also catches the
   case where an account is scheduled and still not polled (the `poll_idx.filter(…)` arm), and
   because `last_reading_at` already exists, so it needs no new state.
5. **Widen the overshoot readout with the core, not with the panel** — because a manual swap records
   `session_pct=0` by design and the SLI filters to `reason=session`, so today **no readout would
   move if the fix landed**. Without this the core fix is unfalsifiable. It has no wire surface, so
   nothing forces it to wait.
6. **PRD R-9 is withdrawn** — the landing-watch disarm it flagged as a compounding defect is
   deliberate and correct (`#613`: a parked-account watch must not fire against a now-active
   account). Once active, an overshoot is the active-side detector's job — which is exactly what the
   observation hole disabled. R-9 collapses into R-1. **This removes work rather than adding it.**
7. **Defer a `set_active()` helper** — hoisting all six invalidations behind one call is the better
   end state, but it is a restructure across two files that the ratified appetite does not cover.
   Recorded as a follow-up rather than silently dropped.

## Design Tracks

| Track | Approach | Key Trade-off |
|---|---|---|
| Technical Architecture | Three lines at each of two sites, mirroring an existing precedent | Chose the precedent-matching fix over the cleaner `set_active()` refactor — small now, follow-up recorded |
| Performance | Rate neutrality argued structurally from `rotation_len()`, gated by the unmodified stagger locks | None — the fix reorders and cannot add load |
| Testing | Hermetic oracle driving a mid-cycle change of active on **every** path, asserting on elapsed time and on the unchanged complement | Must be RED pre-fix. A pre-fix pass **falsifies the diagnosis** rather than being explained away |
| API / wire | Core crosses **no** wire; surfacing half costed at `STATUS_SCHEMA_VERSION` 1.14 → 1.15 with its full fixture obligation | Splitting lets the core ship without waiting on golden + Swift work |

## Open Questions

Neither blocks the core fix. Both sit on `should` requirements, and the core is scoped without them.

- **`{T}` — how stale may a swap target's reading be before it stops counting as viable?**
  Context: target selection has no staleness term at all today, and grounding confirmed that is an
  absence in the design corpus, not just the code. Impact if deferred: the daemon can keep routing to
  a target whose reading is old. **The shape is already settled and constrains any answer:
  fresh-enough-or-repoll-first, never hard exclusion** — with five of eight accounts weekly-exhausted
  at the incident instant, a hard filter can collapse the viable set to `NoViableTarget`, turning a
  latency defect into an availability one.

- **`{D}` — how fast must an at-limit active account reach the operator without a panel open?**
  Context: the surfacing half is sized separately and its wire cost is enumerated in the design § 8.
  Impact if deferred: the panel work cannot be scoped, but nothing in the core depends on it.

## Deferred with a tracking destination, not dropped

- **A 429 nulls `last_reading`, hiding a genuinely-free target.** `last_reading = result.ok()` is
  universal, so `pick_target_with_reason_ranked`'s `filter_map` drops the account entirely. On the
  incident night this hid the very account the operator manually switched to. A real, separate
  defect — its own item. The core fix routes correctly to the best *visible* target, which is
  strictly better than today and does not depend on this.
- **Peer starvation under swap churn.** Clearing restarts the peer sweep; the default cooldown is
  60 s against a ~525 s sweep. `ADR-0012` pre-ratifies relaxed peer cadence — but *not* the
  second-order effect, where staler peer readings feed target ranking. Bounded by a test assertion
  (Cap-1.5), and surfaced as net-new rather than absorbed.

## Full Design

See [active-account-observation-continuity-solution-design.md](../design/active-account-observation-continuity-solution-design.md)
for the complete specification.
