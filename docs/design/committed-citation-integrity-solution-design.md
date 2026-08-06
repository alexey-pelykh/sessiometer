---
title: Committed-Citation Integrity — Solution Design
source: docs/requirements/committed-citation-integrity.md
created: 2026-08-06
status: locked
tracks:
  technical-architecture: complete
  testing-architecture: complete
  infrastructure: complete
---

# Solution Design: Committed-Citation Integrity

**Input PRD**: `docs/requirements/committed-citation-integrity.md` — `dor_status: passed-with-findings`
(proceeded; findings are two NEAR-COMPLETE features this design resolves).
**Tree**: `efa1a3e`. **Requirements**: R-1 … R-18. **Appetite**: 2 days.

## 1. Goals and Drivers

Make every path-valued citation in a committed document resolve in a fresh clone, and make that
property *enforced* rather than *maintained by discipline* — because discipline has already been
measured failing (a correct pointer became fabricated inside one hour, through a reviewed CI-green PR).

## 2. Constraints

- **2-day appetite.** A check script plus 8 line edits. Not a documentation platform.
- **`yq` is already a CI dependency** (`check-ci-ok-needs.sh` requires it, preinstalled on ubuntu runners).
- **Adding a CI job forces two other gates**: `scripts/check-ci-ok-needs.sh` requires the new job in
  `ci-ok.needs`; `scripts/check-gate-change-ack.sh` requires a `Gate-Change-Acknowledged:` trailer on
  any commit touching `.github/workflows/**`.
- **The private HQ stays private.** No design may require it to be readable from a clone.
- **The 14 existing briefs are in the MAIN working tree**, not this worktree.

## 3. Context and Scope

**In**: YAML frontmatter of `.md` files under `docs/`. **Out**: prose links, code fences, inline
Markdown links, non-`docs/` frontmatter (none exists today).

## 4. Solution Strategy

### The load-bearing decision: one invariant, plus a per-key note allowance

The four broken keys were initially framed as four repair problems. They are not. A survey of every
frontmatter citation in the repo — including the 14 briefs this scope tracks, which enter the check's
scan scope the moment they are committed — shows the rule is **one invariant with a per-key rider**:

> **The invariant**: a value that is *path-shaped* MUST be a **repo-root-relative path to a git-tracked
> file**. A value that is not path-shaped is a provenance note, and is legal wherever a note is allowed.

| Key | Note allowed instead of a path? | Why |
|---|---|---|
| `design-doc`, `requirements-brief`, `design-brief`, `scope-brief`, `prd`, `design` | **No** | These exist to *point at an artifact*. A note here is a pointer that points nowhere |
| `source`, `parent-requirements` | **Yes** | Their referents are often inherently unreachable — see below. Both forms occur in-repo today, and both are legitimate |

**An earlier draft of this design classified `source` and `parent-requirements` as note-*only*. Survey
data falsified that**: of 9 path-shaped brief citations, 6 sit on `source:`, pointing at tracked
in-repo files. Making notes mandatory there would have failed 6 correct citations. `source` is
genuinely bimodal — in a *brief* it names the primary document being summarized (a tracked path); in a
*requirements doc* it names the session or scratch file the document came from (a note).

The invariant handles both without special-casing, because it keys on the **value's shape**, not on
the author's intent: `docs/design/foo.md` is path-shaped and tracked → legal. `.tmp/scopes/foo.md` is
path-shaped and gitignored → **illegal**, which is exactly sites 5 and 6. `session /investigate — stats
roster aggregate` is not path-shaped → legal note.

**Why `source` and `parent-requirements` may fall back to a note.** In the two defective requirements
docs their referents are *inherently unreachable*, and no repair changes that:

- `source` records where a document came from — a `/investigate` session, a `.tmp/` scratch file the
  producing operation deletes by design. Scratch is *supposed* to vanish. Writing it as a path claims
  a durability the referent never had.
