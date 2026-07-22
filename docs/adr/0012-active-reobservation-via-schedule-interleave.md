---
type: architecture-decision-record
number: 12
title: "Active-account re-observation via schedule interleave, not a lower `poll_secs`"
date: 2026-07-09
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0012: Active-account re-observation via schedule interleave, not a lower `poll_secs`

## Status

**Accepted** — 2026-07-09. Records the cadence decision behind the **#366**
active-interleave change (shipped at `dd20afc`), paired with the **#363**
reaction-latency umbrella, so a contributor does not reverse-engineer *why* the
active account is re-observed more often than its peers while the global
`poll_secs` default was left untouched. Like ADR-0008 and ADR-0009, this ADR
records a **shipped** behavior change, now enforced in `src/daemon.rs`.

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a
roster of accounts, and the daemon decides swaps on the **last-known** per-account
usage reading, re-observing each account once per **staggered round-robin** cycle
(#80). At the shipped cadence the active account was re-read roughly once per
`poll_secs` (~5 min).

**The reaction-latency gap (#363).** When the active account's session usage climbs
quickly, the reading can cross the **entire swap-away band** — from just under the
session swap-away trigger up to 100% — *between* two consecutive observations of the
active account. The daemon's next observation is then already at the usage ceiling,
so the swap-away lands **late**. The event log confirmed this empirically: swap
events at `session_pct=100` (measured by the #365 `late=` marker), where the active
account should have swapped **inside** the `[trigger, 100%)` band. This is purely a
per-account **observation-cadence** gap — not an all-exhausted condition (a viable
target existed) — so the lever is the **active account's re-observation interval**.

The active account is the **only** one that can reach its usage ceiling *while
active* (a swap only ever moves *away* from it); the peers are solely swap targets,
ranked by soonest weekly reset (#37). So only the **active** interval needs
tightening — the peers' does not.

Two levers could tighten it:

- **(A) Lower `poll_secs` globally** — shrink the per-account cadence for *every*
  account.
- **(B) Interleave the active account in the poll schedule** — re-observe it more
  often *within* the existing cadence, changing no per-account rate.

Lever **(A) is constrained by the endpoint.** The usage endpoint is **source-scoped**
and serves ~one request per short **rate-limit window**; the staggered loop exists
precisely to spread a cycle's N polls `poll_secs / N` apart so **each request lands in
its own window** (`src/daemon.rs` module doc, ~L20–24). A former poll-of-all *burst*
had all-but-one request `429`-fail at the CDN edge — the burst that the round-robin
stagger (**#80**) and the per-account back-off (**#293**, ADR-0009) were introduced to
prevent. Lowering `poll_secs` tightens the **per-source** request spacing back toward
that window and re-opens that burst exposure — a roster-wide rate-limit cost paid to
fix a single-account (active-only) latency gap.

## Decision

**Close the reaction-latency gap by interleaving the active account in the poll
schedule (#366), and keep the `poll_secs` default unchanged.** A lower `poll_secs` is
retained only as a small-roster fallback, gated on a source-window check.

1. **Keep the `poll_secs` default.** `DEFAULT_POLL_SECS = 300`
   (`src/config.rs`, `default_poll_secs`) stays as-is; the
   global poll interval is **not** lowered to close the gap. Lowering it is rejected
   for the endpoint reason above (see Alternatives 1).

2. **Interleave the active account before each peer.** `build_poll_schedule`
   (`src/daemon.rs:1811`) emits `[active, p1, active, p2, …, active, p_{N-1}]` — the
   active account inserted before **each** enabled, non-quarantined peer — instead of
   one active slot per full sweep. The active account therefore re-observes roughly
   every **`2·poll_secs / N`** (~2 sub-intervals) instead of once per ~`poll_secs`
   sweep. Degenerate rosters stay valid: no peers → `[active]` alone; no active →
   peers only (nothing to interleave).

3. **The interleave is rate-neutral — this is the load-bearing invariant.** It
   lengthens only the schedule **vector** (to ~2N). The tick **divisor**
   `rotation_len()` (`src/daemon.rs:1885`) still counts the **N distinct rotation
   accounts** taken from the roster — **not** the ~2N schedule length — so
   `next_subinterval()` (`src/daemon.rs:3603`) keeps consecutive ticks `poll_secs / N`
   apart (≈40–45 s for a typical roster). The **per-tick spacing, the aggregate
   request rate, and the `poll_secs / N` per-source floor are all unchanged.** No new
   timer, async task, or concurrent poller is added (one would fire outside the
   stagger and re-open the **#80/#293** burst); the change is purely this vector plus
   holding the divisor at N.

4. **The active tightens to `2·poll_secs / N`, and the 1:1 interleave is the cap.**
   That interval is deliberately **2× above** the `poll_secs / N` per-source floor —
   inserting the active more than once per peer would push a **peer's** re-observation
   past `2·poll_secs` (peers re-observe every `2·poll_secs·(N-1)/N < 2·poll_secs`,
   which is fine — they are only swap targets). So the schedule tightens the one
   interval that matters without ever driving any single source below its floor.

5. **Lowering `poll_secs` is a small-roster fallback only, behind a source-window
   check.** For a small roster the interleave yields little (few ticks per cycle; a
   single-account roster degenerates to `[active]`, re-observed once per `poll_secs`).
   If a tighter active cadence is genuinely required there, the only remaining lever is
   a **lower `poll_secs`** — admissible **only** when the resulting **per-source**
   request spacing (`poll_secs / N`) still clears the endpoint's short **rate-limit
   window**, so it cannot reopen the burst (#80/#293). It is a **bounded, checked
   fallback**, never the default lever.

## Alternatives considered

1. **Lower `poll_secs` globally** — shrink the per-account cadence for the whole
   roster so the active account is re-observed sooner.
   - **Pros**: a trivial one-value config change; tightens every account's cadence at
     once.
   - **Cons**: it raises the **per-source** request rate against a **source-scoped**
     usage endpoint that serves ~one request per short rate-limit window — re-opening
     the poll-of-all burst the round-robin stagger (#80) and per-account back-off
     (#293, ADR-0009) exist to prevent. It also over-tightens the **peers**, which are
     only swap targets (ranked by weekly reset) and do not need a tighter cadence.
   - **Why rejected**: it pays a **roster-wide rate-limit cost** to fix a
     **single-account** (active-only) latency gap. The interleave reaches the same
     active interval with **no** change to any per-source rate. (Retained only as the
     source-window-checked small-roster fallback, Decision 5.)

2. **A separate concurrent poller / timer for the active account** — a second poll
   path that re-reads the active on its own tighter schedule, decoupled from the
   staggered loop.
   - **Pros**: the tightest possible active cadence, independent of roster size.
   - **Cons**: a second poll path fires **outside** the stagger, re-opening exactly
     the burst #80/#293 closed (two requests to the source-scoped endpoint can now
     collide in one rate-limit window). It also adds concurrent state, a new timing
     seam, and more to test.
   - **Why rejected**: the in-schedule interleave reaches ~`2·poll_secs / N` with
     **zero** new concurrency and provable rate-neutrality (Decision 3); the extra
     poller buys a marginally tighter interval at the cost of reintroducing the burst.

3. **A reactive fast-path** — on observing the active near the top of the swap-away
   band, immediately schedule an extra re-read rather than waiting for its next tick.
   - **Pros**: reacts precisely when the risk is highest, spending an extra request
     only near the band.
   - **Cons**: it is still an **extra** request to the source-scoped endpoint, so it
     must itself respect the rate-limit window; and after the interleave lands, the
     active is already re-observed every ~`2·poll_secs / N`, which the measurement
     (#365) is expected to show closes the gap on its own.
   - **Why (deferred, not rejected)**: tracked as the open question **#369**, assessed
     **redundant** once the interleave is measured; revisited only if the `late=`
     marker still fires after #366. A valid future lever, not a competing decision.

## Consequences

### Positive

- **The reaction-latency gap closes for the account that matters.** The active
  account — the only one that can reach its usage ceiling *while active* — is
  re-observed every ~`2·poll_secs / N` instead of once per ~`poll_secs` sweep, so a
  fast climb is far less likely to cross the whole swap-away band between two
  observations. Verified in-band by the #367 hermetic regression test and the #365
  `late=` marker.
- **No new rate-limit exposure.** The per-tick spacing, aggregate request rate, and
  `poll_secs / N` per-source floor are unchanged, so the #80/#293 source burst does
  not reopen. The gap is closed at **zero** additional request cost.
- **No new concurrency.** The change is a longer schedule vector plus an unchanged
  divisor — no timer, task, or second poller — so the whole loop stays hermetically
  unit-testable over the existing `Clock`/`RosterPoller` seams, with no real clock or
  network.
- **Peers relax slightly, harmlessly.** Peer re-observation stretches to
  `2·poll_secs·(N-1)/N` (`< 2·poll_secs`); peers are only swap targets ranked by
  weekly reset, so a looser peer cadence costs nothing.

### Negative / trade-offs

- **The schedule vector grows to ~2N.** `build_poll_schedule` allocates up to
  `2·roster.len()` entries instead of ~N. Accepted: it is a small bounded `Vec<usize>`
  rebuilt per cycle, and holding the **divisor** at N is what preserves rate-neutrality.
- **The active tightens to `2·poll_secs / N`, not the `poll_secs / N` floor.** The 1:1
  interleave is a deliberate cap that leaves a 2× headroom above the floor (tightening
  further would push a peer past `2·poll_secs`). Accepted: `2·poll_secs / N` is
  sufficient to close the observed gap, and the floor is a hard per-source limit, not a
  target.
- **Small rosters benefit little.** With few accounts there are few ticks to interleave
  into (a single-account roster gains nothing). The documented fallback (a
  source-window-checked lower `poll_secs`, Decision 5) covers that case rather than the
  interleave. Accepted: small rosters have a proportionally longer window between polls
  anyway, and the fallback is available when genuinely needed.
- **Warm-up spans ~2·(N-1) ticks.** The first-cycle warm-up hold (#80) — during which
  the swap-away decision HOLDS on a partial reading set — now covers the longer
  schedule before it releases. Accepted: warm-up is a one-time startup cost that
  correctly refuses to act on incomplete readings.

## Related

- **Issues**: **#364** (this ADR). Umbrella **#363** (the reaction-latency gap this
  records the decision for — **open**). Paired fix **#366** (interleave the active in
  the poll schedule — **closed** at `dd20afc`). Measurement **#365** (`late=` swap
  marker — **closed**) and **#367** (hermetic in-band regression test — **closed**).
  Follow-ups: **#368** (reconsider the trigger headroom — **open**, gated on
  measurement) and **#369** (reactive fast-path — **open** question, assessed redundant
  after the interleave; see Alternatives 3). Prior art: **#80** (the staggered
  round-robin poll + warm-up this interleaves into — closed), **#293** (per-account
  rate-limit back-off — ADR-0009, closed), **#38** (jitter decorrelation each
  sub-interval inherits — closed), **#5** (per-account usage quota — closed), **#76**
  (the original poll cadence + back-off framing — closed).
- **Code**: `src/daemon.rs` — `build_poll_schedule` (the active interleave, ~L1811),
  `rotation_len` (the divisor held at N, ~L1885), `next_subinterval` (the `poll_secs/N`
  tick spacing, ~L3603), the poll-loop module doc on the source-scoped rate-limit
  window (~L17–29). `src/config.rs` — `DEFAULT_POLL_SECS` = 300,
  `default_poll_secs`.
- **Prior art (ADRs)**: **ADR-0009** (rate-limit back-off scoped per-account — the
  sibling rate-limit decision whose per-source, source-scoped-endpoint model this
  rate-neutrality argument rests on). **ADR-0008** (a shipped behavior-change ADR, the
  same record-what-shipped posture).
