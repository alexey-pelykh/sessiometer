---
type: design-brief
date: 2026-08-06
source: docs/design/committed-citation-integrity-solution-design.md
workflow: /design-solution
status: final   # locked — the survey resolved both open forks
---

# Design Brief: Committed-Citation Integrity

## Problem

A committed document can cite a path that resolves nowhere, and nothing notices — because nothing
dereferences these pointers. Eight such citations exist. One of them *regressed during this scope's
own run*: a correct pointer became a fabricated one through a reviewed, CI-green PR whose last commit
was about restoring provenance. That is the argument for a gate rather than a repair.

## Key Decisions

1. **One invariant, not a per-key taxonomy** — *a path-shaped value must be a git-tracked,
   repo-root-relative path.* Keys differ only in whether a prose note may stand in instead. This
   replaces a first draft that classified `source` and `parent-requirements` as note-**only**.
2. **Repo-root-relative is the single legal base.** Two conventions are live in the same directory on
   the same key today; the ratified worked example (`design-doc`, 3/3 correct) uses root-relative, and
   root-relative survives file movement — which matters, since a *rename* is what broke site 3.
   Rejected "accept either base": a typo that happens to resolve under the other base would pass.
3. **Reachability is `git ls-files --error-unmatch`, never `test -e`.** Path-existence is
   machine-dependent — it passes an untracked local file on the author's disk and fails it in CI,
   which is the bug, not a check for it.
4. **Sites 1 and 2 are deletions, not repairs.** Their referents were never written. Inventing
   plausible briefs to satisfy the pointers would manufacture provenance — the exact defect, committed
   deliberately.
5. **A zero-citation run FAILS.** A gate that passes because it evaluated nothing is the same
   write-only-field failure this scope exists to end, one level up.
6. **A new `doc-gates` CI job, not a step inside `gate-change-ack`** — that job is PR-range-scoped
   plumbing this whole-tree check does not use, and hosting it there would silently disable it on
   `push` builds. Named plurally because issue #1056's acceptance criteria already commit a second
   docs gate to CI; it lands as a step rather than a second job.
7. **The job carries no path filter.** `ci-ok` counts a `skipped` job as a **pass**, so a filtered
   gate that misses is indistinguishable from one that ran clean — AC-4's degenerate-subject failure,
   one level up in the CI graph. The repo has no `docs` filter to reuse anyway.

## What the survey changed

Two decisions in the first draft were **falsified by evidence gathered while writing it**, both by
surveying the 14 briefs — which this scope commits, and which therefore enter the check's scan scope:

- **`source` is bimodal.** In a brief it names the primary document (a tracked path); in a
  requirements doc it names a session or scratch file (a note). Of 9 path-shaped brief citations, **6
  sit on `source`**. The note-only rule would have failed 6 correct citations on its first run.
- **"Contains a `/`" is not a path test.** The probe I used to survey the briefs reported
  `source: session /investigate — stats roster aggregate and fleet runway` as a path. The repo holds a
  second instance (`scope: GUI/CLI capability parity`). The rule is now *no whitespace AND ends in a
  file extension*, pinned by a test.

Both are now falsification tests in the plan (T6, T11) rather than prose warnings, alongside T2
(path-existence) and T13 (zero-subject). Each kills a specific wrong implementation — and two of the
four kill implementations this design itself briefly held.

## What this costs beyond the original estimate

The migration is **not a pure copy**: 6 brief citations use the document-relative base and must be
rebased before those briefs can be committed. The rebases are mechanical (`../` → `docs/`) and every
referent is already tracked, so nothing needs creating.

Ordering matters: repairs and migration land **before** the CI job is wired, or the gate's first run
fails on this scope's own files — the worst possible debut for a gate people have to trust.

## Still open

Nothing load-bearing. One informational: whether `docs/findings/` (new, from PR #1054) grows
frontmatter of its own. It has none today and the check covers all of `docs/` by construction.
