---
type: requirements-brief
date: 2026-08-14
source: docs/requirements/credential-grant-survival.md
workflow: /capture-requirements
status: final
---

# Requirements Brief: Credential Grant Survival

## Problem Being Solved

Sessiometer exists so the operator never thinks about credentials. Since **2026-08-11** it has been
producing the exact toil it exists to remove — roughly **one manual `claude /login` per day**.

Accounts able to refresh unaided fell from **6 of 6** to **2 of 6** in four days.
`credential_unrecoverable` fired **three times in four days on three different accounts**, having
never fired once in the preceding 41 days. An account the operator relogged on 08-13 at 11:56 was
dead **16 h 07 m** later — with **8 hours of access-token life and 15 days of refresh-token life
still on it**. The grant was invalidated while both tokens were live.

The affected user is the sole operator, who is also the sole developer.

## Key Requirements

1. **Capture the OAuth sub-reason on a 401** (R-1) and **classify every death as shape A (live grant
   invalidated) or shape B (token lapsed)** (R-2). Today the daemon cannot tell revocation from
   expiry anywhere in anything it produces — which is why every candidate cause is a hypothesis.
2. **If the sub-reason is not observable through `claude -p`, record that negative finding — do not
   substitute an inferred cause** (R-3). The instrument-validity gate.
3. **Identify the second writer by experiment, with a stated falsifier and a committed action per
   branch** (R-4). A second writer has been rotating these credential families ~2×/day since 07-29:
   **26 of 26** foreign writes changed the refresh-token bytes, **24 of 26** at rotation cadence
   rather than login cadence.
4. **Throttle the recovery loop; never terminate it** (R-8, R-10). 932 refresh attempts on 08-12
   against a ~15/day baseline — but a quarantined account was genuinely rescued by that same loop,
   so a circuit-breaker would strand recoverable accounts.
5. **Stop deriving credential health from usage-poll reachability** (R-12, R-13). One account
   produced 9 `healthy`/9 `unknown` transitions in a day, each `unknown` merely a rate-limited poll;
   another reported `healthy` while 401ing continuously.
6. **A fleet SLI with a push surface** (R-14, R-15, R-16) — the collapse ran seven days and was
   noticed by the operator, not the system.
7. **Widen the CC verified range only on a refresh-lifecycle walk** (R-20) — #714's canary checks the
   keychain derivation and is not evidence about the lifecycle.

## Key Decisions

1. **The root cause is NOT established, and the PRD says so in a dedicated section (§ 1c).** An
   earlier pass asserted a cause the operator falsified in one sentence. § 1c partitions every claim
   into established / falsified / open, so the next reader does not re-derive the dead hypothesis.
2. **Claude Code's version is not the cause — falsified by operator experiment.** A second machine
   runs CC 2.1.231, also outside the verified range 2.1.181–2.1.217, with an older sessiometer, and
   is healthy. The temporal correlation was strong (four CC builds landed in the four collapse days)
   and is recorded precisely because it is seductive.
3. **The success metric is accounts-alive-without-intervention, never per-attempt success rate.**
   The raw rate collapses 100% → 0.5%, but its denominator is inflated 62× by the recovery loop's own
   retries — optimising it would make a throttle look like a cure.
4. **Two death shapes, not one.** The ratified scope carried shape A only; shape B (lapsed token,
   refresh cannot revive) occurred the same day. A fix verified against one is not evidence about
   the other.
5. **Status is `draft`, not `locked`.** Assumption A-1 — that a capture point for the sub-reason
   exists at all — is an open feasibility question that R-1 and most of the PRD rest on. Locking
   over an open load-bearing question would be a false lock.
6. **Cross-machine credential leasing stays out of scope.** #466 found no Claude Code knob; #613 put
   cross-machine sync outside the product boundary. If the spike concludes the writer is another
   machine, that routes to an operator decision, not to an implementation task.

## Two contradictions surfaced, neither auto-resolved

- **🔴 Finding #465's verdict clause** — *"not a server-side revocation"* — against death shape A,
  which is server-side invalidation of a live grant. The finding's *mechanism* stands; its verdict
  is scoped to the scrub it measured (R-7).
- **🔴 A code-asserted invariant** — `src/refresh_tick.rs:377-382` states the recovery bypass fires
  *"at most once per idle period (poll cadence)"*, while a quarantined account was measured at
  **11 attempts in 27 m 34 s, mean spacing 2.6 min, against `poll_secs = 300`**. Either the code
  violates its own invariant or "idle period" recurs faster than the poll cadence. Neither may be
  assumed (R-9).

## Scope Membership

**13 items, operator-ratified, binding** — none added, none dropped. All 13 map to one of 8 features
(§ 5b). Seven premortem findings were folded into existing requirements rather than added as items.

## Definition-of-Ready

**PASS-WITH-FINDINGS.** EARS form, acceptance criteria with BUT NOT clauses, Planguage METER, and the
assumption registry all pass. Two findings: seven requirements (R-3, R-5, R-6, R-7, R-9, R-11, R-16)
elaborate ratified items rather than tracing to one directly — a reviewer should confirm that
reading; and the quality-attribute thresholds are pipeline-proposed, not operator-set.

**Operator action required** (not a decision): R-4's discriminating experiment needs sessiometer
paused on exactly one machine for 24 h. Nothing else in this PRD can identify the second writer.
