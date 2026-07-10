---
type: architecture-decision-record
number: 15
title: "Reactive on-401 refresh is unconditional; `[refresh].enabled` gates only proactive maintenance"
date: 2026-07-10
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0015: Reactive on-401 refresh is unconditional; `[refresh].enabled` gates only proactive maintenance

## Status

**Accepted** — 2026-07-10. Records the **#426** re-scope of what
`[refresh].enabled` gates, and the **consent posture** that re-scope crosses, so a
contributor does not later "re-unify" reactive recovery back under the toggle and
silently reopen the false-death window it closes.

This ADR records a **shipped** behavior change in `src/cli.rs` (the daemon wiring)
whose safety rests on **unchanged** `src/daemon.rs` guards. It **supersedes in
part** ADR-0007 — its **decision 2** ("no in-place active refresh") and **decision
3** ("hold pending the #262 spike"), which ADR-0007 itself flagged as revisitable
(its § Negative, "*decision 3 is a hold, not a permanent no … in-place active
refresh may be reconsidered and this ADR revisited or superseded*"). ADR-0007's
decisions **1** and **4** stand; it is not superseded as a whole.

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a
roster of accounts. `[refresh].enabled` (default `false`, unchanged here — that
default is **#409**'s separate call) historically gated **both** kinds of
credential upkeep at once:

- **Proactive maintenance** — refresh healthy tokens on a cadence so they never
  expire: the **#106** periodic sweep (`RefreshTick`, `src/refresh_tick.rs`) and
  the **#282** keep-warm of the active account. Legitimately opt-in: each spawns an
  isolated `claude -p` on a cadence with no external 401 provoking it.
- **Reactive recovery** — on a usage **401**, attempt **one** isolated
  refresh-then-retry **before** the 401 counts toward the **#42** dead-credential
  streak (**#162**, `should_refresh_retry` / `refresh_retry`, `src/daemon.rs`). A
  **correctness** path, not maintenance: it fires only in response to an observed
  401, and without it a recoverable credential is quarantined.

Conflating the two under one switch has a sharp failure mode with `[refresh]`
**off** (the default): a **parked** account whose ~8 h access token expires is
never refreshed, so its next poll 401-streaks it into quarantine — a **false 🔴**
("needs `claude /login`") even though its **refresh token is still valid** (`poke`
proves the refresh token is alive; the daemon simply is not allowed to use it).
`should_refresh_retry` short-circuits because `poll_refresh` is `None` — the
reactive seam was never wired. The recoverable credential dies holding the means of
its own recovery.

The reactive seam (**#162**) already carries the guards that make it safe on **any
account**; wiring it was gated only by the toggle. The open question this ADR
settles is **which concern the toggle should gate** — both (status quo), or only
proactive maintenance.

> This item is **complementary to #409**, not a substitute: #409 flips the
> *maintenance default* to on; this re-scopes what the toggle *gates* so reactive
> recovery runs even for an operator who keeps maintenance **off**. #409 is
> **held pending this item** and the `.enabled` default is **untouched** here.

## Decision

**The #162 reactive on-401 refresh-then-retry is hoisted OUT of the
`if refresh_enabled { … }` block, so `poll_refresh` is ALWAYS `Some`. The
`[refresh].enabled` toggle now gates ONLY the proactive maintenance — the #106
periodic sweep and the #282 keep-warm.**

1. **Wiring split (`src/cli.rs`).** `daemon.with_refresh_engine(…)` (the reactive
   `poll_refresh` seam) is now called **unconditionally**. `with_keep_warm_engine`
   (#282) **stays inside** `if refresh_enabled`, and `RefreshTick` still receives
   `refresh_enabled` and no-ops the periodic sweep when it is `false`. The daemon
   itself has **no knowledge** of `[refresh].enabled` on the reactive path — it
   keys purely off `poll_refresh.is_some()` — so hoisting the wiring is the whole
   change; the poll-loop code is untouched.

2. **The #253 guards travel with the engine, UNCHANGED.** The hoist adds no new
   safety reasoning — it relies on `should_refresh_retry`'s existing conjunction
   (`src/daemon.rs`): a wired seam, a 401, **not quarantined**, **once-per-episode**
   (`consec_401 == 0`, so no `claude -p` storm — cf. #408), **and the active-account
   exclusion** (`state.active != Some(i)`, resolved **token-first** at top-of-tick,
   #207). The isolated engine rotates the server-side refresh token but CAS-writes
   only the account's **STASH**, never the live canonical item every session reads
   — so it targets **parked** accounts only.

3. **The swap-race is safe by construction (council falsifier, proven by test).**
   The concern: a reactive refresh must not leave an account **promoted to active
   this tick** holding a torn canonical. It cannot, because (a) the refresh fires
   while the target is still **parked** (`state.active != Some(i)`), so no live
   session reads its token; (b) the swap runs **strictly after** the refresh in the
   single-threaded tick (`refresh_retry` at the poll seam, **then** `decide_action`
   — ADR-0001's `current_thread` runtime means no concurrency); and (c) the swap
   promotes **from that same stash** (`incoming = target.stash()`, read back in
   `record_swap`), so the canonical a session reads post-swap is exactly the token
   the refresh left in the stash. There is **no in-tick ordering** that promotes the
   account first and then refreshes its now-live canonical, and on any **later**
   tick the promoted account is the active one and is excluded outright. Locked by
   `a_reactive_refresh_of_a_swap_target_cannot_race_the_promotion` (`src/daemon.rs`).

4. **The active account's reactive backstop stays opt-in — deliberately.** The
   **#282** keep-warm reactive backstop (`should_keep_warm_retry`, the active-only
   complement of #162) **promotes to the live canonical**, so it stays behind
   `[refresh].enabled`. With `[refresh]` off, a dead **active** token still recovers
   via the **#42 emergency swap** to a live spare (ADR-0007 decision, in force),
   exactly as before — this ADR does not make the active account's *in-place*
   refresh unconditional, only the **parked**-account recovery.

5. **Consent posture — a ratified product decision.** Making reactive refresh
   unconditional means the daemon performs a **real OAuth exchange** (rotating the
   server-side refresh token) for a parked account **without the operator opting
   into `[refresh]`**. This is credential-**safe** (the #253 stash-only boundary of
   step 2: no live session is touched, no token is stranded), but it **crosses the
   consent line** the toggle currently draws — the operator did not ask for *any*
   `claude -p` OAuth exchange. The maintainer (this project's owner) has **ratified**
   this as a deliberate posture: reactive recovery is a **correctness** property,
   not maintenance, and the alternative the operator gets today — a parked account
   dying while holding a valid refresh token — is a worse outcome they did not
   consent to either. The heavier, live-canonical-rotating **proactive** paths
   remain strictly opt-in (step 4), so the toggle still governs every operation that
   rotates a **live** credential.

## Alternatives considered

1. **Option A — hoist reactive recovery out of the toggle** (chosen): `[refresh]`
   gates proactive maintenance only; reactive on-401 recovery is unconditional.
   - **Pros**: closes the false-🔴 for parked accounts at the default config; reuses
     the #162 guards verbatim (no new safety surface); keeps the live-canonical
     rotations (proactive sweep + keep-warm) opt-in.
   - **Cons**: crosses the consent line for an isolated OAuth exchange (step 5) —
     accepted as a ratified posture; a subtle split (one seam always wired, two
     gated) a contributor must not "tidy" back together.

2. **Option B — default `[refresh].enabled` to `true`** (#409, deferred, not
   chosen *here*): flip the maintenance default so parked tokens are kept warm
   proactively and never expire to a 401 in the first place.
   - **Pros**: a parked token never reaches the false-death window; one switch.
   - **Cons**: a **different** answer — it makes *proactive maintenance* the
     default (a cadence of `claude -p` spawns for every operator), and still leaves
     reactive recovery coupled to the toggle, so an operator who *turns maintenance
     off* is back to the false-🔴. Complementary to this ADR, not a replacement;
     tracked as #409.

3. **Option C — leave the coupling; document the toggle** (rejected): keep both
   concerns under `[refresh].enabled` and rely on the #138 status advisory to tell
   the operator their parked credentials will lapse.
   - **Why rejected**: it accepts a **recoverable credential dying** as normal
     operation and pushes the recovery burden onto the operator (`claude /login`)
     for a token the daemon could have refreshed itself. The advisory names the
     symptom; it does not fix the false death.

## Consequences

### Positive

- **A parked account with an expired access token but a live refresh token no
  longer false-dies at the default config.** Its next-poll 401 triggers one
  isolated refresh + re-poll before the #42 streak advances, exactly as it already
  did with `[refresh]` on — now regardless of the toggle. This is the #426 fix.
- **No new safety surface.** The hoist relies entirely on the existing #162/#253
  guards; the swap-race is proven safe by an added regression test and the
  single-threaded-tick + swap-promotes-from-stash invariants (ADR-0001, ADR-0003).
- **Live-credential rotations stay opt-in.** Every operation that rotates the
  **live** canonical token — the #106 sweep, the #282 keep-warm (proactive and its
  active-401 backstop) — remains behind `[refresh].enabled`. The toggle still
  governs consent for anything a live session depends on.
- **Un-blocks #409 cleanly.** With reactive recovery decoupled, #409's "default
  maintenance on" is a pure product-default call, no longer entangled with the
  correctness path.

### Negative / trade-offs

- **An isolated OAuth exchange now runs without an explicit `[refresh]` opt-in.**
  A parked account's 401 spawns a `claude -p` refresh even with the toggle off.
  Accepted as a **ratified** posture (§ Decision step 5): it is credential-safe
  (#253 stash-only) and strictly better than the false death it replaces; the
  heavier proactive paths stay opt-in.
- **The toggle's meaning narrowed.** `[refresh].enabled` no longer means "the
  daemon will/won't use refresh tokens at all" — it means "the daemon will/won't
  perform **proactive maintenance**". The #138 status advisory keys off
  `config.refresh.enabled` (via `with_refresh_enabled`, unchanged) and so now
  describes the **proactive** gate specifically; its "non-active credentials lapse
  without maintenance" framing is still accurate for *proactive* upkeep, but a
  parked 401 is now caught reactively. Accepted; a future advisory-wording pass
  (#138 territory) can sharpen it.
- **A split a contributor could mis-tidy.** One refresh seam is always wired while
  two siblings are gated, which reads as an inconsistency inviting "cleanup".
  Guarded by an in-code comment at the wiring site (`src/cli.rs`) and this ADR.

## Related

- Issues: **#426** (this ADR — the reactive-vs-proactive re-scope). Complementary:
  **#409** (default `[refresh].enabled` true — held pending this item, default
  untouched here). Prior art the guards come from: **#162** (the reactive
  refresh-then-retry this makes unconditional), **#253** (the active-account
  exclusion / stash-only boundary that makes it safe), **#207** (token-first active
  resolution), **#282** (the keep-warm — proactive + active-401 backstop — that
  **shipped** in-place active refresh, superseding ADR-0007 decision 2), **#262**
  (the refresh-token-rotation spike, now resolved — "server rotates" — retiring
  ADR-0007 decision 3's hold), **#106/#105** (the periodic sweep that stays gated),
  **#408** (the once-per-episode storm guard the reactive path shares), **#42** (the
  dead-credential streak / emergency swap the active account still relies on),
  **#138** (the status advisory that keys off the toggle), **#15** (diagnostics stay
  secret-free — no token material on any refresh path).
- Code: `src/cli.rs` — `with_refresh_engine` now hoisted out of the
  `if refresh_enabled` block (the reactive seam, always wired);
  `with_keep_warm_engine` + `RefreshTick` still gated. `src/daemon.rs` —
  `should_refresh_retry` (the reactive guard, active-excluded, once-per-episode),
  `refresh_retry` (stash-only re-poll), `should_keep_warm_retry` (active-only #282
  complement, stays gated), `record_swap` (the swap promotes from the stash), and
  the falsifier test `a_reactive_refresh_of_a_swap_target_cannot_race_the_promotion`.
- ADR-0007 (**superseded in part**: decision 2 "no in-place active refresh" — #282
  shipped it; decision 3's hold pending #262 — resolved; decisions 1 and 4 stand);
  ADR-0003 (the no-torn-swap invariant the swap-race proof rests on); ADR-0001 (the
  single-threaded `current_thread` runtime that serializes refresh-then-swap within
  a tick); ADR-0009 / ADR-0014 (the per-account / tick back-offs that bound the
  reactive and proactive refresh cadences).