- `parent-requirements` points into a private sibling repo. Correcting the depth (both current values
  are wrong, and wrong *differently*) yields a path that resolves on the author's machine and nowhere
  else. That is the same failure with better arithmetic.

So the fix is not a better path — it is **declaring that these keys were never path-valued**. They
record provenance, and provenance is prose.

**Alternatives considered**:

| Option | Rejected because |
|---|---|
| Make every key path-valued; delete the two that can't comply | Loses genuine provenance information for no gain. The HQ lineage of `stats-honesty-cross-surface` is real and worth recording |
| Make every key note-valued; drop the check to a lint of prose | Discards the one class that IS mechanically checkable, and `design-doc` is already 3/3 correct — it would demote a working contract |
| Vendor the private HQ into the repo | Out of scope by PRD § 1b, and defeats the reason it is private |
| Allow a `# uncommitted` annotation to suppress | This IS the observed failure. The annotation is what made a fabricated referent indistinguishable from a real one |

### The second load-bearing decision: repo-root-relative is the only legal base

Two base conventions are in use **for the same key, in the same directory**, right now:

| Base | Count | Example | Population |
|---|---|---|---|
| **Repo-root-relative** | 5 | `docs/design/foo-solution-design.md` | all 3 `design-doc:` + 2 briefs |
| Document-relative | 6 | `../design/foo-solution-design.md` | 6 brief citations |

**Decision: repo-root-relative.** Three reasons, in order of weight:

1. **The ratified worked example uses it.** `design-doc:` is the one key the PRD names as done-right,
   and 3/3 of its values are repo-root-relative — verified: all three resolve under `git ls-files`
   root-relative and *none* resolve document-relative. Adopting the other base would mean declaring
   the repo's only correct citation key to be the wrong one.
2. **A citation's meaning stops depending on where the citing file sits.** Moving a document to
   another directory silently breaks every document-relative pointer it carries; a root-relative one
   survives. Given that this scope exists because a *rename* broke a pointer, base-fragility under
   file movement is not a hypothetical concern.
3. **The value is directly usable.** `git ls-files <value>` and `gh` links work on a paste, with no
   mental base resolution.

Rejected: **accept either base** (try root-relative, fall back to document-relative). It sounds
lenient and is actively harmful — a value's meaning would depend on which resolution happened to
succeed, so a typo that accidentally resolves under the other base passes silently. It would also
perpetuate the same-key-two-conventions defect that sites 7 and 8 exhibit and this scope exists to end.

**Consequence, stated plainly**: 6 brief citations must be rebased before those briefs can be
committed. The migration in § 7 is therefore not a pure copy — see there.

### Path-shape detection

The invariant turns on "is this value path-shaped?", so that test is load-bearing. **It cannot be
"contains a `/`".** Drafting this design, a probe using exactly that test produced a false positive on
`source: session /investigate — stats roster aggregate and fleet runway` — prose whose only slash
belongs to a slash-command name. The repo holds a second instance (`scope: GUI/CLI capability
parity — …`), so this is a class, not a one-off.

**Rule**: strip any trailing `#` comment, then take the value's **first whitespace-delimited token**.
The value is path-shaped iff that token ends in a known **file extension** (`.md`, `.yml`, `.yaml`,
`.toml`, `.json`, `.sh`, `.rs`, `.swift`).

**Why the first token and not the whole value.** A first draft of this rule said *"no whitespace in the
value AND ends in an extension."* Tested against the real corpus, it **missed 2 of the 8 defect sites** —
because their values are a real path followed by parenthetical prose:

```
source: .tmp/scopes/migration-credential-portability.md (/investigate + /scope 2026-07-31 → 2026-08-04)
source: .tmp/findings-for-scope.md (/investigate 2026-07-30)
```

Both contain whitespace, so the whole-value rule would classify them as legal notes — a 25% false
negative rate on the exact set the gate exists to catch, and silently, in the permissive direction.

