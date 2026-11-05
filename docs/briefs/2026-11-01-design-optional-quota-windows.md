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

The decisions D0 to D15, AD-4b and AD-13 are settled by council at the maintainer's delegation of 2026-11-02 (D6 and D13 on 2026-11-02, the rest on 2026-11-03; each entry under Open Questions carries its ruling and its record); decision 16's tranche 0, budget and runbook-text placement are the maintainer's, decided on 2026-11-01; and the rest stay ratification-pending, as design § 12 records them. The PRD's readiness gate failed on check 6
(pipeline-authored, unratified requirements), the acknowledgment to proceed was given by the `/scope`
orchestrator on the operator's behalf, and the design is carried as provisional. Rows once marked
as the author's own choice had no pipeline recommendation in the PRD; each is now settled by council, and the design's § 12 Flag cells say so.

1. **An absent window is `None`, carried by the type** — `Usage`, `UsageReport`, `LastGood` and
   `BlindAnchor` take `Option<f64>` per window, `Copy` kept on the three that carry it — `UsageReport` is `Clone`, never `Copy` (AD-1). The compiler then enumerates every
   consumer (33 + 17 production errors measured); a flag beside an `f64` must be consulted at all 93
   reads or the fabricated value stands, and a sentinel `0.0` is byte-identical to an idle window. One bounded exception: the tranche-0 hot-fix serves a weekly-only reply with a `0.0` session and stores no sample for it (decision 16).
2. **The parser composes per-window Options from the unchanged `dimension` lookup**; a 200 reply with
   *neither* window stays a hard `UsageParse` (D1 option a), and `"five_hour": null` is treated
   identically to an absent key (D2) (AD-2). Two invariants are pinned by tests: a reset stamp implies
   its fraction; a served reading carries at least one window (AD-3).
3. **Every consumer takes one of four named moves — skip, pass, exclude, name — and never a numeric
   default**, in tests as well as production; each decision arm owes one test over a reading whose
   window is `None` (AD-4; B-3 enumerates fifteen arms with their move).
4. **The blind gate reads false on an absent anchor** through its single pure predicate, so its
   two callers (`note_blind_gate_eligibility` and the report arm `blind_active_view`) cannot drift, and `blind_swap`'s anchor arm, which compares the same `Option` against the config thresholds, reads false on `None` with them; `status` makes no auto-protection claim for a weekly-only active.
   Whether a *weekly* anchor should arm the swap stays outside #1590 until the reopen evidence the D3 ruling names is observed (AD-5; OAD-2 / D3, settled by council on 2026-11-03).
