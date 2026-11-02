---
type: scope-brief
date: 2026-11-01
workflow: /scope
status: final
---

# Scope Brief: Optional Quota Windows

## Problem

Since 2026-10-30 the usage endpoint answers `"five_hour": null` for verified Ultra accounts, and `parse_usage` rejects such a reply whole because it demands a session window and a weekly window together. When the scope began, 15 of the maintainer's 17 accounts read as blind: no sample was written, selection dropped them, and `next_swap` reported the fleet out of capacity while the accounts were healthy. The maintainer called the daemon unusable and wants an urgent fix, plus a parser where every quota window is independently optional, in case an account without a weekly window follows.

## What's In Scope

The items sit under umbrella #1590 and are listed in delivery order. Each group opens with its purpose.

**Tranche 0, the hot-fix.** Purpose: make the daemon usable now, on the maintainer's known path, before the full fix exists.

1. **#1613** (fix) usage: serve a weekly-only reply with its session read as idle on the decision path, and store no sample for it, until #1596 lands. It is committed as `ae1297c`, pushed on `fix/1613-serve-weekly-only-usage-reply`, and in its own pull request, #1617. A local emergency build of it has served on the maintainer's machine since 2026-11-01. #1596 and #1598 retire it.

**Tranche 1, the full fix.** Purpose: retire the hot-fix's costs. The parser, reading model, decision arms and sample store carry an absent window as `None`, and the deploy proves the fleet recovered. The order is the cut order reversed (design AD-23): three precursors, with the ADR amendments before the parse test they move; then the compile unit, the store and the deploy.

2. **#1591** (test) usage: Oracles A to D for the parser fix, landed `#[ignore]`d and proven RED before any fix. The fix's executor cannot edit them.
3. **#1592** (test) usage: the captured null-session reply and its four sibling bodies, frozen and pinned green at the `dimension` seam. It reads #1591's fixtures by name.
4. **#1593** (docs) adr: amend ADR-0013, 0017, 0025 and 0033 in place, so each says "every dimension the account has" where it assumed a fixed pair. Its criteria read the PRD, so the scope pull request merges first.
5. **#1596** (fix) usage: `parse_usage` serves every window a reply carries. A null session window or an absent weekly window reads as `None`, never a fabricated 0. It removes the hot-fix's branch and flags.
6. **#1597** (feat) usage: `Usage`, `LastGood` and `BlindAnchor` take `Option<f64>` per window, `Copy` kept, so the compiler lists every consumer.
7. **#1602** (feat) daemon: every decision arm names its move on an absent window (skip, pass, exclude or name), never a numeric default. #1596, #1597 and #1602 land in ONE pull request.
8. **#1598** (feat) usage-store: write an absent window as `"session":null`, never omitted and never 0. Read, retain and fold such a line without a fabricated statistic.
9. **#1600** (feat) stats: the five sample readers exclude a reading with no session window from every session figure and keep it in the weekly ones. It lands in #1598's pull request.
10. **#1604** (chore) deploy: ship tranche 1 to the maintainer's machine per the runbook and record the fleet recovery readings. It is run by hand on the maintainer's machine and closed on the readings, with no pull request. Its first-step precondition, the D6/D13 record, is met by the council ruling of 2026-11-02 on #1598.

**Tranche 2, the wires and the records.** Purpose: let the status and `stats` wires say "no window" instead of reading as a failed poll. It also amends the warm-up design and writes the deploy rules into the runbook. It follows the fleet's recovery as a small batch, and its circuit breaker is to ship what is done.

11. **#1599** (feat) status: serve "no session window" on the status/watch wire with an omittable absent-windows key, an optional last-known percent on the blind block, the next `STATUS_SCHEMA_VERSION` minor, goldens, and Swift in lockstep. #1594 waits on its key.
12. **#1601** (feat) stats: say "no session window" on the `stats --json` wire (schema 2) and in the table, with the Swift `StatsWire` optional in lockstep. D8 may move it into tranche 1.
13. **#1595** (docs) warmup: amend the warm-up PRD and design in place. The design then tells a missing session window from a stopped session clock by the session fraction, not the reset stamp.
14. **#1603** (docs) deploy: add the both-halves rule (the daemon and the offline verbs replace together) and the fleet recovery check to the daemon-replacement runbook. By the maintainer's answer it lands after the tranche-1 deploy.
15. **#1608** (chore) deploy: ship tranche 2 to the maintainer's machine and record the wire readings. It is run by hand and closed on the readings, with no pull request.

