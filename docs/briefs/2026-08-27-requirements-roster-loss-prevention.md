---
type: requirements-brief
date: 2026-08-27
source: docs/requirements/roster-loss-prevention.md
workflow: /capture-requirements
status: final
---

# Requirements Brief: Roster Loss Prevention — Absent-Config Refusal, the Never-Shrink Invariant, and Roster Durability

## Problem Being Solved

On 2026-08-27 a `sessiometer login` found no `config.toml`, treated that as a first run, wrote a
**one-account** file, and notified the daemon — which adopted it and discarded the **six live
accounts it alone still held**. The fleet was destroyed by a verb that can only ever append.

The roster lives in two places — disk and daemon memory — synchronised in one direction, on one
trigger, with the daemon adopting whatever it reads and leaving no record that it did. The operator
cannot see the divergence, because `status` is a pure control-socket client that never reads disk.
Three properties failed at once, and any one of them alone would have prevented the loss: nothing
distinguishes *"never configured"* from *"configuration disappeared"*; nothing stops an append-only
verb's reload from shrinking a live roster; and no durable prior copy of the roster exists.

The affected user is the sole operator, who is also the sole developer.

## Key Requirements

1. **An append-only verb that finds no config while a live daemon holds accounts must refuse before
   writing** (R-1), identically over the socket as on the CLI (R-2) — the refusal is a property of
   the operation, not of the entry point.
2. **A reload from an append-only verb may never shrink the live roster** (R-3); a removal verb
   legitimately may, including to zero (R-4). An intent-less notification takes the refusing
   treatment (R-3a) — fail-closed by requirement, not convention.
3. **The system must distinguish "never configured" from "configuration disappeared"** before any
   append-only write commits (R-6). Stated as an outcome on purpose: whether a write verb may
   consult the control socket is a genuine architectural fork the design stage owns.
4. **A durable prior copy of the roster must survive a replacement** at the same `0o600` mode (R-8),
   with an operator path to enumerate and restore it (R-9).
5. **Disk/memory divergence must be detected and reported, never resolved by writing** (R-10).
6. **A roster-reload outcome must reach a destination an operator can inspect** (R-14) — which
   requires first adding a reload event type, since none exists (R-15).
7. **A new refusal reason must land on all four surfaces in one change** (R-12): the Rust enum, the
   Swift mirror, the panel's capture-states reference, and its gate. The Swift decoder fails closed.

## Key Decisions

1. **The problem was reframed away from *"`login` has a bug."*** `login` behaved exactly as
   written, and its append-only shape is correct. The defect is that a verb which cannot shrink a
   roster was nonetheless able to shrink the live one.
2. **Rejecting an empty roster on load was ruled out.** An empty roster is legitimate *by design* —
   `Config`'s own doc comment records that `capture` must be able to load a tunables-only file to
   add the first account. The guard belongs at the narrowing boundary, not at parse.
3. **The refusal is not novel — it already exists in-repo.** `perform_config_set` refuses on an
   absent config with `ConfigSetRejection::NoConfig`. One of three write paths already has the
   convention; two lack it. This is an inconsistency to propagate, not a design to invent.
4. **B2.2 was corrected on evidence, and the correction was ratified.** It was approved as an
   *empty*-scoped floor. The incident was **6 → 1**, so that floor never fires on it — and worse, an
   append-only verb always leaves ≥1 account in the file it saves, so an empty floor is **inert on
   every append-only path** and fires only on a legitimate removal-to-zero. The operator was shown
   this alongside the alternatives and ratified the shrink-scoped, intent-partitioned invariant
   together with its wire-change cost.
5. **Durability is in scope because the cause is unattributed.** The investigation ruled out every
   `sessiometer` code path, directory-level deletion, four path-divergence mechanisms, `cargo test`
   and every Claude session, and closed on a stated ABSTAIN. Hardening the write paths bounds the
   amplification but not the cause; a backup makes an unattributed deletion survivable whatever it
   turns out to be.
6. **The reload itself is a Chesterton's fence.** It was introduced as the fix for issue #139. It was
   scoped for the *widening* direction; narrowing was never considered. R-18 exists to keep #139
   working — disabling the reload is a regression, not a fix.

## Assumptions & Risks

- 🔴 **A-4 — backup-on-write may capture the loss rather than prevent it.** Retaining the *previous*
  contents at save time is useless in the incident's own sequence (delete → login → save), where the
  previous contents were nothing. The backup that matters is the one written at the last **good**
  save, which must survive subsequent bad ones. The sharpest finding in the premortem, and no
  category checklist produces it.
- 🔴 **A-1 — the deletion is unattributed and may recur.** Treated as a reason to build durability,
  never as a reason to defer it.
- 🟡 **A-5 — the event log may have no reader.** Routing reload failures there instead of `eprintln!`
  is the same failure one layer up unless paired with a surfaced signal.
- 🟡 **A-6 — carrying intent on the wire.** Mitigated by R-3a's fail-closed default, which promotes
  the hedge to a requirement.
- 🟡 **A-3 — appetite.** Two weeks of evenings; R-3's wire change and R-12's cross-language
  propagation are the two that cross a boundary.

## Stats

- Objects: 7 | Requirements: 19 | Acceptance Criteria: 8 (each with BUT NOT clauses)
- Assumptions: 1 green / 3 yellow / 2 red
- DoR verdict: **PASS-WITH-FINDINGS** — all six checks pass; R-17/R-18 recorded as
  enrichment-derived and flagged for the operator to strike if unwanted

## Full PRD

See [roster-loss-prevention.md](../requirements/roster-loss-prevention.md)
