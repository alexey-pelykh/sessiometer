---
type: scope-brief
date: 2026-11-04
workflow: /scope
status: final
---

# Scope Brief: Spawn-Env-Scrub Leftovers

## Problem

Committed records still describe the isolated spawn's scrub list, `SPAWN_ENV_REMOVE` (`src/isolated_spawn.rs:250-262`), as it stood before `86ab6aa` (#1009, 2026-08-14) or before `b434419` (#1535, merged on 2026-11-03 through PR #1536). The investigation behind PR #1536 named the GUI/CLI parity PRD's R-13, the spawn-env-scrub spec's Rule 3 comment and the #101 spike script, and a sweep for the same defect found the parity design's SP-1 and the label-integrity PRD's § 2.4 bullet and § 7 row. The maintainer asked on 2026-11-04 to scope those leftovers and prepare to close the session.

## What's In Scope

Must:

1. **#1669** — (chore) scripts: retire the #101 spike probe. The executor deletes `scripts/spike-101-isolated-refresh-probe.sh` in one commit that names `b626007`, the script's only commit, as the place to recover it.

Should (with no appetite set, Should orders the work and cuts nothing):

2. **#1671** — (docs) parity: dated notes beside R-13 and SP-1 saying `SPAWN_ENV_REMOVE` has removed the triple since `86ab6aa`, plus a change-log row.
3. **#1672** — (docs) requirements: dated notes on the label-integrity PRD's § 2.4 bullet and § 7 row saying the list has named `ANTHROPIC_AUTH_TOKEN` since `b434419`, plus the PRD's next change-log row and its two header lines.
4. **#1670** — (docs) specs: Rule 3's comment scoped to tests that read a spawned child, and `# Automated:` lines naming the tests that automate Rules 1–3.

Every item is standalone, with `Related to` lines and no parent. Each item's acceptance criteria are its own. The batch's one success criterion: every surface each item's propagation sweep returns carries a disposition, and each corrected site states the facts as of its own landing commit.

## Key Decisions

A three-panelist council (documentation-guardian, project-manager, security-architect; parallel and independent) settled the run's open decisions on 2026-11-05. It ruled under the maintainer's instruction of 2026-11-02, "go through /council and escalate only if can not be settled", quoted on #1598, where it was given for that issue's decisions; this run applied it to its own open decisions. Nothing was escalated.

1. **The route is Meeting content** (D5, unanimous) — the input was session-extracted action items, each with a path, a line range and its evidence, so no PRD and no solution design were written. The requirements already sit in committed records: R-13, the spec, and the ledger's membership rule for the list.
2. **#1672 stays, standalone** (D1, unanimous) — its sites were not among those the investigation named and the maintainer asked to scope; the class sweep found them. Keeping a small, fully specified correction costs less than leaving two verified-false lines with no tracked carrier. Folding it into the #1550 re-authoring stays the operator's option 2 in the item. The ruling ratifies this run's selection of the two sites. It does not ratify the label-integrity PRD, whose own enumeration stays ratification-pending.
3. **Priorities** (D2, unanimous) — #1671, #1672 and #1670 are Should: each stale passage errs toward caution, calling landed work open, so acting on it costs a redundant check. #1669 stays Must: "do not run it" is no workaround for the reviewer its header addresses.
4. **#1670 keeps the Rules 1–2 `# Automated:` lines** (D3, two to one, held after an adversarial second round) — narrowed to Rule 3, the spec would name `the_oauth_refresh_triple_is_scrubbed_by_name` under Rule 3 but not under Rule 1, which the same test automates. The lines are an add-on the council ratified, not a correction: Rule 2's test (`7038ac5`) predates the spec (`f0cc921`). The dissent held that a correction item should correct only what is stale.
5. **#1669 deletes the script** (D4, unanimous) — nothing in the repository runs or names it, and the spike's findings of record live in `build/version-compat.md` `# Issue #101`. Mirroring, deriving, or guarding the list by hand would each keep a copy that drifts at the next widening (#1626, #1619). A re-runnable probe under an `env -i` allowlist would be a new item that owns its allowlist measurement.
6. **No umbrella** (D6, unanimous) — the four items have no edges between them, and #1008's item list holds capability waves, not record corrections. #1671 and #1670 name #1008 on their `Related to` lines.
7. **Category coverage** (D7, two to one) — seven categories read N/A for a documentation and spike-artifact maintenance scope with no runtime surface, each under its own reason. The dissent: Operational features applies to #1669, because a manual diagnostic is an admin tool, and #1669's AC-1 already owns what it would need.
8. **"Claude Code 2.1.294" in #1669 is provenance** (D8, unanimous) — it dates the static read the in-repo ledger records, and no criterion asks anyone to reach or run that build.
9. **Not selected** — `scripts/spike-466-invalid-grant-scrub-probe.sh` has no `env -u` line and is cited by the ledger, so it is not a sibling. A recurrence guard against documents restating a code constant would change gate paths (`scripts/**` and the `doc-gates` job), outside "those leftovers".
10. **Every item is kept, though each shows dust indicators** — the coverage gate's dust review found that no item has a dependency either way, that #1669 is a single change, and that #1671 and #1672 share a note form. Keeping each is justified. #1669 is the scope's only `scripts/**` change, so merging it into a docs item would put that item through the `rust`-filtered jobs and the gate-change trailer. #1671 and #1672 differ in owner and in landing commit (`86ab6aa` against `b434419`), and only #1672 carries the fold option. Independent leftovers have no edges between them by nature.

**Judgments the gates rest on, each ratification-pending.** Each line says what changes if the judgment is wrong.