**Deferred by design.** Purpose: render the no-session-window state only after the maintainer classifies it. The panel's design register forbids the pipeline from inventing a token.

16. **#1594** (feat) menubar/status: render the no-session-window account's cells as the design register classifies them. It is not ready until the D0/D15 register row exists and #1599 and #1601 have landed.

## Key Decisions

The run was headless, so every decision below is ratification-pending except the three the maintainer answered on 2026-11-01. One pending decision blocks work and needs the maintainer: the D0/D15 register row (blocks #1594). The D6/D13 record that blocked #1604 exists since the council ruling of 2026-11-02 on #1598. Next Steps repeats both.

1. **Every quota window is independently optional, not only the session window** — The maintainer's direction on 2026-11-01: "we need to make the parser more robust and flexible, maybe eventually they will make some account w/o weekly limits e.g." The reading model therefore carries each absent window as `None`, and the compiler lists every consumer that must name a move. The run rejected a flag beside an unchanged `f64`, because every read must check it or the fabricated value stands. The weekly-less arms follow by symmetry, with no instance to measure against (design AD-4b, author-chosen).
2. **Tranche 1's budget is the human effort-day estimate the coverage gate sized, and a hot-fix exists as tranche 0 (#1613)** — The maintainer, asked on 2026-11-01 whether tranche 1 could stay inside one calendar week, answered: "Bigger budget, but we need a known path to have a hot-fix for the issue as rugiht now sessiometer is unsuable". That cleared the gate's appetite blocker. It also overrode, for the hot-fix only, the PRD's rule that the fabricating shortcut is never the fallback.
3. **The hot-fix deploys first, and the local daemon is rebuilt and restarted** — The maintainer added on 2026-11-01: "The hotfix must happen ASAP and local daemon rebuilt-restarted so that you can continue working". An emergency build served from a local branch before any pull request existed. That build is not #1613's own deploy criterion (AC-6), which calls for a rebuild from the pull request's head before merge.
4. **The runbook text #1603 moves to tranche 2 and no longer gates the tranche-1 deploy #1604** — Asked on 2026-11-01 whether #1603 must gate #1604, the maintainer answered: "Yes, after deploy (Recommended)". #1603 went from Must to Should, and #1604 follows the runbook's current text until #1603 merges.
5. **The hot-fix is the one allowed fabrication, and it is bounded** — It reads an absent session window as idle (`0.0`) on the decision path and stores no sample for it. Until tranche 1 retires it, the status wire and the panel show 0% session, event-log session tokens read 0, and `stats` shows a gap for the account. The usage-gap tracker also counts the poll as a reading, and a window the reply carries but cannot read is served as no window (#1614, a ruling for the maintainer). The hot-fix is invalid if tranche 1 merges and deploys before it. The mechanism is the pipeline's design under the maintainer's order, so it is ratification-pending.
6. **The scope is provisional end to end** — The PRD's readiness verdict is `failed` on one check: every requirement is pipeline-authored and unratified. The `/scope` orchestrator acknowledged that on the maintainer's behalf and recorded it as ratification-pending, not as the maintainer's acknowledgment. The design is not locked, and the design brief stays `status: draft`. The coverage gate passed its second round with actions, each dispositioned on #1590. Its first round failed on two blockers: the appetite, which the maintainer's budget answer cleared, and a `reliability` census default, which the `reliability --json` call below settles.
7. **D0 to D15 stand open, each with the default the items are built on** — PRD § 8 holds D0 to D10 and the design's § 14 holds D11 to D15. "Pending" means ratification-pending: a default the pipeline chose headlessly. No default is ratified.

   | Decision | Default the items are built on | Status |
   |---|---|---|
   | D0 `render-no-window-classification`, with D15 | None. The design register forbids inventing a render token, so #1594 waits. D15 extends D0 to the blind held block, the Stats-tab cell and its spoken summary. | Open, maintainer-owned; blocks #1594 only |
   | D1 `parser-neither-window-decided` | A 200 reply with no window at all stays a hard `UsageParse` error, so the loop skips and never swaps on it. | Pending |
   | D2 `parser-null-versus-absent-key` | `"five_hour": null` and an absent `five_hour` key read identically. One test pins the relation. | Pending |
   | D3 `blind-gate-weekly-only-active` | The ADR-0017 gate reads false on an absent session anchor, and `status` claims no auto-protection for it. Arming on a weekly anchor is a separate ADR amendment. | Pending |
   | D4 `near-limit-fast-poll-weekly-only` | The near-limit band is session-only: not applicable, stated on purpose. | Pending |
   | D5 `census-excludes-absent-window` | ADR-0033's session census excludes a structurally absent window. The sentence lands in #1593. | Pending |
   | D6 `store-readers-version-skew` | Write `"session":null` on the versionless sample line, every window key always written. No pre-#1598 build runs as the daemon against the store after its first null line; #1598 builds a rollup fence that stalls an older daemon's compaction. | Settled by council on 2026-11-02 (comment on #1598): default ratified, amended with the fence and the corrected deployment rule |
   | D7 `mixed-fleet-tie-break` | On a soonest-weekly-reset tie, an absent session-velocity average reads 0.0, the existing "safest landing" rule. | Pending |
   | D8 `stats-wire-rides-first-or-follows` | The reader half rides tranche 1 (#1600). The wire half (#1601) follows in tranche 2. The design recommends riding first. | Open; decide with the design's OAD-3 as one question |
   | D9 `weekly-scoped-binding-cap` | PR #1537 carries its own `weekly_scoped` claim. This work lands first and #1537 rebases. | Pending |
   | D10 `urgent-tranche-wire-interim` | Tranche 1 ships a documented transitional wire state: `session_pct: null` beside present weekly fields, and no `blind_active` block for a weekly-only blind active. The key lands in tranche 2. | Pending; ratify with its signpost (#1599 lands by the end of tranche 1's budget, or D10 is re-ratified) |
   | D11, the wire key's shape | `absent_windows: [WindowKind]`, omitted when empty, emitted only from a served reading. | Author-chosen; pending |
   | D12, `Sample.weekly` optional | The weekly sample field takes the same `null` shape, because the compiler forces the writer to decide it. | Author-chosen; pending |
   | D13, the rollup tiers' shape | Per-axis `Option<HourStat>` and `Option<DayStat>`, omitted when no reading in the bucket carried that axis; every present stat also records `samples: Option<u32>`. | Settled by council on 2026-11-02: default ratified, amended with the per-axis sample count (#1610 absorbed into #1598) |
   | D14, event-field absence | Every reachable per-window event field is optional, and an absent one is an omitted token. | Author-chosen; pending |

8. **Calls the orchestrator made headless, each ratification-pending with the maintainer and never the maintainer's own** — These stand until the maintainer rules.
   - The take-2 `met` rule for deployment readings (#1604, #1608 and the PRD's § 4 row): a reading counts as met only when its goal holds by the second take. A third-take meet is recorded `late` and not met.
   - D6 and D13 gated #1604's first step until a record existed. The council ruling of 2026-11-02 on #1598 is that record: both defaults ratified, D6 amended with a rollup fence and the corrected deployment rule, D13 with a per-axis sample count. The amendments are #1598's to build; #1604 proceeds on the ruling.
   - #1594 is deferred by design until the D0/D15 register row exists.
   - The units of merge: #1596, #1597 and #1602 land in ONE pull request, because the type change cannot compile in parts, and their `Blocked by` edges are commit order. #1598 and #1600 share the next one, because #1598's lift forces the five readers to move. The scope pull request merges before #1593, whose criteria read the PRD. #1613 merges before the compile unit, because #1596 removes its code.
9. **Other calls the items record as ratification-pending** — The items carry these, and the maintainer has not ruled on them.
   - `reliability --json` moves 13 → 14 in the compile unit's pull request. #1602's census gives a blind window with no session axis its own disclosed bucket, `without_session_window`, so the committed exact-partition rule (#484) stands. The earlier no-bucket default broke that rule.
   - A Retry-After swap on a session-less anchor fires but builds no record and shows no `recent_blind_preempt_swap` notice until #1599 restores it with an optional percent. The run never fabricates an integer for it.
   - `status` never prints "auto-protection OK" for a session-less anchor: #1599 adds a fourth, report-only `blind_active_view` arm that reads degraded past the blind gate.
   - Deploys build in a dedicated worktree at the deploy commit and install the signed bundle over the registered path with `ditto`. They never switch the main checkout's branch. The runbook's stop-then-relaunch path is the deploy step (#1604, #1608, #1603); `sessiometer daemon restart`, which runs `launchctl kickstart -k`, is the runbook's shortcut that does not re-register and is never the deploy step.
   - The design recommends that this work lands before PR #1537, which rebases, and that the status wire takes the next free minor relative to PR #1588. The landing order of other authors' pull requests stays the maintainer's call (design AD-22).
10. **Excluded, with the reason each** — Each of these stays out of every tranche; two deferred items follow the list with what reopens them.
    - Fabricating a session value anywhere but the hot-fix (`0.0`, or `session = weekly`): the parser's contract says "never a fabricated 0", the store's gap-honesty rule reserves "no line" for a failed poll, and ADR-0033 governs the census.
    - Reusing `session_pct: null` for "no window": the wire defines it as "the last poll failed", and CLI goldens pin its `n/a`. The new key carries "no window". D10's documented, transitional state is the one exception, and it ends when #1599 lands.
    - Reading `/api/oauth/profile`: it carries no per-account window signal and would open a second egress endpoint, gated by #737.
    - A keep-warm change, a `reliability` rule change, a `log --json` schema change and platform-specific work: none is needed.
    - The private HQ's REQ-USG-B-001 amendment: unreachable from a clone and owed by the maintainer. #1593 checks that the PRD records it as owed.

    Two items are deferred rather than excluded, each with what reopens it. PR #1537's `weekly_scoped` cap claim stays with that pull request (D9) and is reopened if D9 is ruled for the parser rework. Arming ADR-0017's preemptive swap on a weekly anchor is an ADR amendment outside this fix (D3) and is reopened when the maintainer opens it.
11. **Follow-ups filed while scoping, each Related to #1590 and none its child** — #1605 carries each window as one `Option<Window>` (a cleaner end state, kept off the urgent path because it reshapes every `Usage` literal). #1606 reads severity from `limits[]`. #1607 re-points ADR-0013's drifted code pointers. #1609 names the poll failure class on `usage_gap`. #1610 counts samples per rollup axis. #1611 keeps a viable peer visible to selection when its poll fails. #1612 slow-polls a persistently rejected peer. #1614 tells a window the reply lacks from one it cannot read, and needs the maintainer's ruling. #1615 fixes a stale `poll_secs` doc comment. #1616 names one account the same way on every event-log line. The existing #1524 tracks the ADR README's Immutability bullet, which #1593's in-place amendments rely on.
12. **Questions the coverage gate carried to the maintainer, with the defaults the run proceeds on** — Keep #1598 and #1600 as Must, or downgrade both to Should (default: keep both, because the gap stays on every weekly-only account until they land). Accept the hot-fix mechanism and its costs, including #1614 (default: it stands until tranche 1 retires it). Ratify the gate's four exemptions and PRD § 15's three not-applicable rows (default: the scope proceeds under them). All three sit in #1590's body.
13. **Scoping-process calls** — The existence and desirability gate was exempt because the input was an investigation report. Multi-context design mode was skipped on substance: the design lists seven bounded contexts, but they are one binary, one app, one maintainer and a shared kernel. The hierarchy home is a new dedicated umbrella, #1590, on the #1550 precedent. #1600 was split from #1601 by layer, and #1603 from #1604 by deliverable kind. One issue for the whole compile unit was the structurally cleaner choice and stays with the maintainer; the run kept three items.

## Stats

- **Work Items**: 16 in GitHub (`alexey-pelykh/sessiometer`): 15 cohort items (#1591 to #1604, and #1608) plus the tranche-0 hot-fix #1613, under umbrella #1590
- **Ready**: 15/16, counting #1604 as ready pending the D6/D13 ratification; #1603 became READY at its round-5 re-grade of the rows two sibling edits had drifted
- **Gaps accepted**: 4/16
  - #1596: the typed `ACCEPTED_GAP` `weekly-half-of-the-sample-skip-untested`. No test drives the re-keyed sample skip with the weekly window absent, because no such account exists and #1598 rewrites the skip.
  - #1599: `ac-11-guards-regression-not-the-new-panel-state`. No render-level check covers the percent-less blind held block until #1594 lands.
  - #1601: `shared-swift-files-with-1599`. No merge order is declared against #1599, and whichever merges second rebases.
  - #1608: advisories left as awareness for the executor. Step 1 compares `exe=` byte for byte where step 4 accepts elided forms, and the panel reading is the one judgment aspect.
- **Deferred**: 1/16 (#1594, not ready by design until the D0/D15 register row exists)

Counting rule: each item's latest verdict in the Stage 4 ledger (the pipeline's machine-local readiness record, not committed; the verdicts are restated here and each issue carries its own acceptance), with READY-PENDING-RATIFICATION counted as Ready. Gaps accepted counts READY items whose latest verdict leaves a gap or advisory open on purpose. Deferred counts items not ready by design. The umbrella #1590 is a tracking item and is not counted.

## Artifacts Produced

**Assertion**: 5/5 declared artifacts verified, 0 amended-absent, 0 repaired
**Disposition**: 0 tracked, 11 committed here, 0 ignored, 0 undeterminable, 0 filed — of 11 artifacts (tracked + committed here + ignored + undeterminable partition the 11 artifacts; filed counts filings, which overlap ignored and undeterminable)

- **PRD** — `docs/requirements/optional-quota-windows.md` [committed here]
- **Solution design** — `docs/design/optional-quota-windows-solution-design.md` [committed here]
- **Design brief** — `docs/briefs/2026-11-01-design-optional-quota-windows.md` [committed here]
- **Feature files** — 7 at `docs/specs/` [committed here], one per deterministic-output item, named `<slug>.feature.md`: `parser-optional-quota-windows` (#1596), `reading-model-optional-window` (#1597), `sample-store-session-null` (#1598), `status-wire-no-window-key` (#1599), `stats-readers-no-session-window` (#1600), `stats-wire-no-session-window` (#1601), `decision-arms-absent-window` (#1602). The remaining items record their formulation escapes in their own bodies: pre-authored graders, pure text, operator-run measurement, or waiting.
- **Scope brief** — `docs/briefs/2026-11-01-scope-optional-quota-windows.md` [committed here] — in the subject of `/scope`'s Step 1.7 point 1 (the tracking-disposition step, which gives each durable artifact a bracketed marker such as [committed here]), so it carries a marker like any other durable artifact
- **Work items** — 16 in GitHub (`alexey-pelykh/sessiometer`), under umbrella #1590 — no marker: items have no in-repo path, so they are outside the subject of `/scope`'s Step 1.7
- _No `user-stories` document_ — requirements are EARS + OOUX; story-shaped work is tracked items typed `story`.

## Next Steps

- `/do-next` — start highest-priority ready item
- `/do-all` — execute all ready items
- Order for `/do-all`, as the ledger records it: the scope pull request (branch `docs/1590-scope-optional-quota-windows`) merges before #1593 runs, and #1613 (pull request #1617) merges before the compile unit. Then #1591, #1592 and #1593, the compile unit #1596 + #1597 + #1602 as one pull request, #1598 + #1600 as one, and the deploy #1604. Tranche 2 follows the fleet's recovery: #1599, #1601 and #1595, then #1603 after #1604, and #1608 last. #1603 is ready (its round-5 re-grade, Stats above) and runs after #1604.
- Settled on 2026-11-02, unblocking #1604 and through it #1603 and #1608: a council ruled #1598's D6 and D13 at the maintainer's delegation (a comment on #1598). D6 writes `"session":null` on the versionless sample line, amended with a rollup fence and the rule that no pre-#1598 daemon runs against the store after its first null line. D13 keeps per-axis optional bucket stats, amended with a per-axis sample count. The fence's conditions and the count are recorded in the planning set by a records change after this pull request merges.
- Open, blocks #1594: record the D0/D15 register row in `apps/menubar/design/README.md`. It classifies the no-session-window state and its three spoken strings. Also rule on two riders: whether D15 reaches the glance label's stale-anchor claim (`glance-label-unowned`), and how a weekly-less account's cells render (`weekly-less-cells-unclassified`).
