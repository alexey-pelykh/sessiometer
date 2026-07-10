---
type: architecture-decision-record
number: 14
title: "Refresh error back-off is tick-owned, not on `AccountHealth`"
date: 2026-07-10
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0014: Refresh error back-off is tick-owned, not on `AccountHealth`

## Status

**Accepted** — 2026-07-10. Records the design behind the **#408** refresh-error
back-off, so a contributor does not later "unify" it onto `AccountHealth`
alongside the poll-path back-off and silently re-couple the sweep's timing to the
daemon's clock.

Like ADR-0009 (the sibling **poll-path** per-account back-off), this ADR records
a **shipped** behavior change — here in `src/refresh_tick.rs`, not `src/daemon.rs`
— and that split of homes is exactly the decision recorded.

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a
roster of accounts. Two independent loops touch each account:

- The **poll loop** (`src/daemon.rs`) reads each account's usage endpoint on a
  staggered cadence. Its rate-limit / transient back-off is **per-account**, held
  on `AccountHealth` (`poll_backoff_streak` / `poll_backoff_until`) — **ADR-0009**.
- The **refresh sweep** (`RefreshTick`, #105/#106, `src/refresh_tick.rs`) mints a
  fresh token for a parked account by spawning an isolated `claude -p` (~40 s) on a
  copy of its stash. #105 deliberately gave the tick **its own roster copy and its
  own `Clock`**, decoupled from the daemon's: the run loop threads nothing from the
  daemon into the sweep — "*the tick owns its own roster copy + clock, so the sweep
  below needs nothing from it*" (`src/daemon/run_loop.rs:225`).

The refresh path had **no back-off on the error path**. On a sustained failure —
issue **#375**'s stale-`claude`-binary burst is the archetype — every cycle
re-spawned a doomed `claude -p`. The cadence is normally `cadence_secs` (3600 s),
but under a **recovery-pending** roster (a quarantined account the sweep could
restore) the tick becomes **due within the idle floor** so the restore is not
delayed a whole hour (#280). That idle floor defaults to **60 s**
(`idle_after_secs`), so a persistent error retried at **~1 refresh/min/account** —
a `claude -p` spawn storm (a 3 h incident spawned **374** subprocesses). #378
added a systemic-failure **detector** (`DEFAULT_REFRESH_SYSTEMIC_FAILURE_N`) and
#377 a `reason=` sub-class on the error event, but **neither throttles** — #408 is
the missing guard.

The poll path (ADR-0009) is the precedent for the *mechanism* — per-account
exponential streak, monotonic deadline, cleared on the first non-signal outcome.
The open question this ADR settles is **where the state lives**: reuse the
existing per-account `AccountHealth` home (Option B), or add a new home on the
tick (Option A)?

> This item is the **gate for #409** (defaulting `[refresh].enabled` true): a
> default-on refresh must not be able to storm. #408 adds only the throttle; the
> `.enabled` default stays #409's decision and is untouched here.

## Decision

**The refresh error back-off lives on the tick (`RefreshTick`), not on
`AccountHealth`.** Back-off is *refresh-timing* state, and #105 made refresh
timing the tick's own concern — so the state belongs where the timing lives.

1. **A per-account back-off vector on the tick.**
   `refresh_backoff: Vec<Option<RefreshBackoff>>` (`src/refresh_tick.rs:217`),
   sized to `roster.len()` at construction and indexed positionally like the poll
   path — the roster is fixed for the daemon's life, so no pruning. `RefreshBackoff`
   carries `streak: u32` and a monotonic `until: Instant` on the tick's **own**
   `Clock` (`src/refresh_tick.rs:89`) — the same `until`-deadline idiom ADR-0009
   uses, but on the tick's clock rather than the daemon's.

2. **`run_sweep` skips a backing-off account EARLY.** The skip sits **before**
   `stored_expires_at` (a `security` keychain subprocess, ADR-0002) **and before**
   the quarantine-restore bypass, so a backing-off account costs **neither a
   `claude -p` spawn nor a keychain read** (`src/refresh_tick.rs:359`): if
   `now < backoff.until`, `continue`. It is a full skip — no event, no observation
   — matching the exclusion skip already in the loop.

3. **Widen on `Error`, clear on anything else.** After the classified outcome, an
   `Error` advances the account's streak and arms
   `until = now + refresh_backoff_delay(streak, base)` — `base × 2^min(streak,
   REFRESH_BACKOFF_MAX_SHIFT=6)` clamped to `REFRESH_BACKOFF_CAP=3600 s`
   (`src/refresh_tick.rs:110`). **Any** non-error outcome (refreshed / no-change /
   dead / cas-discarded) clears the entry. `base` is the sweep's own idle floor
   `idle_after()` **floored at `REFRESH_BACKOFF_MIN_BASE=60 s`** — robust when
   `idle_after_secs` is its valid `0` (`src/refresh_tick.rs:421`).

4. **No `Retry-After`.** Unlike the poll path, a `claude -p` spawn returns **no
   server-advised wait** — there is no `429`/`Retry-After` to honour — so the
   refresh back-off is purely the self-capped exponential. The armed seconds are
   surfaced as `backoff_secs=<n>` on the `event=refresh` **error** line
   (`src/observability.rs:536`), the widening mirror of the poll path's
   `backoff_secs` on `diag=tick`.

5. **Defer-and-apply keeps `run_sweep` borrow-clean.** `run_sweep(&self)` cannot
   mutate `self.refresh_backoff` while iterating `&self.roster` and awaiting
   `engine.refresh`, so it **emits deltas** — `(outcome, Vec<(usize,
   Option<RefreshBackoff>)>)` (`src/refresh_tick.rs:473`) — and `sweep(&mut self)`
   applies them after the loop (`src/refresh_tick.rs:534`). This mirrors the
   existing deferral of the `last_refresh` / restore writes.

6. **`recovery_pending` stays back-off-UNAWARE — deliberately.** The tight
   idle-floor wakes a recovery-pending roster earns (#280) are exactly the
   **re-check clock** the sub-cadence back-off needs; the step-2 skip makes those
   wakes **spawn-free**. `recovery_pending` therefore still gates on what the sweep
   *would restore*, not on whether an account is backing off — the divergence is
   load-bearing and carries an in-code comment warning not to "fix" it
   (`src/refresh_tick.rs:490`).

## Alternatives considered

1. **Option A — tick-owned back-off** (chosen): the state and the skip live on
   `RefreshTick`, on the tick's own clock.
   - **Pros**: keeps refresh timing wholly inside the component #105 made its
     owner; the run loop still threads **nothing** from the daemon into the sweep
     (`run_loop.rs:225` stays true); the whole machine is unit-testable over the
     tick's `Clock` seam with **no real clock or network**, like the existing sweep
     tests.
   - **Cons**: a **new** back-off home distinct from the poll path's — two
     structurally-similar mechanisms in two files. Accepted: they are genuinely
     different timers (poll cadence vs refresh cadence) on different clocks.

2. **Option B — daemon-owned back-off on `AccountHealth`** (rejected): reuse the
   ADR-0009 home — a `refresh_backoff_streak` / `refresh_backoff_until` pair beside
   the poll fields — and pass the decision into the sweep as a third parameter,
   folding the outcome back in a post-idle daemon step.
   - **Pros**: one back-off "home" for both loops; visually unifies with ADR-0009.
   - **Cons**: it **re-couples the sweep's timing to the daemon's clock** — the
     exact decoupling #105 severed (`run_loop.rs:225`). The tick would need the
     daemon's `AccountHealth` to decide whether to skip, so the run loop must thread
     health state *into* the sweep and the sweep's outcome *back out* — splitting
     one state machine across a **third sweep parameter plus a post-idle fold**. It
     also invites a later misuse: a *refresh-throttle* field sitting on
     `AccountHealth` reads as fair game for an **activation / health** decision it
     has no business influencing (the health machine drives quarantine and swap
     eligibility, ADR-0007).
   - **Why rejected**: it trades the #105 decoupling invariant and a clean
     single-owner state machine for a cosmetic "one home" that the two clocks make
     false anyway. The poll and refresh timers are independent; their back-offs
     should be too.

## Consequences

### Positive

- **A sustained refresh failure can no longer storm.** A backing-off account is
  skipped whole — no `claude -p`, no keychain read — so a persistent error climbs
  from the 60 s floor to the ~1 h cap in a handful of doublings instead of
  re-spawning at ~1/min/account. The #408 incident shape (374 spawns in 3 h) is
  bounded to a handful.
- **The #105 decoupling invariant is preserved.** The sweep still needs nothing
  from the daemon; `run_loop.rs:225` remains literally true. A future contributor
  reading the run loop sees no health state threaded into the sweep.
- **Per-account and self-clearing, like the poll path.** The streak is scoped to
  one account and cleared by its first clean refresh, so one broken account never
  slows another's upkeep, and recovery needs no manual reset.
- **Fully unit-testable over the tick's `Clock`.** The skip, the widen, the clear,
  the per-account scoping, and the elapse-then-reattempt are all exercised with no
  real clock or network (`src/refresh_tick.rs` tests), matching the sweep's
  existing hermetic discipline.

### Negative / trade-offs

- **Two back-off mechanisms to keep in mind.** The poll back-off (`AccountHealth`,
  `src/daemon.rs`) and the refresh back-off (`RefreshTick`,
  `src/refresh_tick.rs`) are structurally similar but deliberately separate.
  Accepted: they time different loops on different clocks; unifying them would
  re-couple exactly what #105 decoupled (Option B).
- **No server signal to honour.** The refresh back-off cannot mirror the poll
  path's `Retry-After`-as-minimum, because a `claude -p` spawn advises none; the
  wait is purely the self-capped exponential. Accepted: there is no signal to
  honour, and the cap already bounds the worst case.
- **A recovery-pending roster still wakes at the idle floor.** The tight wakes are
  unchanged — only the *spawn* is suppressed by the skip. Accepted: the wake is
  the back-off's re-check clock and costs nothing but a monotonic-clock comparison
  (§ Decision step 6).
- **Systemic-failure (#378) detection is delayed, not defeated.** A fully
  backing-off sweep emits no observation, so the #378 classifier reads `NoSignal`
  — which is *neutral* (`src/systemic_refresh.rs:118`: it neither advances nor
  clears the streak), so the throttle can never fabricate a recovery that masks a
  genuine outage. But because the erroring re-attempts now fire on the back-off
  cadence (60 s → … → 3600 s) rather than every idle-floor wake, the consecutive
  all-error streak climbs to the threshold N more *slowly* — detection latency now
  tracks the back-off cadence, not the idle floor. Accepted: it still fires within
  minutes at the default N (vs the ~4.5 h blind window #378 was created to close),
  and a storming-but-instantly-detected failure is strictly worse than a
  throttled-but-slightly-later-detected one. Locked by
  `a_fully_backed_off_sweep_reads_as_no_signal_and_cannot_mask_a_systemic_failure`.

## Related

- Issues: **#408** (this ADR — the refresh-error back-off). Sibling: **#293** /
  **ADR-0009** (the poll-path per-account back-off this mirrors — closed), **#294**
  (the `Retry-After` cap — the refresh path has no server signal to cap). Root
  cause the back-off guards: **#375** (stale `claude` binary burst — fixed
  2026-07-09). Adjacent refresh-observability, **not** throttles: **#378** (the
  systemic-failure *detector*), **#377** (the `reason=` error sub-class the
  `backoff_secs=` field trails). **Gate for #409** (default `[refresh].enabled`
  true — untouched here). Prior art: **#105/#106** (the refresh tick that owns its
  own roster copy + clock), **#280** (the recovery-pending idle-floor wake this
  back-off re-checks on), **#260** (the absolutely-anchored idle floor `base`
  derives from).
- Code: `src/refresh_tick.rs` — `RefreshBackoff` (~L89), `refresh_backoff_delay`
  (~L110), `REFRESH_BACKOFF_MIN_BASE` = 60 s (~L66), `REFRESH_BACKOFF_MAX_SHIFT` = 6
  (~L72), `REFRESH_BACKOFF_CAP` = 3600 s (~L79), the `refresh_backoff` vector
  (~L217), the early skip in `run_sweep` (~L359), the widen/clear fold (~L415), the
  emitted deltas (~L473), the apply in `sweep` (~L534), the deliberately
  back-off-unaware `recovery_pending` comment (~L490). `src/daemon/run_loop.rs` —
  the #105 decoupling invariant the placement preserves (~L225). `src/observability.rs`
  — the `backoff_secs=` field on the `event=refresh` error line (~L536, rendered
  ~L805).
- ADR-0009 (the poll-path per-account back-off whose *mechanism* this mirrors and
  whose *home* this deliberately does **not** share); ADR-0007 (the `AccountHealth`
  health machine Option B would have overloaded); ADR-0002 (the `security`-CLI
  keychain read the early skip avoids).
