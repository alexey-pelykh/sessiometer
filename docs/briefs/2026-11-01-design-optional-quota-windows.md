---
type: design-brief
date: 2026-11-01
source: docs/design/optional-quota-windows-solution-design.md
workflow: /design-solution
status: draft
---

# Design Brief: Optional Quota Windows

## Problem

On 2026-10-30 the usage endpoint began returning `"five_hour": null` for verified Ultra accounts —
the five-hour session window is removed per account, automatically, and the weekly window stays. The
daemon's parser treats both windows as mandatory, so it rejects such a reply whole: the account gets
no sample, renders `n/a`, drops out of target selection, and `next_swap` reports
`no_viable_target cause weekly`. Fifteen of the operator's seventeen accounts have read as blind
since, on a fleet that is healthy. A hot-fix (tranche 0, #1613) restores their weekly readings ahead of the design below, which replaces it. The operator's two messages (PRD § 2.1) ratify the class of work
and its shape: support these accounts urgently, and make **every** window independently optional —
a weekly-less account is the explicit future case.

## Key Decisions

Every decision below is **ratification-pending**: the PRD's readiness gate failed on check 6
(pipeline-authored, unratified requirements), the acknowledgment to proceed was given by the `/scope`
orchestrator on the operator's behalf, and the design is carried as provisional. Rows marked
**author-chosen** had no pipeline recommendation in the PRD and are the ones to read first. The exception is decision 16: the maintainer decided its tranche 0, its budget and its runbook-text placement on 2026-11-01.

1. **An absent window is `None`, carried by the type** — `Usage`, `UsageReport`, `LastGood` and
   `BlindAnchor` take `Option<f64>` per window, `Copy` kept (AD-1). The compiler then enumerates every
   consumer (33 + 17 production errors measured); a flag beside an `f64` must be consulted at all 93
   reads or the fabricated value stands, and a sentinel `0.0` is byte-identical to an idle window. One bounded exception: the tranche-0 hot-fix serves a weekly-only reply with a `0.0` session and stores no sample for it (decision 16).
2. **The parser composes per-window Options from the unchanged `dimension` lookup**; a 200 reply with
   *neither* window stays a hard `UsageParse` (D1 option a), and `"five_hour": null` is treated
   identically to an absent key (D2) (AD-2). Two invariants are pinned by tests: a reset stamp implies
   its fraction; a served reading carries at least one window (AD-3).
3. **Every consumer takes one of four named moves — skip, pass, exclude, name — and never a numeric
   default**, in tests as well as production; each decision arm owes one test over a reading whose
   window is `None` (AD-4; B-3 enumerates fifteen arms with their move).
4. **The blind gate reads false on an absent anchor** through its single pure predicate, so the
   three callers cannot drift; `status` makes no auto-protection claim for a weekly-only active.
   Whether a *weekly* anchor should arm the swap is left open (AD-5; OAD-2 / D3).
5. **Transitional wire state before the new key lands (D10)**: under schema `1.14` a served
   weekly-only reading carries `session_pct: null` (in place of the hot-fix's `0`) beside present weekly fields, the `session_pct`
   doc comment is amended to name the transition, and — **author-chosen** — a weekly-only active that
   goes blind gets **no `blind_active` block**, because the block cannot be emitted under `1.14`
   without a `u8` and a `0` would be the fabrication. Cost stated precisely: for those days such an account is indistinguishable from an ordinary failed-poll row on both surfaces (blindness reaches them only through that block), duration lost as well as percent, and its Retry-After and velocity report arms are silent — bounded by two facts (no regression against the bundle that ran before the hot-fix, since no weekly-only account had held a `last_good` — the hot-fix itself holds such an anchor's session as `0.0`; and the captured reply is the operator's active account, so the cost lands there) and by a signpost: S6 lands by the end of tranche 1's budget, or is pulled into tranche 1 under the extend-with-justification breaker, or D10 is re-ratified (AD-6). A healthy weekly-only row costs less than it looks: a failed poll nulls both windows, so the weekly cell tells no-window from no-reading before the key lands.
6. **The wire says "no window" with one omittable per-account key, `absent_windows: ["session"]`**
   (`Vec<WindowKind>`, emitted only from a served reading, omitted when empty), so a weekly-less
   account rides the same key with no second bump; Swift decodes it with `decodeIfPresent ?? []`
   — **author-chosen** (AD-7 / D11; alternatives: a boolean, a per-window enum).
7. **`BlindActive.last_known_session_pct` becomes optional and is omitted when absent**, exactly the
   `rotated` reshape; Swift `UInt8?`; the CLI `~N%` cell falls back and the blind sentence drops the
   percent (AD-8).
8. **The minor is chosen at landing under the queue rule** (`1.15` if S6 lands before PR #1588, `1.16`
   after); the five goldens are regenerated once, two *new* golden rows are added, and the Swift
   fixture sweep (27 + 14 sites plus the `"minor":15` future frame) moves in lockstep (AD-9).
9. **The sample line writes `"session": null` — the key never omitted** (#1580's exact-key-set oracle),
   and **author-chosen**: `Sample.weekly` takes the same shape because the writer is compiler-forced
   to decide the weekly axis too (AD-10 / D12).
10. **Rollup tiers carry per-axis `Option<HourStat>` / `Option<DayStat>`, omitted when no sample in
    the bucket carried that window**, so the fold never reaches `-∞`; counts stay counts. Stated
    asymmetry: an older binary drops a raw `null` line *silently* but refuses such a rollup *loudly*
    — **author-chosen** (AD-11 / D13).
11. **`stats` excludes an absent window from every session figure and from the census (D5)**; on its wire `session`, `band` and the session-cap counts `cap_hits` / `time_at_cap_secs` become omittable under `JSON_SCHEMA_VERSION` 1 → 2, Swift's
    `StatsDim` goes optional, the one golden is regenerated, and the CLI table reuses its own `—` UNKNOWN sentinel for the session, signal, velocity, runway and cap columns (it then does not distinguish no-window from never-observed; only the wire does). Field set **author-chosen** (AD-13); whether the wire half rides in tranche 1
    is OAD-3 / D8 (recommendation: ride first — small cost, no queue contention).
12. **Event fields carry typed absence as an omitted token**; `reliability`'s parsers tolerate it;
    no `log`/`reliability` schema move expected, named if a wire field reshapes — **author-chosen**
    (AD-14 / D14).
13. **Oracles A–D land `#[ignore]`d and RED first**, proven by a filtered `--ignored` run, never
    `--include-ignored`; the scoping party re-proves RED (AD-18).
14. **The records move before the pinned test does**: four ADRs amended in place (keep `date:`, add
    an `**Amended**` paragraph, update the README row), the parser doc rewritten, the HQ
    `REQ-USG-B-001` amendment recorded as owed by the operator (AD-19); the warm-up design's C-1
    reads two states from the fraction and the stamp (AD-20); the daemon-replacement runbook gains
    the both-halves rule and a fleet recovery check (AD-21); that text is #1603, now in tranche 2 and landing after the deploy (AD-23).
15. **Open-PR composition**: this design lands first and #1537 rebases (its D9 stays its own); S6 takes the next free minor relative to #1588; #1538 composes on the blind seam — **author-chosen**: the landing order of another author's open PR is the operator's call, so this is a recommendation, not an order (AD-22).
16. **A hot-fix comes first, then the tranche order is the cut order reversed** (AD-23; decided by the
    maintainer on 2026-11-01). **Tranche 0** is the hot-fix #1613, outside the twelve blocks: it serves a
    weekly-only reply with a `0.0` session (a private flag marks it), stores no sample for it, and is
    deployed to the operator's machine before it merges. It costs 0% session on the wire and the panel,
    event-log session tokens that read 0 (so `reliability` reads them as idle sessions over that window),
    and no sample for such an account — a gap in `stats`, never a zero. Tranche 1 retires all three
    (#1596 removes the flag, #1598 writes `"session":null`), and the hot-fix is invalidated if tranche 1
    deploys first. **Tranche 1** is budgeted at the human effort-day estimate the coverage gate sized (PRD
    § 3.1), no longer one calendar week: oracles → the one compile unit (parser, model, arms, fixtures,
    store, records) → transitional wire → deploy (#1604). **Tranche 2**: wire key → stats wire → warm-up
    amendment ‖ render cells, plus the runbook text (#1603), moved here and landing after the deploy. With
    the hot-fix deployed, weekly readings recover at tranche 0; tranche 1's deploy newly turns on the
    `"session":null` sample line and `session_pct: null` in place of the hot-fix's 0.
17. **S11 is specified as a waiting item with three D0 options and its gates, and the design picks no
    token**; D15 extends D0 to the panel's Stats-tab cell and its spoken summary, and to the blind held block that S6's reshape forces on the panel (the `BlindMeter` reads `lastKnownSessionPct` as a value), so S6's panel render half waits on D0 while its decode half lands (AD-24); until D0 classifies it, the panel renders a decoded percent-less `blind_active` block as no block — today's two-meter `n/a` row — at the stated cost that the blind duration and verdict stay invisible on the panel alone (the CLI footer shows them).
18. **The weekly-less arms are decided by symmetry** — a weekly-less account is never weekly-blocked,
    `use` returns `Viable`, it is never `weekly_exhausted` — compiler-forced once `Usage.weekly` is
    `Option`, for an account class with no observable instance today — **author-chosen** (AD-4b).

## Design Tracks

| Track | Approach | Key Trade-off |
|---|---|---|
| Technical Architecture | Per-window `Option` lift; fifteen enumerated decision-arm moves; three absence representations chosen by each surface's own precedent (omittable wire key, `null` sample key, omitted `stats` block) | Chose the compiler-enumerated lift over a flag or sentinel; left the cleaner `Option<Window>` struct as a follow-up (#1605) because it reshapes every `Usage` literal and the `Copy` decision type on the urgent path |
| UX / IA | Generative mode for one surface class (the no-window account on the panel row, the CLI cells, the Stats tab); inventory and state coverage complete; **blocked on D0** with three options and their gates specified | Accepts a documented, dated transitional `n/a` for days rather than inventing a token against the panel register |
| Testing Architecture | Unit-heavy pyramid; ten capabilities (Cap-1…Cap-10); Oracles A–D executor-uneditable; every existing oracle to keep green or move named; the five `test` steps, `msrv`, both `deny` checks, `swift`, an armed `panel-goldens` run and the three trailers as gates | Seven requirements are non-testable by code (documentation, procedure) and are graded by the doc gates and a manual fleet check |
| API Design / Data Architecture | Folded into Technical (single-binary internal contracts; precedent `docs/design/daemon-diagnostic-integrity-solution-design.md`); § 8 states every wire contract and version move | None — kept the contracts in one place rather than splitting two short tracks out |

## Ratification standing

- **Readiness**: `dor_status: failed` (check 6 only). Acknowledged by the `/scope` orchestrator on
  the operator's behalf, not by the operator; recorded as ratification-pending. Every stop in the
  design-solution command was pipeline-decided and is listed in the design's § 14.
- **Product lens** (`product-strategist` agent): **APPROVE WITH CONCERNS** — "The design builds exactly the class and shape the operator's two messages ask for, covers all 46 PRD requirements (verified by ID set, not taken from the matrix), ships nothing the non-goals forbid, makes every scope extension compiler-forced and flagged, and returns the one appetite change (OAD-3) to the PRD; the concerns are about what the operator must see beside the author-chosen rows before ratifying, and none changes the work items Stage 3 would write." Six non-blocking concerns, all applied (design § 14 lists each with its disposition).
- **UX lens** (`ux-architect` agent): **BLOCKING** on the first pass — "The structure, flows and D0 routing are sound and invent nothing on the panel, but § 9a's inventory misses one state S6 forces on the panel at compile time — the blind held block for a weekly-only blind active — so Stage 3 would scope S6 with an unclassified render decision inside it, the exact failure REQ-CEL-C-001 and the register rule exist to prevent; the fix is a row in § 9a and a sentence in OAD-1, and tranche 1 is unaffected." Revised as prescribed (fifth RenderCell in § 9a and § 15, OAD-1, B-6, § 11) with its seven non-blocking concerns applied; re-dispatched once — second pass **APPROVE WITH CONCERNS**: "The blocking concern is resolved — § 9a now inventories the blind held block as the fifth RenderCell, § 15, OAD-1, AD-24, B-6 and § 16 route it to D0 under D15, and § 11 hands its spoken phrase over — so every panel state S6 forces is classified-pending rather than unrouted; one narrower gap remains in how S6's 'decode half lands, render half waits' split behaves while D0 is open, and it is a sentence to add, not a decision to make." Applied (design B-6, § 9b, DP-1, R-D9); all seven first-pass concerns confirmed addressed.
- **Maintainer decisions, 2026-11-01** (design AD-23; decided, not pending). Asked whether tranche 1 can
  stay a small batch inside one calendar week when the coverage gate sized it at a much larger human
  effort-day estimate: "Bigger budget, but we need a known path to have a hot-fix for the issue as rugiht
  now sessiometer is unsuable". Then: "The hotfix must happen ASAP and local daemon rebuilt-restarted so
  that you can continue working". Asked whether the runbook text #1603 must gate the tranche-1 deploy
  #1604: "Yes, after deploy (Recommended)".
- **Lock**: NOT LOCKED. `status: draft` stands while any load-bearing question below is open.

## Open Questions

Every item is load-bearing; `status: final` is forbidden while any stands. Each carries the default
the design applied, so Stage 3 can proceed on the default and the operator can overturn it.

- **D0 (+ D15) — how the no-session-window state renders** on the panel row, the CLI `status` cells and (D15, design-found) the blind held block S6 forces on the panel, the panel's Stats-tab cell and its spoken summary. Options: decided-to-be-nothing with the mock's
  `—` OBSERVED-ABSENCE token; as-authored by a mock frame; genuinely open. Default: none — S11 waits.
  Impact: the operator reads `n/a` for a healthy account until it lands, and after S6 the panel shows no blind block for a weekly-only blind active until D0 classifies the held block.
- **D1 — a 200 reply with no window**: default (a) hard `UsageParse`, so the loop skips, not swaps.
- **D2 — `"five_hour": null` vs an absent key**: default identical; one test pins the relation.
- **D3 — a weekly-only active blind past the gate**: default unprotected and said so (surfacing
  half built); the arming half is OAD-2 for the operator.
- **D4 — the near-limit band**: default session-only, reads false on an absent session, with a test.
- **D5 — ADR-0033's census**: default excludes a structurally absent window.
- **D6 — version skew on the store**: default both halves replace together before the first `null`
  line (runbook rule); the residual asymmetry (silent raw drop, loud rollup refusal) is stated.
- **D7 — the velocity tie-break on an absent EMA**: default `0.0`, the existing "safest landing"
  rule, watched by `MET-R-001`.
- **D8 — when the `stats` wire half lands**: OAD-3 recommends tranche 1; the operator decides because it moves the appetite in PRD § 3.1 and puts the first Swift edit and a `stats` schema bump on the urgent path — ratify D8 and OAD-3 as one question.
- **D9 — whether PR #1537's scoped cap may fill a `None` weekly**: stays with #1537.
- **D10 — the transitional wire state**: default as decision 5 above, including the author-chosen omitted `blind_active` block and the signpost at the end of tranche 1's budget — ratify both together.
- **The weekly-less arms (AD-4b)** and **the #1537 landing order (AD-22)**: author-chosen; surfaced for ratification beside D11–D14.
- **D11 — the wire key's shape**: author-chosen `absent_windows: [WindowKind]`.
- **D12 — `Sample.weekly` optional**: author-chosen, compiler-forced by S2.
- **D13 — the tier representation**: author-chosen per-axis `Option`, omitted.
- **D14 — event-field absence**: author-chosen omitted token.

## Deferred with a tracking destination, not dropped

- **`Option<Window>` per window on `Usage`** — the better end state; a follow-up refactor, never a
  prerequisite (§ 12b). Tracked as #1605.
- **A per-axis sample count in the rollup tiers** — more honest than a bucket-wide `count`, more shape
  than the fix needs (AD-11). Tracked as #1610.
- **The spoken label of an absent Session meter** — handed to D0's register entry, not decided (§ 9b).
  Carried by #1594.
- **PR #1537's D9** — the `weekly_scoped` binding-cap claim; the design lands first and #1537 rebases.
- **Follow-ups filed while scoping**, each Related to #1590 and none its child: the `usage_gap` event's
  missing cause (#1609); slow-polling a peer whose polls keep ending in a rejection (#1612); severity from
  `limits[]` beneath a top-level window (#1606); ADR-0013's drifted code pointers (#1607); keeping a viable
  peer visible to swap-target selection when its usage poll fails (#1611); tranche 2's deploy (#1608).

## Full Design

See [optional-quota-windows-solution-design.md](../design/optional-quota-windows-solution-design.md)
for the complete specification.
