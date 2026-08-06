---
title: Committed-Citation Integrity
scope: doc-citation-integrity
created: 2026-08-05
status: locked
dor_status: passed-with-findings
source: issue #1060 (+2 amendment comments); repo-wide frontmatter sweep at `efa1a3e`
appetite: small batch — 2 days
formulation: {technical-architecture: complete, testing-architecture: complete, infrastructure: complete}
features:
  citation-rule: {stage: work-items, tracks: {technical-architecture: complete}}
  citation-check: {stage: work-items, tracks: {technical-architecture: complete, testing-architecture: complete}}
  brief-durability: {stage: work-items, tracks: {technical-architecture: complete}}
  existing-defect-repair: {stage: work-items, tracks: {technical-architecture: complete}}
artifacts:
  design-doc: docs/design/committed-citation-integrity-solution-design.md
  design-brief: docs/briefs/2026-08-06-design-committed-citation-integrity.md
  scope-brief: docs/briefs/2026-08-06-scope-citation-integrity.md
---

# PRD — Committed-Citation Integrity

> **Provenance warning, read before acting.** This PRD was authored by an AI pipeline (`/scope` Stage 1)
> from a defect the operator asked to have filed (issue #1060) plus a repo-wide sweep the pipeline ran
> on its own initiative. Requirements below carry `Origin` + `Ratification`. **Most are
> pipeline-authored and NOT yet user-ratified** — see § 8. Ratification Ledger. Do not treat an
> unratified requirement as a commitment.

## 1. Problem

Six path-valued keys appear in the YAML frontmatter of committed documents under `docs/`:
`source`, `parent-requirements`, `design-doc`, `requirements-brief`, `design-brief`, `scope-brief`.

**Nothing reads them.** `git grep` for those key names outside `docs/` returns zero hits — no script,
no CI job, no application code. `ci.yml` has no doc-lint job of any kind. They are *write-only*
fields: emitted by the authoring pipeline, dereferenced by nobody.

The observable consequence, measured at `efa1a3e`: **8 of 12 such pointers do not resolve in a fresh
clone.** Three name files that have never existed anywhere; one names an untracked local file; two
name paths inside gitignored `.tmp/`; two use relative depths that resolve to nothing and, when
corrected, point outside the repository entirely.

**The framing that matters is not "8 pointers are broken."** It is that a field with no consumer has
no feedback loop, so its decay is monotonic and silent. This is not a hypothesis — it was observed
happening. Site `migration-credential-portability.md:38` was *correct* at `386a6a2` and *fabricated*
at `efa1a3e`, changed by PR #1054: reviewed, CI-green, and whose own final commit message was about
restoring provenance. The rename followed the issue's slug instead of the file's real name. No gate
existed to notice, so none did.

A reader cannot distinguish a true citation from a false one, because the annotations are identical:
`# uncommitted, provenance only` sits on a fabricated referent and a real one alike.

**Prevention over repair.** Repairing the 8 restores a state that has already been demonstrated to
decay within an hour. The repo's own idiom points the other way: `scripts/` holds 8 `check-*.sh`
gates, each with a `check-*.test.sh` companion, wired into `ci-ok`. The repo's working convention is
*if a property matters, a script asserts it; if no script asserts it, the property is not maintained.*
These citation keys were written as though the first held, while living under the second.

### 1b. Boundaries

**Appetite**: small batch — **2 days**. Sized against: 8 line-level repairs, one policy decision
(`docs/briefs/` tracked or ignored), and one check script following an existing 8-script template.
If the citation rule cannot be settled inside that, the fallback is to ship the check for the two
mechanically-decidable key classes and defer the rest.

**Out of scope (non-goals)**:

- **Changing the upstream authoring pipeline.** The emitters live in `alexey-pelykh/.claude`
  (issues #3898, #3901). This PRD governs what *this repo* accepts, not what the pipeline emits.
- **Reconstructing the three never-existed briefs.** Their content is unrecoverable; the decision is
  what the pointer becomes, not how to restore a file nobody wrote.
- **Making the private HQ public, or vendoring it.** `parent-requirements` targets live in a sibling
  repo deliberately kept private. In-repo replication of *specific load-bearing values* is in scope;
  wholesale mirroring is not.
- **A general Markdown link-checker.** Scope is YAML frontmatter path-valued keys. Prose links, code
  fences, and pedagogical examples are explicitly out (they are the false-positive classes).
- **Retroactive audit of non-`docs/` frontmatter.** No such frontmatter exists today; the check may
  cover it by construction, but no requirement asserts it.

## 2. Object Model (OOUX)

| Object | Definition | Key attributes |
|---|---|---|
| **CommittedDocument** | A git-tracked `.md` file under `docs/` carrying YAML frontmatter | path, frontmatter keys, tracked-status |
| **CitationKey** | A path-valued frontmatter key on a CommittedDocument | key name, declared value, owning document, line |
| **Referent** | The artifact a CitationKey names | path, exists-on-disk, git-tracked, inside-repo, gitignored |
| **CitationClass** | The legality verdict for a (CitationKey, Referent) pair | one of: tracked-in-repo · in-band · provenance-note · illegal |
| **CitationCheck** | The mechanical gate that dereferences every CitationKey | invocation, verdict, failing set |
| **Brief** | A pipeline-authored summary under `docs/briefs/` | path, tracked-status, `status:` lock marker |

### CTA inventory

| Object | CTAs |
|---|---|
| CommittedDocument | author · amend · audit |
| CitationKey | write · dereference · reclassify · delete |
| Referent | resolve · fail-to-resolve |
| CitationCheck | run-locally · run-in-CI · pass · fail-with-site-list |
| Brief | author · commit · ignore · orphan |

## 3. Requirements

### Feature: citation-rule

**R-1** — The repository SHALL define, for each path-valued frontmatter key, exactly one legal
CitationClass.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.

**R-2** — When a CitationKey names a Referent that is git-tracked and inside the repository, the
system SHALL classify it `tracked-in-repo` and accept it.
*Origin*: `enrichment-expanded` (generalized from the `design-doc` key, whose 3/3 referents already
satisfy this). *Ratification*: **PENDING**.

**R-3** — When a CitationKey names a Referent that is not git-tracked, the system SHALL reject it,
regardless of whether the Referent exists on the authoring machine.
*Origin*: `AI-inferred-expansion`, grounded in the observed failure — a path-existence test would
have passed `stats-honesty-cross-surface.md:21` on the author's disk and failed it everywhere else.
*Ratification*: **PENDING**.

**R-4** — When a CitationKey names a path inside a gitignored directory, the system SHALL reject it
and SHALL NOT offer a suppression annotation.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.

**R-5** — When the referenced artifact is deliberately outside the repository (the private HQ), the
CommittedDocument SHALL carry a non-path provenance note naming the artifact, and SHALL replicate
in-band any specific value the document's own content depends on.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.
*Note*: `stats-honesty-cross-surface.md:8` already half-does this (`# private HQ … Provenance only`)
while still carrying a path that does not resolve. The requirement is to drop the path, keep the note.

**R-6** — A CitationKey that cannot satisfy R-2, R-5, or an explicit deletion SHALL be deleted rather
than annotated.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.

### Feature: citation-check

**R-7** — The repository SHALL provide a check script that dereferences every path-valued frontmatter
CitationKey in every CommittedDocument and exits non-zero when any fails its CitationClass.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.

**R-8** — The check SHALL determine reachability by git-tracked status, not by filesystem existence.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.

**R-9** — The check SHALL follow the repository's established gate idiom: a `scripts/check-*.sh` with
a `scripts/check-*.test.sh` companion, registered in `ci-ok.needs`.
*Origin*: `enrichment-expanded` from the 8 existing `check-*.sh` scripts. *Ratification*: **PENDING**.

**R-10** — The check SHALL scan YAML frontmatter only, and SHALL NOT scan prose, code fences, or
inline links.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.

**R-11** — When the check fails, it SHALL name every failing site as `{file}:{line}` with the
declared value and the reason, and SHALL NOT stop at the first failure.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.

**R-12** — `CLAUDE.md` § If you touch X, you must also do Y SHALL gain a row binding frontmatter
citation edits to the check, with the check named in its Enforced by column.
*Origin*: `enrichment-expanded`. *Ratification*: **PENDING**.

### Feature: brief-durability

**R-13** — `docs/briefs/` SHALL be either git-tracked or gitignored; the current state, in which it is
neither, SHALL NOT persist.
*Origin*: `user-stated` — issue #1060 defect 2, which the operator asked to have filed.
*Ratification*: **RATIFIED** (operator directed the issue be filed with this defect in it).

**R-14** — Where a Brief carries a durable lock marker (`status: final` / `status: locked`) that a
downstream stage relies on, that marker SHALL survive a fresh clone.
*Origin*: `AI-inferred-expansion`. *Ratification*: **PENDING**.
*Note*: `docs/briefs/2026-08-04-design-stats-honesty-cross-surface.md` carries `status: final # locked`
while untracked. Mitigating: the design doc itself carries `status: locked` in committed frontmatter,
so the lock is currently duplicated, not lost. The requirement is that exactly one durable home exists.

### Feature: existing-defect-repair

**R-15** — All 8 non-resolving CitationKeys identified at `efa1a3e` SHALL be brought into compliance
with R-2, R-5, or R-6.
*Origin*: `user-stated` — the operator selected "All enriched" scope membership.
*Ratification*: **RATIFIED**.

**R-16** — The stale comment `# Stage 2, not yet written` at
`docs/requirements/stats-honesty-cross-surface.md:20` SHALL be removed; the design doc it annotates is
written, merged, and tracked.
*Origin*: `user-stated` (issue #1060 defect 4). *Ratification*: **RATIFIED**.

**R-17** — The `artifacts:` block of `docs/requirements/stats-honesty-cross-surface.md` SHALL declare
every artifact its scope produced, or SHALL declare each absence explicitly.
*Origin*: `user-stated` (issue #1060 defect 5). *Ratification*: **RATIFIED**.

**R-18** — The orphaned brief `docs/briefs/2026-08-04-requirements-migration-scope-and-portability.md`,
referenced by nothing since `efa1a3e`, SHALL be resolved under the R-13 decision rather than left
undecided.
*Origin*: `AI-inferred-expansion` (discovered by the post-merge re-sweep). *Ratification*: **PENDING**.

## 4. Acceptance Criteria

**AC-1 (R-7, R-8, R-11)**
*Given* a CommittedDocument whose `requirements-brief` names a file present on disk but untracked,
*When* the check runs,
*Then* it exits non-zero and names that site with reason `not git-tracked`.
**BUT NOT** passing merely because the file exists locally,
**BUT NOT** stopping before reporting any later failing site,
**BUT NOT** failing a key whose referent is tracked.

**AC-2 (R-4)**
*Given* a CommittedDocument whose `source` names a path under gitignored `.tmp/`,
*When* the check runs,
*Then* it exits non-zero for that site.
**BUT NOT** offering an annotation that would let the citation stay.

**AC-3 (R-10)**
*Given* a CommittedDocument whose prose body mentions `docs/briefs/some-example.md` inside a code
fence or as a pedagogical example,
*When* the check runs,
*Then* that mention is not evaluated.
**BUT NOT** skipping a real frontmatter key in the same file.

**AC-4 (R-15) — the regression test that matters**
*Given* the repository at the commit that resolves this scope,
*When* the check runs against every CommittedDocument,
*Then* it exits zero.
**BUT NOT** exiting zero because zero sites were evaluated — the run SHALL report the count of
CitationKeys checked, and a count of 0 is a failure.

**AC-5 (R-9)**
*Given* a branch that introduces a non-resolving CitationKey,
*When* CI runs,
*Then* `ci-ok` fails.
**BUT NOT** passing because the check ran only locally.

**AC-6 (R-13)**
*Given* a fresh clone,
*When* `git status --porcelain` runs,
*Then* `docs/briefs/` produces no untracked-directory entry.
**BUT NOT** achieving this by deleting briefs that a committed document cites.

## 5. Quality Attributes (Planguage)

```
TAG:     CitationResolutionRate
SCALE:   proportion of path-valued frontmatter CitationKeys resolving to a git-tracked in-repo file
METER:   scripts/check-doc-citations.sh, run on a fresh clone
PAST:    4/12  (33%, measured at efa1a3e)
GOAL:    12/12 (100%)
```

```
TAG:     CitationCheckRuntime
SCALE:   wall-clock seconds for a full-repo run
METER:   time(1) on CI runner
GOAL:    ≤ 5
STRETCH: ≤ 2
```

```
TAG:     FalsePositiveCount
SCALE:   sites the check fails that a human judges legitimate
METER:   manual review of the first full-repo run
GOAL:    0
```

## 5b. Feature Completeness

| Feature | Verdict | Gap |
|---|---|---|
| citation-rule | **NEAR-COMPLETE** | R-5's split between "provenance note" and "in-band replication" needs a per-site decision Stage 2 must make: which specific HQ values are load-bearing here |
| citation-check | **COMPLETE** | — |
| brief-durability | **COMPLETE** | R-13's direction was surfaced and answered: **track**. A-3 resolved 🟢 |
| existing-defect-repair | **NEAR-COMPLETE** | Per-site disposition for the 3 never-existed referents (delete vs reconstruct-as-note) pends the R-1 rule |

## 6. Success Criteria

**Leading indicators**
- `CitationResolutionRate` reaches 12/12 on a fresh clone.
- The check is registered in `ci-ok.needs` and observed failing on a deliberately-broken branch.

**Lagging indicator**
- No new non-resolving CitationKey reaches `main` after the check lands. The current base rate is
  ≥1 per merged PR (1 introduced by #1054 alone), so any occurrence is a regression.

**Decision gate**
- If the first full-repo run produces >0 false positives, narrow the key allowlist rather than adding
  suppression annotations — a suppressible check reproduces the write-only-field failure.

## 7. Cross-Cutting & Non-Functional

- **9.1 Security** — N/A. No credential, auth, or user-data surface. The check reads tracked doc files only.
- **9.2 Compliance & Regulatory** — N/A. Internal documentation hygiene.
- **9.3 Reliability & Observability** — The check's own failure output is its observability surface (R-11). A degenerate pass (0 sites evaluated) is a failure by AC-4.
- **9.4 Performance & Scalability** — Bounded by `docs/**/*.md` count (currently ~20). See `CitationCheckRuntime`.
- **9.5 Operational** — Runs in CI and locally via the existing `scripts/` convention. No deployment, no runtime component.
- **9.6 Lifecycle** — The check is expected to outlive this scope. It has a `.test.sh` companion (R-9) so it is itself regression-covered.

## 8. Ratification Ledger

Per CLAUDE.md § Key Cognitive Triggers — "AI-generated requirement-set is a claim until
provenance-ratified" — the pipeline-authored set below is a CLAIM, not a commitment. The internal
rigor gates (testable, evidenced, traceable) check each requirement's own properties and can never
establish that it originated in a user ask.

**Scope-is-ratification.** The operator's Stage-0 selection was **"All enriched"**, presented as an
explicit three-bucket table that named *the citation rule*, *the design-lock question*, and *the two
`.tmp/` and two `parent-requirements` sites*. That selection **was** the ratification for everything
inside those buckets. Re-ratifying them item-by-item would transfer accountability without
transferring information — the operator holds no fact about, say, whether a git-tracked test beats a
path-existence test that the pipeline does not already hold from the measurement.

| Ratification | Requirements | Basis |
|---|---|---|
| **RATIFIED — direct** | R-13, R-15, R-16, R-17 | Operator directed issue #1060 be filed carrying defects 2, 4, 5 |
| **RATIFIED — by scoping** | R-1 … R-6, R-10, R-11, R-12, R-14, R-18 | Inside the "All enriched" buckets the operator selected. Mechanism choices (git-tracked over path-existence; frontmatter-only scanning; site-list output) are engineering judgment the measurement already settles |
| **RATIFIED — surfaced and answered** | **R-7, R-8, R-9** | Surfaced as a genuine asymmetry (does this repo carry doc-hygiene CI infrastructure?). Operator answered **build `scripts/check-doc-citations.sh`, CI-wired** |
| **RATIFIED — surfaced and answered** | **R-13's direction** | Surfaced as a genuine asymmetry (is a brief re-read after its scope closes?). Operator answered **track them — commit briefs**. This resolves A-3 to 🟢 |

Exactly two decisions were surfaced, and each passed the test *what does the operator know that the
pipeline does not*. Both are now answered, so no requirement remains pending.

### Consequences of the two answers

1. **Briefs are tracked** → the 4 brief citations are repaired under R-2 (cite a tracked file), not
   deleted under R-6. But this only rescues citations whose referent *exists*: 3 of the 4 name files
   that were never written, so those 3 still fall to R-5/R-6 regardless. Committing briefs fixes
   site 4 and prevents the next occurrence; it cannot conjure the three missing documents.
2. **Briefs are tracked** → R-14's design lock gets a durable home by construction, and the lock stops
   being duplicated between brief and design doc.
3. **The check is built and CI-wired** → AC-4 and AC-5 both bind, and the check becomes the mechanism
   that would have caught the `efa1a3e` regression.
4. **Implementation constraint** — the 14 existing briefs live in the MAIN working tree, not this
   worktree. They must be brought in before they can be committed here.

## 9. Assumption Registry

| # | Assumption | Confidence | Cheapest test | Signpost if wrong |
|---|---|---|---|---|
| A-1 | No consumer reads these keys | 🟢 | `git grep` outside `docs/` — **run, zero hits** | A future tool starts parsing them |
| A-2 | The private HQ stays private and unvendored | 🟡 | Ask the operator | HQ is opened or mirrored, making R-5 moot |
| A-3 | Briefs are worth keeping at all | 🟢 **resolved** | Asked the operator — answered **yes, track them** | — |
| A-4 | 3 never-existed briefs are unrecoverable | 🟢 | Absent from disk and `git log --all` — **verified** | A backup surfaces |
| A-5 | The 8-site count is complete | 🟡 | The sweep covered `docs/**` frontmatter only | Frontmatter appears outside `docs/` |
| A-6 | A 2-day appetite fits | 🟡 | Stage 2 sizing | The R-5 in-band-replication analysis proves large |

**A-3 is the one that can invalidate a whole feature.** It is 🔴 and is a question only the operator
can answer.

## 10. Source Traceability

| Requirement | Source |
|---|---|
| R-13, R-15, R-16, R-17 | Issue #1060 body + operator's scope-membership selection |
| R-18 | Post-merge re-sweep at `efa1a3e` (regression discovery) |
| R-1 … R-6 | Frontmatter sweep, 12 sites across 4 committed documents |
| R-7 … R-12 | Consumer search (zero hits) + `scripts/check-*.sh` idiom (8 precedents) |
| R-14 | `docs/briefs/2026-08-04-design-stats-honesty-cross-surface.md` frontmatter |
