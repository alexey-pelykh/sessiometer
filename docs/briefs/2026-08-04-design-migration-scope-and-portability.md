---
type: design-brief
date: 2026-08-04
source: docs/design/migration-credential-portability-solution-design.md
workflow: /design-solution
status: final
---

# Design Brief: Migration Artifact Scope & Portability

Amendment adding §§ 4.7–4.9 and AD-5 … AD-11, correcting § 4.1, § 4.2 and OQ-1, and closing § 17's
R-3 gap.

## The Position

**Scope is a property of the apply, never of the artifact.** The artifact describes what it carries;
the operator decides what is applied; the operator's decision is a **ceiling, never a floor**.

That splits into two mechanisms that do not substitute for each other:

- **§ 4.7 — what the operator asked for.** `import --accounts` / `--settings`, default everything.
- **§ 4.8 — what the system permits.** A portability allowlist that binds *regardless* of the flag.

## Key Decisions

1. **AD-5 — scope on import only; export unchanged.** Export scope is disclosure hygiene; import scope
   is input validation. Only the latter defends against a hostile artifact, because the attacker mints
   the export.
2. **AD-6 — scope is presence-derived; the artifact carries no scope field.** A declared scope is
   attacker-controlled on the unauthenticated `--plaintext` path. Presence cannot lie.
3. **AD-7 / AD-8 — allowlist, not denylist; `claude_bin` refused unconditionally.** A denylist rots.
   A second confirmation flag was rejected outright: the error message becomes the exploit instruction.
4. **AD-9 — default stays "everything", and it is explicitly coupled to AD-8.** With capability keys
   refused, the residual delta is a KDF downgrade. If AD-8 is ever reversed, AD-9 must be re-decided
   **first**.
5. **AD-11 — `conflict_policy` non-portable, over a recorded dissent** (PRD § 9 D-1).

**It costs nothing at the format layer.** `Payload`'s two fields are both emptiable and every
`RawConfig` field is `#[serde(default)]` including `account`. `FORMAT_VERSION` does not move, golden
fixtures do not regenerate, ADR-0006 is not reached.

## Three Corrections to the Existing Design

- **§ 4.1 named a command that does nothing.** The guidance was "run `sessiometer use <label>`"; for
  the *active* account that is a provable no-op — `SwapTarget::resolve` compares service names, never
  contents, and a committed test asserts `canonical == b"A-token"`, `calls == 0`. It would have
  reproduced the original failure through its own remediation. Now `use --force`, with `--activate`
  inheriting the same requirement.
- **§ 4.2 (AD-2) rested on a false cost argument.** It priced a *payload* field at the *header* rate,
  contradicting ADR-0006 § BREAKING(3). Conclusion survives on false-assurance grounds; the cost
  sentence is **deleted**, not softened — an argument both wrong and unnecessary is worse than none.
- **OQ-1 was defective on both halves.** It omitted `remove` — the only label-resolving command whose
  first-match-wins is **irreversible** — and offered as an alternative a behaviour `use` already ships,
  i.e. a regression dressed as a symmetric option.

## Risks Worth Naming

- 🔴 **RSK-6 — R-9 shipping without R-11.** A flag that adopts a code-execution path *on request* is
  strictly worse than today, where no gesture advertises it as supported. They ship as **one unit**;
  a plan that splits them has broken the requirement, not resequenced it.
- 🔴 **RSK-7 — the allowlist rots** without R-11d's compile-time guard.
- 🔴 **RSK-8 — AD-9 silently retained if AD-8 weakens.** The coupling is recorded for this reason.
- 🟡 **RSK-9 / RSK-11 — overclaiming.** `--shred` is `rm` with intent, not forensic erasure (APFS gives
  no reliable overwrite-in-place); R-15 is namespace hygiene, **not** path traversal. Both are stated
  at their real severity deliberately.

## Open

- **OQ-4** — `--no-secrets` hard-remove vs deprecate-then-remove. The scope's only breaking CLI change.
- **OQ-5** — R-16's deliverable. We cannot patch already-released binaries; only the version-floor
  message and forward-tolerance are in reach.
- **OQ-1** — the single duplicate-label policy across `use` / `enable` / `disable` / `remove`.

## Full Design

See [migration-credential-portability-solution-design.md](../design/migration-credential-portability-solution-design.md)
