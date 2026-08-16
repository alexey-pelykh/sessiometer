# Scope Brief — Migration Credential Portability

**Date**: 2026-08-04 · **Repo**: `alexey-pelykh/sessiometer` · **Umbrella**: #999
**Entry**: `Audit findings` (user-confirmed) · **Pipeline**: Stages 1 → 2 → 3 → 4, Stage 0.7 exempt
**Source**: `/investigate` 2026-07-31 → `/scope` 2026-08-04

> Transient pipeline scratch — deliberately **not committed**, matching the
> `panel-presentation-reference-coverage` precedent. The PRD and design doc are the durable,
> self-contained artifacts.

## Headline

An `export`/`import --overwrite` between two machines silently killed two accounts. The import did
not fail — **it succeeded, and the success is what killed them**. The artifact is a point-in-time
snapshot of a *rotating* secret; the source kept rotating after the snapshot was taken, and nothing
in the format, the CLI, or the docs represents that the snapshot has a shelf life.

Nine issues filed: umbrella **#999** + eight children **#1000–#1007**.

## What was decided

| # | Decision | Choice |
|---|---|---|
| K1 | Scope membership | **B — all enriched** (E1, E2, I1, I2, I3, M1; M2 folded as ACs, M3 folded into E1) |
| K2 | Classification | **`Audit findings`** → Stage 0.7 exempt; all other gates fired |
| AD-1 | Canonical promotion | `import` does **not** write the canonical item — it reports non-adoption and names `use`, which drives the existing #64-locked swap engine |
| AD-2 | Staleness detection | **No `format_version` bump.** Age ≠ supersession; a bump buys a heuristic and costs the ADR-0006 frozen baseline |
| AD-3 | `rotated` telemetry | Move it inside the `refreshed` variant — unrepresentable, not merely unprinted |
| AD-4 | `status` provenance | Display-only; which slot is authoritative does not change |

## Two things that changed during the work

**A carried claim was falsified.** The investigation's working explanation — "`--overwrite` is
uuid-keyed, so it was inert against the same-label accounts" — is **wrong**. The roster key
`account_uuid` (`src/config.rs:343`) is the *Claude* account uuid, stable across machines. The uuids
matched, `--overwrite` did fire, the stashes **were** replaced — which is exactly how machine A's
token reached machine B to be replayed and refused. Recorded in PRD § 9 F-1 and in #1005's scope
note so the wrong narrative does not resurface. The incident is fully explained without it.

**A feasibility question resolved the opposite way to expectation.** R-4a asked whether a freshness
signal is derivable from v1 data. It *is* — `credential_clocks()` reads both deadlines from the blob
with no format change. **But it does not detect this failure**: at import time the artifact had ~55
minutes of access-token validity left. The token was not *expired*, it was *superseded*, and
supersession leaves no trace in the blob. This turned R-4 from "build a freshness check" into
"warn unconditionally, and do not build a check that creates false assurance."

## Coverage Quality Gate (Stage 3.7) — **PASS WITH FINDINGS**

**Requirement-to-issue coverage: complete.** R-1/R-1a→#1000 · R-2/R-2a→#1001 · R-3→#1003 ·
R-4/R-4a→#1002 · R-5/R-5a→#1004 · R-6/R-6a→#1005 · R-7→#1006 · R-8→#1007. No orphans, no gaps.

**Category coverage**: security ✓ (C-3 redaction as an AC on every output-touching item) ·
observability ✓ (#1004, #1006) · operational/docs ✓ (#1000, #1007) · testing ✓ (M2 folded as ACs on
#1001/#1002/#1005 plus five spec stubs) · compliance — n/a, no regulatory surface.

**Design-reference carry-forward**: all three registered references reached items — ADR-0006 → #1002
(AD-2) and #1003; `docs/findings/README.md` → #1000; finding 0465 → #1004.

**Findings (non-blocking, must not be lost):**

1. **R-3 (#1003) has no design, by intent.** The non-roster config-merge policy is a decision needing
   an ADR; choosing it silently is the exact failure the PRD's provenance warning guards against.
2. **OQ-1 gates half of #1005.** Whether `enable`/`disable` should refuse like `use`, or `use` should
   take first like `enable`, is a product call. Design lean is (i) refuse; not decided.
3. **OQ-2 shapes #1001's first increment** — whether `--activate` ships at all initially.
4. **A-3 and A-4 are n=1.** They are the entire basis for #1000. Restating them later as guarantees
   would manufacture a security assumption from one lucky sample.

## Readiness (Stage 4)

| Issue | Verdict |
|---|---|
| #1000 findings note | **READY** |
| #1001 canonical adoption | **READY** (OQ-2 affects scope of increment, not readiness) |
| #1002 staleness warning | **READY** — AD-2 settled the format question |
| #1003 config-merge ADR | **READY as a decision item**; not ready as implementation, and not intended to be |
| #1004 `rotated` telemetry | **READY** — 0465 verified unaffected |
| #1005 duplicate label | **CONDITIONALLY READY** — the R-6 warning half is ready; the R-6a consistency half is blocked on OQ-1 |
| #1006 status provenance | **READY** |
| #1007 runbook | **READY** |

## Recommended order

1. **#1007 (runbook)** — cheapest, and the only item that prevents recurrence *before* any code lands.
2. **#1002 (warning)** + **#1006 (provenance)** — small, independent, and they make the failure
   legible instead of silent.
3. **#1001 (adoption)** — the core defect.
4. **#1004**, **#1005** (R-6 half), **#1000**.
5. **#1003** and **#1005**'s R-6a half — decision-gated; unblock via OQ-1 / an ADR.

## Artifacts

| Artifact | Path | State |
|---|---|---|
| PRD | `docs/requirements/migration-credential-portability.md` | written, `dor_status: passed-with-findings` |
| Solution design | `docs/design/migration-credential-portability-solution-design.md` | written, `draft` |
| Spec stubs (5) | `docs/specs/{import-credential-adoption, import-staleness-warning, import-duplicate-label, refresh-rotation-signal, status-expiry-provenance}.feature.md` | written |
| Tracked items | #999 + #1000–#1007 | filed |
| Scope brief | this file | written (uncommitted scratch) |
| Working doc | `.tmp/scopes/migration-credential-portability.md` | transient |

**Uncommitted**: every doc above is written to the working tree but **not committed**. The repo is
rebase-only and lands work via PR; committing them is a separate, deliberate step.

## Operational residue (not scoped — needs a human)

The two accounts killed on the other machine are genuinely dead server-side. They need
`sessiometer login <label>` **on that machine**; no item here recovers them.