- `delegation-reaches-this-scope` — the council's authority is the maintainer's instruction of 2026-11-02, quoted on #1598 for that issue's decisions; this run applied it to its own. If it does not reach this scope, #1672's two sites and #1670's Rules 1–2 add-on have no ratified driver, and the coverage gate's verdict becomes FAIL.
- `ic-only-criteria-fit-a-record-correction-scope` — the batch's success criterion is implementation-tier only, which `success-criteria-evaluation` allows for bug-fix and tech-debt work. Read literally, `scope-quality-gate` Phase 4 wants one validation criterion and blocks until one is added.
- `inline-seams-are-a-validation-plan` — #1669, the Must item, has no titled validation plan; each criterion names its seam and its static check instead. Read as a form requirement, the Must item lacks a plan, which blocks unless Stage 4 records a typed exception.
- `cc-version-in-1672-is-provenance` — "CC 2.1.294" in #1672 sits inside a quoted heading of an in-repo ledger section, the reading council D8 applied to #1669's token. Read as a governed vendor designator, it needs a public anchor or an escape, and blocks.

## Stats

- **Work Items**: 4 in GitHub (`alexey-pelykh/sessiometer`): #1669, #1670, #1671, #1672, each standalone with no parent
- **Ready**: 4/4, each at its second readiness round
- **Gaps accepted**: 0/4. Every advisory the gates raised is on its item under `## Advisories (readiness)`, as a caution for the executor; none is an accepted gap, and no typed exception was recorded
- **Deferred**: 0/4

Counting rule: each item's latest verdict in the Stage 4 ledger (the pipeline's machine-local readiness record, not committed; the verdicts are restated here and each issue carries its own acceptance). Round 1 returned #1669 and #1671 READY and #1670 and #1672 NOT READY, each with three blockers; a repair of every body followed, and round 2 returned all four READY.

Models: the three Stage 3 dispatches and the second readiness round of #1670, #1671 and #1672 ran on Fable 5.1. Every other subprocess of the run, the coverage gates, the council panelists and the adversarial second round, the repairs, the first readiness round and #1669's second, fell back from Fable 5.1 to Opus 5.5 after a 429, and the orchestrator thread ran on Opus 5.5 for most of the run for the same reason. One pass, the remainder verification after the coverage gate, was dispatched on Sonnet 5.5 at medium effort by choice. A verdict's calibration rests on the model that graded it, so each round's model is in the ledger beside its verdict.

## Artifacts Produced

**Assertion**: 2/2 declared artifacts verified, 1 amended-absent, 0 repaired (Step 0's 1/1 over the tracked work items, folded with Step 1.5's assertion of this brief)
**Disposition**: 0 tracked, 1 committed here, 0 ignored, 0 undeterminable, 0 left untracked — of 1 artifact, 0 filed (the brief is the only durable artifact with an in-repo path; `git ls-files --error-unmatch` exited 1 and `git check-ignore -q` exited 1, so it was untracked and is committed in the same change that creates it)

- **PRD** — not written on this route — the input was session-extracted action items (Meeting content), so no requirements document was authored; the requirements already sit in committed records: parity R-13, the spawn-env-scrub spec, and the ledger's membership rule for the list.
- **Solution design** — not written on this route — same clause; the design is the repository's in-place dated-note practice, carried in each item's build-reference line.
- **Feature files** — none — Stage 3.5 was skipped under its skip clause, amended 2026-11-04T23:40:32Z: every item carries a typed-exception escape in its § Implementation Notes (`formulation: skipped — pure text` on #1670, #1671 and #1672; `— deprecation/removal` on #1669), so the doc gates, a diff property or a byte comparison, plus review, grade each one.
- **Scope brief** — `docs/briefs/2026-11-04-scope-spawn-env-scrub-leftovers.md` [committed here] — in the subject of `/scope`'s Step 1.7 point 1 (the tracking-disposition step, which gives each durable artifact a bracketed marker), so it carries a marker like any other durable artifact
- **Work items** — 4 in GitHub (`alexey-pelykh/sessiometer`), standalone — no marker: items have no in-repo path, so they are outside the subject of `/scope`'s Step 1.7

## Next Steps

- `/do-next` — start highest-priority ready item
- `/do-all` — execute all ready items
- Review the four judgments under Key Decisions, starting with `delegation-reaches-this-scope`. Each line says what changes if it is wrong.
- One more site of this scope's class, found after the coverage gate, is filed as #1681 after the run: the comment at `src/isolated_spawn.rs:677-678` says deleting a name from `SPAWN_ENV_REMOVE` "must fail this test". That held from `86ab6aa`, when the list was exactly the six names the test spells out, until `b434419` widened it to eleven. The brief first left the choice between a sibling `(docs) isolated_spawn:` item and a fold into #1670 to the maintainer, who asked on 2026-11-05 for the remaining submissions to go ahead; the sibling form was taken because the site is under `src/**` and owes the `rust`-filtered jobs, the reason this brief keeps #1669 apart from the docs items.

Found on the way, outside this scope, and not acted on:

- #1567 and #1569 say their checks are "the two doc-gate scripts". The `doc-gates` job also runs `./scripts/check-readme-cc-range.sh` and `./scripts/check-test-runner-env-form.sh`.
- This repository's `CLAUDE.md` § Before you push copies the `rust` path filter without `apps/menubar/Sources/CaptureAck.swift` and `build/version-compat.md`. Both are in `.github/workflows/ci.yml`.
- No recurrence guard exists for documents that restate a code constant such as `SPAWN_ENV_REMOVE`. One would change gate paths, so it was not selected (Key Decisions, item 9).
- If #1621's preferred route lands, the first clause of the sentence in the doc comment of `the_harvest_command_scrubs_the_full_credential_set` (`src/paths.rs:3138-3145`), "A live child can only ever demonstrate the absence of variables the ambient environment happened to set", becomes false.