The first-token rule was then run against all 8 `source:` / `parent-requirements:` values in
`docs/requirements/` and **discriminates perfectly**: the 4 defective ones classify as PATH-SHAPED
(`.tmp/scopes/…`, `.tmp/findings-for-scope.md`, `../hq/…`, `../../hq/…`), the 4 legitimate notes as
notes (first tokens `issue`, `operator`, `session`, `session-context`).

It also buys the ergonomics the note form needs: **a note may name its referent without the check
demanding the file be tracked**, because the filename is not the first token —
`parent-requirements: private HQ (prd-stats.md), REQ-STA-* family` is a legal note. That is what makes
"convert to a note" a real option rather than a euphemism for deleting the information.

§ 10 T11 and T15 pin both directions.

### In-band replication (R-5)

A note names its referent but carries no content. Where a committed document's own
reasoning *depends* on a specific value from the private HQ, that value is replicated in-band at the
point of use — not left behind a pointer no reader can follow.

Scope check: `stats-honesty-cross-surface.md` already does this (it carries `REQ-STA-*` identifiers
and their content inline; its § 0 restates the parent requirements it derives from).
`migration-credential-portability.md` needs an audit for the same property. **No new replication work
is expected** — this is a verification step, not an authoring step, which is what keeps R-5 inside the
appetite.

## 5. Building Blocks

| Component | Kind | Feasibility |
|---|---|---|
| `scripts/check-doc-citations.sh` | bash + yq + git | **FEASIBLE** — precedent: 8 sibling scripts; `check-ci-ok-needs.sh` already parses YAML with yq |
| `scripts/check-doc-citations.test.sh` | bash, throwaway git repo | **FEASIBLE** — precedent: `check-gate-change-ack.test.sh` builds a fixture repo in `mktemp -d` and asserts exit codes |
| `doc-gates` CI job (unfiltered) | GitHub Actions | **FEASIBLE** — precedent: `gate-change-ack` job, which likewise hosts multiple script steps |
| 8 site repairs + 2 hygiene fixes | line edits | **FEASIBLE** |
| 14-brief migration | `cp` + 6 rebases + `git add` | **FEASIBLE** — the files are in another working tree (Risk R-2); the 6 rebases are mechanical and all referents already tracked (§ 7) |

### `check-doc-citations.sh` contract

```
Usage: ./scripts/check-doc-citations.sh [root]     # default: docs

Exit 0  every citation legal AND at least one evaluated
Exit 1  any citation illegal, OR zero citations evaluated
```

Behaviour, per requirement:

- **R-8** — reachability is `git ls-files --error-unmatch <path>`, never `test -e`. A path-existence
  test passes an untracked local file on the author's machine and fails it in CI; that non-determinism
  IS the defect.
- **R-10** — reads only the frontmatter block (between the first `---` and the next `---`). Prose,
  code fences, and inline links are never parsed.
- **R-11** — collects all violations, prints `{file}:{line}  {key}: {value}  → {reason}`, exits once
  at the end. No stop-at-first.
- **AC-4 (degenerate-subject guard)** — prints `evaluated N citations across M documents`. **N == 0
  exits 1.** A gate that passes because it looked at nothing is not evidence; this is the same class
  of failure as the write-only field it exists to prevent, one level up.

Rejection reasons are distinct strings so the test can assert on them:
`not-git-tracked` · `outside-repo` · `gitignored` · `note-in-pointer-key` · `not-root-relative`.

**Key allowlist** — explicit, because a "scan every key" implementation misreads prose. Verified
against every frontmatter key present in `docs/` today:

| Keys | Class |
|---|---|
| `design-doc`, `design-brief`, `requirements-brief`, `scope-brief`, `prd`, `design`, `scope-working-doc` | **Pointer** — must hold a tracked root-relative path |
| `source`, `parent-requirements` | **Pointer-or-note** — a path must be tracked; a note is legal |
| `type`, `date`, `status`, `workflow`, `umbrella`, `items`, `scope`, `title`, `created`, `tracks`, `dor_status` | **Ignored** — never a path |

