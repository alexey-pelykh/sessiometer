---
type: scope-brief
date: 2026-08-06
workflow: /scope
umbrella: "#1060"
items: 4 (#1061, #1063, #1064, #1065)
prd: docs/requirements/committed-citation-integrity.md
design: docs/design/committed-citation-integrity-solution-design.md
source: issue #1060 (+2 amendment comments); repo-wide frontmatter sweep at `efa1a3e`
status: final
---

# Scope Brief: Committed-Citation Integrity

## What this scope is

Eight frontmatter citations in committed documents resolve nowhere. Four keys are involved and each
fails differently; `design-doc:` is the fifth and is the worked example done right, 3/3.

The scope covers all eight, plus two doc-hygiene defects, plus the rule itself and a CI gate that
enforces it — the "all enriched" membership the operator selected.

## Why a gate and not a repair

The class **regressed during this scope's own run**. Site 3 was correct at `386a6a2` and fabricated at
`efa1a3e`, changed by PR #1054 — reviewed, CI-green, and whose own last commit was about restoring
provenance. The rename followed the issue slug rather than the file's real name.

Nothing dereferences these pointers, so nothing caught it. A field with no consumer has no feedback
loop, and its decay is monotonic. A pipeline mints these pointers mechanically, so the count grows on
its own. A one-time repair is insufficient by construction.

## The rule

> A frontmatter value that is **path-shaped** must be a **git-tracked, repo-root-relative** path.
> A value that is not path-shaped is a provenance note, legal only on `source` and `parent-requirements`.

Reachability is `git ls-files --error-unmatch`, never `test -e` — path-existence is machine-dependent,
passing an untracked local file on the author's disk and failing it in CI. That is the bug, not a
check for it.

## The four items

| # | Item | Depends on |
|---|---|---|
| #1061 | Track `docs/briefs/` (14 files) + rebase 6 citations to the repo-root base | — |
| #1063 | Repair the 8 citations + 2 hygiene defects | #1061 |
| #1064 | `scripts/check-doc-citations.sh` + its falsifier test suite | — |
| #1065 | Wire the unfiltered `doc-gates` CI job + CLAUDE.md rows | #1061, #1063, #1064 |

Ordering is load-bearing: #1065 turning on first makes the gate's debut a red build on this scope's
own files.

## What the scope found that the issue did not

- **A second population of the same defect.** Two base conventions are live on the same key in the
  same directory. Repo-root-relative wins — the ratified worked example uses it 3/3, and it survives
  file movement, which matters because a *rename* is what broke site 3. Six brief citations need
  rebasing, so the migration is not the pure copy it looked like.
- **`source` is bimodal.** In a brief it names the primary document (a tracked path); in a requirements
  doc it names a session or scratch file (a note). Six of nine path-shaped brief citations sit on it.
- **A CI-graph blind spot.** `ci-ok` counts a `skipped` job as a **pass**, and the repo has no `docs`
  path filter. So the gate job must be unfiltered — a filtered gate that misses is indistinguishable
  from one that ran clean.
- **Three wrong rules, each caught by testing against the corpus rather than reviewing the reasoning**:
  a note-only rule for `source` (would have failed 6 correct citations); a contains-a-slash path
  detector (false-positived on `session /investigate …`); and a whole-value path-shape rule that
  **missed 2 of the 8 real defect sites**, both being a real gitignored path with a trailing
  parenthetical — a 25% false negative, in the permissive direction.

Each is now a named falsifier test (T6, T11, T15) rather than a prose warning.

## Adjacent, deliberately not merged

#1056 gates *claim phrasings* across docs **and issue bodies**; #1058 gates *line numbers in prose*.
This scope gates *frontmatter path resolution*. Three detectors, three surfaces — verified by reading
both, not inferred from titles. #1056's committed CI wiring is why the job is named `doc-gates`
(plural): that gate lands as a step, not a second job.

## Recursion, resolved

This brief lands in `docs/briefs/` — the directory whose untracked status is a defect *in this very
scope* — and is **committed**, setting the precedent the scope codifies. It is not the 15th untracked
file. Its own `prd:`, `design:` and `source:` values satisfy the rule above.