5. **Transitional wire state before the new key lands (D10)**: under schema `1.14` a served
   weekly-only reading carries `session_pct: null` (in place of the hot-fix's `0`) beside present weekly fields, the `session_pct`
   doc comment is amended to name the transition, and — settled by council with D10 on 2026-11-03 — a weekly-only active that
   goes blind gets **no `blind_active` block**, because the block cannot be emitted under `1.14`
   without a `u8` and a `0` would be the fabrication. Cost stated precisely: for those days such an account is indistinguishable from an ordinary failed-poll row on both surfaces (blindness reaches them only through that block), duration lost as well as percent, the CORNERED alarm dropped with the block (`cornered_state` composes `blind_active` with `no_viable_target`), and its Retry-After report arm is silent — measured against the tranche-0 bundle that serves today (whose block for such an active carries a fabricated `0 %` and a false `auto-protection OK`; whether the bundle before the hot-fix ever emitted a block for such an account is unestablished, and the cost landing on the operator's active account is the whole exposure, not a bound on it) and bounded by a dated signpost: S6 (#1599) merges and #1608 deploys it within 14 calendar days of the tranche-1 deploy record on #1604, S6 exempt from tranche 2's ship-what-is-done breaker, or before that day a comment on #1590 names the cause and re-ratifies D10 with a new dated bound (AD-6, as amended by the D3/D4 and D9/D10 rulings, and the D11 ruling's correction of AD-8: the velocity arm cannot fire on a session-less anchor either way). A healthy weekly-only row costs less than it looks: a failed poll nulls both windows, so the weekly cell tells no-window from no-reading before the key lands.
6. **The wire says "no window" with one omittable per-account key, `absent_windows: ["session"]`**
   (a `Vec` of a two-variant enum serialised lowercase, emitted only from a served reading whose window is `None`, omitted whenever the list is empty), so a weekly-less
   account rides the same key with no second bump; Swift decodes it with `decodeIfPresent ?? []`
   — settled by council on 2026-11-03: the list key ratified as the wire shape, its text amended, the Rust identifier the executor's and not `WindowKind` (AD-7 / D11; alternatives: a boolean, a per-window enum).
7. **`BlindActive.last_known_session_pct` becomes optional and is omitted when absent**, exactly the
   `rotated` reshape; Swift `UInt8?`; the CLI `~N%` cell falls back and the blind sentence drops the
   percent — settled by council on 2026-11-03: ratified as amended, the reshape covering both `last_known_session_pct` fields and `BlindPreemptSwapRecord`'s in one minor (AD-8 / D11).
8. **The minor is chosen at landing under the queue rule** (read from the queue at landing; `1.15` is the lowest unclaimed today, claimed by #1457, #1586, #1587 and PR #1588); the five goldens are regenerated once, two *new* golden rows are added (one new golden file carrying two account rows, never a re-baseline of the five), and the Swift
   fixture sweep (27 + 14 sites plus the `"minor":15` future frame) moves in lockstep — settled by council on 2026-11-03: ratified as amended (AD-9 / D11).
9. **The sample line writes `"session": null` — the key never omitted** (#1580's exact-key-set oracle),
   and — settled by council on 2026-11-03 (D12, a faithful application of D6) — `Sample.weekly` takes the same shape because the writer is compiler-forced
   to decide the weekly axis too (AD-10 / D12).
10. **Rollup tiers carry per-axis `Option<HourStat>` / `Option<DayStat>`, omitted when no sample in
    the bucket carried that window**, so the fold never reaches `-∞`; counts stay counts. Settled by
    council on 2026-11-02 (AD-11 / D13): ratified and amended with a per-axis `samples` count (#1610
    absorbed into #1598). Version skew under the rollup fence, the format-2 envelope
    (`{"format":2,"rollup":{…}}`) that a #1598 build writes to `usage-rollup.json` from its first
    compaction: on a store a #1598 build has compacted, an older *daemon* fails to read that envelope,
    so its compaction stalls before it rewrites either store file and erases no `null` line. The stall
    is silent, because the daemon discards the compaction error, and it ends when a #1598+ build runs
    again. An older `stats` refuses every fenced rollup loudly and deletes nothing. An older
    `reliability` leaves `null` lines out of its report and deletes nothing. The ruling accepts four
    residuals, and an older daemon erases `null` lines in only the first two: (1) lines appended
    before the first fenced write; (2) a rollup deleted or moved by hand; (3) an older `reliability`'s
    report that leaves `null` lines out; (4) every pre-#1598 build's own `stats` and compaction,
    stopped for good on each machine that has run #1598.
11. **`stats` excludes an absent window from every session figure and from the census (D5)**; on its wire `session`, `band` and the session-cap counts `cap_hits` / `time_at_cap_secs` become omittable under `JSON_SCHEMA_VERSION` 1 → 2 — and in the same bump `weekly` (the weekly symmetry) and a `velocity` object emitted when either axis has a rate, with a nullable session rate — Swift's
    `StatsDim` goes optional, the one golden is regenerated, and the CLI table reuses its own `—` UNKNOWN sentinel for the session, signal, velocity, runway and cap columns (it then does not distinguish no-window from never-observed; only the wire does). Field set ratified by council on 2026-11-03 (AD-13); the wire half rides in tranche 1
    (OAD-3 / D8, settled ride first by council on 2026-11-03: #1601 in its own pull request after #1598's and before #1604, at #1601's own estimate of 1.5–2.5 working days).
12. **Event fields carry typed absence as an omitted token**; `reliability`'s blind parsers give it a disclosed bucket;
    no `log` schema move; `reliability` moves 13 → 14 in the compile unit for that bucket, named there — settled by council on 2026-11-03 (D14; AD-14's text amended with the bump's value-domain half and the bucket's admission rule)
    (AD-14 / D14).
13. **Oracles A–D land `#[ignore]`d and RED first**, proven by a filtered `--ignored` run, never
    `--include-ignored`; the scoping party re-proves RED (AD-18).
14. **The records move with or before the pinned test**: four ADRs amended in place (keep `date:`, add
    an `**Amended**` paragraph, update the README row) in their own pull request first, the parser doc rewritten in the commit that moves the test it governs, the HQ
    `REQ-USG-B-001` amendment recorded as owed by the operator (AD-19); the warm-up design's C-1
    reads two states from the fraction and the stamp (AD-20); the daemon-replacement runbook gains
    the no-older-daemon rule and a fleet recovery check (AD-21); that text is #1603, now in tranche 2 and landing after the deploy (AD-23).
15. **Open-PR composition**: this design lands first and #1537 rebases (its D9 stays its own); S6 takes the next free minor relative to #1588; #1538 composes on the blind seam — settled by council on 2026-11-03 (D9; AD-22): the landing order and the non-absorption ratified, recommended to #1537's author and to the maintainer, never ordered.
16. **A hot-fix comes first, then the tranche order is the cut order reversed** (AD-23; decided by the
    maintainer on 2026-11-01). **Tranche 0** is the hot-fix #1613, outside the twelve blocks: it serves a
    weekly-only reply with a `0.0` session (a private flag marks it), stores no sample for it, and is
    deployed to the operator's machine before it merges. It costs 0% session on the wire and the panel,
    event-log session tokens that read 0 (so `reliability` reads them as idle sessions over that window),
    and no sample for such an account — a gap in `stats`, never a zero. Tranche 1 retires all three
    (#1596 removes the flag, #1598 writes `"session":null`), and the hot-fix is invalidated if tranche 1
    deploys first. **Tranche 1** is budgeted at the human effort-day estimate the coverage gate sized (PRD
    § 3.1), no longer one calendar week: oracles (#1591) → fixtures (#1592) ‖ the ADR records (#1593), each its own pull request → the one compile unit (parser, model, arms; #1596 + #1597 + #1602, carrying the parser doc) → the store with the five sample readers (#1598 with #1600, one pull request) → the transitional wire state it ships → deploy (#1604). **Tranche 2**: wire key (#1599) → warm-up amendment (#1595), plus the runbook text (#1603), moved here and landing after the deploy, and the tranche-2 deploy (#1608); the stats wire (#1601) left tranche 2 for tranche 1 under D8 (settled ride first by council on 2026-11-03: its own pull request after #1598's and before #1604); the render cells (#1594) wait outside both tranches on #1599, and on #1601 for the Stats cells. With
    the hot-fix deployed, weekly readings recover at tranche 0; tranche 1's deploy newly turns on the
    `"session":null` sample line and `session_pct: null` in place of the hot-fix's 0.
17. **S11 is specified as a waiting item with three D0 options and its gates, and the design picks no
    token**; D15 extends D0 to the panel's Stats-tab cell and its spoken summary, and to the blind held block that S6's reshape forces on the panel (the `BlindMeter` reads `lastKnownSessionPct` as a value), so S6's panel render half is #1594's while its decode half lands (AD-24); D0 and D15 classified it by council on 2026-11-03, and until #1594 builds it the panel renders a decoded percent-less `blind_active` block as no block — today's two-meter `n/a` row — at the stated cost that the blind duration and verdict stay invisible on the panel alone (the CLI footer shows them).
18. **The weekly-less arms are decided by symmetry** — a weekly-less account is never weekly-blocked,
    `use` returns `Viable`, it is never `weekly_exhausted` — compiler-forced once `Usage.weekly` is
    `Option`, for an account class with no observable instance today — settled by council on 2026-11-03 on the #1614 Answer 2 premise, the rationale amended with the cached-path bound (AD-4b).

## Design Tracks

| Track | Approach | Key Trade-off |
|---|---|---|
| Technical Architecture | Per-window `Option` lift; fifteen enumerated decision-arm moves; three absence representations chosen by each surface's own precedent (omittable wire key, `null` sample key, omitted `stats` block) | Chose the compiler-enumerated lift over a flag or sentinel; left the cleaner `Option<Window>` struct as a follow-up (#1605) because it reshapes every `Usage` literal and the `Copy` decision type on the urgent path |
| UX / IA | Generative mode for one surface class (the no-window account on the panel row, the CLI cells, the Stats tab); inventory and state coverage complete; its three options and their gates specified, and D0 settled by council on 2026-11-03 at option (i) | Accepts a documented, dated transitional `n/a` for days rather than inventing a token against the panel register |
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
- **Lock**: NOT LOCKED. `status: draft` stands; no decision question below is open (each was settled by council), and locking is `/scope`'s.

## Open Questions

Every item is load-bearing; `status: final` is forbidden while any stands, and none stands: each is settled by council at the maintainer's delegation and carries its ruling in one line with its record, so Stage 3 proceeds on the ruling and the maintainer can still reverse it.

- **D0 (+ D15) — how the no-session-window state renders** on the panel row, the CLI `status` cells and (D15, design-found) the blind held block S6 forces on the panel, the panel's Stats-tab cell and its spoken summary: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1594#issuecomment-6605473790), [correction](https://github.com/alexey-pelykh/sessiometer/issues/1594#issuecomment-6609661976)) — option (i), decided to be nothing: no track in the Session meter or the held block; three *Decided to be nothing* register rows and one *Genuinely open* row (#1646); frames are #1645 and gate nothing.
  Impact: the operator reads `n/a` for a healthy account until #1594 lands, and after S6 the panel shows no blind block for a weekly-only blind active until #1594 builds the held block as classified.
- **D1 — a 200 reply with no window**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1596#issuecomment-6605332649)) — option (a) ratified (a windowless 200 reply stays a hard `UsageParse`), its account of (b) amended.
- **D2 — `"five_hour": null` vs an absent key**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1596#issuecomment-6605332649)) — identical, ratified; one test pins the relation.
- **D3 — a weekly-only active blind past the gate**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1602#issuecomment-6606329404)) — default ratified, row amended with its cost and reopen evidence; the arming half (OAD-2) stays outside #1590 until that evidence is observed.
- **D4 — the near-limit band**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1602#issuecomment-6606329404)) — not applicable, ratified; session-only, reads false on an absent session, with a test.
- **D5 — ADR-0033's census**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1600#issuecomment-6608233904)) — census exclusion ratified, sentence restated in the code's subtractive form.
- **D6 — version skew on the store**: settled by council on 2026-11-02 (a comment on #1598): `null` ratified, amended with a rollup fence #1598 builds and the rule that no pre-#1598 daemon runs against the store after its first `null` line. Under the fence an older daemon stalls silently instead of erasing, and an older `stats` refuses the fenced rollup loudly. The ruling accepts four residuals: (1) lines appended before the first fenced write; (2) a rollup deleted or moved by hand; (3) an older `reliability`'s report that leaves `null` lines out; (4) every pre-#1598 build's own `stats` and compaction, stopped for good on each machine that has run #1598. Residuals (1) and (2) are the only cases where an older daemon still erases `null` lines.
- **D7 — the velocity tie-break on an absent EMA**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1602#issuecomment-6606474332), [correction](https://github.com/alexey-pelykh/sessiometer/issues/1602#issuecomment-6607274125)) — amended: pass move, typed key `observed_climb`, EMA reset; REQ-RDG-C-001 not narrowed; MET-R-001 does not watch the tie.
- **D8 — when the `stats` wire half lands**: settled by council on 2026-11-03 with OAD-3 as one question ([record](https://github.com/alexey-pelykh/sessiometer/issues/1601#issuecomment-6608235127)) — "follow" rejected, ride first: #1601 in tranche 1 before #1604; AD-13 stands.
- **D9 — whether PR #1537's scoped cap may fill a `None` weekly**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1597#issuecomment-6605411361)) — stands: the rework lands first and #1537 rebases; the fill and the `weekly_scoped` policy stay the maintainer's.
- **D10 — the transitional wire state**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1597#issuecomment-6605411361)) — the transitional `1.14` state stands, amended; the omitted `blind_active` block and the signpost ratified together, the signpost dated 14 days from #1604's deploy record.
- **The weekly-less arms (AD-4b)** and **the #1537 landing order (AD-22)**: settled by council on 2026-11-03 — AD-4b ratified on the #1614 Answer 2 premise ([record](https://github.com/alexey-pelykh/sessiometer/issues/1602#issuecomment-6606329404)); AD-22 ratified with D9 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1597#issuecomment-6605411361)).
- **D11 — the wire key's shape**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1599#issuecomment-6605342412)) — the `absent_windows` list key stands in shape, text amended; `BlindActive`/`BlindPreemptSwap.last_known_session_pct` optional in the same minor.
- **D12 — `Sample.weekly` optional**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1598#issuecomment-6605413298)) — stands, faithful to D6; compiler-forced by S2.
- **D13 — the tier representation**: settled by council on 2026-11-02, with D6 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1598#issuecomment-6570338036)): per-axis `Option` ratified, amended with a per-axis `samples` count (#1610 absorbed into #1598).
- **D14 — event-field absence**: settled by council on 2026-11-03 ([record](https://github.com/alexey-pelykh/sessiometer/issues/1602#issuecomment-6605414098)) — stands: the omitted token; AD-14 and the `BlindGateEligible` sentence amended.

## Deferred with a tracking destination, not dropped

- **`Option<Window>` per window on `Usage`** — the better end state; a follow-up refactor, never a
  prerequisite (§ 12b). Tracked as #1605.
- **A per-axis sample count in the rollup tiers** — more honest than a bucket-wide `count` (AD-11). Was tracked as #1610; the D6/D13 council of 2026-11-02 absorbed it into #1598, so it ships in tranche 1.
- **The spoken label of an absent Session meter** — decided by D0's register row (the D0/D15 council ruling of 2026-11-03): "no session window", in place of "session N% resets in X" (§ 9b).
  Carried by #1594.
- **PR #1537's D9** — the `weekly_scoped` binding-cap claim; the design lands first and #1537 rebases.
- **Follow-ups filed while scoping**, each Related to #1590 and none its child: the `usage_gap` event's
  missing cause (#1609); slow-polling a peer whose polls keep ending in a rejection (#1612); severity from
  `limits[]` beneath a top-level window (#1606); ADR-0013's drifted code pointers (#1607); keeping a viable
  peer visible to swap-target selection when its usage poll fails (#1611); a window the reply carries but cannot read (#1614); the `Tunables.poll_secs` doc comment's sweep cadence (#1615); one identifier per account on every event-log line (#1616). Tranche 2's deploy (#1608) is a child of #1590, not a follow-up.

## Full Design

See [optional-quota-windows-solution-design.md](../design/optional-quota-windows-solution-design.md)
for the complete specification.