`umbrella: 748`, `items: 11 (#1028–#1038)`, and `scope: GUI/CLI capability parity — …` are why the
third row is enumerated rather than assumed: each would be misread by a scan-everything implementation,
and `scope:` would additionally trip a naive path-shape test. **Unknown keys are ignored, not failed**
(see Risk R-5) — the allowlist is the contract, and adding to it is the deliberate act R-12 registers.

**`scope-working-doc` is in the list because omitting it hid a defect.** An earlier allowlist was built
by surveying `docs/briefs/` and `docs/requirements/`'s known keys; it missed this one, and the site it
guards — a committed PRD citing gitignored `.tmp/` scratch — was therefore absent from the scoped
defect table. The lesson is structural, not clerical: **an allowlist built from a partial survey has a
blind spot exactly the size of what the survey missed**, and a check built on it reports clean over
that blind spot. The allowlist above is derived from a sweep of *every* frontmatter key in *all* of
`docs/` (64 distinct keys, 9 of them path-valued), not from the keys the defect table happened to name.

### CI wiring

A **new job**, not a step inside `gate-change-ack`. That job is PR-range-scoped (`BASE_SHA`/`HEAD_SHA`,
`fetch-depth: 0`) because trailer checks inspect a commit range; this check inspects the *tree*, so it
needs neither. Hosting it there would couple a whole-tree check to PR-range plumbing it does not use,
and would silently disable it on `push` builds.

**Name it `doc-gates`, not `doc-citations`, and host the citation check as its first step.** Issue
\#1056 is open and its acceptance criteria commit a second docs gate to CI ("Wired into CI, with the
`Gate-Change-Acknowledged:` trailer"). A plural job lets that one land as a step — no second
`ci-ok.needs` edit, no second entry in the job graph. This is not speculative generality: the second
consumer is already specified and tracked. Precedent in-repo: `gate-change-ack` hosts two script steps.

**The job carries NO path filter — it runs on every PR.** This is the load-bearing wiring decision.
`ci-ok` is `if: always()` and its rollup treats `skipped` as a **pass** (only `failure`/`cancelled`
fail). So a path-filtered gate that skips is indistinguishable from one that ran and passed — a gate
that evaluated nothing, reported green. That is precisely the degenerate-subject failure AC-4 guards
*inside* the script, recurring one level up in the CI graph, and it is not hypothetical: this repo has
**no `docs` filter** at all (only `rust`, `swift`, `formula`), so there is no correct filter to reuse.
The check is a bash pass over `docs/` — filtering it buys nothing and risks exactly the silence it
exists to break.

Consequences, all three mandatory and all three easy to forget:

1. Add `doc-gates` to `jobs.ci-ok.needs` — else `ci-ok-needs-complete` fails.
2. The commit touching `.github/workflows/ci.yml` carries
   `Gate-Change-Acknowledged: adds an unfiltered docs-only tree check; widens the gate, weakens nothing`.
3. Adding `scripts/check-doc-citations.sh` **triggers the full Rust job suite** — `scripts/**` is in
   the `rust` path filter, deliberately ("broad but conservative-safe"). Expected, not a defect; worth
   knowing so a long first CI run is not misread as a hang.

## 6. Per-Site Disposition

All 8 sites, verified at `efa1a3e`. Line numbers are from that tree.

