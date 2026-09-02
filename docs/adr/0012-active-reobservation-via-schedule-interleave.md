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

**Amended 2026-09-02 (#1454) — Decision 4's interval was derived for a STATIC active; it was never
derived for a mid-cycle change of active.** *Mid-cycle* here means: while a schedule vector built by
`build_poll_schedule` is still being consumed, before `next_poll_index` reaches its end and rebuilds.

**The guarantee, restated so it holds across a change of active.** `2·poll_secs / N` binds **from the
moment an account is designated active, by any path — not from the start of the cycle in which that
designation happens.** This *extends* Decision 4's domain and revises nothing in it: for a static
active the interval and the 1:1-interleave cap are unchanged and still correct. **No mechanism in
the tree delivers this yet** — read it as the standard the code owes, not as a description of
current behaviour. Conformance is tracked at **#1452**, gated on the **#1451** oracle being
demonstrated RED against the pre-fix tree first, and on the **#1453** instrument, without which a
never-attempted poll emits nothing and a landed fix moves no readout.

*By any path* is meant literally, and the enumeration is wider than it looks. `record_swap`
(`src/daemon.rs`) is the **shared swap-commit path** — the autonomous swap-away reaches it, and so
do the operator's daemon-routed `swap` command (`perform_socket_swap`, `src/daemon/commands.rs`) and
the scrubbed-canonical recovery (`src/daemon/canonical.rs`). `adopt_manual_swap`
(`src/daemon/commands.rs`) is the out-of-band `use` seam. And a **third** path reaches neither:
`reconcile_canonical_change` (`src/daemon/canonical.rs`) drops `state.active` to `None` when an
out-of-band login repoints the canonical at a different roster account, and the top-of-tick resolve
in `tick` re-resolves it to the new index within the same tick. A mechanism that covers only the two
swap seams leaves that one carrying the whole defect; an enumeration of `poll_schedule` writes will
not find it, because it is an `active` write.

**Why the bound did not apply, rather than being violated.** Everything below reasons about one
active held fixed across a schedule's life. The premise is nowhere stated, because nothing here had
cause to state it: the question this record settles is *how often the active is re-read*, not *which
account is the active* while the schedule re-reading it is consumed. Decision 4's arithmetic is
therefore a statement about the account the vector was **built for** — and `next_poll_index` rebuilds
only at a cycle boundary, so on a mid-cycle change the two come apart. The interval goes on holding
for the *former* active, whose slots the vector still carries at every even index; the incoming
account holds at most its single peer slot, and once the cursor is past that slot it is not
scheduled again until the rebuild. An observation hole far beyond the bound was measured on
2026-09-01 (`docs/design/active-account-observation-continuity-solution-design.md` § 13 records the
figures and the scale they were taken on). Recording only the overrun would frame a gap in the
reasoning as a bug in the mechanism, and would leave the next reader deriving the same guarantee
over the same unexamined premise.

**The same shape sits in the code.** The one site that *does* invalidate the schedule —
`reconcile_roster` (`src/daemon/commands.rs`) — gives its reason as **index validity**: the schedule
held OLD roster indices. A change of active moves no index, so that reason does not reach it.
Neither artifact was wrong about the case it covered; both were silent about the one they did not.

**Two limits the restated guarantee is subject to, named here because it does not resolve either.**

- **It bounds the SCHEDULE, not the poll.** A scheduled account is still skipped while its
  per-account rate-limit window is open (`account_backing_off`, ADR-0009 / #293), and that predicate
  takes no `active` argument — unlike its ADR-0019 sibling `exhausted_slow_polling`, which re-checks
  the role. So an account that armed a peer-scoped window and is then designated active inherits it
  at the **peer** ceiling `POLL_BACKOFF_CAP`, not the tighter `ACTIVE_POLL_BACKOFF_CAP` that #453
  set for exactly this account. Designation alone does not clear it. Repairing that is a code
  decision this record does not take.
- **The peer bound below is derived over an UNINTERRUPTED traversal.** Decision 4's
  `2·poll_secs·(N-1)/N` and § Consequences → Positive's "peers relax slightly, harmlessly" both
  assume the cursor walks the vector to its end. A mechanism that re-derives the vector on every
  change of active does not preserve that assumption for free, and nothing in this ADR bounds peer
  coverage under repeated changes of active. **Not pre-ratified by this record**, whatever
  Decision 4 says about peers being *only* swap targets — that argument is about a *relaxed* cadence,
  not an unbounded one, and stale peer readings feed target ranking. Tracked at **#1455**.

**Decision 3 is carried forward intact, and it is what keeps a repair admissible.** The tick divisor
is `rotation_len()` — the count of DISTINCT rotation accounts taken from the roster, **not** the
schedule length — so `next_subinterval()` keeps consecutive ticks `poll_secs / N` apart. **This
amendment does not weaken that invariant**, and the weakening is what a reader should be alert for: a
wider guarantee invites closing the gap with a second poller or a shorter interval, and both stay
refused (Alternatives 1 and 2, and Decision 3's own "no new timer, async task, or concurrent
poller"). Because the divisor never reads the schedule vector, re-deriving that vector cannot move a
tick's timing, add a tick, or add a request. Note precisely what that buys and what it does not: it
holds the three quantities Decision 3 names — per-tick spacing, aggregate request rate, and the
`poll_secs / N` per-source floor — and it says nothing about how the requests are *distributed
across accounts*, which is the second limit above. The existing stagger locks are the gate on the
three, and they are expected to pass **unmodified**; a lock that needed editing to accommodate a
change would itself be evidence that change was not rate-neutral. Decisions 1 and 5 are likewise
unchanged: `poll_secs` is not lowered, and the source-window-checked small-roster fallback stays as
bounded as it was.

**What else inherits the static-active scope.** Decisions 1, 2, 3 and 5 stand in force, but Decision
2's restatement of the interval, Alternatives 3's ground for deferring **#369** as *"assessed
redundant"*, and § Consequences → Negative's "`2·poll_secs / N` is sufficient to close the observed
gap" were each derived under the same premise and are each scoped by this amendment, whether or not
they carry a note. Only Decision 4 and the first Positive consequence are annotated in place; treat
that as economy, not as a boundary.

**Amended, not superseded — and the choice is deliberate.** This directory reserves `Superseded` for
a decision that a later ADR *replaces* (§ Status vocabulary). Nothing here is replaced: the lever
chosen over a lower `poll_secs` is untouched, every Decision item remains in force, and Decision 4's
conclusion is correct **within** the domain it was derived for. What this amendment does is widen
that domain and name the premise that bounded it. A superseding record would have to restate the
whole interleave rationale in order to change one derivation's scope, and would put items that still
govern behind a banner that does not describe them. The house precedent for this shape — a decision that stands, with
a case it did not anticipate now recorded against it — is ADR-0006 (amended by #1053) and ADR-0020
(amended by #1123).

**Line-number citations repaired to symbols (#1454).** `build_poll_schedule`, `rotation_len` and
`next_subinterval` were cited by file-and-line; every one of those numbers had drifted, and a drifted
number still resolves, still looks like evidence and still reads as verified — the silent rot
`scripts/check-doc-citation-rot.sh` exists to stop. They are now cited by **symbol** alone, which
re-derives from any tree and cannot rot; refreshing the numbers would only re-arm the same failure.
This repairs *pointers*, not reasoning: it is the form § Conventions → Provenance already asks for
(`file` / symbol), no sentence's meaning changes, and every claim the numbers accompanied stands as
written.

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
its own window** (`src/daemon.rs` module doc, *One tick* step 2). A former poll-of-all *burst*
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
   (`src/daemon.rs`) emits `[active, p1, active, p2, …, active, p_{N-1}]` — the
   active account inserted before **each** enabled, non-quarantined peer — instead of
   one active slot per full sweep. The active account therefore re-observes roughly
   every **`2·poll_secs / N`** (~2 sub-intervals) instead of once per ~`poll_secs`
   sweep. Degenerate rosters stay valid: no peers → `[active]` alone; no active →
   peers only (nothing to interleave).

3. **The interleave is rate-neutral — this is the load-bearing invariant.** It
   lengthens only the schedule **vector** (to ~2N). The tick **divisor**
   `rotation_len()` (`src/daemon.rs`) still counts the **N distinct rotation
   accounts** taken from the roster — **not** the ~2N schedule length — so
   `next_subinterval()` (`src/daemon.rs`) keeps consecutive ticks `poll_secs / N`
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

   > **This interval was derived for a STATIC active — see § Status → Amended 2026-09-02
   > (#1454).** *The item above is left as written, per this directory's immutability
   > convention; this note records what is now known against it.* Every quantity here is
   > measured over a schedule built for one particular active, so the guarantee attaches
   > to the account the vector names rather than to whichever account is active while it
   > is consumed. When the active changes **mid-cycle** the two come apart, and this item
   > does not reach that case. The amendment widens the domain — the interval binds from
   > the moment an account is **designated** active, by any path — and leaves the number
   > and the 1:1-interleave cap exactly as stated. **No mechanism in the tree delivers the
   > widened form yet** (#1452); and the peer bound stated above is derived over an
   > uninterrupted traversal, which a conforming mechanism does not preserve for free.

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

  > **Scoped to a static active — see § Status → Amended 2026-09-02 (#1454).** Neither
  > witness named here reaches a **mid-cycle change of active**. The #367 lock
  > (`the_interleave_re_observes_the_active_in_band_and_swaps_before_the_ceiling`,
  > `src/daemon.rs`) drives the staggered loop only until the first swap fires and then
  > stops, so it never observes the window *after* the active changes. The #365 `late=`
  > marker grades the **outgoing** account's `session_pct` at swap time (`Event::Swap`,
  > `src/observability.rs`) — a different quantity from the **incoming** account's
  > first-sight latency, which it cannot bound. An observation hole far beyond the bound
  > was later measured in exactly that window. The gap this bullet claims to close *is*
  > closed for the case it was derived for; the amendment names the case it was not.

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
- **The 2026-09-02 amendment (#1454)**: **#1454** (this amendment — record that Decision 4
  was never derived for a mid-cycle change of active, and repair the drifted line
  citations). The conformance work it names: **#1452** (invalidate the poll schedule on
  every change of active — the mechanism the restated guarantee owes), gated on two
  prerequisites: **#1451** (pre-author the mid-cycle regression oracle, RED before any fix) and
  **#1453** (make an unobserved active visible — a never-attempted poll emits nothing today, so
  without it a landed fix moves no readout). Alongside them: **#1455** (the peer-coverage bound
  under repeated changes of active — and the cooldown it would lean on is cleared by a restart),
  **#1456** / **#1457** (target-reading staleness and panel-free surfacing — each blocked on an
  operator-owned threshold) and **#1458** (the tunables that would carry either — conditional on
  those two, not blocked). The issue states in the bullet above are as of 2026-07-09 and have not
  been refreshed; re-read them rather than trusting them.
- **Derivation**: `docs/requirements/active-account-observation-continuity.md` (R-1 for the
  restated guarantee, R-12 for this amendment) and
  `docs/design/active-account-observation-continuity-solution-design.md` (§ 4 for the
  mechanism and for the same reasoning-gap shape in the code, § 13 for the measurement and
  the scale it was taken on).
- **Code**: `src/daemon.rs` — `build_poll_schedule` (the active interleave),
  `rotation_len` (the divisor held at N), `next_subinterval` (the `poll_secs/N` tick
  spacing), `next_poll_index` (the cycle-boundary rebuild the 2026-09-02 amendment
  turns on), `record_swap` (the SHARED swap-commit path — autonomous, operator-routed and
  canonical-recovery swaps all reach it), and the poll-loop module doc's *One tick* step 2 on
  the source-scoped rate-limit window. `src/daemon/commands.rs` — `adopt_manual_swap` (the
  out-of-band `use` seam), `perform_socket_swap` (the operator's daemon-routed `swap`),
  `reconcile_roster` (the one site that already invalidates the schedule, for index validity).
  `src/daemon/canonical.rs` — `reconcile_canonical_change` (the third change-of-active path,
  which reaches neither swap seam). `src/config.rs` — `DEFAULT_POLL_SECS` = 300, `default_poll_secs`.
  Cited by **symbol**, never by line: a line number rots silently while still
  resolving (#1454, and `scripts/check-doc-citation-rot.sh`).
- **Prior art (ADRs)**: **ADR-0009** (rate-limit back-off scoped per-account — the
  sibling rate-limit decision whose per-source, source-scoped-endpoint model this
  rate-neutrality argument rests on). **ADR-0008** (a shipped behavior-change ADR, the
  same record-what-shipped posture).
