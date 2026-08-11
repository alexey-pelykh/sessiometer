---
type: scope-brief
date: 2026-08-11
workflow: /scope
status: final
---

# Scope Brief: Session warm-up on reset

## Problem

Anthropic's **five-hour session window is anchored at first use** — it does not tick while an account
is idle. A parked account that has reset sits with **no window running**, and its next reset instant is
created only when something touches it. Idling therefore pushes each successive reset further out.

Measured on this fleet's own usage store (**15 784 samples, 6 accounts, 2026-07-28 → 2026-08-11**, the
window immediately after issue #726's corpus, no overlap):

| Measure | Value |
|---|---|
| Stopped-clock intervals ≥30 min (no `session_resets_at` at all) | **242** |
| Idle-clock duration | median **2.17 h**, p90 **3.76 h**, max **7.36 h** |
| Total idle-clock time | **582.52 h** = **27.9 %** of 2 088 account-hours |
| New reset instant, measured from the re-arm sample | median **4.73 h** ahead |

Warming a parked account when its window ends would bring its next reset forward by up to that idle
duration, on the **~76 of 143** holds #726 recorded as `cause=session` with weekly head-room.

## What's in scope

1. **#1231** — *spike, gating.* Measure the **weekly** cost of one warm-up cycle, and confirm whether a
   cycle opens a session window at all. Returns GO / NO-GO.
2. **#1232** — *build, blocked by #1231.* Opt-in daemon warm-up when a parked account's window has
   ended: detection predicate, eligibility gate, actuation through the existing isolated refresh
   engine, config flag (off by default), per-account throttle, durable redacted event + readout.

## Key decisions

1. **Two items, hard-gated — not one.** A warm-up spends **weekly** quota, and weekly is the binding
   axis (#726: 16/18 weekly windows peak ≥0.97). So warm-up trades the scarce resource to buy latency
   on the one that is 27.9 % slack. #1231's result can invert the feature's sign, so #1232 is *blocked
   by* it rather than merely informed.

2. **Actuate via the isolated refresh engine, not by swapping.** The idea was phrased as "briefly
   swapped to". Two swaps per warm-up would inject churn into the shared canonical credential, which
   finding #465 shows is scrub-prone — *"the first such session scrubs the item for the whole fleet."*
   The `poke` path uses an ephemeral `CLAUDE_CONFIG_DIR` and never touches the live item.

3. **The weekly axis is excluded on evidence, not assumption.** **12/12** weekly anchor transitions
   landed at exactly **7.0000 d** despite 582 h of idle in the same corpus. Weekly is a fixed
   hard-reset; warm-up cannot move it or relieve the Fri–Sun trough. On fresh data this also answers,
   for a *running* account, what issue **#792** was opened to settle.

4. **This is not the lever #726 rejected.** That NO-GO was for *delaying* first use to disperse anchors
   — *"there is no surplus to fund the idle."* This is the opposite direction, *advancing* it, whose
   cost is one cheap spawn. #726 measured phasing **spread** between accounts (+16 min residual) and
   never measured idle-clock intervals, because the delay lever gave it no reason to.

5. **Formulation deferred as a typed exception (ACCEPTED_GAP).** #1232's Gherkin waits on #1231,
   because the scenarios' `Given` depends on a predicate the design records as unresolved — whether
   `session_resets_at == None` means "no window running". Retrofit trigger: when #1231 returns GO,
   before #1232 execution.

## Stats

- **Work Items**: 2 in GitHub (#1231, #1232)
- **Ready**: 2/2 (#1232 READY-BLOCKED on #1231)
- **Gaps accepted**: 1/2 — #1232 ACCEPTED_GAP for formulation, with a concrete retrofit trigger
- **Deferred**: 0/2

## Artifacts Produced

**Assertion**: 4/4 declared artifacts verified, 2 amended-absent, 0 repaired

- **PRD** — `docs/requirements/session-warmup-on-reset.md` (8 EARS `SHALL` requirements + OOUX model)
- **Solution design** — `docs/design/session-warmup-on-reset-solution-design.md` (5 components with
  feasibility verdicts, 6 alternatives weighed, 5 risks)
- **Work items** — 2 in GitHub
- **Scope brief** — this document
- *Amended absent*: design brief (folded here — at 2 items it would restate this verbatim);
  feature-file stubs (Stage 3.5 ACCEPTED_GAP, above)
- *No `user-stories` document* — requirements are EARS + OOUX in the PRD; story-shaped work is tracked
  items typed `story`.

## Open question owned by the operator

Whether a *small but favourable* #1231 result justifies the config surface and the new spawn path at
all is a judgement call, not a measurement. Surfaced in the PRD § 8 rather than decided.

## Next Steps

- `/do 1231` — run the gating spike
- `/do 1232` — only after #1231 returns GO