| # | Site | Now | Action | Req |
|---|---|---|---|---|
| 1 | `panel-presentation-reference-coverage.md:24` | `design-brief:` → never existed | **DELETE the key** | R-6 |
| 2 | `panel-presentation-reference-coverage.md:25` | `requirements-brief:` → never existed | **DELETE the key** | R-6 |
| 3 | `migration-credential-portability.md:38` | `requirements-brief:` → never existed | **REPAIR** to `docs/briefs/2026-08-04-requirements-migration-scope-and-portability.md` + track that brief | R-2 |
| 4 | `stats-honesty-cross-surface.md:21` | `requirements-brief:` → untracked | **TRACK the brief**, drop the `# uncommitted, provenance only` comment | R-2 |
| 5 | `migration-credential-portability.md:7` | `source:` → gitignored `.tmp/` | **CONVERT to note** | R-5 |
| 6 | `panel-presentation-reference-coverage.md:7` | `source:` → gitignored `.tmp/` | **CONVERT to note** | R-5 |
| 7 | `migration-credential-portability.md:10` | `parent-requirements:` → absent, wrong depth | **CONVERT to note** | R-5 |
| 8 | `stats-honesty-cross-surface.md:8` | `parent-requirements:` → absent, wrong depth | **CONVERT to note** (it is half-converted already — keep the prose, drop the path) | R-5 |

**Site 3's repair rests on a verified hypothesis, not a guess.** The orphaned brief
`docs/briefs/2026-08-04-requirements-migration-scope-and-portability.md` carries
`type: requirements-brief` and `source: docs/requirements/migration-credential-portability.md` — it
points at exactly the PRD whose `:38` cites a non-existent file. PR #1054 renamed the pointer to
follow the *issue* slug rather than the *file* name. Restoring the real name is a repair, not a
reconstruction, and it simultaneously resolves R-18 (the orphan stops being an orphan).

**Sites 1 and 2 are deletions, not repairs, and that asymmetry is deliberate.** Tracking briefs makes
brief citations legal *when the brief exists*. These two name documents that were never written; there
is nothing to track. Inventing plausible briefs to satisfy the pointers would manufacture provenance —
the exact defect, committed deliberately.

Plus two hygiene fixes: drop the stale `# Stage 2, not yet written` at
`stats-honesty-cross-surface.md:20` (R-16), and complete that file's `artifacts:` block (R-17).

### Two further sites the scoped table missed — found by sweeping, not by reading it

| # | Site | Now | Action |
|---|---|---|---|
| 9 | `migration-credential-portability.md:39` | `scope-working-doc:` → gitignored `.tmp/` | **DELETE the key** |
| 10 | `docs/design/stats-honesty-cross-surface-solution-design.md:3` | `source:` → document-relative | **REBASE** to repo-root-relative |

Neither was a clerical oversight; each names a way a sweep's *subject* can be smaller than its *claim*:

- **Site 9 was invisible to the key list.** `scope-working-doc` was not recognised as path-valued, so
  the sweep never asked about it. A survey iterating a key list cannot see a defect on a key absent
  from that list — the allowlist blind spot described in § 5.
- **Site 10 was invisible to the directory scope.** The original sweep covered `docs/requirements/`;
  `docs/design/` frontmatter was never examined at all. The finding "8 sites" was true of the subject
  actually evaluated and false of the population it was reported about.

Both are why the check scans **all of `docs/`** by construction rather than a curated file list, and
why AC-4 reports the evaluated count: a number the reader can compare against the corpus is the only
thing that distinguishes "swept clean" from "swept narrowly."

## 7. Brief Migration

The 14 briefs live in `/Users/alexey-pelykh/Sessiometer/sessiometer/docs/briefs/` (main working tree).
They are untracked, and `docs/briefs/` carries **no ignore rule** — `git check-ignore` exits 1. So
nothing needs un-ignoring; the directory was simply never added.

**It is not a pure copy.** Committing the briefs moves them inside the check's scan scope, so their
own citations must satisfy the invariant first. Surveyed state of the 14:

| Group | Count | Action |
|---|---|---|
| Path-shaped, **document-relative** (`../design/…`, `../requirements/…`) | 6 citations across 5 files, on `source` / `prd` / `design` | **Rebase to repo-root-relative** before committing |
| Path-shaped, repo-root-relative | 2 | Already conformant |
| Prose notes (`session /investigate …`, `session context (#817 thread)`) | 2 | Already conformant — legal on `source` |
| No frontmatter at all | 2 files | Nothing to check; contribute 0 citations |

