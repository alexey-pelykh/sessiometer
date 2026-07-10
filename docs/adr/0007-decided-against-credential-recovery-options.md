---
type: architecture-decision-record
number: 7
title: "Decided-against credential-recovery options for dead accounts"
date: 2026-07-03
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0007: Decided-against credential-recovery options for dead accounts

## Status

**Accepted** — 2026-07-03. Records four credential-recovery mechanisms a
maintainer-convened **council** (on auto-reviving dead accounts) **decided
against**, together with the recovery design they were weighed against, so a
contributor does not re-litigate them.

Unlike ADR-0001–0005 (each records a choice or invariant *in force*), this ADR
records a set of deliberate **non-decisions** — mechanisms considered and
rejected. It lands **no code change**; it consolidates rationale that otherwise
survives only in issue threads and `src/daemon.rs` doc-comments.

> **Superseded in part by [ADR-0015](0015-reactive-refresh-unconditional-proactive-gated.md)** (2026-07-10).
> **Decision 2** ("no auto-revive of the ACTIVE account in place") and **decision
> 3** (the #253 exclusion "hold pending the #262 spike") — which § Negative below
> already flagged as revisitable — are now retired: **#282** shipped in-place
> active refresh (keep-warm), and **#262** resolved that Anthropic's endpoint
> **rotates** the refresh token on exchange. **Decisions 1 and 4 stand**; this ADR
> is not superseded as a whole.

## Context

sessiometer keeps one live Claude Code credential active by rotating across a
roster of accounts. An account whose stored token is rejected `monitor_401_n`
times in a row is declared **DEAD** and **quarantined**: the daemon stops polling
and selecting it until it recovers (`AccountHealth.consec_401`,
`AccountHealth.quarantined`; `src/daemon.rs:806`). The health machine is
**edge-triggered and poll-cycle-counted, not wall-clock** — `consec_401` advances
on a 401 and resets on any non-401 outcome — so a quarantined account revives only
through one of the rails below, never on the passage of time alone.

Two recovery rails already exist, plus one escape:

- **The #106 quarantine-restore cadence.** The periodic refresh sweep re-probes
  quarantined / parked accounts on a **fixed cadence** and restores one whose
  token answers again (`Event::CredentialRestored`). This is the designated home
  for *revival timing*. *(Its starvation bug — quarantined accounts never actually
  getting swept — is tracked separately as **#260**.)*
- **Spontaneous revival of a stuck active account.** A dead **active** account
  with no viable swap target stays active and keeps being re-probed;
  `monitor_recovery_m` consecutive `Live` polls un-quarantine it in place
  (`AccountHealth.recovery_successes`; `src/daemon.rs:825`).
- **`emergency_swap` (the live-spare escape).** When a dead active account *does*
  have a live spare, `emergency_swap` rotates to it immediately — bypassing the
  swap-away trigger and cooldown — demoting the dead account to parked, where the
  #106 cadence later revives it (`src/daemon.rs:2343`).

Against this backdrop the council asked whether the daemon should add **more
aggressive auto-revival** — a wall-clock timer, or an in-place refresh of the
active account's own dead token. It decided **no**, on four specific points.

## Decision

Credential recovery **stays on the existing rails**: the fixed-cadence #106 sweep
for quarantined / parked accounts, `emergency_swap` (demote-then-#106-revive) for
a dead active *with* a live spare, and spontaneous re-probe revival for a dead
active *without* one. The daemon adds **no** further auto-revival machinery. All
four options below were **decided against**:

1. **No wall-clock "auto-revive after dead ≥ N seconds" timer.** It is an
   impedance mismatch with the health machine, which is edge-triggered and counts
   poll cycles (`consec_401`, `recovery_successes`), not wall-clock. At the
   `poll_secs` cadence a *sub-cadence* timer would fire with **no new
   information**; and pointed at a dead-refresh-token account it degenerates into a
   futile isolated-`claude -p` retry storm. Revival timing belongs on the (fixed)
   **#106** quarantine-restore cadence instead.

2. **No auto-revive of the ACTIVE account in place.** Per **#253**, an isolated
   refresh **rotates the refresh token server-side** and CAS-writes only to the
   stash — breaking concurrent live sessions and stranding the fresh token. The
   design's answer for a dead *active* account is `emergency_swap` (demote to
   parked), after which **#106** revives it.

3. **Do not touch the #253 active-account refresh exclusion** — it stays in place
   **pending the spike (#262)** on whether Anthropic's token endpoint rotates /
   invalidates the old refresh token on exchange. Until that behavior is known,
   removing the exclusion risks exactly the session-orphaning #253 fixed.

4. **`ActiveDeadNoTarget` requires a manual `claude /login` by design.** A dead
   active account with **no live spare** cannot `emergency_swap` (there is no
   viable target) and cannot be safely refreshed in place (decision 2). The daemon
   holds on the dead active and emits the durable "needs re-login" status; a human
   re-login is the only safe recovery (`src/daemon.rs:2375`).

## Alternatives considered

1. **A wall-clock revival timer** (the alternative to decision 1) — revive any
   dead account once it has been dead for ≥ N seconds.
   - **Pros**: superficially simple; bounds worst-case downtime by a tunable
     constant rather than by a poll cadence.
   - **Cons**: the health machine carries **no wall-clock state** — it counts poll
     cycles, so a timer bolts a second, unsynchronized clock onto an
     edge-triggered machine. Firing between polls yields **no new signal** (nothing
     was re-probed), and on a dead *refresh* token each fire is a fresh, isolated
     `claude -p` attempt — a retry **storm** with no back-pressure.
   - **Why not**: the #106 cadence already owns revival timing on the *same* clock
     the poller uses. If revival is too slow, the lever is #106's starvation bug
     (**#260**), not a competing timer.

2. **In-place refresh of the active account's dead token** (the alternative to
   decisions 2 and 3) — instead of swapping away, refresh the active account's
   credential where it sits.
   - **Pros**: would recover the active account without a rotation, keeping the
     current selection.
   - **Cons**: an isolated refresh **rotates the refresh token server-side** and
     CAS-writes only to the stash (**#253**), so any concurrent live session keeps
     using the now-invalidated token and the freshly-minted one is stranded in the
     stash — precisely the session-orphaning #253 removed from the poll-refresh
     path.
   - **Why not**: `emergency_swap` → parked → #106 is the safe recovery for a dead
     active *with* a spare. And whether an isolated refresh is *ever* safe hinges on
     Anthropic's rotation behavior, which is **unknown** until the **#262** spike
     resolves — so the #253 exclusion is **held, not lifted** (decision 3). This is
     the escape hatch to revisit **if** #262 shows the old refresh token survives
     exchange.

3. **Auto-reviving an `ActiveDeadNoTarget`** (the alternative to decision 4) — try
   to recover a dead active with no spare automatically rather than asking the
   operator to re-login.
   - **Pros**: removes the one recovery corner that needs a human.
   - **Cons**: the only automated moves available are the two already rejected — an
     in-place active refresh (decision 2, corrupts live sessions) or a timer-driven
     retry (decision 1, a `claude -p` storm). There is no viable swap target by
     definition, so `emergency_swap` cannot fire (`src/daemon.rs:2375`).
   - **Why not**: no safe automated path exists; a manual `claude /login` is the
     only correct recovery. The daemon fails **loud** (durable "needs re-login"
     status) rather than silently churning. The spontaneous-revival path
     (`recovery_successes`) still covers the case where the account's *own* token
     starts answering again without operator action.

## Consequences

### Positive

- A durable, discoverable **"no"** for four tempting auto-revival mechanisms —
  contributors get the rationale here instead of re-deriving it from #253 / #106
  threads and daemon doc-comments.
- The recovery model stays **simple and thrash-free**: one fixed cadence (#106)
  owns revival timing, `emergency_swap` owns the live-spare escape, a stuck active
  self-heals via re-probe, and a no-spare dead active fails **loud** rather than
  silently corrupting sessions.
- No new isolated-`claude -p` retry storms, and no orphaned refresh tokens /
  broken live sessions from an in-place active refresh — keeping the
  credential-adjacent surface small (the CONTRIBUTING.md minimal-surface line).

### Negative / trade-offs

- A dead **active** account with **no live spare** (`ActiveDeadNoTarget`) needs
  **human intervention** (`claude /login`) — the one recovery corner with no
  automation. Accepted as the only safe option; partially mitigated by the
  spontaneous-revival path if the token starts answering again on its own.
- Revival latency for quarantined / parked accounts is bounded by the **#106
  cadence**, not by a tighter dedicated timer. Acceptable given the timer's
  impedance mismatch; the real latency lever is the #106 starvation fix (**#260**).
- Decision 3 is a **hold, not a permanent no**. The #253 active-account refresh
  exclusion stays until the **#262** spike resolves Anthropic's refresh-token
  rotation behavior; if that spike shows the old token survives exchange, in-place
  active refresh may be reconsidered and this ADR revisited or superseded.

## Related

- Issues: **#263** (this ADR; source: `/council` on auto-reviving dead accounts);
  **#260** (the #106-sweep starvation fix — open); **#262** (the
  refresh-token-rotation spike — open); **#253** (active-account refresh exclusion
  — closed); **#106** (refresh events + restore-on-success — closed); **#42**
  (credential-dead / quarantine + emergency-swap origin).
- Code: `src/daemon.rs` — `AccountHealth.consec_401` (~L806),
  `AccountHealth.recovery_successes` (~L825), `emergency_swap` (~L2343),
  `TickAction::ActiveDeadNoTarget` (~L2375); the edge-triggered health machine and
  the #106 refresh sweep.
- ADR-0002 / ADR-0003 (the credential-handling invariants this recovery model
  rests on); CONTRIBUTING.md (minimal credential-adjacent surface).
