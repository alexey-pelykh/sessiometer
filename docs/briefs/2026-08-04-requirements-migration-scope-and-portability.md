---
type: requirements-brief
date: 2026-08-04
source: docs/requirements/migration-credential-portability.md
workflow: /capture-requirements
status: final
---

# Requirements Brief: Migration Artifact Scope & Portability

Amendment pass adding **R-9 … R-16** to the existing PRD, plus a rewrite of **R-3** and corrections to
**R-6a** and **AC-2**. Driven by two `/council` rounds on 2026-08-04.

## Problem Being Solved

The original PRD framed the incident through one mechanism: the artifact is a point-in-time snapshot
of a **rotating secret**, and the source keeps rotating past it. That explains every symptom the
operator observed — and **none** of what the council then found.

A second, orthogonal mechanism does: the artifact carries three payload classes with **different
portability and trust properties** — rotating credentials, machine-independent roster identity, and
settings that range from freely portable through machine-bound to **capability-granting** — and
applies all three at **one trust level**. The operator cannot choose which classes apply, and the
system enforces no floor on what may *ever* apply.

That conflation is why adopting an artifact's config is an unattended code-execution path, why a
security parameter is downgradable, and why local policy is silently overwritable. None of it is
about credentials.

## Key Requirements

1. **R-9 — import scope selection.** `--accounts` / `--settings`, default everything (today's
   behaviour byte-for-byte). Today no such gesture exists: a fresh target adopts the artifact's whole
   config unconditionally and the operator cannot decline.
2. **R-9a — scope is presence-derived, never self-declared.** On a `--plaintext` export nothing is
   authenticated, so a declared scope is attacker-controlled. The operator's flag is a **ceiling,
   never a floor**.
3. **R-11 — portability allowlist**, non-portable by default. `claude_bin` refused unconditionally
   regardless of flag; `kdf_*` on a monotonic floor; `conflict_policy` non-portable.
4. **R-11d — the allowlist must fail closed on a new `Config` key.** An unenforced allowlist is a
   denylist with extra steps, and denylist-rot is the exact failure it was chosen to avoid.
5. **R-10 — remove `export --no-secrets`.** Roster-without-secrets is not a supported state.
6. **R-12 … R-15** — artifact shred, export-side daemon-liveness probe, digest + scope on the
   export/import events, `account_uuid` validation.
7. **R-16 — the `[credential]` backward-import break**, discovered here and previously untracked.

## Key Decisions

1. **R-3's original demand is withdrawn, not satisfied.** It required a documented per-block merge
   policy. A merge policy answers "which side wins per block", which presupposes every block is the
   same *kind* of thing — they are not, and no win/lose policy can express "the operator may choose
   this one and may never choose that one." Decomposing into scope × class expresses it directly.
2. **Scope on import only; export unchanged.** Export scope is disclosure hygiene, import scope is
   input validation, and only the latter defends against an artifact the attacker minted — because
   the attacker controls the export.
3. **`--accounts` / `--settings`, not `--config`.** `--config` is reserved and value-bearing for
   issue #24, and is semantically wrong: `account` is a `RawConfig` field, so accounts *are* config.
4. **R-9 and R-11 ship as one unit.** Either alone is worse than neither.
5. **R-11c resolved conservatively over a recorded dissent** (§ 9 D-1) — two panelists reached
   opposite conclusions from the same verified facts. Normative, not factual.
6. **The KDF boundary was narrowed, not broken.** § 1b previously excluded KDF outright; R-11b
   governs only whether an artifact's `kdf_*` may be *adopted*. Construction and parameters stay #147's.

## Assumptions & Risks

- 🔴 **R-10a is undecided** — hard-remove vs deprecate-then-remove for a **shipped** flag.
- 🔴 **R-16 is filed, not resolved.** R-9b repairs only the roster-only path.
- 🟡 **R-11c rests on a dissent**, not a convergence. Defensible, not evidence-forced.
- 🟡 **No mechanism in R-9 … R-16 is user-ratified.** All are ratified *in-scope* only, via a bounded
  22-item selection. Each remains reversible.
- 🟡 **AD-2's cost argument is falsified** (§ 9 F-3) and must be re-argued in Stage 2 before the
  design closes. Its conclusion survives; its reason does not.

## Stats

- Requirements: **34** (was 18) | Acceptance criteria: **21** | Objects: **7** (added `ImportScope`,
  `PortabilityClass`)
- Provenance: `user-stated` 3 · `council-added` 8 · `enrichment-expanded` 5 (amendment set only)
- Falsified claims recorded: **2** (F-1 carried in; F-3 found here) · Dissents recorded: **1** (D-1)

## Full PRD

See [migration-credential-portability.md](../requirements/migration-credential-portability.md)