The 6 rebases are mechanical (`../` → `docs/`) and all 6 referents are already tracked, so no
referent needs creating. The two frontmatter-less briefs are a reminder that the check must tolerate
a document with no frontmatter without counting it as a failure *or* as an evaluated citation.

Sequence: copy the 14 into this worktree → rebase the 6 citations → `git add` them explicitly (never
`git add -A`) → commit. This scope's own brief lands in the same directory at Completion and is
committed with them, which makes this run the first instance of the convention it establishes.

**Ordering constraint**: the briefs must be committed (and rebased) **before** the CI job is wired,
or the job's first run fails on the very files this scope is landing. § 6's site repairs are subject
to the same ordering.

## 8. Risks

| # | Risk | L×I | Mitigation |
|---|---|---|---|
| R-1 | The check's frontmatter parser mis-handles an edge case (multi-doc YAML, `---` inside prose) and under-reports | 2×3=6 **MED** | AC-4's non-zero-count assertion catches wholesale failure. Test fixture includes a `---` horizontal rule in prose |
| R-2 | Copying 14 briefs from the main tree picks up a file that was deliberately left uncommitted | 2×2=4 **MED** | Enumerate explicitly; all 14 verified `.md`, no other file types present |
| R-3 | `parent-requirements` conversion loses information a future reader needs | 2×2=4 **MED** | R-5's in-band audit runs before conversion; the note keeps the artifact name |
| R-4 | The new CI job is added but omitted from `ci-ok.needs`, so it cannot block | 1×3=3 **LOW** | `ci-ok-needs-complete` mechanically prevents exactly this |
| R-5 | A future doc adds a key class the check does not know | 2×2=4 **MED** | Unknown keys are ignored, not failed — the check's allowlist is explicit. Accepted: a new key is a deliberate act, and the CLAUDE.md row (R-12) is where it gets registered |
| R-6 | The CI job is wired before the briefs are rebased and the sites repaired, so its first run fails on this scope's own files | 3×2=6 **MED** | § 7's ordering constraint: repairs and migration land before the job. Cheap to detect, cheap to fix — but it would make the gate's debut a red build, which is the worst possible introduction for a gate people must trust |
| R-7 | The path-shape test rejects a legitimate value whose form nobody anticipated (a path with no extension, a URL) | 2×2=4 **MED** | T11 pins the known false-positive class. A URL is not root-relative and would be reported — accepted, since no citation key holds a URL today; if one appears, it is an allowlist decision (R-5) |

**No HIGH risks.** 10x test: the largest component is the check script; if it took 10× longer, the 8
site repairs and the brief migration still deliver independently — R-15/R-16/R-17 do not depend on R-7.

## 9. Crosscutting

- **Security** — N/A. Reads tracked doc files; no credentials, no network.
- **Observability** — the check's failure output is the whole surface (R-11). A zero-count run is a
  failure, not a silent pass.
- **Testing** — see § 10.
- **Error handling** — missing `yq` exits 1 with an install pointer, matching `check-ci-ok-needs.sh`.

## 10. Master Test Plan (abbreviated — proportionate to appetite)

**Risk surface (ACC)**: one component, one capability — *does the gate correctly classify a citation?*

`scripts/check-doc-citations.test.sh`, following `check-gate-change-ack.test.sh`'s shape (throwaway
`git init` repo in `mktemp -d`, `pass`/`fail` counters, exit-code assertions):

