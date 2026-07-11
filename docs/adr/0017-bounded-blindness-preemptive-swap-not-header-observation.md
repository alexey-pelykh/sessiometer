---
type: architecture-decision-record
number: 17
title: "Bounded-blindness preemptive swap-away, not header-based active-observation"
date: 2026-07-11
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0017: Bounded-blindness preemptive swap-away, not header-based active-observation

## Status

**Accepted** — 2026-07-11. Records the **#452** decision to swap the active
account away **preemptively on bounded blindness** — spending the *swap* lever when
fresh near-limit active usage is provably unavailable — and, importantly, to
**reject header-based active-observation** as the means of getting that freshness,
so the "just read the rate-limit headers" idea is not re-litigated each time the
blindness bites.

Unlike ADR-0009 and ADR-0012 — which record **shipped** behaviour — this ADR
records a decision whose **implementation is pending** (tracked in **#452**), with
interim constants `T=300s` / `risk_band=65%` to be finalised by the **#451**
confirmation gate. It **supersedes nothing**: it *affirms* [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)
(active-observation stays favoured), *extends the consequence* of
[ADR-0009](0009-rate-limit-backoff-per-account.md) for the active account, and
*cross-references* [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md).
No existing swap-eligibility predicate moves; the reactive and emergency paths are
untouched.

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a
roster, deciding swaps on each account's **last-known** usage reading. Active
observation is deliberately favoured over the peers — [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)
interleaves the active account into the poll schedule so it is re-read more often
than accounts that are merely swap targets. The active account is the only one that
can reach its usage ceiling *while active*, so it is the only one whose observation
latency matters.

**The bounded-blindness gap (S1 spike, 2026-07-10→11).** The active account `429`'d
on its `/oauth/usage` poll at session usage `0.68`, went **blind for 755s**
(`Retry-After`-dominated), and Claude Code burned it to `0.98` before any fresh
reading returned. The reactive `session_trigger=95` swap **never fired** — it keys
off an *observation*, and no observation crossed the trigger because the account was
blind across the entire swap-away band.

Two accepted decisions explain *why* the blindness is bounded but unpierceable by
more observation:

- **Per-account back-off ([ADR-0009](0009-rate-limit-backoff-per-account.md)).** A
  `429` on the active account's usage poll — correctly — backs off **only its own**
  next poll (`poll_backoff_until`, never below the server `Retry-After`), and the
  account **carries its last reading**. That per-account scoping is right and
  unchanged; its *consequence for the active account* is a blindness window whose
  length the server dictates.
- **Interleave, not harder polling ([ADR-0012](0012-active-reobservation-via-schedule-interleave.md)).**
  Interleaving re-reads the active account more often, but it cannot re-read a
  *backing-off* account — and lowering `poll_secs` to poll harder was already
  rejected there (it re-opens the **#80** burst-`429` exposure). More observation
  budget does not pierce a `Retry-After` wall.

**Fresh near-limit active usage is provably unavailable by any safe local means:**

1. **Claude Code does not persist its usage.** The `anthropic-ratelimit-unified-5h-*`
   headers carry exact usage on every inference response, but Claude Code stashes
   nothing locally to harvest (verified by a local sweep 2026-07-11; upstream FR
   [anthropics/claude-code#55333](https://github.com/anthropics/claude-code/issues/55333)).
2. **Eliciting the headers means impersonating Claude Code.** The only way to draw a
   fresh header is a minimal-inference probe using the subscription OAuth tokens —
   i.e. impersonating Claude Code — which courts an **account ban**.
3. **`/oauth/usage` self-blinds under pressure.** The account's own usage endpoint
   `429`s exactly when polled hard enough to stay fresh — the failure mode above.

So the daemon cannot buy a fresh reading in time. The decision spends a different
lever: swap the active account **away** *before* the missing reading would have
tripped the trigger, keyed on the retained pre-blind anchor.

## Decision

**Add a gated preemptive swap-away path — separate from `emergency_swap` — that
fires on *bounded blindness* of the active account, keyed on the retained pre-blind
anchor, and do NOT pursue header-based observation to get a fresher reading.**

The path fires when **all** hold:

- `blind_elapsed > T` — the active account's last reading is stale beyond the
  threshold, **and**
- `last_good.session >= risk_band` — the retained pre-blind anchor (**#450**)
  was already near the band, **and**
- **a viable swap target exists** — a peer below `target_max_session_usage`
  ([ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md)).

Interim constants `T=300s`, `risk_band=65%` (to be finalised by **#451**). New
tunables `session_blind_swap_secs` (`=T`, default `300`) and
`session_blind_risk_band` (default `65`), hand-emitted and cross-field-validated
(`target_max_session_usage <= effective session trigger`) per
[ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md). Setting
`session_blind_swap_secs` arbitrarily high **disables** the path — a config
kill-switch.

The path honours the **#369** cautions on a reactive fast-path:

- **Never fires on a `None`/missing reading.** It keys off the retained `last_good`
  anchor (**#450**) — *not* the `429`, *not* `last_readings` — so a
  genuinely-unknown account produces no spurious swap.
- **Not `emergency_swap`.** A separate *availability* path: it does not raise the
  target gate to infinity, does not bypass cooldown, and preserves the
  dead-vs-exhausted model (**#42**).
- **Respects the no-torn-swap invariant
  ([ADR-0003](0003-no-torn-swap-invariant.md)).**

## Alternatives considered

1. **Reactive-only — wait for `session_trigger=95` to observe the near-limit active
   account** (status quo, rejected).
   - **Why rejected**: the S1 spike proved the reactive trigger cannot fire through
     a blindness window. The reading that would trip `session_trigger=95` never
     arrives in time — the account `429`s, goes `Retry-After`-blind, and Claude Code
     burns it to exhaustion before any observation returns. Reacting to an
     observation that will not come is a guaranteed late swap.

2. **Header-based active-observation — read the `anthropic-ratelimit-unified-5h-*`
   headers** (rejected; recorded explicitly so it is not re-proposed).
   - The headers carry the **exact** usage on every inference response, which is
     genuinely fresher than anything `/oauth/usage` can offer near the limit. Two
     **independent** disqualifiers rule it out:
     - **(a) There is nothing to harvest.** Claude Code does not persist the
       headers — a local sweep (2026-07-11) found no stash; the upstream FR to
       persist them ([anthropics/claude-code#55333](https://github.com/anthropics/claude-code/issues/55333))
       is open. Passive reading is impossible.
     - **(b) Eliciting them is impersonation.** The only way to draw a fresh header
       is a minimal-inference probe with the subscription OAuth tokens — i.e.
       impersonating Claude Code. For a **fleet-health** tool whose entire purpose is
       to keep accounts alive, courting an account ban to read a number is
       self-defeating and categorically disqualifying.
   - Either disqualifier alone is fatal; together they close the path.

3. **Escalate `/oauth/usage` polling — lower `poll_secs` / poll the active account
   harder to stay fresh** (rejected).
   - **Why rejected**: `/oauth/usage` **self-blinds** exactly when polled hard
     enough to stay fresh (the `429` *is* the failure), and lowering `poll_secs`
     globally was already rejected by
     [ADR-0012](0012-active-reobservation-via-schedule-interleave.md) — it re-opens
     the **#80** burst-`429` exposure the staggered round-robin exists to prevent.
     Polling harder buys **blindness**, not freshness.

4. **Preemptive swap-away on bounded blindness — spend the swap lever, not the
   observation lever** (chosen).
   - **Pros**: fires *before* self-exhaustion (fake-clock replay of the S1 trace
     swaps at ~`0.87–0.93`, before `0.98`); no impersonation and no account-ban
     risk; adds no new burst exposure; config-gated and kill-switchable; keys off
     the retained `last_good` anchor, so no spurious swap on a missing reading.
   - **Cons**: acts on a **stale** pre-blind anchor rather than fresh truth — a
     deliberate trade, since the fresh reading is provably unavailable. A **static**
     `risk_band` may over- or under-trigger; mitigated by treating the constants as
     interim (finalised by **#451**) and deferring a velocity-projection arm to
     **#455**.

## Consequences

### Positive

- **The active account swaps away before it self-exhausts**, closing the
  reaction-latency gap (**#363**) that
  [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)'s interleave
  alone cannot close through a `Retry-After` blindness window.
- **No account-ban risk.** The tool never impersonates Claude Code: it reads only
  `/oauth/usage` (the account's own endpoint) and acts on the retained anchor. This
  is the load-bearing reason header-observation is rejected, not merely deferred.
- **Bounded, gated, reversible.** Separate from `emergency_swap`; respects
  no-torn-swap ([ADR-0003](0003-no-torn-swap-invariant.md)); honours the
  swap-target reserve (`target_max_session_usage`,
  [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md)) because the
  new path is **not** emergency and so does **not** take ADR-0013's emergency
  exemption; disabled by a single config value.
- **The rejected header path is recorded**, so "just read the headers" is not
  re-proposed each time the blindness recurs.

### Negative / trade-offs

- **Acts on a stale pre-blind anchor, not fresh truth.** It can swap away an account
  that (unobserved) recovered, spending a swap it did not strictly need. Bounded by
  `risk_band` (only near-band anchors qualify) and by requiring a viable target.
- **Interim constants are not yet empirically tuned.** `T=300s` / `risk_band=65%`
  await **#451**; a velocity-projection arm (`last_good + rate_preblind ×
  blind_elapsed >= trigger`) is deferred to **#455** pending SLI evidence. Wrong
  constants over- or under-trigger; the kill-switch (`session_blind_swap_secs` set
  high) is the escape hatch.
- **Implementation is pending (#452).** This ADR records the decision ahead of the
  code, so until #452 lands the daemon still exhibits the S1 blindness. The record
  exists to lock the rationale — especially the header-path rejection — before the
  build, not to claim it is shipped.

## Related

- Issues: **#454** (this ADR). **#452** (the design recorded here — the
  bounded-blindness preemptive swap; implementation tracked there). **#451** (the
  premise-confirmation + constants finalisation gate for `T` / `risk_band`).
  **#450** (the retained `last_good` pre-blind anchor the path keys off). **#363**
  (the reaction-latency umbrella). **#369** (the reactive fast-path open question
  whose cautions this honours). **#42** (the dead-vs-exhausted model the separate
  availability path preserves). **#455** (the deferred velocity-projection arm,
  gated on SLIs). **#80** (the burst-`429` exposure that rules out harder polling).
  [anthropics/claude-code#55333](https://github.com/anthropics/claude-code/issues/55333)
  (upstream FR: Claude Code does not persist its usage headers). **#15**
  (diagnostics stay secret-free — the swap keys off the account's own usage and
  label handles, never a token or email).
- Code (pending **#452**): the new gated path lands as a peer of `emergency_swap` in
  `src/daemon.rs`; the new tunables `session_blind_swap_secs` /
  `session_blind_risk_band` in `src/config.rs`, hand-emitted and cross-field-validated
  (`target_max_session_usage <= effective session trigger`) per
  [ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md). `/oauth/usage` remains
  the **only** usage source — no header probe is added.
- ADRs: [ADR-0009](0009-rate-limit-backoff-per-account.md) (per-account `429`
  back-off — **extended** here: the same per-account scoping that correctly bounds a
  throttle is what leaves the *active* account carrying a stale reading = bounded
  blindness; the per-account decision itself is unchanged).
  [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)
  (active-observation via interleave — **affirmed**: the active-observation budget
  stays favoured; the preemptive swap covers only the residual the interleave cannot
  observe through). [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md)
  (`target_max_session_usage` swap-target reserve — **x-ref**: the "viable target"
  condition honours the reserve; the new path is **not** emergency, so it does
  **not** take ADR-0013's emergency exemption).
  [ADR-0003](0003-no-torn-swap-invariant.md) (no-torn-swap invariant — respected).
  [ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md) (config hand-emit +
  cross-field validation — the new tunables follow it). **None superseded.**
