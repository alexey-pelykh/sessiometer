---
type: scope-brief
date: 2026-08-04
workflow: /scope
status: final
---

# Scope Brief: Migration Artifact Scope & Portability

**Repo**: `alexey-pelykh/sessiometer` · **Umbrella**: #999 · **Entry**: `Audit findings` (council-produced)
**Predecessor**: the 2026-08-04 first pass (#999 + #1000–#1007). This run **extends** it.

## Problem

The first pass framed the incident through one mechanism: the artifact is a point-in-time snapshot of
a **rotating secret**, and the source keeps rotating past it. That explains every symptom the operator
observed — and **none** of what two `/council` rounds then found.

A second, orthogonal mechanism does. The artifact carries three payload classes with **different
portability and trust properties** — rotating credentials, machine-independent roster identity, and
settings ranging from freely portable through machine-bound to **capability-granting** — and applies
all three at **one trust level**.

That conflation is why importing an artifact adopts its `claude_bin` and hands it to a daemon that
spawns it on a timer; why a KDF parameter is downgradable; and why the target operator's conflict
policy is silently overwritable. None of it is about credentials, which is why the original framing
could not reach it.

## What's In Scope

**Security core — ships as one unit:**

1. **#1045** — portability allowlist. `claude_bin` refused unconditionally, `kdf_*` monotonic floor,
   `conflict_policy` machine-bound. Allowlist, not denylist.
2. **#1046** — `import --accounts` / `--settings`. Default unchanged. **Must not ship before #1045.**
3. **#1047** — the allowlist fails closed when a new `Config` key is added.
4. **#1003** — *(transformed)* the ADR recording the classification.

**Hardening tail — independent, individually shippable:**

5. **#1048** — remove `export --no-secrets`. The scope's only breaking CLI change.
6. **#1049** — `import --shred`.
7. **#1050** — `export` daemon-liveness probe.
8. **#1051** — artifact digest + scope on export/import events.
9. **#1052** — `account_uuid` validation.
10. **#1053** — the `[credential]` backward-import break.

**Corrections to existing items**: #1001, #1002, #1005, #1007.

## Key Decisions

1. **R-3's original demand was withdrawn, not satisfied.** It required a per-block merge policy, which
   presupposes every block is the same kind of thing. `[tunables]` is a preference, `kdf_*` is a
   security parameter, `claude_bin` is a capability grant — no win/lose rule can express *"the operator
   may choose this one, and may never choose that one."* Decomposing into scope × class expresses it
   directly and closes design § 17's stated gap.
2. **Scope on import only; export unchanged.** Export scope is disclosure hygiene, import scope is
   input validation — only the latter defends against an artifact the attacker minted.
3. **Scope is presence-derived, never declared.** A scope field would be attacker-controlled on the
   unauthenticated `--plaintext` path, and would additionally invalidate AD-2's own cost argument.
4. **`--accounts` / `--settings`, not `--config`.** `--config` is reserved and value-bearing for #24,
   and semantically wrong: `account` is a `RawConfig` field, so accounts *are* config.
5. **Default stays "everything", explicitly coupled to the `claude_bin` refusal.** If that refusal is
   ever weakened, the default must be re-decided first — AD-9 records the coupling.
6. **It costs nothing at the format layer.** `Payload`'s two fields are both emptiable and every
   `RawConfig` field is `#[serde(default)]` including `account`. No `FORMAT_VERSION` bump, no fixture
   regeneration, ADR-0006 not reached.

## Three defects found in the existing artifacts

- **#1001 named a command that does nothing.** The adoption guidance was `use <label>`; for the active
  account that is a provable no-op — `SwapTarget::resolve` compares service names, never contents, and
  a committed test asserts `canonical == b"A-token"`, `calls == 0`. It would have **reproduced the
  original failure through its own remediation.** Now `use --force`.
- **#1002 and design AD-2 carried a false cost argument.** It priced a *payload* field at the *header*
  rate, contradicting ADR-0006 § BREAKING(3). The conclusion survives on false-assurance grounds; the
  cost claim is deleted rather than softened.
- **#1005's OQ-1 was defective on both halves.** It omitted `remove` — the only label-resolving command
  whose first-match-wins is irreversible — and offered as an alternative a behaviour `use` already
  ships, i.e. a regression dressed as a symmetric choice.

## Stats

- **Work items**: 9 new (#1045–#1053), 1 transformed (#1003), 4 amended, umbrella updated
- **Requirements**: 34 (was 18) · **Acceptance criteria**: 21 · **Objects**: 7
- **Coverage gate**: **PASS** — 17/17 new capabilities have scenarios; 34/34 requirements named by an
  open issue
- **Falsified claims recorded**: 2 (F-1 carried in, F-3 found here) · **Dissents recorded**: 1 (D-1)

## Artifacts Produced

**Assertion**: 6/6 declared artifacts verified, 0 amended-absent, 1 repaired

- **PRD** — `docs/requirements/migration-credential-portability.md` (amended)
- **Requirements brief** — `docs/briefs/2026-08-04-requirements-migration-scope-and-portability.md`
- **Solution design** — `docs/design/migration-credential-portability-solution-design.md` (amended)
- **Design brief** — `docs/briefs/2026-08-04-design-migration-scope-and-portability.md`
- **Feature stubs** — 5 at `docs/specs/` (23 scenarios)
- **Work items** — 14 touched in GitHub issues
- *No `user-stories` document* — requirements are EARS + OOUX; story-shaped work is tracked items.

**Repaired (1)**: the requirements-brief row was absent from the Stage 0 birth manifest and was
inserted at Stage 1 (amendment M-1). `/capture-requirements` owes that brief unconditionally, so
Stage 0 under-declared. Recorded rather than silently added.

## Open — not blocking, must not be lost

1. **OQ-4** — `--no-secrets` hard-remove vs deprecate-then-remove (#1048). A product call on a shipped flag.
2. **OQ-5** — #1053's deliverable. Already-released binaries cannot be patched; only the version-floor
   message and forward-tolerance are in reach.
3. **OQ-1** — the single duplicate-label policy across `use` / `enable` / `disable` / `remove` (#1005).
4. **AD-11 rests on a recorded dissent**, not a convergence (PRD § 9 D-1).
5. **No mechanism in R-9 … R-16 is user-ratified** — all ratified *in-scope* only, via a bounded
   22-item selection. Each remains reversible.

## Next Steps

- `/do 1003` — the ADR unblocks #1045
- `/do 1045` then `/do 1046` — the security core, in that order
- `/do-next` — the hardening tail is independent