| Case | Fixture | Expect |
|---|---|---|
| T1 | pointer key → tracked root-relative path | exit 0 |
| T2 | pointer key → file on disk but **untracked** | exit 1, `not-git-tracked` — *the discriminating test; a path-existence implementation passes this and is thereby falsified* |
| T3 | pointer key → nonexistent file | exit 1, `not-git-tracked` |
| T4 | pointer key → gitignored path | exit 1, `gitignored` |
| T5 | pointer key holding a prose note | exit 1, `note-in-pointer-key` |
| T6 | `source` holding a tracked path | exit 0 — *the bimodality; a note-only rule fails this and is thereby falsified* |
| T7 | `source` holding a prose note | exit 0 |
| T8 | pointer key → **document-relative** path that resolves under the other base | exit 1, `not-root-relative` |
| T9 | path-shaped string in **prose / code fence** | exit 0 — not scanned |
| T10 | two bad sites | exit 1, **both** reported |
| T11 | note containing a slash (`session /investigate — …`, `GUI/CLI parity`) | exit 0 — *the false positive this design's own drafting probe produced* |
| T12 | document with **no frontmatter** | exit 0, contributes 0 to the count |
| T13 | zero documents | **exit 1** — degenerate subject |
| T14 | `---` horizontal rule inside prose body | exit 0 — frontmatter boundary not confused |
| T15 | gitignored path **followed by parenthetical prose** (`.tmp/x.md (/investigate 2026-07-30)`) | exit 1, `gitignored` — *falsifies the whole-value path-shape rule, which missed 2 of the 8 real defect sites this way* |
| T16 | note whose **later** token is a filename (`private HQ (prd-stats.md), REQ-STA-*`) | exit 0 — a note may name its referent without the file having to be tracked |

**Five tests carry the design's load, and each falsifies a specific wrong implementation**: T2 kills
path-existence; T6 kills the note-only rule for `source`; T11 kills the contains-a-slash detector;
**T15 kills the whole-value detector**; T13 kills a gate that passes on nothing. A suite without them
goes green on every defect this design exists to prevent — including **three** that were live in its
own drafts, two of which were caught only by running the candidate rule against the real corpus rather
than reasoning about it. Test the detector against the corpse, not against the fix.

**Quality gate**: `doc-gates` in `ci-ok.needs`, unfiltered.

## 11. Requirement-to-Track Coverage

| Req | Covered by | Status |
|---|---|---|
| R-1 | § 4 two-class table | covered |
| R-2 | § 4 path-valued contract; § 6 sites 3, 4 | covered |
| R-3, R-8 | § 5 contract (`git ls-files`); § 10 T2 | covered |
| R-4 | § 5 (`gitignored` reason); § 10 T4 | covered |
| R-5 | § 4 note-valued class + in-band replication; § 6 sites 5–8 | covered |
| R-6 | § 6 sites 1, 2 | covered |
| R-7, R-9 | § 5 components; § 5 CI wiring | covered |
| R-10 | § 5 (frontmatter-only); § 10 T7, T10 | covered |
| R-11 | § 5 (collect-all); § 10 T8 | covered |
| R-12 | § 12 below | covered |
| R-13 | § 7 brief migration | covered |
| R-14 | § 7 — tracking the design brief gives the lock a durable home | covered |
| R-15 | § 6 all 8 sites | covered |
| R-16, R-17 | § 6 hygiene fixes | covered |
| R-18 | § 6 site 3 — the orphan becomes the referent | covered |
| AC-4 | § 5 degenerate-subject guard; § 10 T9 | covered |

No UNCOVERED entries. No PHANTOM elements — every component traces to a requirement above.

## 12. CLAUDE.md Amendment (R-12)

Add to § If you touch X, you must also do Y:

| If you touch | You must also | Enforced by |
|---|---|---|
| A path-shaped frontmatter value in `docs/**` | Make it a **git-tracked, repo-root-relative** path — or, on `source` / `parent-requirements` only, replace it with a prose note | `scripts/check-doc-citations.sh` |
| The `doc-gates` CI job | Leave it **unfiltered**. `ci-ok` counts `skipped` as a pass, so a path filter turns a miss into a silent green | — (convention; stated here because nothing mechanical catches it) |

## 13. Open Questions

None load-bearing. One informational: whether `docs/findings/` (added by PR #1054) will grow
frontmatter keys of its own. It has none today, the check covers all of `docs/` by construction, and
nothing is blocked either way.
