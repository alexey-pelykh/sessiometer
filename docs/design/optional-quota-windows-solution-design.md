---
title: Optional Quota Windows — Solution Design
scope: optional-quota-windows
created: 2026-11-01
status: draft
source: docs/requirements/optional-quota-windows.md
tracks:
  technical-architecture: complete
  ux-ia: pending
  testing-architecture: complete
---

# Solution Design: Optional Quota Windows

**Input PRD**: `docs/requirements/optional-quota-windows.md` — `dor_status: failed` on check 6 alone
(every requirement is pipeline-authored and unratified; checks 1–5 pass as sections pending
ratification). **Tree**: `fd8640b` (`origin/main`, 2026-10-30), against which every code citation
below was re-read on 2026-11-01. **Requirements**: 46 (`REQ-RPL-B-001` … `REQ-DEP-Q-001`, PRD § 10).
**Components**: S1–S12 (PRD § 0). **Decisions**: D0–D10 (PRD § 8) plus five this design adds
(D11–D15, § 12), every one settled by council at the maintainer's delegation — D6 and D13 on 2026-11-02, the rest on 2026-11-03 (§ 14).

> **Provenance and standing.** Authored by an AI pipeline (`/scope` Stage 2, headless) from the PRD,
> the investigation report and the scope seed it names in its frontmatter `source` (both machine-local
> scratch; everything relied on is quoted in-band). **The readiness gate's `failed` verdict was
> acknowledged by the `/scope` orchestrator on the operator's behalf, not by the operator**, so this
> design is carried as **PROVISIONAL** — the readiness verdict stands; the decisions D0–D15, AD-4b and AD-13 are settled by council at the maintainer's delegation (§ 12, § 14), and every decision no council ruled stays ratification-pending, and
> labelled so where it makes one. The operator-authored inputs are the two messages the PRD quotes
> in its § 2.1 and the maintainer's decisions of 2026-11-01, quoted verbatim in § 12 AD-23 (decided,
> not ratification-pending). The operator and the maintainer are one person, the repository's owner, as PRD § 2
> defines the two words. Every `src/<file>.rs:NNN` citation names the symbol that occurs at those lines, per
> `scripts/check-doc-citation-rot.sh`.

## 1. Goals and Drivers

The wire changed under the daemon on 2026-10-30: verified Ultra accounts lost the five-hour session
window and kept the weekly one. The parser treats both windows as mandatory, so it rejects such a
reply whole, and the daemon reports a fleet of 15 healthy accounts as unreadable and the roster as out
of capacity (PRD § 2). The operator's two messages ratify the class of work — support these accounts,
urgently — and the shape of the parser — every window independently optional, a weekly-less account
being the explicit future case.

| # | Goal | PRD source | Success criterion |
|---|---|---|---|
| G-1 | Every enabled, healthy-credential account is served, sampled and selectable whatever windows its plan carries | § 2, § 4 `MET-NSM-001` | `REQ-DEP-Q-001`: 17/17 by the second poll cycle after deploy — first met by tranche 0's deploy (weekly readings), held by tranche 1's (B-12) |
| G-2 | Each quota window is independently optional on the wire and in the model, weekly included | § 2.1 message 2, S1–S3 | Oracles A–D green; `REQ-RDG-B-001` |
| G-3 | No surface ever shows an absent window as a number — "never a fabricated `0`" stays the law | § 3.3, R2–R3 | `MET-T-003` = 0 from tranche 1's deploy; tranche 0's hot-fix is the one bounded exception (§ 1b) |
| G-4 | "No window" is distinguishable from "no reading" on the status wire, in the store and in `stats` | S6, S7, S8 | `REQ-STL-I-001`, `REQ-SMP-B-001`, `REQ-STS-B-001` |
| G-5 | The records that said "both dimensions" say "every dimension the account has", before the pinned test moves | S9, S10 | `REQ-REC-C-001` |
| G-6 | The fix reaches the running bundle and the fleet is seen to recover | S12 | `MET-T-001`, `MET-T-002`, `MET-T-004` |

**Driver**: every poll of a dark account is a full-cadence request that yields nothing, and the
share of such accounts only grows (the lift is per account and automatic). Doing nothing leaves the
daemon unable to rotate for as long as the operator holds Ultra accounts.

## 1b. Non-Goals

Carried from PRD § 3.3 and binding on this design:

- No fabricated `session = 0.0` or `session = weekly`, anywhere, ever — not even as a transitional
  shortcut (§ 4.1).
- No flag beside an unchanged `f64`; no reuse of `session_pct: null` for "no window" — except the documented transitional state of B-6t under D10, dated, bounded by its signpost and retired by B-6 (PRD § 3.3 carries the same exception beside its row).
- **One exception: tranche 0, the hot-fix #1613 (AD-23).** The maintainer decided on 2026-11-01 that a hot-fix
  path exists; the mechanism below is the scoping pipeline's design under that decision, settled by council
  on 2026-11-04 at the maintainer's delegation (the record is a comment on #1590).
  The two bullets above stay binding on B-1 … B-12. #1613 sits outside the twelve blocks and is the only
  code that breaks them, for a bounded time, and tranche 1 removes it. It accepts exactly the cost § 4.1
  names for a flag beside an unchanged `f64`: the fabricated value stands wherever the flag is not
  consulted.
  - *What it does.* `parse_usage` serves a reply whose session window is absent and whose weekly window
    is present as `Ok` with `session: 0.0`, no session reset stamp and no session severity, and sets a
    private flag `UsageReport.session_absent = true`. `PolledReading` gains
    `pub(crate) session_window_absent: bool`, set from the report. A reply with neither window, or with
    the weekly window alone absent, stays `Err(UsageParse)`, as today. The doc comment of `parse_usage`
    states the exception.
  - *How it is narrowed.* `append_sample_for_poll` returns early on `session_window_absent`, so no sample
    line is written for such a reading: a gap, never a stored zero. That, and the poll loop's
    once-per-episode stderr line (`DecisionState::note_session_window_absence`), are the two places the flag
    is consulted; every other reader sees `0.0`; on the swap decision that reads as "not session-blocked",
    which is what an account with no session window needs. #1596 (B-1) removes the weekly-only branch and
    both flags (its § 7, AC-17); #1598 (B-7) replaces the sample skip with a `"session":null` line (its
    § 8, AC-16).
  - *What it costs, settled by council on 2026-11-04 (the record is a comment on #1590); tranche 1's deploy
    retires all but the unreadable-window cost, which #1614's pull request retires.* The status/watch wire
    and the panel show 0% session for a weekly-only account. Event-log lines that carry a session token
    (swap, `usage_velocity`, the blind events) read 0, so `reliability` over the hot-fix window reads them
    as idle sessions. No usage sample exists for such an account while it serves, so `stats` shows a gap,
    never a zero; a skipped sample is never written later, so the account's history for the interval is
    lost. The usage-gap tracker counts such a poll as a reading (`maintain_stats_store` keys on
    `polled.is_err()`), so the account's gap streak clears and no `usage_gap` event is logged while no
    sample is stored. Because no sample exists, the store-derived roster-wide values leave the account out
    while it is served: `fleet_runway` skips a `seen == 0` account, so the pool and its `n of m` cardinality
    omit it; `capacity_holds` and the all-accounts-high census (`aggregate_with_roster`) keep it in the
    roster intersection with nothing covering it, so the jointly covered time reads UNKNOWN for the whole
    roster; and `fleet_runway_low` probes the same pooled figure (#1613's review, 2026-11-02). A session
    window the reply carries but the parser cannot read is served as no window, like an absent one; #1614 is
    ruled Answer 2.
  - *Where it stands.* It is deployed to the operator's machine before it merges, and it is invalidated
    if tranche 1 merges and deploys first. #1591's oracles (B-5) stay `#[ignore]`d; against the hot-fix,
    Oracle B fails on its session-window assertion (`Some(0.0)`) instead of the parse error, Oracle A
    passes, Oracle C stays green and Oracle D still fails on the weekly parse error. It does not breach
    ADR-0033: that ADR governs the `stats` census, and the hot-fix stores no sample, so the census never
    sees its `0.0`.
- No read of `/api/oauth/profile`; the sole-egress oracle
  `the_sole_egress_endpoint_is_the_documented_usage_url` (`src/usage.rs:1593`) stays green.
- No keep-warm, `reliability`-rule, polling-cadence (C3, tracked as #1612) or `usage_gap`-cause (C2, tracked as #1609) change.
- No absorption of PR #1537's `weekly_scoped` binding-cap claim (D9); no re-scoping of PRs #1537,
  #1588 or #1538 (§ 4.6).
- No platform-specific work; no `log --json` schema change. `reliability --json` is the one wire whose records change meaning: its census gains a no-session-window bucket, so its `JSON_SCHEMA_VERSION` moves 13 → 14 (§ 4.5, AD-14).
- No render token invented for the no-window state on any surface ahead of D0 (§ 9).

## 2. Constraints

| Constraint | Source | Consequence for the design |
|---|---|---|
| "A window that cannot be found is a hard `Error::UsageParse` (never a fabricated `0`)" is rewritten, not weakened: the value for an absent window is **`None`**, never `0.0` | parser doc at `src/usage.rs:473-474` (`UsageParse`); HQ `REQ-PCY-B-002` "never swap on missing data" (quoted PRD § 2.4) | The model gains a third per-window state; every consumer decides what it does with it (§ 5 B-3) |
| `session_pct: null` already means "the last poll for this account failed" | `session_pct` at `src/daemon/snapshot.rs:758-763`; Swift `sessionPct` at `apps/menubar/Sources/WireModel.swift:648` | A new omittable key carries "no window" (B-6); the transitional state is documented as transitional and retired (B-6t) |
| Four independent wires; "additive means no bump" holds only for an omittable key (`CLAUDE.md` § Schema versions), and this design adds that an existing key changing meaning is a bump; minor 15 is claimed by #1457, #1586, #1587 and PR #1588 under "whichever lands second takes the next minor" | `CLAUDE.md` § Schema versions; `STATUS_SCHEMA_VERSION` at `src/daemon/snapshot.rs:538-541` | S6 takes the next free minor at landing; the reshape of `last_known_session_pct` follows the `rotated` precedent (`src/daemon/snapshot.rs:141-142`); `stats` bumps its own `JSON_SCHEMA_VERSION` (`src/stats.rs:80`) |
| Oracles A–D are executor-uneditable; a RED test cannot merge (`ci-ok` only, `--admin` impossible), so they land `#[ignore]`d; `cargo test -- --include-ignored` rewrites a frozen migration fixture and is forbidden | PRD § 10.5; `CLAUDE.md` § Schema versions | S5 is the first item; RED is proven with a filtered `--ignored` run (§ 11 Master Test Plan) |
| The panel's register forbids building against an unclassified axis; the mock authors no no-window row | `apps/menubar/design/README.md:1293-1295`; `apps/menubar/design/menubar-preview.html:1121-1126` | S11 is designed as an item carrying its options and gates (§ 9); D0 was settled by council on 2026-11-03, and B-11 (#1594) waits on #1599, and on #1601 for the Stats cells only |
| Sample lines are versionless; an older daemon skips a line it cannot parse and its compaction erases it (an older `stats` or `reliability` only reads) | `read_samples` at `src/usage_store.rs:449-467`; `read_samples` … `write_samples` at `src/usage_store.rs:535-544`; the one production caller of `compact_and_roll` is the daemon's `stats_events_for_poll` (`src/daemon.rs:839`) | No pre-#1598 build runs as the daemon against a store that carries a `null` line (D6 as the council ruled it on 2026-11-02, `REQ-DEP-C-001`); the rollup fence #1598 builds stalls an older daemon's compaction before it rewrites, and an older `stats` fails loudly on it (§ 5 B-7) |
| Open PRs on the same seam: #1537 (the `dimension` lookups at `src/usage.rs:507-508`), #1588 (minor 15, the five goldens, `src/usage.rs`), #1538 (the blind seam, ADR-0017) | PRD R1; live `gh` read 2026-11-01 (§ 4.6) | Composition stated per PR; none re-scoped |
| Gates every code item owes: the five `test` steps, `msrv`, both `deny` checks, `swift`, an **armed** `panel-goldens` run where the panel changes, and the three golden/gate trailers where they apply | `CLAUDE.md` § Before you push | § 11 Quality gates |
| Platform-neutral; Linux is landed but un-gated in CI | `CLAUDE.md` § What this repo is | § 4.7 states the assumption |
| `cargo build` deploys nothing; the serving daemon is whatever bundle was last launched — built 2026-09-05 when the scope began, the #1613 emergency build since 2026-11-01 | `docs/runbooks/replacing-the-running-daemon.md` § Why `cargo build` deploys nothing (the mechanism); the build dates are the investigation's and the scope brief's readings of the serving bundle, not the runbook's | S12 amends that runbook (B-12) |
| Appetite: tranche 0 (the hot-fix #1613) deployed as soon as it is built; tranche 1 sized to the human effort-day estimate the coverage gate computed (PRD § 3.1), no longer one calendar week; tranche 2 one to two weeks; S11 unsized (its sizing is `/scope`'s; D0 settled by council on 2026-11-03) | PRD § 3.1, A-9, as re-set by the maintainer's decision of 2026-11-01 (AD-23) | § 12 AD-23 adds the hot-fix as tranche 0 and orders the rest so the cut order is the tranche order reversed |

## 3. Context and Scope

The daemon polls `GET /api/oauth/usage` per account, parses the reply into a `Usage` reading, and
feeds that reading to every decision arm, to the status/watch wire the CLI and the menu-bar app read,
to the sample store the offline verbs read, and to the event log `reliability` parses offline. One
upstream (the usage endpoint, unchanged in kind and rate) and one external consumer of a changed wire
(the separately-installed menu-bar app, updated in lockstep as every wire change here already assumes).

```
Anthropic /api/oauth/usage ──► B-1 parser ──► B-2 reading model ──► B-3 decision arms ──► swap engine
                                                   │                          │
                                                   ├──► B-7 sample store ──► B-8 stats / reliability
                                                   │
                                                   └──► B-6 status/watch wire ──► CLI table · Swift panel (B-11, after #1599, and after #1601 for the Stats cells only)
B-5 oracles / B-4 fixtures grade B-1..B-2 · B-10 records govern B-3 · B-9 governs #1232 · B-12 ships the bundle
```

**Bounded-context inventory** (what Stage 2.5 reads; seven contexts, one process):

| # | Bounded context | Aggregates | Core concept | Relationship to its neighbours |
|---|---|---|---|---|
| BC-1 | Quota parsing | UsageReply, QuotaWindow | one reply → zero, one or two windows | upstream of everything; conformist to Anthropic's reply shape |
| BC-2 | Swap decision core | Reading (incl. `LastGood`, `BlindAnchor`), DecisionArm | what the daemon does with a reading | customer of BC-1; shared kernel with BC-3 (the `Usage` type crosses) |
| BC-3 | Status/watch wire + Swift mirror | StatusLine | the row the operator and the panel read | published language (`STATUS_SCHEMA_VERSION`), one external consumer |
| BC-4 | Sample store + offline verbs | Sample, StatsReport | the durable record and its figures | customer of BC-2's reading; its own published language (`stats` `JSON_SCHEMA_VERSION`) |
| BC-5 | Design records | DesignRecord | the written premises the code implements | governs BC-2; amended in place |
| BC-6 | Deployment | Deployment | the bundle that actually runs | consumes everything; procedure, not code |
| BC-7 | Rendering (panel row, CLI cells, Stats tab) | RenderCell | where the operator sees the state | conformist to BC-3/BC-4; classified by D0 (council, 2026-11-03), built by B-11 after #1599, and, for the Stats cells only, after #1601 |

Deployment topology is unchanged: one Rust binary (daemon + CLI verbs) inside one app bundle, one
Swift app. No service boundary moves; the decomposition above is a reading of the code's existing
seams, not a proposal to split anything.

## 4. Solution Strategy

### 4.1 The keystone: an absent window is `None`, carried by the type, and the compiler enumerates the consumers

`Usage.session: f64` and `Usage.weekly: f64` (`src/usage.rs:87-116`) become `Option<f64>`, and so do
the raw mirror `UsageReport` (`src/usage.rs:157-182`), the retained pre-blind anchor `LastGood` (`src/daemon/records.rs:194-198`) and the per-slot `BlindAnchor` (`src/daemon/records.rs:222-226`).
`Copy` is kept on `Usage`, `LastGood` and `BlindAnchor`; `UsageReport` holds four `Option<String>` fields and is `Clone`, never `Copy`, today and after. The parser reports `None` for a window it cannot find instead of
returning early with `?`.

Why the type and not a flag or a sentinel (PRD § 2.3 and § 3.3, confirmed here): a flag beside an
unchanged `f64` must be consulted at every one of the 93 lexical production reads or the fabricated
value stands where it was not consulted; a sentinel `0.0` is byte-identical to an idle window the
tree already holds (the API omits `session_resets_at` on most zero-session readings — `src/swap.rs:590-600`,
`SessionHighWater`) and is written to disk forever. The `Option` lift makes every consumer a compile
error until it says what it does with the absent state: the investigation measured 33 production
errors in 6 files for the session lift and 17 in 9 files for the weekly lift (lower bounds), which is
the enumeration this design relies on rather than a grep. The one bounded exception is tranche 0's
hot-fix, which accepts exactly the cost named above — a fabricated value that stands wherever the flag is
not consulted — for a bounded time (§ 1b, § 4.4).

### 4.2 Every consumer takes one of four named moves, and none takes a numeric default

For each decision arm, § 5 B-3 records exactly one move:

| Move | Meaning | Typical site |
|---|---|---|
| **skip** | the arm has nothing to evaluate on this axis and does not fire | `velocity_swap`, the near-limit band, the high-water fold |
| **pass** | the gate is satisfied vacuously (`is_none_or`) | the session anti-thrash gate and the floor in selection |
| **exclude** | the account or sample is left out of a figure or a census | `stats` session figures, ADR-0033's census |
| **name** | the absence is carried as a typed absence on a surface | the wire key, `"session":null`, an omitted event token |

`REQ-RDG-C-001` forbids the fifth move — substitute a number — at every site that meets the absent state, including tests:
a test "fixed" by wrapping the old literal in `Some(0.0)` asserts nothing about the absent state
(premortem P-11), so each arm owes one test over a reading whose window is `None`. The rule binds B-1 … B-12; tranche 0's
hot-fix is outside them (§ 1b).

### 4.3 The wire says "no window" with its own omittable key; the store writes the key as `null`; `stats` omits the block

Three representations, each chosen by the surface's own precedent (§ 5 B-6, B-7, B-8): an omittable
per-account key on the status wire (the `canonical_scrub` / `blind_active` / `rotated` precedents);
`"session":null` on the versionless sample line (the key always written, because #1580's planned
exact-key-set oracle REDs on omission); an omitted `session` block on the `stats` wire under its own
bumped `JSON_SCHEMA_VERSION`.

### 4.4 Two tranches plus one item that waits, with the compiler-forced couplings made explicit

Tranche 0 comes first and sits outside the twelve blocks; the heading counts the two tranches of the
blocks. It is the hot-fix #1613, which the maintainer added so that a known path to a working daemon exists
while tranche 1 is built (AD-23). It serves a weekly-only reply as `Ok` with `session: 0.0`, stores no
sample for it, and is deployed to the operator's machine before it merges (§ 1b). It accepts exactly the
cost § 4.1 names for a flag beside an unchanged `f64` — the fabricated value stands wherever the flag is
not consulted — for a bounded time. Tranche 1 retires it: #1596 (B-1) removes the weekly-only branch and
both flags, and #1598 (B-7) writes `"session":null` where the hot-fix skips the sample. If tranche 1
merges and deploys first, the hot-fix is invalidated. With #1613 deployed, weekly readings recover at
tranche 0. What tranche 1's deploy newly turns on is the `"session":null` sample line and
`session_pct: null` in place of the hot-fix's 0 (B-6t).

Tranche 1 (the fix proper, after the hot-fix; its budget is in § 2): B-5 oracles (#1591), B-4 fixtures (#1592) and
B-10's records half (#1593, the ADR amendments) first, each its own pull request; then B-1 + B-2 + B-3 as one
compile unit (#1596 + #1597 + #1602, one pull request), carrying B-10's parser-doc half, because the `parse_usage`
doc comment moves with the parser and its pinned test; then B-7 (the store, #1598) with B-8's reader half (#1600)
in the next pull request; then B-8's wire half (#1601 — D8, settled ride first by council on 2026-11-03) in its own pull request; then B-12's live half (#1604, the deploy). B-7's placement is a choice, not a
compiler-forced one: the compile unit keeps #1613's sample skip, re-keyed to skip a reading with either fraction
`None`, so it compiles without B-7; a daemon built from the compile unit before #1598 stores nothing for such a reading, exactly as the tranche-0 bundle does, and writes no `null` line, so the D6 deployment rule is not yet engaged; nothing enforces that no such daemon runs, and the compile unit's pull request body says "not for deploy before #1598" (D10, settled by council on 2026-11-03). The wire ships the documented transitional state (D10, B-6t).
Tranche 2: B-6 (the key, the minor, the lockstep), B-9 (the warm-up design amendment), and B-12's runbook
text (#1603), which the maintainer moved here from tranche 1 and which lands after the deploy; tranche 2
deploys as #1608. Waits: B-11 (on B-6, and on B-8's wire half for the Stats cells; D0 is settled).

### 4.5 What does not move

`classify_poll` (`src/daemon/classify.rs:55-62`) — a served reply is `Live` with no edit.
`should_keep_warm_retry` (`src/daemon/keep_warm.rs:44`) reads liveness only. `reliability`'s rules
(`REQ-STS-C-001`): a weekly-only account produces no `reason=session` swaps, `relief_of` reads
`session_blocked` false (`src/usage_stats.rs:1055-1057`), and `compute_landing` (`src/reliability.rs:2138`) treats an absent reading as unmeasured. The `log --json` wire
(`JSON_SCHEMA_VERSION` at `src/log.rs:167`) carries no per-window reading that changes meaning. The
`reliability --json` wire (`JSON_SCHEMA_VERSION` at `src/reliability.rs:264`) does: its blind-window
census must give a session-less `blind_window` line a disclosed bucket to keep the #484 exact
partition, an always-present key, so it moves 13 → 14 (§ 5 B-3, #1602 Specification § 3). The same move narrows `n_malformed` — a session-less line is no longer corrupt — a value-domain correction the constant's doc comment names beside the added key; an older `reliability` over a newer log counts such lines `malformed` and erases nothing.

### 4.6 Composition with the three open pull requests (read live on 2026-11-01; the #1588 row re-read 2026-11-02)

| PR | State read | Seam shared | Disposition |
|---|---|---|---|
| **#1537** `fix(usage): let an active weekly_scoped cap outvote the all-models window` | OPEN, `CHANGES_REQUESTED`; MERGEABLE on 2026-11-01, CONFLICTING since #1613 merged (`7f5f7f4`, 2026-11-02) — #1537 rebases at least once regardless of this design; touches only `src/usage.rs` (+135/−1); its hunk is `@@ -450,10 +450,20 @@ fn parse_usage` and keeps both `?` lookups | the same lines B-1 rewrites | **This design lands first and #1537 rebases onto it.** The two are textually conflicting and semantically independent: B-1 makes each window optional, #1537 overrides a present weekly fraction with a higher active scoped cap. After rebase #1537's override applies to `weekly: Some(w)`; whether it may also fill a `None` weekly from a scoped cap is **D9**, left with #1537 (C6). Nothing here depends on #1537 |
| **#1588** `(feat) storage: add file-backed Claude credentials and token-only accounts` | OPEN, `CONFLICTING`, 60 files; moves `STATUS_SCHEMA_VERSION` 1.14 → 1.15 and rewrites all five goldens and the Swift fixtures; `src/usage.rs` +461/−99 (a header-derived usage path for token-only accounts) | the minor, the goldens, the Swift fixtures, and two new production constructions of the lifted types: a `Usage {` in `parse_probe_headers` (`src/usage.rs`) and a `LastGood {` in `src/daemon.rs`, plus a `Usage {` test literal under `src/daemon.rs`'s `mod tests` (read from `gh pr diff 1588` on 2026-11-02; the hunk header names `mod tests`) | **Queue, not block.** S6 takes the next free minor at landing, read from `main` and the open claimants then — 1.15 is the lowest unclaimed today — with the lockstep counts re-derived then (`REQ-STL-C-001`). Whichever lands second rebases; #1588's new `Usage` and `LastGood` constructions meet B-2's `Option` shape at the compiler, so none is missed silently. `parse_probe_headers` returns an error when the five-hour header is missing; whether a header-derived reading may carry an absent session is #1588's question and the parser's, not this design's. If #1588 lands first, #1597's sweep baselines grow by these sites. The transitional state (B-6t) needs no minor and does not contend |
| **#1538** `(fix) daemon: the blind-preempt swap also fires on the #584 velocity projection` | OPEN, `CHANGES_REQUESTED`; MERGEABLE on 2026-11-01, CONFLICTING since #1613 merged (`7f5f7f4`, 2026-11-02; live `mergeable_state=dirty`); edits `blind_gate_armed`, `blind_swap`, `records.rs`, one `BlindActive` doc line and ADR-0017 in place | the blind seam B-3 touches; ADR-0017, which B-10 also amends in place | **Composes.** Both read the anchor's session value; under B-3 that value is `Option<f64>` and both arms read false on `None`. Whichever lands second rebases; the two ADR-0017 `**Amended**` paragraphs are additive and order by date. No re-scoping of #1538's velocity arm |

### 4.7 Platform

The parse path, the classifier, selection, the store and the wire carry no target-specific code
(`cfg` only under `cfg(test)`, per the investigation). The change is platform-neutral by
construction; Linux is landed but no CI job compiles for it, so this design assumes — and says —
that a green run proves nothing about Linux, and that the change adds no syscall, path or crate that
could regress the #963 port.

## 5. Building Blocks

### B-1 — the parser serves every window it finds (S1, S2)

`dimension` (`src/usage.rs:548-582`) is unchanged: it already reads the top-level object first
(`fraction_of` at `src/usage.rs:553-557`) and then scans `limits[]`, skipping any entry whose `kind`
is outside the window's alias list (`is_some_and` … `continue` at `src/usage.rs:563-566`), which is
why `weekly_scoped` and the 25 null top-level keys are inert at the lookup (`REQ-QW-B-001`); what PR #1537 does after the lookup is D9's. `parse_usage`
(`src/usage.rs:503-537`) stops using `?` on the two `dimension` lookups at `src/usage.rs:507-508` and composes:

```rust
let session = dimension(&root, "five_hour", &["session", "five_hour"]);
let weekly  = dimension(&root, "seven_day", &["weekly_all", "seven_day", "weekly"]);
if session.is_none() && weekly.is_none() {
    // D1 (a), settled by council on 2026-11-03 — Oracle C pins it.
    return Err(Error::UsageParse("no quota window (neither five_hour nor seven_day)".into()));
}
Ok(UsageReport { session: session.map(|w| w.0), session_resets_at: session.and_then(|w| w.1), … })
```

- **D1 (a)**: a 200 reply carrying no applicable window stays a hard `UsageParse`, so a 200 reply
  with nothing to decide on maps to `PollOutcome::Transient` exactly as today and the loop skips and never swaps on the reading (the ADR-0017 preempt off a retained anchor is unchanged); settled by council on 2026-11-03. Option (b) — `Ok` with both windows `None` — writes no data, so nothing durable prevents reversal; in code it is one guard, Oracle C's (b) form (the scoping party's to author), N4 dropped and three tests and two doc comments moved, and in substance it opens the `Live` path to every 200 body the parser does not recognise, so it waits on an observed windowless account and on a reader that tells such an account from such a body.
- **D2 (identical)**: `"five_hour":null` and an absent `five_hour` key both fall through to the
  `limits[]` scan today, and no code distinguishes them; one test over both bodies pins the relation.
- `UsageReport` keeps its two `Option<String>` resets and two `Option<String>` severities;
  `to_usage` (`src/usage.rs:191-204`) maps per window. Invariant **I-1**: a `*_resets_at` is `Some`
  only when its fraction is `Some`, because `dimension` returns the tuple together — pinned by a
  test, so no consumer ever meets a stamp without a fraction.
- The doc comment at `src/usage.rs:473-474` (`UsageParse`) is rewritten by B-10.
- Tranche 0 meets this block first (§ 1b): the hot-fix's weekly-only branch in `parse_usage` and the flag
  `UsageReport.session_absent` (carried as `PolledReading.session_window_absent`) exist when #1596
  starts, and #1596 removes the branch and both flags (its § 7, AC-17). The composition above is the end
  state.

### B-2 — the reading model expresses "does not apply" per window (S3)

`Usage { session: Option<f64>, weekly: Option<f64>, weekly_resets_at, session_resets_at }` with
`Copy`; `LastGood` and `BlindAnchor` likewise. `to_pct` (`src/daemon/snapshot.rs:1075`) is unchanged
and applied through `Option::map`. The retained anchor at `src/daemon.rs:3119` (`LastGood`) copies the
`Option` through; a weekly-only active therefore retains an anchor whose session is `None`, which is
what makes the blind gate's "nothing to protect" reading honest rather than absent (B-3 row 7).

### B-3 — every decision arm names its move (S3)

| # | Arm | Site at `fd8640b` | Move | Design statement | Test it owes (Cap-2) |
|---|---|---|---|---|---|
| 1 | Target selection, session gates | `session_ceiling` … `is_none_or` at `src/daemon/selection.rs:252-253` | pass | `usage.session.is_none_or(\|s\| s < session_ceiling)` and the floor likewise; the weekly line at `src/daemon/selection.rs:247` (`weekly_ceiling`) becomes `usage.weekly.is_none_or(…)` — a weekly-less account is never weekly-blocked | a weekly-only spare at weekly 0.90 is viable; at 0.98 it is excluded exactly as a two-window peer (`REQ-ARM-B-001/002`) |
| 2 | Target selection, missing reading | `filter_map` at `src/daemon/selection.rs:246` | — (unchanged) | a served reading is `Some(usage)`; only a failed poll is dropped here | covered by row 1 |
| 3 | Reactive swap | `decide` at `src/swap.rs:129-134` | skip (per axis) | `usage.session.is_some_and(\|s\| s >= session_threshold) \|\| usage.weekly.is_some_and(…)`; reason attribution (`SwapReason` at `src/daemon.rs:4588-4592`) reads `Session` only from a `Some` session at/over its threshold | a weekly-only active at weekly 0.99 swaps with `reason=Weekly`; no `ParkedLanding` (`src/daemon.rs:4610-4614`) is armed (`REQ-ARM-B-003`) |
| 4 | Velocity projection | `velocity_swap` at `src/daemon.rs:5465`; `active_usage.session` at `src/daemon.rs:5502-5516` | skip | returns `TickAction::Held` when the session fraction is `None` — nothing to project | held with an absent session at any EMA |
| 5 | Exhaustion bookkeeping | `note_exhausted_poll` at `src/daemon.rs:6092-6115`, `out_of_rotation` | skip (per axis) | `out_of_rotation = weekly.is_some_and(>= line) \|\| session.is_some_and(>= ceiling)`; `exhausted_poll_window` (`src/daemon.rs:6193`) reads only the blocking axis | a weekly-only peer arms the slow-poll window on its weekly line only |
| 6 | All-exhausted relief | `all_exhausted_relief` at `src/daemon.rs:6297-6341`; `spare_relief` at `src/daemon.rs:6241` | name | `session_blocked = usage.session.is_some_and(…)`; a weekly-only blocked spare yields `cause: weekly`, `hold` = that spare, `resets_at` = its weekly reset — a positive case, never the `(None, None)` default at `src/daemon.rs:6339` (`SwapReason::Weekly`) | `REQ-ARM-B-005`; `next_swap` is not `no_viable_target` while a spare whose reply was rejected before this change for its missing session window now has a served reading with weekly headroom (`REQ-ARM-B-006`; `AwaitingData` at `src/daemon.rs:5666-5679` unchanged) |
| 7 | Blind gate (ADR-0017), surfacing half | `blind_gate_armed` at `src/daemon.rs:470-472`; `blind_velocity_projected_armed` at `src/daemon.rs:503`; `plausible_anchor_session` at `src/swap.rs:745`; `blind_active_view` at `src/daemon/records.rs:487` | skip + name | the pure predicate takes `anchor_session: Option<f64>` and reads **false** on `None`, so its two production callers — `note_blind_gate_eligibility` (`src/daemon.rs:3947`) and the report arm `blind_active_view` — cannot drift (the single-source rule the gate already states); `blind_swap` (`src/daemon.rs:5081`) does not call the predicate: its anchor arm compares `plausible_anchor_session(…)` against the config thresholds `session_blind_risk_band` and `session_blind_swap_secs`, which the code keeps deliberately distinct from the interim constants, and under the `Option<f64>` signature that comparison reads false on `None` too; `plausible_anchor_session` becomes `Option<f64> -> Option<f64>`; `blind_active_view` emits `last_known_session_pct: None` (B-6) and never `0`. The **arming** half — whether a weekly anchor should arm a preemptive swap — is **OAD-2** (D3), outside this fix | a weekly-only active blind past `BLIND_GATE_SECS` (`src/daemon.rs:370`): no swap, no "auto-protection OK" (`REQ-ARM-B-007`) |
| 8 | Near-limit fast poll (D4) | `plausible_active_usage` at `src/daemon.rs:5799-5804`; `near_limit_fast_poll_engaged` at `src/daemon.rs:5826` | skip | the band is session-only and reads false for an absent session — stated, with a test, not accidental | `REQ-ARM-B-008` |
| 9 | Session high-water mark | `fold` at `src/swap.rs:651-665` | skip / re-seed | `let window = usage.session_resets_at?; let session = usage.session?;` — I-1 makes the second `?` unreachable alone; a vanished stamp releases the mark as today; the next `Some` reading seeds a fresh mark | `REQ-ARM-B-004`, both directions of `REQ-ARM-B-011`; `session_high_water_needs_a_window_stamp` (`src/swap.rs:1603`) stays green |
| 10 | `use` verb gate | `poll_viability` at `src/use_account.rs:407-424`; fallback `cached_viability` … `poll_viability` at `src/use_account.rs:451-462`; daemon-routed `weekly_exhausted` at `src/daemon/commands.rs:146-148` | name / pass | `reading.usage.weekly.is_some_and(\|w\| w >= weekly_ceiling)` → `WeeklyExhausted`, else `Viable`; a weekly-less reading is `Viable` (no line to exhaust) | `REQ-ARM-B-009`: `use` without `--force` serves the weekly verdict instead of aborting |
| 11 | Liveness | `classify_poll` at `src/daemon/classify.rs:55-62` | — (unchanged) | `Ok(_)` is `Live`; gap and blind streaks end; sampling resumes | `REQ-ARM-B-010` |
| 12 | Velocity EMA and the tie-break (D7) | `usage_velocity` at `src/daemon/classify.rs:121-125`; `velocity_rate` at fd8640b, `observed_climb` at `src/daemon/selection.rs:156-163` on `main` | skip / pass | `usage_velocity` returns per-axis `Option<i16>`; the EMA fold skips an absent axis and resets the slot to `None` on every served reading whose session window is absent, as a session drop resets it today, so no consumer reads a pre-vanish EMA and a window that reappears seeds a fresh one (`REQ-ARM-B-011`); on a reset tie the velocity axis passes vacuously for a spare whose reading carries no session window — it ranks in the no-observed-climb class with every spare whose EMA is unsustained or zero, ahead of every sustained climber and level with the rest, which the jitter key orders — read from the reading by a typed key, never as a number (D7, settled by council) | `REQ-ARM-B-012`: a weekly-only spare and a two-window spare rank on the same soonest-weekly-reset axis; on a tie the weekly-only spare beats a sustained climber and shares the tie with a flat or un-warmed peer, and still beats a climber when its own slot keeps a steeper EMA from an earlier window (test 24 arm (B)); `REQ-ARM-B-011`: a vanished window leaves no EMA (test 23) |
| 13 | Weekly-exhausted verdict on the wire | `weekly_exhausted` at `src/daemon/snapshot_build.rs:439-440` | pass | `is_some_and(\|usage\| usage.weekly.is_some_and(…))` | a weekly-less account is never `weekly_exhausted` |
| 14 | Keep-warm | `should_keep_warm_retry` at `src/daemon/keep_warm.rs:44` | — (unchanged) | reads liveness only | `REQ-ARM-C-001` |
| 15 | Event fields | `Swap` … `session_pct: u8` at `src/observability.rs:1364-1368` on `main`; `UsageVelocity` … `session_delta_pct` at `src/observability.rs:2049-2051` on `main`; `BlindWindow` at `src/observability.rs:2092` on `main`; `session_high_water_pct` at `src/observability.rs:2126` on `main`; `BlindEnter` at `src/observability.rs:2151` on `main`; `BlindExit` at `src/observability.rs:2196` on `main`; `BlindGateEligible` at `src/observability.rs:2228` on `main` | name | every **reachable** per-window event field becomes `Option`, rendered as an **omitted token** on the log line (the `session_high_water_pct` precedent: an absent token means "none", never "unknown"); `BlindGateEligible.session_pct` stays `u8`, because its only emitter, `note_blind_gate_eligibility`, runs past `blind_gate_armed` on the anchor's plausible session, and under D3's default (row 7) `blind_gate_armed` reads false on `None` while `plausible_anchor_session` passes a `None` anchor through (#1602 tests 11 and 12, mutants M-l and M-m), so the field cannot meet an absent window while that default holds (#1602 Specification § 3; the scope brief's D14); the emitter narrows the raw anchor's `Option` beside that gate with a `let Some(raw) = anchor.session else { return; }` — a not-armed-style return that leaves the `blind_gate_signaled` latch untouched — never with `unwrap`, `expect` or `unwrap_or(0)`, which AC-16's greps do not see; a D3 amendment that arms the gate on an anchor without a session window lifts this field to `Option<u8>`, rendered as an omitted token, in the same change. `reliability`'s swap handlers already read `session_pct` optionally, but its blind-projection parser does not — `record_blind_projection` (`src/reliability.rs:1287-1301` on `main`) counts a missing `session_pct` as `malformed` — so the implementer gives the `blind_*` parsers a disclosed bucket for the absent token, `n_without_session_window`, rather than tolerance, keeping the #484 exact partition and admitting a line only when its always-present `duration_secs` and `near_limit` tokens parse, so a torn append stays `malformed`. `log`'s `JSON_SCHEMA_VERSION` does not move (`LogWire` at `src/log.rs:865` carries the line's own tokens); `reliability`'s moves 13 → 14 for that bucket (§ 4.5, AD-14, § 8) | `REQ-RDG-C-001`: an event for a weekly-only account carries no fabricated session percent and `reliability` decodes the emitted shape |

Nothing durable remembers the state: no persisted flag, no config key, no new field in the daemon's
state file (`REQ-ARM-B-011`). A window that disappears stops acting as a current reading because every
consumer re-derives per poll (rows 4, 7, 9) or clears its retained value (row 12); a window that reappears is a fresh window because the
next `Some` reading seeds the mark and re-arms every session gate.

### B-4 — the fixture bodies the tree lacks (S4)

Inline `r#"…"#` bodies in `src/usage.rs` `mod tests`: the captured quota fields of PRD § 2.2
verbatim (no identity field — never the email, label or UUID); the same body with the `five_hour`
key absent (D2); Oracle D's session-only body; Oracle C's windowless body; and the two-entry
`limits[]` with `weekly_scoped` inactive. Three of these — the captured body, Oracle C's and Oracle D's —
land first with B-5 (AD-23), so B-4 reads them by name and declares only the absent-key and
`limits[]`-only bodies: one constant per body, never a second copy under another name. Every
parse-test body present at `fd8640b` (the newest dated
2026-06-29) stays green under the new shape — the investigation's feasibility axis measured it, and
Cap-1 re-measures it. A captured shape is frozen: never re-baselined.

### B-5 — Oracles A–D land `#[ignore]`d and RED first (S5)

The first, separate item adds four ignored tests to `src/usage.rs` `mod tests` and records their RED
runs in the item — A, B and D against `origin/main`, C against the fabricating mutant (the RED column
below); the fix item un-ignores them and never edits them.

| Oracle | Proposed test name | Asserts | RED against |
|---|---|---|---|
| A | `oracle_a_the_captured_null_session_reply_parses_with_its_weekly_window` | `parse_usage(captured)` is `Ok`; weekly `0.90 ± 1e-9`; `weekly_resets_at` `"2026-11-06T22:59:59.916061+00:00"`; weekly severity `None` (C7, tracked as #1606, pinned deliberately) | `fd8640b`: `Err("no session (five_hour) dimension")` |
| B | `oracle_b_the_captured_reply_reports_no_session_window_never_a_fraction` | the same parse's session window is `None` — never `0.0`, never any fraction | the fabricating mutant (`session == 0.0`), which passes A |
| C | `oracle_c_a_reply_with_no_window_is_never_two_fabricated_windows` | `{"five_hour":null,"seven_day":null,"limits":[]}` is `Err(UsageParse)` under D1 (a); under (b) the assertion is "both `None`"; never any fraction | `fd8640b` passes (a) today only by the session error string — the oracle asserts the D1 outcome and the absence of fractions, so it is RED against the mutant |
| D | `oracle_d_a_session_only_reply_parses_with_its_weekly_window_absent` | `{"five_hour":{"utilization":42.0,"resets_at":null},"seven_day":null,"limits":[]}` → `Ok`, session `0.42`, weekly `None` | `fd8640b` and the mutant alike |

RED proof: `cargo test -- --ignored oracle_` filtered to the four, **never** `--include-ignored`.
The scratch proof the investigation ran is the feasibility axis's own and not independent; the
scoping party re-proves RED (Cap-8).

**Against the tranche-0 tree** (§ 1b): the table's RED column is read against `fd8640b`. Once #1613 has
merged, the hot-fix is itself a fabricating mutant, so A passes against it, B fails on its session-window
assertion (`Some(0.0)`) instead of on the parse error, C stays green, and D still fails on the weekly
parse error. The oracles stay `#[ignore]`d, and each RED record names the tree it ran against.

### B-6 — the status/watch wire says "no window" (S6)

**The key** (D11, settled by council on 2026-11-03 — the list key ratified as the wire shape and amended in text, § 12 AD-7): one omittable per-account key

```json
"absent_windows": ["session"]
```

- Rust: `absent_windows`, a `Vec` of a two-variant enum, on `AccountStatusLine` (`src/daemon/snapshot.rs:737`),
  `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, the enum serialised lowercase
  (`session` before `weekly`, so the bytes are deterministic; its identifier is the executor's and not `WindowKind`, which already names a private enum in `src/stats.rs` — D11). Emitted only from a **served** reading whose window is `None`, never derived from the retained anchor (a failed-poll row and a blind row carry no key; a blind row's no-session-window fact rides the percent-less `blind_active` block, AD-8); a failed poll
  has no reading and emits nothing, so "no reading" stays `session_pct: null` without the key and "no
  window" is `session_pct: null` **with** it — distinguishable by bytes (`REQ-STL-I-001`).
- Why a list rather than a boolean `no_session_window`: message 2's weekly-less account rides the
  same key with `["weekly"]` and no second minor; the omit-when-absent precedents — `blind_active`,
  added at 1.4; the daemon-level `canonical_scrub`, added at 1.5; `rotated`, reshaped at 1.14 (the
  1.10 → 1.11 `systemic_refresh_source` and 1.11 → 1.12 `expiry` bumps followed the same pattern), in
  the history the `STATUS_SCHEMA_VERSION` doc comment keeps at `src/daemon/snapshot.rs:538-541` — are
  all keys that are absent when nothing applies. Rejected: an enum per window (`session_window: "absent"`) — one fact split across two keys, and a second minor only if the weekly key is deferred (D11).
- Swift: `let absentWindows: [String]` decoded with `decodeIfPresent … ?? []` beside `sessionPct` (`apps/menubar/Sources/WireModel.swift:648`); beyond this read and the `UInt8?` reshape below, production `WireModel.swift` needs no decode edit (the support gate is major-only, `decodeIfPresent` at `apps/menubar/Sources/WireModel.swift:704`); the panel renders the state as D0 classified it (B-11, #1594).

**The reshape**: `BlindActive.last_known_session_pct: u8` (`src/daemon/snapshot.rs:167`) becomes
`Option<u8>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`, exactly the `rotated` reshape (`src/daemon/snapshot.rs:141-142`): a weekly-only blind active keeps its `blind_secs` and
`auto_protection_degraded` and carries no fabricated percent (`REQ-STL-I-002`). Swift
`lastKnownSessionPct: UInt8` (`apps/menubar/Sources/WireModel.swift:499`) becomes `UInt8?` via `decodeIfPresent` — the decode half of S6's Swift lockstep.

**The render half is a panel state the mock never authored.** `BlindMeter` reads the percent as a value for its held bar, band and text (`lastKnownSessionPct` at `apps/menubar/Sources/StatusPanelRoster.swift:911-924`) and the row's spoken phrase says "last-known session N percent" (`apps/menubar/Sources/StatusPanelFormat.swift:2197`), so what the held block shows for a weekly-only blind active is classified with D0 (D15, § 9a's fifth cell; settled by council on 2026-11-03), and S6's panel render half is #1594's exactly as S8's is.

**Why the two halves land together.** `blindActive` is decoded with `decodeIfPresent(BlindActive.self, …)` (`apps/menubar/Sources/WireModel.swift:712`), and `BlindActive`'s doc says a present block missing a required field throws rather than mis-reads (`apps/menubar/Sources/WireModel.swift:486-493`), so a frame carrying a percent-less block fails to decode on an un-updated app, and the decode half ships with S6. In the other direction, `lastKnownSessionPct: UInt8?` does not compile until `BlindMeter`'s `utilSeverity(_:)` and `Double(_:)` reads (`lastKnownSessionPct` at `apps/menubar/Sources/StatusPanelRoster.swift:911-922`) change with it: both take a non-optional `UInt8`. The spoken interpolation (`lastKnownSessionPct` at `apps/menubar/Sources/StatusPanelFormat.swift:2197`) compiles against an optional with a warning only — `apps/menubar/project.yml` sets no warnings-as-errors — and is changed with them so that it never prints `Optional(87)`.

**The panel interim, and its cost.** D15 (settled by council, 2026-11-03) classifies the percent-less held block and leaves its build to B-11 (#1594), so S6 draws none of it: until #1594 lands, the panel renders a decoded percent-less block **as no block**, today's two-meter `n/a` row — the failed-poll row B-6t already ships for this account, which no mock frame draws — while the menu-bar glance reads the block's verdict (#1599 § 5, settled by council, 2026-11-04). On the panel row the blind duration and verdict the wire now carries stay invisible until #1594 (the CLI footer shows them), so if #1594 stalls after S6 deploys, DP-1's days become "until #1594" on the panel row alone (R-D9).

**CLI**: the `~N%` cell at `src/cli.rs:3867` (`last_known_session_pct`) falls back to
`pct(account.session_pct)` on `None`, and the blind sentence in `render_blind_active` (`src/cli.rs:3236`) states the duration without a percent — prose in the footer, not a token in the
SESSION% cell, which stays B-11's.

**The minor and the lockstep** (`REQ-STL-C-001`): `STATUS_SCHEMA_VERSION` (`src/daemon/snapshot.rs:538-541`)
takes the next free minor under the queue rule (§ 4.6); regenerate the five wire goldens once with
`cargo test -- --ignored emit_wire_golden_fixtures` (`src/daemon/snapshot_build.rs:3862`; the pin
`the_committed_wire_goldens_still_match_the_daemon_encoders` at `src/daemon/snapshot_build.rs:3929`);
add one **new** golden row carrying `absent_windows` and one carrying a `blind_active` block without
`last_known_session_pct` — every current golden row carries `session_pct: 60`, so these are new
fixtures, not re-baselines; sweep the 27 `"minor":14` lines in `apps/menubar/Tests/Fixtures.swift` (first at `apps/menubar/Tests/Fixtures.swift:43`), the 14 sites across both spellings in
`apps/menubar/Tests/WireDecoderTests.swift` (first `SchemaVersion(major: 1, minor: 14)` assertion at `apps/menubar/Tests/WireDecoderTests.swift:18`)
and the `"minor":15` "future daemon" frame at `apps/menubar/Tests/WireDecoderTests.swift:325`, which
stops being future on the bump and moves one minor ahead; add a pinned fixture at the outgoing minor
where tolerance was proved there. The wire test
`status_response_carries_handles_and_percentages_and_never_a_secret` (`src/daemon/snapshot_build.rs:642`) keeps its "never a fabricated `0`" assertion and gains the
no-window row.

### B-6t — the transitional wire state tranche 1 ships before B-6 (D10)

Tranche 0's hot-fix (#1613) ships before this state and shows `session_pct: 0` for a weekly-only account
(§ 1b). The table is what tranche 1's deploy changes that to, so `session_pct: null` replacing the 0 is
the state this block documents.

Settled by council (D10) on 2026-11-03 at the maintainer's delegation: the transitional state, the omitted block (AD-6's sub-default) and the dated signpost ratified together, amended as the rows below record:

| Element | Transitional behaviour under `1.14` | Retired by B-6 |
|---|---|---|
| A served weekly-only reading | `session_pct: null`, `session_resets_at: null`, `weekly_pct` and `weekly_resets_at` present, `weekly_exhausted` from the weekly line | the same line gains `absent_windows` |
| `session_pct` doc comment (`src/daemon/snapshot.rs:758-763`) | amended: "`null` if the last poll failed — or, transitionally until the status-wire item #1599 lands its no-window key (S6), a `null` beside a non-null `weekly_pct`: the served reading carried no session window (a failed poll nulls both percents)" | the transitional sentence is deleted and the comment keeps its steady-state meaning (`REQ-STL-B-001`): `null` if the last poll failed — and gains exactly one sentence (#1599 § 7): a `null` on a line whose `absent_windows` lists `session` is not a failed poll, and that key is the only way to tell the two apart; the Swift mirror's doc comment (`AccountStatusLine.sessionPct`) gains the same sentence in the same change |
| CLI SESSION%/RESET cells (`pct` at `src/cli.rs:4595-4598`, `reset_cell` at `src/cli.rs:4365-4368`) | `n/a` / `n/a`, exactly as today — the failed-poll token, which the mock calls a misreport for an observed absence; accepted for days, documented | B-11 under D0 |
| 1.14 Swift app | decodes without edit; the Session meter is uncoloured `n/a` (`sessionSeverity` at `apps/menubar/Sources/StatusPanelFormat.swift:1755`) beside a normal Weekly meter; each meter carries its own reset label (`UsageMeter` … `reset:` at `apps/menubar/Sources/StatusPanelRoster.swift:443-446`), so the session slot shows the failed-poll `n/a` and the weekly slot its real reset | B-11 under D0 |
| **A weekly-only active that goes blind** (sub-default) | **no `blind_active` block is emitted**: `blind_active_view` returns `None` when the anchor's session is `None`, because under `1.14` the block cannot be emitted without a `u8` and a `0` would be the fabrication this design forbids. Cost, stated precisely: blindness reaches both surfaces only through that block — the CLI `health_cell` (`src/cli.rs:3937`) carries the auth rollup alone, and the panel's blind treatment keys solely on `blindActive` (`apps/menubar/Sources/HonestStateMachine.swift:386`) — so for those days such an account is indistinguishable from an ordinary failed-poll row, duration lost as well as percent (the pre-#479/#485 presentation), PRD § 11.1's Stale "Required" cell is unmet, the footer makes no auto-protection claim (`REQ-ARM-B-007`, D3's surfacing half), and the Retry-After report arm is silent for it (the velocity arm cannot fire on a session-less anchor either way — the D11 ruling's correction of AD-8). Two corrections to the bounds this row once claimed: the bundle before tranche 0 could not read such an account, but an account active across the lift kept its last pre-lift two-window anchor (`last_good` is set only by a successful active poll and dropped only by a swap or an active loss; a parse failure leaves it), so whether that bundle emitted a block for it is unestablished, and the comparison that holds is against the tranche-0 bundle, whose block carries such an anchor's session as `0.0` (§ 1b) — a `0 %` and an `auto-protection OK` the omission rightly drops (measured against the tranche-0 bundle that serves today, which emits the block for such an anchor with `last_known_session_pct: 0` and, blind past `BLIND_GATE_SECS` with no `Retry-After` hold, `auto_protection_degraded: false` (the anchor arm reads `0.0 >= 0.60` as false)), together with the blind duration and the CORNERED alarm (`cornered_state` composes `blind_active` with `no_viable_target`), which it loses; and the captured reply is the operator's active account (PRD § 2.2) and only the active carries `blind_active`, so the interim cost landing on the active account is the whole exposure, not a bound on it. Signpost: S6 (#1599) merges and #1608 deploys it within 14 calendar days of the tranche-1 deploy record on #1604 (the comment that closes it) — tranche 2's own one-to-two-week appetite (PRD § 3.1), measured from the day the transitional state first serves; S6 is exempt from tranche 2's ship-what-is-done breaker, because REQ-STL-B-001 forbids the transitional state outliving it. Before that day passes, the operator records on #1590 either #1599's merge commit and #1608's deploy record, or a comment naming the cause and re-ratifying D10 by council with the interim's observed cost in hand and a new dated bound. (R-D5) | the block returns with `last_known_session_pct` omitted; the blind row, its duration and its verdict return with it |
| **A `Retry-After` swap off a session-less anchor** (D10's analogue; settled by council with D10 on 2026-11-03 — the omitted block and this notice ratified together; #1602 Specification § 4) | the swap still fires and its `swap` event-log line records it with `session_pct=` omitted, but **no `BlindPreemptSwapRecord` is built**, so no `recent_blind_preempt_swap` notice is served. The #582 `Retry-After` arm fires whatever the anchor's session (the `server_retry_after_holding` arm of the blind gate in `src/daemon.rs`), and every link of the notice is a `u8` under `1.14`: the record's field (`src/daemon/records.rs:330` on `main`), `recent_blind_preempt_swap_view` (`src/daemon.rs:597-614`), which copies it unconditionally, and the always-present wire key (`src/daemon/snapshot.rs:204`). A `0` there would be the fabrication this design forbids. Cost, stated precisely: for such a swap the CLI `status` prints no `swapped off {from} (blind @ last-known {pct}%) → {to}` narration and no `use <from>` undo hint (`src/cli.rs:3261-3272`); the menu-bar app does not decode the notice, so the panel loses nothing; the swap, its log line and the #582 walk record are unchanged, and a two-window anchor's notice is unchanged. Rejected: lifting the record to `Option<u8>` in tranche 1, which would make the wire key omittable under `1.14`, a reshape that belongs to B-6's minor | the record and `BlindPreemptSwap.last_known_session_pct` become `Option<u8>`, the key is omitted for a session-less anchor, and the notice returns with its labels and its undo hint (#1599 § 2b) |

What the transitional state is not: a redefinition of `null`. The steady-state contract stays "the
last poll failed"; the transitional meaning is named, dated and tied to the item that retires it.

### B-7 — the sample store carries `"session":null` and every reader copes (S7)

- Tranche 0 meets this block first (§ 1b): the hot-fix returns early from `append_sample_for_poll`
  (`src/daemon.rs`) when `session_window_absent` is set, so no sample line exists for such a
  reading. #1598 replaces that skip with the `"session":null` line below (its § 8, AC-16). The first `null`
  line is written at tranche 1's deploy, which is when B-12's no-older-daemon rule starts to bind.
- `Sample.session: f64` (`src/usage_store.rs:135-166`) becomes `Option<f64>` with
  `#[serde(default)]` and **no** `skip_serializing_if`, so the key is always written — `null` for an
  absent window (`REQ-SMP-B-001`; #1580's exact-key-set oracle is green on `null` and RED on
  omission). **D12** (settled by council on 2026-11-03 as the session half's shape on the second axis; compiler-forced by S2): `Sample.weekly`
  takes the same shape, because `append_sample_for_poll` (`src/daemon.rs:718-754`) passes
  `reading.usage.weekly` into `Sample::new` and a weekly-less served reading (`REQ-RPL-B-002`) leaves
  the writer the same three choices on that axis — a number (forbidden), no line (contradicts gap
  honesty), or `null`.
- `read_samples` (`src/usage_store.rs:449-467`) decodes such a line; `compact_and_roll` (its `read_samples` … `write_samples` sequence at `src/usage_store.rs:535-544`) retains it (`REQ-SMP-B-002`).
- **The tiers** (D13, ratified by the council ruling of 2026-11-02, which also added the per-axis
  `samples: Option<u32>` count absorbed from #1610): `HourBucket.session` /
  `HourBucket.weekly` (`src/usage_store.rs:237`, `HourStat` at `src/usage_store.rs:250`) and
  `DayBucket.session` / `DayBucket.weekly` (`src/usage_store.rs:261`, `DayStat` at `src/usage_store.rs:278`) become `Option<_>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`;
  the fold at `src/usage_store.rs:669-680` (`HourStat`) folds each axis over the samples that carry
  it and writes `None` when none does, so `max_of` (`src/usage_store.rs:807-809`, `NEG_INFINITY`) is
  never called on an empty slice and `serde_json` never meets a non-finite float. A bucket's `count`
  stays the hour's sample count; a per-axis stat is over the readings that carried that axis — an
  observation, not a fabrication — and the doc comments say so. The tiers' per-axis stats have no
  production reader today beyond their keys (`read_rollup` at `src/stats.rs:215` feeds the window
  bounds), so this is a representation change with no consumer to move. **Version-skew asymmetry,
  stated**: only an older **daemon** could erase a raw `"session":null` line, because
  `compact_and_roll`'s one production caller is the daemon's store maintenance
  (`stats_events_for_poll` at `src/daemon.rs:839`) — and the rollup fence stops it before it does. On a
  store a #1598 build has compacted, an older daemon's `read_rollup` fails on the format-2 envelope
  (`read_rollup` at `src/usage_store.rs:475-486` hard-errors with `UsageRollupMalformed`), so its
  `compact_and_roll` returns before it writes either file (`read_rollup` at `src/usage_store.rs:536` runs
  before `write_rollup` and `write_samples` at `src/usage_store.rs:543-544`); the daemon discards that error
  (`if let Ok(summary) = compact_and_roll(…)` at `src/daemon.rs:894`, no `else`), stops compacting without
  a word, rewrites nothing and erases no `null` line — a stall of that store's compaction, which ends
  when a #1598+ build runs again; residual (4) below covers the older binary's own compaction. An older
  `stats` or `reliability` only reads and deletes nothing: `stats` **loudly** refuses every fenced
  rollup with `UsageRollupMalformed` ("malformed usage rollup"), whatever its buckets hold, and
  `reliability`, which never reads the rollup, leaves the `null` lines out of its report in silence.
  #1598's fence condition C6, the operator text, makes that "malformed usage rollup" message from an
  older build expected after #1598.
  The ruling accepts four residuals (PRD R4 records the risk), and an older daemon erases `null` lines
  in only the first two: (1) lines appended before the first fenced write — the first poll after the
  deploy appends, then compacts and writes the fence, so only a failed first pass leaves lines
  unfenced, for up to one compaction cadence, an hour; (2) a rollup deleted or moved by hand, after
  which an older build reads the store as new and its compaction erases every `null` line — C6 says
  the store files are never deleted or moved; (3) an older `reliability`'s silent omission of `null`
  lines from its report; (4) every pre-#1598 build's own `stats` and compaction, stopped for good on
  each machine that has run #1598. That stop is the loud failure the fence exists for, and only its
  `stats` half is loud: an older daemon's compaction stops in silence, as above. Loud is the better failure.
  The D6/D13 council ruling of 2026-11-02
  (a comment on #1598) ratified this representation and amended it with a **rollup fence** #1598 builds —
  `usage-rollup.json` becomes `{"format":2,"rollup":{…}}`, which every pre-#1598 build fails to read, so
  its `compact_and_roll` returns before it rewrites the raw file — and with the deployment rule B-12
  carries (`REQ-DEP-C-001`). The fence's conditions C1 to C5 and C7 take their scenarios in the stub's
  Rule 6, with C1's committed-fixture arm in Rule 4's T10 scenario; C6, the operator text, is the
  module doc's under Rule 5 and the runbook item #1603's; the per-axis `samples` count takes its
  scenarios in Rule 4, with its module-doc clause and its count-posture sweep (AC-27) in Rule 5 and
  its pins in Rule 6's T17 — all in `docs/specs/sample-store-session-null.feature.md`.
- Tests that move with the shape (`REQ-SMP-C-001`): the canary
  `the_sample_store_cannot_carry_a_non_finite_session_reading` (`src/reliability.rs:7386`) moves twice.
  Its decode loop (`from_str` … `is_err` at `src/reliability.rs:7414-7423`) moves its `"null"` arm to assert
  `Ok(None)` and keeps the spellings `Infinity`, `-Infinity`, `NaN`, `1e400`, `-1e400` refused. Its encode
  loop, which serialises in-memory `f64::INFINITY`, `NaN` and `NEG_INFINITY` readings and asserts that the
  result fails to decode, moves to assert that the serialisation itself is refused: `serde_json` writes a
  non-finite `f64` as `null`, which under `Option<f64>` would decode as `Ok(None)`, so #1598's field
  serializer refuses a non-finite `Some` before it reaches disk and `null` on disk keeps one meaning
  (#1598 Specification § 1; the sample-store stub's Rule 1);
  `sample_round_trips_with_and_without_optionals` (`src/usage_store.rs:870`) gains the `null` case
  for both axes; a compaction test over a store seeded with weekly-only lines asserts retention and a
  finite rollup (`REQ-SMP-Q-001`).

### B-8 — `stats` never renders an absent window as a reading (S8)

- `usage_stats` computes every session figure over the samples whose `session` is `Some`: `dim_stats` (`src/usage_stats.rs:1242`) over the present subset; the census `high_windows` (`src/usage_stats.rs:865`) excludes a sample with no session reading (**D5**, ADR-0033's amendment
  sentence in B-10); `relief_of` (`src/usage_stats.rs:1055-1057`) reads `session_blocked` false.
  An account whose samples carry no session reading contributes to the weekly figures and to no
  session figure (`REQ-STS-B-001`).
- **The wire** (shape decided here; timing is **OAD-3** / D8): on `AccountWire` (`src/stats.rs:2734`) every field that is a statistic *of* the session series becomes omittable —
  `session: Option<DimWire>` (`DimWire` at `src/stats.rs:2799-2803`) and `band: Option<Band>`
  (`Band` at `src/stats.rs:2913`; `Band::of` at `src/stats.rs:3061`), both
  `skip_serializing_if = "Option::is_none"`. In the same bump `accounts[*].weekly` becomes omittable
  too (the weekly symmetry, D12's shape on the wire; #1601 AC-13), and the velocity object is emitted
  when either axis has a rate, with `session_pct_per_min` nullable, so a weekly-only account's weekly
  rate and runway reach the wire — today `velocity_wire` returns early on an unknown session rate
  (`src/stats.rs:3072-3073`) and hides them (AD-13 as amended 2026-11-01; § 8). The session-cap counts `cap_hits` and `time_at_cap_secs` (`src/stats.rs:2741-2757`, both counts *at the session cap*) take the same absence treatment — `Option`, omitted — because a `0` there reads as "never hit its session cap" for an account that has no session cap, which `MET-T-003` counts as a fabricated value on a surface. An existing key becoming
  omittable is a reshape, so `JSON_SCHEMA_VERSION` (`src/stats.rs:80`) moves 1 → 2 — the honest
  signal, by the log wire's own precedent (`src/log.rs:167`, `JSON_SCHEMA_VERSION`).
- **Lockstep**: Swift `StatsWire` (`apps/menubar/Sources/WireModel.swift:936`) decodes `schema` as a required `UInt32` (`apps/menubar/Sources/WireModel.swift:937`) and compares it to nothing — the major-only support gate guards the status wire alone — so the bump reaches the app only through its fixtures; `let session: StatsDim`
  (`apps/menubar/Sources/WireModel.swift:1010`) becomes `StatsDim?` (`StatsDim` at `apps/menubar/Sources/WireModel.swift:1029`) and the band likewise, via `decodeIfPresent`; the
  `statsBasic` fixture (`apps/menubar/Tests/Fixtures.swift:367`), its derivative `statsConfigUnreadable`
  (`apps/menubar/Tests/Fixtures.swift:385`, `statsBasic` plus one key), the in-source preview document
  `statsPreviewFixtureJSON` (`apps/menubar/Sources/PanelStatsModel.swift:202`) and `build/fixtures/wire-stats-basic.json`
  move to `"schema":2`, the golden regenerated once with `cargo test -- --ignored emit_wire_stats_golden_fixture` (`src/stats.rs:10991`).
- **The CLI `stats` table** reuses the surface's own UNKNOWN sentinel — the `—` that `stats` already
  prints for a figure it refuses to state (its doc comment calls it "the UNKNOWN sentinel"; one
  vocabulary across the surface) — for the session columns of such an account. That is reuse of a
  classified token on the same surface, not an invention. Its documented meaning there is UNKNOWN — "I could not see" — so the table does not distinguish "no window" from "never observed"; only the wire does, and G-4 holds on the wire. The SIGNAL cell (`signal_cell` at `src/stats.rs:2498-2504`), the velocity and runway cells, and the `cap_hits` / `time_at_cap_secs` columns (`src/stats.rs:3509-3527`), all keyed on the session series, degrade to `—` for such an account alike. Any change under `build/fixtures/cli-renders/**`
  (a new `stats` render fixture included) owes `emit_cli_render_goldens_stats` (`src/stats.rs:11771`)
  and the `CLI-Goldens-Rebaselined:` trailer.
- **The panel's Stats tab** renders `account.session.mean/peak` through `statsSessionMeanPeak` (`apps/menubar/Sources/StatusPanelFormat.swift:2332`; the cell at `apps/menubar/Sources/StatusPanelStats.swift:197`). A `nil` session dimension there was an
  unauthored state on a panel surface until the D0/D15 ruling — the register rule reaches it (the row's spoken summary, `accessibilityLabel` at `apps/menubar/Sources/StatusPanelStats.swift:216-224`, reads "Session mean N percent, peak N percent" and the cap-hit count today). **D15** — settled with D0 by council on 2026-11-03 at the maintainer's delegation; the three **no-session-window account** rows under *Decided to be nothing* in `apps/menubar/design/README.md` are the authority. The blind held block of a weekly-only blind active draws no held bar and no dashed outline, because nothing is held; it shows `—` in its own percent cell, at the edge its held percent occupies today, keeps the `blind {dur}` chip, captions `RATE-LIMITED`, and speaks "no session window" in place of "last-known session N percent". Its DEGRADED verdict line, the row's spoken verdict clause and the menu-bar glance label mirror the CLI's DEGRADED parenthetical for a block without a percent (#1599), never "acting on a stale anchor". The Stats tab's `Session m/pk` and `Cap-hits` cells read `—` in `.secondary`, the signal pill is omitted, the sparkline band stays empty, and the row summary speaks "No session window." in place of its signal, session and cap-hit clauses. The Swift half of B-8 still decodes `nil` without crashing, and B-11 (#1594) builds these cells.

### B-9 — the warm-up design reads the fraction (S9)

`docs/requirements/session-warmup-on-reset.md` (§ 3 at `docs/requirements/session-warmup-on-reset.md:95`;
R-2 at `docs/requirements/session-warmup-on-reset.md:122-124`) and
`docs/design/session-warmup-on-reset-solution-design.md` (C-1 at
`docs/design/session-warmup-on-reset-solution-design.md:45`; the predicate
`session_resets_at == None … clock stopped` at `docs/design/session-warmup-on-reset-solution-design.md:57`;
its own R-3 at `docs/design/session-warmup-on-reset-solution-design.md:90`; every line citation in this paragraph is at `fd8640b`) are amended in place so
C-1 reads two states instead of one: `session.is_none()` is **no window, nothing to warm** (R-2 never
matches); `session.is_some() && session_resets_at.is_none()` is the **stopped-clock state** R-1's
spike (#1231, closed GO on 2026-08-11; its AC-3 confirmed the stamp-versus-zero split) already
established. No shipped code changes; R-1's spike result is untouched (`REQ-REC-B-001`).

### B-10 — the records say "every dimension the account has" (S10)

In-place amendments on the repo's own practice — keep the original `date:`, add an
`**Amended 2026-11-… (#NNN)**` paragraph (the `**Amended 2026-07-21 (#665)**` form ADR-0013 already
carries), update the index row in `docs/adr/README.md` to the `Accepted (amended … by #NNN — …)` form
its rows 0006, 0012 and 0020 already use:

| Record | Passage | Amendment sentence (draft, operator's to ratify, except ADR-0033's, settled by council) |
|---|---|---|
| ADR-0013 | "Sound because **both** windows hard-reset" (`docs/adr/0013-session-floor-default-on-reserve-emergency-exempt.md:134`) | "…sound because every window the account has hard-resets; an account with no session window has no session floor to reserve against, and `pick_target`'s session gates pass it vacuously." |
| ADR-0017 | the gate's constants `T=300s` / `risk_band=60%` (`docs/adr/0017-bounded-blindness-preemptive-swap-not-header-observation.md:22`); "Never fires on a `None`/missing reading. It keys off the retained `last_good`" (`docs/adr/0017-bounded-blindness-preemptive-swap-not-header-observation.md:257`) | "…an anchor whose session window does not apply is such a `None`: the gate reads false, `status` claims no auto-protection on its strength, and whether a weekly anchor should arm the swap stays outside this fix until the reopen evidence the D3 ruling names is observed (OAD-2, settled (a) by council on 2026-11-03)." |
| ADR-0025 | "at least as strict as the negation of the release predicate on BOTH dimensions" (`docs/adr/0025-weekly-trigger-ceiling-semantics.md:48-50`) | "…on every dimension the account has; a dimension the account lacks is neither released nor acquired on." |
| ADR-0033 | "The census reports what it observed and refuses to interpolate" (`docs/adr/0033-weekly-saturated-session-low-stays-unknown.md:54-58`) | D5, settled by council on 2026-11-03 (the record is a comment on #1600), as ADR-0033 now states it: "…a structurally absent session window is excluded from the session census: it is not an unobserved reading. The census set is the roster minus every account none of whose in-period readings carries a session reading; a reading with no session window anchors nothing and covers nothing; a rostered account with no in-period readings at all stays in the set and empties the intersection (decision D5, ratified at the maintainer's delegation)." |
| Parser doc comment | `UsageParse` at `src/usage.rs:473-474` | "Each window is independently optional: a window the reply does not carry is reported as `None`, never a fabricated `0`; a reply carrying no window at all is a hard `Error::UsageParse` (D1), so the loop skips, not swaps." |
| Private HQ `REQ-USG-B-001` | "both dimensions" (quoted in PRD § 2.4) | **owed by the operator**, recorded as owed in PRD § 2.4 / § 7 and here (`REQ-REC-B-004`); not reachable from a clone |

Ordering (`REQ-REC-C-001`): the ADR and parser-doc amendments land with or before the commit that
moves `a_missing_dimension_is_a_parse_error_not_a_fabricated_zero` (`src/usage.rs:959`).

### B-11 — the render cells, as a waiting item (S11; waits on #1599, and on #1601 for the Stats cells only)

Specified in § 9. The register entries are the rows the D0/D15 council ruling of 2026-11-03 specifies (three **no-session-window account** rows under *Decided to be nothing*), entered by this records change (#1656); #1594 builds the cells once #1599 lands, and the Stats cells once #1601 lands.

### B-12 — deployment and live verification (S12)

Amend `docs/runbooks/replacing-the-running-daemon.md`, never add a second runbook: under
*§ Prerequisites* add the **no-older-daemon rule** — from the store's first `null` line on either axis, no
pre-#1598 build may run as the daemon against that store, by any path (relaunching a kept bundle, the
#1613 hot-fix included; rebuilding the served bundle from a checkout that does not contain #1598;
`sessiometer service install` or `run` from an older build), checked before any rebuild with
`git merge-base --is-ancestor <#1598 squash commit> HEAD`; an older `sessiometer stats` or `reliability`
(a stray `target/` build, an older tap install) deletes nothing, and an older build's "malformed usage
rollup" message is expected after #1598 (the fence); the store files are never deleted or moved
(`REQ-DEP-C-001`; D6 as the council ruled it on 2026-11-02); under *§ Confirm the build you just deployed is the one serving*
add a **fleet recovery check**: three poll cycles of `status --json`, counting `weekly_pct` non-null
over `auth: "healthy"` rows (`REQ-DEP-Q-001`, goal 17/17 by the second cycle); a `jq` count over
`usage-samples.jsonl` keyed on `acct` and `ts` after the relaunch and reported as counts and roster ordinals, never as labels (`MET-T-002`); and the `next_swap`
reading (`MET-T-004`). The runbook's existing preflight for the Developer ID identity stands (PRD
A-5 is unverified here and stays the operator's).

**The split of B-12 by the maintainer's decision of 2026-11-01 (AD-23).** The runbook text (#1603) moves
from Must in tranche 1 to Should in tranche 2 and lands after the deploy. The live half (#1604) is tranche
1's deploy and its verification; it follows #1603's text where that has merged and the runbook's current
text otherwise. Tranche 0's deploy, the rebuild and restart of the local daemon with #1613, follows the
runbook's current text too. Tranche 2 deploys as #1608.

**What each deploy turns on** (`REQ-DEP-B-002`). With #1613 deployed first, weekly readings recover at
tranche 0, so the fleet recovery check measures a recovery there. Tranche 1's deploy newly turns on the
`"session":null` sample line, because sampling resumes for the weekly-only accounts, and
`session_pct: null` in place of the hot-fix's 0 (the documented transitional state, D10 / B-6t). Under
tranche 1 the `weekly_pct` count is therefore a no-regression check, and the new evidence is the `null`
sample lines and the `session_pct` cells. No step records an account label: every count is reported as a
total, and a failing row is named by its roster ordinal.

## 6. Runtime View

### 6a. Task flows (operator)

1. **Deploy and see the fleet recover** — first the tranche-0 hot-fix (rebuild and restart the local daemon with #1613 before it merges, by the runbook's current text; weekly readings recover), then tranche 1: merge → runbook: identity preflight → quit the app and `sessiometer daemon stop` → bundle
   rebuild (`release-macos.sh --sign-only`) → launch the rebuilt app → confirm the serving build →
   fleet recovery check (B-12; a no-regression check on `weekly_pct`, plus the `null` sample lines and `session_pct: null` cells that tranche 1 newly turns on). Error path: a healthy-credential account still `n/a` after three
   cycles → capture that account's quota fields (naming it by roster ordinal, never by identity) and compare against Oracle A's
   shape; a second shape is a new fixture, not a rollback (PRD § 4 decision gates).
2. **Read a weekly-only account's row** — `status` shows its weekly percentage and reset; the session cell shows `n/a` transitionally (B-6t; `0%` under tranche 0's hot-fix until tranche 1 deploys, § 1b), then the `—` gap sentinel D0 classified (B-11, #1594). `status --json` carries `absent_windows` after B-6. Discriminator during the transition: a failed poll nulls *both* windows, so a row reading `n/a | n/a | 90% | 5d` — an uncoloured Session meter beside a coloured Weekly meter — cannot occur today, and the weekly cell tells no-window from no-reading before B-6 lands. Its limit is the error path: a weekly-only active that goes blind (B-6t row 5) is indistinguishable from a failed-poll row on both surfaces until S6.
3. **Switch to a weekly-only account by hand** — `sessiometer use <label>` without `--force` returns the weekly verdict (B-3 row 10); no `UsageParse` abort. Error path: a weekly-exhausted account returns `WeeklyExhausted` instead of switching, exactly as a two-window account does, and `--force` overrides it as today (`REQ-ARM-B-009`).
4. **Read the record** — `stats` shows the account's weekly figures and `—` for its session figures (B-8); `stats --json` omits its `session` block under schema 2. Error paths: an older `stats` run against a store carrying `null` lines refuses the fenced rollup loudly, and an older `reliability` leaves the lines out of its report — neither deletes anything; only an older daemon's compaction erases, which the runbook's no-older-daemon rule and the rollup fence exist for (B-7, D6); and OAD-3's "follows", which would have kept a zero `session` block in `stats --json` for such an account until B-8's wire half landed, was rejected with D8 by council on 2026-11-03 (ride first).

### 6b. System interactions

```
poll ─► parse_usage ─► Ok(UsageReport{session:None, weekly:Some(0.90), …})     [D1 (a): neither → Err(UsageParse)]
     ─► classify_poll → Live ─► last_reading = Some(usage); last_good (active) = Some(LastGood{session:None,…})
     ─► append_sample_for_poll → {"ts":…,"session":null,"weekly":0.9,"weekly_resets_at":…}
     ─► tick: decide (weekly axis only) · velocity_swap (held) · note_exhausted_poll (weekly line) · blind gate (false)
     ─► snapshot: session_pct:null · weekly_pct:90 · [absent_windows:["session"] after B-6] · blind_active per B-6/B-6t
     ─► watch frame / status reply ─► CLI cells (B-6t → B-11) · Swift decode (B-6t: no edit; B-6: the key read, `UInt8?`) · render (B-11, as D0 classified)
```

Tranche 0's hot-fix differs from this flow on two lines (§ 1b): the report carries `session: 0.0` with the
private flag, and `append_sample_for_poll` writes nothing for it.

Flip sequences (`REQ-ARM-B-011`): window present at poll *n*, absent at *n*+1 — the high-water mark
is released by the vanished stamp, the velocity EMA fold skips, the anchor is refreshed to
`session: None` on the next successful active poll; absent at *n*, present at *n*+1 — the next `Some`
reading seeds a fresh mark and every session gate applies again. No state crosses the flip.

## 8. Interface Contracts

The API Design track was folded into Technical Architecture — a pipeline-decided fold, recorded in the
machine-local run log the § 14 provenance row names. The precedent is
`docs/design/daemon-diagnostic-integrity-solution-design.md`, which carried a wire version move
(`reliability`'s own `JSON_SCHEMA_VERSION`, 12 → 13, its D-4) inside a design with no separate API track;
it moved no `STATUS_SCHEMA_VERSION` minor and had no track structure. The contracts, in full:

| Wire | Change | Version move | Lockstep |
|---|---|---|---|
| `status` reply + the `watch` snapshot frame (the heartbeat frame carries no account rows and carries only the bumped minor — D11) | `absent_windows`, a per-account list of the quota windows the served reading does not carry (tokens `session`, `weekly`), omitted when empty; `blind_active.last_known_session_pct` and `recent_blind_preempt_swap.last_known_session_pct` optional, omitted when absent | `STATUS_SCHEMA_VERSION` minor → next free (queue rule) | five goldens regenerated + two new rows; 27 + 14 + 1 Swift fixture sites; Swift decode (`decodeIfPresent`); CLI consumers of the reshaped field |
| `status` reply, tranche 0 (hot-fix #1613, `1.14`) | `session_pct: 0` beside present weekly fields for a served weekly-only reading, with no session reset stamp; no sample line is written for it (§ 1b) | none | none — replaced by the transitional row below at tranche 1's deploy |
| `status` reply, transitional (`1.14`) | `session_pct: null` beside present weekly fields for a served weekly-only reading; no `blind_active` block for a weekly-only blind active; no `recent_blind_preempt_swap` notice for a `Retry-After` swap off a session-less anchor; doc comment amended | none | none — retired by the row above |
| `stats --json` | `accounts[*].session`, `band`, `cap_hits` and `time_at_cap_secs` omittable (AD-13); in the same bump #1601 also makes `accounts[*].weekly` omittable (the weekly symmetry) and `velocity.session_pct_per_min` nullable, so a weekly-only account's weekly rate and runway reach the wire — both widenings ratified by council on 2026-11-03 (AD-13), the weekly half conditional on D12 | `JSON_SCHEMA_VERSION` 1 → 2 | `wire-stats-basic.json` regenerated; the Swift `StatsWire` mirror optional for every omittable key; `statsBasic` fixture at schema 2 |
| sample line `usage-samples.jsonl` | `"session":null` / `"weekly":null` written; the key never omitted | none (versionless) | readers tolerant; a pre-#1598 build's `Sample` refuses `null`, and its compaction rewrites this file from what it decoded; the rollup fence (the rollup tiers row) stops that compaction before the rewrite; deploy order (B-12) |
| rollup tiers `usage-rollup.json` | per-axis `HourStat`/`DayStat` omitted when the bucket has no reading on that axis; every present `HourStat`/`DayStat` records `samples: Option<u32>`, the number of samples it covers (D13 as ruled) | store generation 1 → 2 under #1598's `ROLLUP_FORMAT`: `usage-rollup.json` is written as `{"format":2,"rollup":{…}}`; a file with no `format` key reads as the legacy generation 1; every pre-#1598 build refuses the envelope before it rewrites either store file | deploy order (B-12) |
| event log lines | per-window tokens omitted when the window is absent | none for `log` (`JSON_SCHEMA_VERSION` at `src/log.rs:167`); `reliability` (`JSON_SCHEMA_VERSION` at `src/reliability.rs:264`) moves 13 → 14 for the census's `n_without_session_window` | `reliability` parsers tolerate the absent token; the census keeps its exact partition |
| `log --json` | unchanged: records carry the line's own tokens (`LogWire` at `src/log.rs:865`), so an omitted token reshapes nothing | none (`JSON_SCHEMA_VERSION` at `src/log.rs:167` stays 2) | — |
| `reliability --json` | the blind-projection block gains `n_without_session_window` beside `n_malformed`, an always-present key, so the census keeps its exact #484 partition | `JSON_SCHEMA_VERSION` 13 → 14 (`src/reliability.rs:264`) | the three `build/fixtures/cli-renders/reliability-*.txt` goldens regenerated; `RELIABILITY_USAGE`'s `schema:` token; no Swift surface |
| `/api/oauth/usage` | unchanged in kind and rate; no second endpoint | — | the sole-egress oracle stays green |

## 9. UX Architecture

The UX/IA track ran in generative mode for the one surface the PRD touches — the no-session-window
account on the panel row and the CLI status cells — and returned with its
inventory, state coverage and options complete; D0 was settled by council on 2026-11-03 at option (i).

### 9a. Information architecture

| Surface | Element | Today | Under this design |
|---|---|---|---|
| Panel account row | the Session meter (`UsageMeter(label: "Session", …)` at `apps/menubar/Sources/StatusPanelRoster.swift:443`; `AccountRow` at `apps/menubar/Sources/HonestStateMachine.swift:361`) | every one of the mock's 98 rows carries one; `nil` percent renders uncoloured `n/a` (a failed poll) | renders the state D0 classified on 2026-11-03 (no track, the `—` gap sentinel in `.secondary`, an empty reset), built by #1594; until then the transitional `n/a` (B-6t) |
| Panel account row, blind held block | the `BlindMeter` held bar, band and percent text, which read `lastKnownSessionPct` as a value (`apps/menubar/Sources/StatusPanelRoster.swift:911-924`), and the row's spoken phrase "last-known session N percent" (`apps/menubar/Sources/StatusPanelFormat.swift:2197`) | every mock blind frame carries a percent; the row's blind treatment keys solely on `blindActive` (`apps/menubar/Sources/HonestStateMachine.swift:386`) | S6 makes the percent optional (B-6), so a weekly-only blind active is a panel state the mock never authored — the fifth RenderCell, classified with D0 (D15, settled by council on 2026-11-03: no held bar, no outline, `—` in its own percent cell); S6's panel render half is #1594's exactly as S8's is, its decode half (`UInt8?`) is not |
| CLI `status` table | SESSION% and RESET cells (`pct` at `src/cli.rs:4594-4598` on `main`; `reset_cell` at `src/cli.rs:4365-4368`) | `n/a` for a failed poll, pinned by the nine `status-*` render goldens under `build/fixtures/cli-renders/` that carry the token | `—` / `—` (`EXPIRY_GAP`), uncoloured — the token D0 named on 2026-11-03; `n/a` transitionally |
| Panel Stats tab | the per-account `StatCell(label: "Session m/pk", …)` (`apps/menubar/Sources/StatusPanelStats.swift:197`) and the row's spoken summary "Session mean N percent, peak N percent … cap hits" (`accessibilityLabel` at `apps/menubar/Sources/StatusPanelStats.swift:216-224`) | always a figure | D15 — settled with D0 by council on 2026-11-03: `—` in `.secondary` for `Session m/pk` and `Cap-hits`, no signal pill, the sparkline band empty, the summary speaking "No session window." |
| CLI `stats` table | the session columns, and the SIGNAL, velocity, runway and cap columns keyed on the session series (`signal_cell` at `src/stats.rs:2498-2504`; the `cap_hits` and `time_at_cap_secs` columns at `src/stats.rs:3509-3527`) | figures | the surface's own `—` UNKNOWN sentinel (B-8) — reuse, not invention; the table then does not distinguish "no window" from "never observed" (only the wire does; G-4 holds on the wire) |

The state matrices of PRD § 11 are the state coverage for these views. This design adds one state the PRD's matrices did not carry — the blind held block of a weekly-only blind active, forced on the panel by S6's reshape — and routed it to D0 under D15 rather than rendering it; D0 and D15 classified it by council on 2026-11-03, and #1594 builds it as classified. No new navigation, control or surface exists, and no task flow changes — the operator reads a cell that today misreports and tomorrow reports.

### 9b. The waiting item's specification (what S11 builds under D0, settled by council on 2026-11-03)

| D0 option | Panel Session meter | CLI SESSION% / RESET | Cost and gates |
|---|---|---|---|
| (i) **decided to be nothing** — chosen by council on 2026-11-03 — the shared `—` gap sentinel, which the mock already says "is deliberately NOT `n/a`" (`apps/menubar/design/menubar-preview.html:1121-1126`), and which the CLI `status` table already spends on the same meaning as `EXPIRY_GAP`, "DELIBERATELY not the `n/a`" of a failed poll (`src/cli.rs:4372-4385`) | `—` in the percent slot, no track (the bar cell reserved, empty), and the per-meter reset label (`UsageMeter` … `reset:` at `apps/menubar/Sources/StatusPanelRoster.swift:443-446`) empty rather than today's `n/a`; the blind held block draws no held bar and no dashed outline, with `—` in its own percent slot and the spoken phrase without a percent; register rows under *Decided to be nothing* naming this reasoning; row geometry unchanged | `—` / `—` — the same table's own `EXPIRY_GAP` precedent, reuse not invention | three rows in the register's *Decided to be nothing* table, plus one *Genuinely open* row; three new panel fixtures (the four-row comparator, a percent-less blind active under DEGRADED, and a Stats render with one weekly-only card) + a positional raster gate (the oracle party, never the executor, writes it, and it runs in the required `swift` job) + an **armed** `panel-goldens` run (`TEST_RUNNER_SESSIOMETER_PANEL_GOLDEN_GATE=1`) + `Panel-Goldens-Rebaselined:` trailer; a new CLI render fixture + `emit_cli_render_goldens_status` (`src/cli.rs:17138` on `main`) + `CLI-Goldens-Rebaselined:` trailer |
| (ii) **as authored** — a mock frame for a weekly-only row | whatever the frame authors (the meter removed, or a one-meter row), the held block included | the token the frame's register row names | a mock edit plus the same gates as (i); a one-meter row changes row height, which feeds the shipped height bound, `PanelRosterGeometryTests` (`apps/menubar/design/README.md` § Genuinely open) and the three-sub-line row grammar — a cost (i) does not carry |
| (iii) **genuinely open** | the transitional `n/a` stands; S6's panel render half stays parked with S11 while its decode half lands, and a decoded percent-less `blind_active` block renders as no block (today's two-meter `n/a` row) until D0 | `n/a` | nothing is built; S11 stays parked |

Whichever option, three spoken strings are part of the classification — what VoiceOver reads for an absent Session meter, for the held block without a percent, and for the Stats-tab row summary — a gap the quality review names (§ 11 Accessibility), handed to D0 rather than decided here. The design never picks the token.

## 11. Crosscutting Concepts

### Security

No new egress, credential read or scope; the sole-egress oracle stays green by requirement. The
captured fixture carries quota fields only (`REQ-FIX-B-001`'s BUT NOT). The store's existing
permission for operator-authored email labels is not widened. **Addressed.**

### Observability

A served reply ends the `usage_gap` and `blind_enter` streaks of the 15 accounts with no classifier
edit; events carry typed absence (B-3 row 15; under tranche 0's hot-fix the session tokens read 0, § 1b). The `usage_gap` event's missing cause (C2, tracked as #1609) is a
deferral the PRD owns, with its reopen trigger, not a gap of this design. **Addressed.**

### Error handling

D1 (a) keeps a windowless 200 reply on the error path the loop already handles (`Transient`, skip).
The store write stays fail-open (the poll loop never breaks on a sampling failure). The rollup reader
fails loudly on a shape it cannot read (`UsageRollupMalformed`), which is the right failure for
lifetime state. **Addressed.**

### Reliability

Compaction retains `null` lines; the tier fold never reaches `-∞`; no pre-#1598 daemon runs against the
store after its first `null` line, and the rollup fence stalls one that does; the transitional state is
documented and dated. **Addressed**, with
the version-skew asymmetry stated in B-7.

### Performance and cost

One `serde_json::Value` walk per poll, unchanged; no I/O, request or hot loop added; rate neutrality
is structural (`REQ-ARM-Q-001`, the existing stagger locks run unmodified). Nothing is provisioned or
bought. **Addressed.**

### Accessibility

No new control. Three spoken strings change meaning and are **a Gap handed to D0** (§ 9b; settled by council, 2026-11-03; the record is a comment on #1594), recorded here so the register entry carries them: the label of an absent Session meter; the blind held block's "last-known session N percent" (`lastKnownSessionPct` at `apps/menubar/Sources/StatusPanelFormat.swift:2197`) when there is no percent; and the Stats-tab row summary's "Session mean N percent, peak N percent" (`accessibilityLabel` at `apps/menubar/Sources/StatusPanelStats.swift:216-224`).

### Testing strategy

Everything surfacing in the other tracks is in the master test plan below. **Addressed.**

### Master Test Plan

**1. Goals and risk surface.** Verify that (a) the parser serves every window a reply carries and
never fabricates one; (b) no consumer defaults an absent window to a number; (c) every surface
distinguishes "no window" from "no reading"; (d) the store and the offline verbs carry absence
honestly; (e) the deployment is seen to recover the fleet. Out of scope: the HTTP taxonomy, the
credential path, keep-warm, cadence. Risk surface (ACC, Attributes × Components → Capabilities):

| Cap | Capability | Components | Requirements bound |
|---|---|---|---|
| Cap-1 | parse a reply with any subset of windows, never fabricating | B-1, B-4 | `REQ-RPL-B-001`…`B-004`, `REQ-QW-B-001`, `REQ-RPL-C-001`, `REQ-FIX-B-001` |
| Cap-2 | carry per-window absence to every consumer with one of four moves and no numeric default | B-2, B-3 | `REQ-RDG-B-001`, `REQ-RDG-C-001`, `REQ-ARM-B-001`…`B-012`, `REQ-ARM-C-001` |
| Cap-3 | the status wire distinguishes no-window from no-reading, and its mirrors decode it | B-6, B-6t | `REQ-STL-I-001`, `I-002`, `C-001`, `B-001`, `Q-001` |
| Cap-4 | the store writes, reads, retains and folds absence without loss or `-∞` | B-7 | `REQ-SMP-B-001`, `B-002`, `C-001`, `Q-001` |
| Cap-5 | `stats` excludes an absent window from its figures and its wire | B-8 | `REQ-STS-B-001`, `REQ-STS-C-001` |
| Cap-6 | the records move before the pinned test does | B-9, B-10 | `REQ-REC-B-001`…`B-004`, `REQ-REC-C-001` — **non-testable by code**: documentation, graded by the doc gates and review |
| Cap-7 | the live fleet recovers on the deployed bundle — weekly readings at tranche 0's deploy; tranche 1's deploy keeps them (a no-regression check) and newly turns on the `"session":null` sample line and `session_pct: null` | B-12 | `REQ-DEP-B-001`, `C-001`, `B-002`, `Q-001` — **manual, operator-run**, by the runbook's check |
| Cap-8 | the oracles are RED before the fix and green after, with no mutant passing | B-5 | `REQ-ORC-B-001`; `MET-A-001` |
| Cap-9 | the render cells show the classified state | B-11 | `REQ-CEL-C-001`, `REQ-CEL-B-001` — the register classifies the state (D0, council 2026-11-03); built by #1594 after #1599 and, for the Stats cells only, after #1601 |
| Cap-10 | rate neutrality | B-3 | `REQ-ARM-Q-001` — the #80/#366 stagger locks, unmodified |

**2. Pyramid.** Unit-heavy: the parser, every decision arm, the store fold and the stats
aggregation are pure functions with hermetic tests in `mod tests`; integration at the snapshot/wire
boundary (the goldens, the Swift decoder bundle); one manual end-to-end (Cap-7). No UI automation
beyond the Swift test bundle and the armed golden comparison. This is the composition the repo's
existing design docs use and the one its CI runs.

**3. gTAA layers.** Generation: Stage 3's `specification-formulation` writes Gherkin stubs per item
under `docs/specs/` with `# Binds:` lines, and the pre-authored oracles are executor-uneditable.
Definition: Rust `#[test]` / `#[tokio::test]` in `mod tests`; XCTest in `MenubarTests`. Execution:
local five-step gate, then CI (`test`, `msrv`, `deny`, `swift`, soft `panel-goldens`). Adaptation:
none — no page objects; the goldens are the adaptation layer for the wires.

**4. Environment topology.** One environment class: the developer's machine and CI, both hermetic
(temp dirs, injected `now`). No staging; the "canary" is the operator's own fleet after deploy
(Cap-7).

**5. Test data.** Fixture bodies inline in `src/usage.rs` (quota fields only — the one privacy rule:
no identity field, ever); store samples synthesised per test; wire goldens under `build/fixtures/`
regenerated only by their emitters.

**6. Traceability.** § 16 binds every requirement to a capability; Stage 3's stubs carry
`# Binds:` to test names; the doc gates (`check-doc-citations.sh`, `check-doc-citation-rot.sh`)
keep the citations honest.

**7. Quality gates.** Per code item: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items`, `cargo build --verbose`,
`cargo test --verbose`; `msrv` (`cargo +<RUST_MSRV> build && cargo +<RUST_MSRV> test`); both `deny`
checks (`./scripts/check-no-security-framework.sh`, `cargo deny check advisories sources licenses`);
`swift` for anything under `apps/menubar/**` or `build/fixtures/**` — B-6, B-8, B-11, and the compile unit,
whose `reliability` bump regenerates three `build/fixtures/cli-renders/reliability-*.txt` goldens (§ 8) —
read off the test cardinality, never off `TEST SUCCEEDED` alone; an **armed** `panel-goldens` run for every
item in that same filter (`CLAUDE.md` § Before you push: the paths that trigger `swift` trigger
`panel-goldens`, and only an armed run shows that no panel golden drifted), read off the
`[panel-goldens] max drift` line, B-11 being the one item expected to move a golden; trailers:
`Panel-Goldens-Rebaselined:` (B-11), `CLI-Goldens-Rebaselined:` (the compile unit's three `reliability-*`
goldens, B-8's two new `stats-*` goldens, B-11's CLI fixture), none of `Gate-Change-Acknowledged:` (no
gate path moves). Never `cargo test -- --include-ignored`.

**8. Frameworks.** Rust's built-in test harness with `tokio::test` where the daemon is driven;
XCTest; no mutation-testing tool — mutation evidence is by hand (the fabricating mutant, Cap-8);
no property-based tool. Where a test cannot compile against the tree before its change, or that tree already passes it, its hand-applied mutant record takes the form #1590 § Binding on every child sets.

**9. AI-augmented testing.** N/A — no AI/LLM in system.

**10. Risks.** See § 14 risk register rows R-D4 (`Some(0.0)` wrappers), R-D6 (goldens regenerated
with `--include-ignored`), R-D2 (a 1.15 collision), each with an owner.

**Existing oracles the change must keep green or move** (named; run, never re-authored): every
`usage::tests` parse test except `a_missing_dimension_is_a_parse_error_not_a_fabricated_zero` (`src/usage.rs:959`), which **moves** to assert Oracle D's reading after B-10 lands;
`a_200_with_an_unparseable_body_is_a_parse_error` (`src/usage.rs:1472`) stands under D1 (a);
`the_committed_wire_goldens_still_match_the_daemon_encoders` (`src/daemon/snapshot_build.rs:3929`) —
RED on the minor bump until the emitter regenerates; the Swift `WireDecoderTests` and `Fixtures`
sites; `PanelGoldenParityTests` armed (B-11 only); the CLI render goldens (unchanged until a cell
renders other than `n/a`); `sample_round_trips_with_and_without_optionals` (`src/usage_store.rs:870`)
extended; the reliability canary `the_sample_store_cannot_carry_a_non_finite_session_reading` (`src/reliability.rs:7386`) with its `null` arm moved;
`session_high_water_needs_a_window_stamp` (`src/swap.rs:1603`) green;
`the_sole_egress_endpoint_is_the_documented_usage_url` (`src/usage.rs:1593`) green.

## 12. Architecture Decisions

D0 to D15, AD-4b and AD-13 are settled by council at the maintainer's delegation, D6 and D13 on 2026-11-02 and the rest on 2026-11-03. Each carries its ruling where the design records it — its § 12 row's Flag cell, an OAD stub below (OAD-1 for D0 and D15), or, for D4, which has no § 12 row, § 16 and § 14 — and the rest stay **ratification-pending** (the design is provisional). No row remains **author-chosen**: each row that was (AD-4b, AD-6, AD-7, AD-10, AD-13, AD-14, AD-22) had no pipeline recommendation in the PRD, is settled by council, and its Flag cell says so.

| ID | Decision | Rationale | Alternatives considered | Flag |
|---|---|---|---|---|
| AD-1 | Per-window `Option<f64>` on `UsageReport`, `Usage`, `LastGood`, `BlindAnchor`; `Copy` kept on the three that carry it (`UsageReport` is `Clone`) | the compiler enumerates every consumer; no flag can be forgotten; no sentinel can be mistaken for an idle window (§ 4.1) | a flag beside `f64` (rejected: 93 reads to audit); `Option<Window{fraction, resets_at, severity}>` per window (cleaner, but reshapes every `Usage` literal and the `Copy` decision type on the urgent path — deferred as a later refactor, § 12b, tracked as #1605) | pipeline-recommended |
| AD-2 | `dimension` unchanged; `parse_usage` composes per-window Options; D1 (a) error only when neither window; D2 identical | the smallest parser change that is also the model change; D1 (a) keeps a 200-with-nothing on the skip path, the only path a 200 body the parser does not recognise can take (a non-object JSON document, an error envelope, both window keys renamed, both windows unreadable, only unlisted kinds) | D1 (b) `Ok` with both `None` (in code one guard, Oracle C's (b) form, N4 dropped, three moved tests and two doc comments; in substance it opens the `Live` path to every unrecognised 200 body, so it waits on an observed windowless account and on a reader that tells such an account from such a body); D2 different (a second fixture — landed by #1592 — and a documented reason, for a difference no code reads) | **settled by council (D1, D2)**, 2026-11-03 — D1 (a) ratified, its alternative's cost amended; D2 identical ratified |
| AD-3 | Invariants I-1 (stamp ⇒ fraction) and I-2 (a served reading has ≥ 1 window under D1 (a)), each pinned by a test | consumers may read a stamp without re-checking the fraction; the high-water fold's second `?` is unreachable alone | state I-1 in a doc comment only (rejected: a later `dimension` edit could break the tuple with nothing red); drop I-2 (rejected: D1 (a) is what makes it hold, and the test is what would catch D1 (b) landing unnoticed) | design-derived |
| AD-4 | Four named moves per consumer; numeric default forbidden at every site that meets the absent state, including tests (B-1 … B-12; tranche 0 is the one bounded exception, § 1b) | P-1 and P-11 are the failure modes a grep cannot catch; a per-arm `None` test is the instrument | a source-lint test over `unwrap_or(0.0)` (rejected: false positives on legitimate non-window floats) | design-derived |
| AD-4b | The weekly-less arms, by symmetry: a weekly-less account is never weekly-blocked (B-3 row 1), `use` returns `Viable` (row 10), it is never `weekly_exhausted` (row 13) — on the premise that a weekly `None` is a window the account does not have, never a window the reply carries in a shape the parser cannot read (#1614, Answer 2) | compiler-forced at the three arms once `Usage.weekly` is `Option` (S2, message 2's future case), and bounded by D1 (a): a served weekly-less reading always carries a session window, so selection still gates it on the session axis and ranks it last (a `None` `weekly_resets_at` sorts after every known reset); extends `REQ-ARM-B-012`'s symmetry to an account class with no observable instance today. The weekly line is the harder limit (a wall holds until `weekly_resets_at`, days, not hours), so the pass move is safe only while the premise holds — under #1614's Answer 1 an unreadable weekly window would make a two-window account read weekly-less and pass the weekly line silently, where the tree before this scope failed closed. One site the compiler cannot reach: `use`'s cached path (`cached_viability_of`, `src/use_account.rs`) reads `weekly_pct.is_some()` as its liveness discriminator, and under `1.14` a weekly-less served reading and a failed poll both serve `weekly_pct: null`, so a weekly-less `use` with a daemon up misses the cache and spends one live poll — a `429` there aborts as `UseViabilityUnverifiable` — until B-6's `absent_windows` lets the cached path read the absence; that same `null` is the weekly half of B-6t's transitional cost, which no record names | refuse a weekly-less reading at the parser (rejected: contradicts message 2 and `REQ-RPL-B-002`); a new `Unknown` verdict for `use` (rejected by council: a `Viability` variant `GateOutcome` must still map to refuse or permit, that the cached path cannot produce and `weekly_exhausted: bool` cannot carry, on an account class the operator expects to be usable, with `use --force` already there) | **settled by council, 2026-11-03** — ratified on the stated premise, the rationale amended with the cached-path bound; reopened by a #1614 ruling for Answer 1, or by a captured weekly-less reply from an account the provider still weekly-limits |
| AD-5 | `blind_gate_armed` and `plausible_anchor_session` take `Option<f64>`; false / `None` on an absent anchor; the single-source rule survives | the predicate's two callers must not drift (the gate's own comment), and `blind_swap`'s own comparison against the config thresholds reads false on `None` with them; D3's surfacing half | caller-side skips at each site (rejected: the drift the gate was built to prevent) | **settled by council (D3)**, 2026-11-03 — default ratified; the PRD row carries D3's cost and its reopen evidence |
| AD-6 | Transitional `1.14` state (D10): `session_pct: null` beside present weekly fields; doc comment amended; **no `blind_active` block for a weekly-only blind active**; a dated signpost — S6 (#1599) merges and #1608 deploys it within 14 calendar days of #1604's deploy record | tranche 1 deploys days sooner (the fleet's weekly readings already recover at tranche 0); a `0` in `last_known_session_pct` would be the fabrication; the cost is stated in full in B-6t row 5 (indistinguishable from a failed-poll row on both surfaces, duration and percent lost, report arms silent) and corrected on both of the bounds it once claimed: the comparison that holds is against the tranche-0 bundle of #1613, whose block for such an active carries a fabricated `0 %` and an `auto-protection OK` read off a `0.0` anchor (measured against the tranche-0 bundle that serves today, which emits the block for such an anchor with `last_known_session_pct: 0` and, blind past `BLIND_GATE_SECS` with no `Retry-After` hold, `auto_protection_degraded: false` (the anchor arm reads `0.0 >= 0.60` as false)) — the omission trades that misreport for no block, so the blind duration and the CORNERED alarm (`cornered_state` composes `blind_active` with `no_viable_target`) are what the interim gives up; the bundle before tranche 0 could not read such an account, but an account active across the lift kept its last pre-lift two-window anchor (`last_good` is set only by a successful active poll and dropped only by a swap or an active loss; a parse failure leaves it), so "no block was ever emitted for it" is not established; and only the active carries `blind_active`, so "the cost lands on the active account" is the whole exposure, not a bound on it | S6 in tranche 1 (no misreport; the 1.15 queue and 27 + 14 + 1 fixture edits on the urgent path) | **settled by council (D10), 2026-11-03** — the transitional state, the omitted block and the notice, and the signpost ratified together; amended: the doc sentence names the observable and the retiring item (B-6t row 2), the four sibling hot-fix sentences are swept in the compile unit's pull request, the no-regression bound is measured against the tranche-0 bundle that serves (B-6t row 5), and the signpost is dated from #1604's deploy record (14 calendar days; S6 exempt from ship-what-is-done; recorded on #1590) |
| AD-7 | `absent_windows`: a per-account list of the quota windows the account's served reading does not carry, wire tokens `session` and `weekly` (lowercase, `session` before `weekly`, so the bytes are deterministic), on the `status` reply and the `watch` snapshot frame — the heartbeat frame has no account rows and carries only the bumped minor. Emitted only from a served reading whose window is `None`, never derived from the retained anchor (a failed-poll row and a blind row carry no key; a blind row's no-session-window fact rides the percent-less `blind_active` block, AD-8), and omitted whenever the list is empty, so every two-window row's bytes are unchanged. Rust: `#[serde(default, skip_serializing_if = "Vec::is_empty")]` over a two-variant enum serialised lowercase, whose identifier is the executor's and not `WindowKind`, which already names a private enum in `src/stats.rs`. Swift: `let absentWindows: [String]` read with `decodeIfPresent … ?? []`, so an unknown future token never costs the frame. | one key for both windows, and for any later window token without a new key; the omit-when-absent precedents (`blind_active` 1.4, `canonical_scrub` 1.5, `expiry` 1.12, each behind `skip_serializing_if`); an explicit discriminator the contract names — under D1 (a) `weekly_pct` already separates a served weekly-only line from a failed poll by bytes (R-D5), so the key's value is that no client infers "no window" from a conjunction and that `null`'s documented meaning stands | boolean `no_session_window` (a one-element list in disguise; the weekly-less case needs a second key); per-window enum `session_window: "absent"` (one fact split across two keys; a second minor only if the weekly key is deferred); a token in `session_pct` (a 1.14 app throws `typeMismatch` on every frame — a MAJOR by the contract's own rule); a per-window object replacing the flat keys (a 1.14 app reads every row as a failed poll — a MAJOR) | **Settled by council, 2026-11-03** (three panels at the maintainer's delegation of 2026-11-02; the record is a comment on #1599): the list key is ratified as the wire shape and amended in text — the tokens `session` and `weekly` in that fixed order, the Rust identifier the executor's and not `WindowKind`, the Swift mirror a `[String]`, the key on the `status` reply and the `watch` snapshot frame only, never derived from the retained anchor; the rationale corrected (the key is the named discriminator; `weekly_pct` already separates the bytes under D1 (a)). |
| AD-8 | `BlindActive.last_known_session_pct` **and** `BlindPreemptSwap.last_known_session_pct` — with `BlindPreemptSwapRecord`'s field behind the second and the `recent_blind_preempt_swap` projection — become `Option<u8>`, `#[serde(default, skip_serializing_if = "Option::is_none")]`, built as `anchor.session.map(to_pct)` and omitted when the anchor carried no session window, never `Some(0)`: the `rotated` reshape of 1.14 applied again, both fields in the one minor this item takes. Forward-compat is asymmetric exactly as at 1.14 — a pre-S6 app types the block's percent as required and drops the whole frame while the active is blind and weekly-only (measured on `b9df076`: `keyNotFound`) — so the Swift `lastKnownSessionPct` becomes `UInt8?` in the same app release; the swap notice has no Swift reader. | the `rotated` reshape precedent; a weekly-only blind active keeps `blind_secs` and `auto_protection_degraded` | drop the block, and the swap notice, for such an account permanently — rejected: it loses the blind duration, the `Retry-After` report arm and the swap's `use <from>` hint for good; the velocity arm cannot fire on a session-less anchor either way; B-6t accepts the loss only transitionally | **Settled by council, 2026-11-03** (same delegation and record): ratified as amended — the reshape covers both `last_known_session_pct` fields and `BlindPreemptSwapRecord`'s, in one minor; the Swift half ships in the same app release because a 1.14 app throws `keyNotFound` on the percent-less block (measured); the rejected alternative's cost corrected (the velocity arm cannot fire on a session-less anchor). The verdict a restored block carries past the gate is D3's surfacing half (#1599 § 2a), not decided here. |
| AD-9 | `STATUS_SCHEMA_VERSION` takes the next free minor at landing under the queue rule (#1457, #1586, #1587 and PR #1588 claim 15), read from `main` and the open claimants at merge time and written last, in the same commit as the goldens and the Swift fixtures — never a number another change already landed. One run of `cargo test -- --ignored emit_wire_golden_fixtures` at the landed minor (re-run at the new minor if a rebase moves it; never `--include-ignored`), over the emitter extended with a sixth golden: the five committed goldens change only in `schema_version.minor`, and one new golden file in the `wire-snapshot-<state>.json` pattern carries two account rows — a served weekly-only row with `absent_windows`, and a weekly-only blind active row with the percent-less `blind_active` block — never folded into the five. The Swift fixtures in lockstep: every current-minor literal in `Fixtures.swift` and both spellings in `WireDecoderTests.swift` (27 + 14 sites at `b9df076`, re-derived at landing) move to the landed minor, the `"minor":15` future frame moves to the landed minor plus one, one new fixture byte-identical to the new golden is added and byte-pinned, and one fixture frozen at the outgoing minor keeps the three-field `blind_active` block under test; version-pinned fixtures at other minors do not move. The constant's doc gains one paragraph naming the key, both reshaped fields and the asymmetric forward-compat, in the argument the `rotated` paragraph makes. | `REQ-STL-C-001`; #1588 and three issues claim 15 | pin 1.15 now (rejected: a second `minor: 15` on `main` is premortem P-3) | **Settled by council, 2026-11-03** (same delegation and record): ratified as amended — the minor is read at merge time and written last; "regenerated once" means one emitter run at the landed minor, re-run on a rebase that finds the minor taken; "two new golden rows" means one new golden file carrying two account rows, never a re-baseline of the five; the lockstep includes a fixture frozen at the outgoing minor and the future frame at the landed minor plus one. |
| AD-10 | `Sample.session` **and** `Sample.weekly` → `Option<f64>`, `#[serde(default)]`, key always written as `null` | gap honesty reserves "no line" for a failed poll; #1580's key-set oracle; the weekly axis is compiler-forced by S2 | omit the key (RED on #1580); a versioned sample line (rejected by the D6/D13 council of 2026-11-02 as well: no shipped build checks a version, so an older daemon skips and erases a versioned line exactly as a `null` one); a number plus an "absent" marker (an older daemon's rewrite strips the marker and leaves a fabricated reading); a new sample-file name (splits the store's history) | **settled by council (D6 and D12)**: the session half ratified on 2026-11-02, amended with the rollup fence (B-7); the weekly half (D12) ratified on 2026-11-03 as the session half's shape on the second axis — the same `Option<f64>`, `#[serde(default)]`, no `skip_serializing_if`, `null` always written — fenced by the same rollup generation and graded by the same key-set and round-trip pins, with the non-finite refusal (AD-12) graded on the weekly axis too |
| AD-11 | Rollup tiers: per-axis `HourStat`/`DayStat` → `Option`, omitted when no reading on that axis; counts stay counts; version skew under the fence: on a store a #1598 build has compacted, an older `stats` fails loudly on every fenced rollup — the format-2 envelope, whatever its buckets hold — and an older daemon fails to read that envelope, discards the compaction error and stops compacting in silence before it rewrites either store file (the ruling's C7 adds an `eprintln!` so that a stalled #1598+ daemon says so). **Amended by the D6/D13 council of 2026-11-02**: every present `HourStat`/`DayStat` also records `samples: Option<u32>`, the number of samples it covers, set at fold time; the same-key merge weights each axis by it when both operands carry one; #1610 is absorbed into #1598 | `max_of` over an empty slice is `-∞`, which `serde_json` writes as `null` and the next `read_rollup` then refuses with `UsageRollupMalformed` (P-12); no production reader of the per-axis stats exists to move; the per-axis subset's size exists only at fold time, because a folded day's raw samples are deleted | `HourStat` with `Option` fields (same skew, uglier; `cap_hits` ambiguous between 0 and unknown); an explicit `null` in buckets (two spellings of "no reading" in the lifetime tier); a count without the per-axis `Option` (a statistic over zero samples must invent a `max` and a `mean`) | **settled by council (D13)**, default ratified and amended |
| AD-12 | The canary's `null` decode arm moves to `Ok(None)`; its non-finite decode arms stay refused; its encode arm moves to assert that a non-finite `Some` is refused at serialisation by a field serializer, because `serde_json` writes a non-finite `f64` as `null`, which would read back as "no window"; the round-trip test gains the `null` case for both axes | `REQ-SMP-C-001`; `null` on disk keeps one meaning | delete the canary (forbidden by the requirement's BUT NOT); accept `null` for a non-finite `Some` and assert `None` on the read side (rejected: two meanings for `null` on disk; #1598 states it as the alternative) | pipeline-recommended |
| AD-13 | `stats`: session figures over present samples; census excludes (D5); wire `session`, `band`, `cap_hits` and `time_at_cap_secs` omittable under schema 2; Swift optional; CLI table reuses `—` for the session, signal, velocity, runway and cap columns; panel Stats tab (cell and spoken summary) renders what D15/D0 classified (council, 2026-11-03), #1601 rendering a transitional `n/a` there until #1594 builds it. Amended 2026-11-01: #1601 widens the field set in the same bump — `weekly` omittable (symmetry with `Sample.weekly`, D12) and `velocity.session_pct_per_min` nullable (the velocity object is emitted when either axis has a rate) — ratified by council on 2026-11-03 | a zero series rendered as a reading is the fabrication on a third surface, and a `0` cap count reads as "never hit its session cap" for an account with no cap (`MET-T-003`); the reshape is a bump by the repo's own rule | a sibling marker beside always-present zeros (rejected: still renders zeros); exclusion with no wire change (impossible: the block must hold something); counts kept at `0` (rejected after the product lens: a fabricated value on a surface) | **settled by council, 2026-11-03** — the field set ratified as amended 2026-11-01 (`session`, `band`, `cap_hits`, `time_at_cap_secs` and `weekly` omittable, `velocity.session_pct_per_min` nullable, one bump 1 → 2, Swift in lockstep, the table's `—`); the weekly half conditional on D12; timing ride first (D8, OAD-3) |
| AD-14 | Event fields typed absence, omitted token on the line; `reliability` parsers tolerant; no `log` version move; `reliability` moves 13 → 14 for the census's no-session-window bucket (the #484 partition kept; no-bucket arm rejected). The bump adds an always-present key **and** narrows `n_malformed`: a `blind_window` line lacking a session token, `malformed` under schema 13, is `without_session_window` under 14 — a value-domain correction the constant's doc comment records beside the key, as its `8 → 9` and `10 → 11` entries do. It moves no figure on a log written before #1602 (every shipped emitter wrote `session_pct=` unconditionally); an older `reliability` over a newer log counts such lines `malformed`, disclosed, and erases nothing. A line enters the bucket only when its other always-present tokens (`duration_secs`, `near_limit`) parse, so a torn append stays `malformed`. | `REQ-RDG-C-001`; the `session_high_water_pct` absent-token precedent | emit `session_pct=0` (the fabrication); skip the event (loses the weekly half) | **settled by council (D14)**, default ratified on 2026-11-03 and amended: the Decision cell carries the value-domain half of the 13 → 14 entry and the bucket's admission rule; B-3 row 15's `BlindGateEligible` clause carries its D3 condition, both predicates, the narrowing form and the lift rule |
| AD-15 | `use` serves the weekly verdict; a weekly-less reading is `Viable` | `REQ-ARM-B-009`; no line to exhaust | abort as today (the symptom); a new `Unknown` verdict for a weekly-less reading (AD-4b's alternative, rejected by the D3/D4 council on 2026-11-03: a `Viability` variant `GateOutcome` must still map to refuse or permit, that the cached path cannot produce and `weekly_exhausted: bool` cannot carry, on an account class the operator expects to be usable, with `use --force` already there — see AD-4b) | pipeline-recommended; the weekly-less `Viable` arm is AD-4b's row 10, settled by council on 2026-11-03 |
| AD-16 | Velocity: per-axis `Option<i16>` deltas; the EMA fold skips an absent axis and resets the slot on a served reading with no session window; on a reset tie a spare with no session window passes the velocity axis and ranks in the no-observed-climb class by a typed key read from the reading, never as `0.0` (D7) | the ranking `velocity_rate`'s `0.0` already gave such a spare, which tranche 0 serves today, kept without a number standing for the absent window; the axis guards against a session re-trip, and an account with no session window has none to re-trip (`REQ-ARM-B-003`); keying on the reading stops a pre-vanish EMA ranking the spare (`REQ-ARM-B-011`) | `0.0` with the rule narrowed to window fractions (rejected: narrows a binding rule at the first arm that met it, and the tie still reads a retained EMA); a spare with no EMA ranked after every spare with one (rejected: prefers the steep climber the axis exists to avoid); ranked strictly before (rejected: a preference the axis's signal does not carry, which also herds every daemon onto that spare where the jitter would disperse them); no preference at the tie (rejected: a pairwise skip is not transitive, so `min_by`'s winner depends on roster order) | **settled by council (D7)**, default's ranking ratified, encoding amended |
| AD-17 | ADR-0033 census excludes a structurally absent window (D5): the set is the roster minus every account none of whose in-period readings carries a session reading; a `None` reading anchors nothing and covers nothing; an unsampled rostered account stays in the set and empties the intersection | an absent window is not an unobserved reading; sound while a stored `None` means absent, not unreadable (#1614) | hold the census UNKNOWN whenever a weekly-only account is present (blunter; hides the roster's real session census) | **settled by council (D5)**, 2026-11-03 — exclusion ratified, the recorded sentence amended to the code's subtractive form |
| AD-18 | Oracles A–D as named in B-5, `#[ignore]`d, RED-proven by a filtered `--ignored` run | the repo's RED-oracle rule; `--include-ignored` rewrites a frozen fixture | land un-ignored (cannot merge); prove RED by a recorded run with no committed test (rejected: nothing executable then grades the fix); land the oracles inside the fix pull request (rejected: the executor could edit them) | pipeline-recommended |
| AD-19 | In-place amendments (B-10), README rows updated, HQ amendment recorded as owed — the ADR amendments before the pinned test moves (#1593, its own pull request); the parser doc rewritten in the commit that moves the test, which pins that very contract (B-10's ordering rule: with or before) | the repo's ADR practice; `REQ-REC-C-001` | a new ADR superseding four (rejected: the outcomes do not change, only their premise's wording — amend in place) | pipeline-recommended |
| AD-20 | Warm-up design C-1 reads two states from the fraction and the stamp (B-9) | contradiction 3 of the investigation; P-4 | leave #1232's design and fix at build time (rejected: the design is where its predicate lives; the feature is unbuilt) | design-derived form |
| AD-21 | Runbook amended with the no-older-daemon rule and the fleet recovery check (B-12); the text is #1603, tranche 2, after the deploy (AD-23) | `REQ-DEP-B-001`, `C-001`, `B-002` | a second runbook (rejected: the seed's step already exists as this runbook) | design-derived commands |
| AD-22 | PR composition per § 4.6: this design first and #1537 rebases (D9 stays its own); S6 takes the next free minor relative to #1588; #1538 composes | read live 2026-11-01; PRD R1 says coordinate, never re-scope | absorb #1537 (R1 grows; D9 is not this design's to decide) | **settled by council (D9), 2026-11-03** — the landing order and the non-absorption ratified: this design lands first with `dimension` unchanged and #1537 rebases onto it, recommended to its author and to the maintainer, who holds `maintainer_can_modify` on the branch, never ordered; the `None`-weekly fill and the `weekly_scoped` policy stay the maintainer's, put to them by #1537's review |
| AD-23 | **Tranche 0 first, outside the twelve blocks: the hot-fix #1613** (§ 1b, § 4.4). Then the tranche order is the cut order reversed: B-5 → B-4 (which reads B-5's bodies by name) ‖ B-10 records (#1593) → B-1+B-2+B-3 (the compile unit, carrying B-10's parser doc) → B-7 with B-8's reader half (#1598 with #1600) → B-8's wire half (#1601, its own pull request; D8, settled ride first by council on 2026-11-03) → B-6t → B-12 live half (#1604) ‖ B-6 → B-9 ‖ B-11, with B-12's runbook text (#1603) in tranche 2, after the deploy. Tranche 1's budget is the human effort-day estimate the coverage gate sized (PRD § 3.1), no longer one calendar week. #1603 moves from Must (tranche 1) to Should (tranche 2); #1604 follows #1603's text where it has merged and the runbook's current text otherwise | The maintainer's decisions of 2026-11-01. Asked whether tranche 1 can stay a small batch inside one calendar week when the coverage gate sized it at a much larger human effort-day estimate, the maintainer answered: "Bigger budget, but we need a known path to have a hot-fix for the issue as rugiht now sessiometer is unsuable". Then: "The hotfix must happen ASAP and local daemon rebuilt-restarted so that you can continue working". Asked whether the runbook text #1603 must gate the tranche-1 deploy #1604: "Yes, after deploy (Recommended)". The compile unit cannot be split; the store (B-7, with B-8's reader half) lands in the next pull request by the pipeline's choice, because the compile unit keeps #1613's sample skip re-keyed on a `None` fraction and so compiles alone, and a daemon built from the compile unit before #1598 stores nothing for such a reading, exactly as the tranche-0 bundle does, and writes no `null` line, so the D6 deployment rule is not yet engaged; nothing enforces that no such daemon runs, and the compile unit's pull request body says "not for deploy before #1598"; B-10's records half lands first so the record precedes the moved test; with the hot-fix deployed, weekly readings recover at tranche 0 rather than at tranche 1's deploy | ship the fabricating shortcut first (never, § 1b) — except tranche 0's narrowed, bounded form, which the maintainer chose; keep tranche 1 inside one calendar week (set aside by the maintainer's answer); #1603 as a Must in tranche 1, landing before the deploy (the earlier placement) | **maintainer-decided** for tranche 0, the budget and #1603's placement; the cut order itself **settled by council, 2026-11-03** — ratified as it stands for what remains after `363698d` (B-10's records half, B-5 and B-4 merged in the designed order); its "no running daemon carries that interim" amended to the bounded statement, and "→ B-6t →" read as "(B-6t, turned on at B-12)" |
| AD-24 | S11 specified as a waiting item with three options and its gates (§ 9b); D15 extends D0 to the Stats-tab cell, its spoken summary and the blind held block | the register rule | pick the `—` token now (forbidden: inventing against the register); build S11 against option (i) and re-baseline if D0 picks (ii) (rejected: a committed golden consumed by the parity gate turns OAD-1's two-way door one-way) | design-derived |
| AD-25 | Platform-neutral; no `cfg`; Linux un-gated and said so | § 4.7 | a `cfg(target_os)` guard on the parse path (rejected: nothing target-specific exists to guard, and the guard would be the first); a Linux CI job inside this scope (rejected: #964 owns it) | design-derived |

**Reversibility of the decisions above.** Every AD row is a two-way door until the first deploy that writes or serves its shape, and most stay two-way after it. A code-only decision (AD-1 … AD-5, AD-4b, AD-15 … AD-20, AD-24, AD-25) reverses by a code change with no data to migrate. A wire decision (AD-7, AD-8, AD-9, AD-13, AD-14) reverses by a further bump of its own wire with the same lockstep, so its cost is one more minor or schema and one more fixture sweep. A store decision (AD-10, AD-11, AD-12) reverses for lines not yet written and for every line and bucket already written, because a later reader reads tranche-1 lines and buckets unchanged — the D6/D13 council of 2026-11-02 found that "a ruling for another shape needs a migration of data already written" is false for both decisions — and what a deploy makes irreversible is only information never recorded (the per-axis `samples` count, which is why the ruling records it) and lines an older daemon erased. D1 (b), D10's transitional state, OAD-1 and OAD-2 state their own reversibility in B-1, B-6t and the stubs below. AD-21, AD-22 and AD-23 are procedure and order, reversed by a later decision at no code cost.

### Architecture decision stubs (context and options filled; each decision settled by council on 2026-11-03)

**OAD-1 — `render-no-window-classification` (D0 + D15).** *Context*: the no-session-window account
has no authored state on the panel row (the Session meter, and — once S6 makes `last_known_session_pct` optional — the blind held block's bar, percent slot, caption and spoken phrase), the panel's Stats tab (the cell and the row's spoken summary), or the CLI `status` cells; the register
rule forbids inventing one. *Options*: (i) decided to be nothing — the shared `—` gap sentinel;
(ii) as authored — a mock frame; (iii) genuinely open. *Evidence that discriminates*: whether the
maintainer wants the row to read as "one window, by design" (i) or as a distinct account kind (ii).
*Reversibility*: a register row and a fixture — a two-way door until a golden is committed and
consumed. *Decision*: **settled by council, 2026-11-03** — option (i); the record is a comment on #1594. *Owner of the row*: the maintainer who ratifies mock
frames, at whose delegation the council sat. S11 (#1594), the panel render half of S6 and the Swift render half of S8 build what it classified, after #1599 and, for the Stats cells, #1601; S6's and S8's decode halves land before them.

**OAD-2 — `blind-gate-weekly-only-active` (D3, the arming half).** *Context*: ADR-0017's preemptive
swap keys on the retained session anchor against `BLIND_GATE_RISK_BAND` (`src/daemon.rs:401`); a
weekly-only active has no such anchor, so it is unprotected while blind. *Options*: (a) leave it
unprotected and say so (this design's surfacing half); (b) arm on a weekly anchor against a weekly
band — a new constant, a new ADR-0017 amendment, and a swap that fires on the slower-moving axis.
*Evidence that discriminates*: a `blind_exit` line from a weekly-only anchor with `was_active=true`, `swapped_away=false` and no `session_pct=` token whose `weekly_pct` is below the weekly rotation line and whose `weekly_at_recovery` is at or over it — a weekly wall reached while blind with no arm acting; `reliability`'s `without_session_window` bucket (#1602) counts such windows and scores none, so the line itself is read until a weekly census exists (MET-R-003 counts `usage_gap` streaks and cannot see a weekly climb). *Reversibility*: (b) adds a
constant and an arm — reversible by removing both. *Decision*: **settled by council, 2026-11-03** — (a), at the maintainer's delegation; the arming half stays outside #1590 until that line is observed, because its constants would be uncalibrated (the session band rests on #451 and #484; no weekly velocity is retained; the one production weekly burn behind blindness is +12 pp over 2 h, 2026-07-17) and the server-dictated blind windows are already the #582 arm's.

**OAD-3 — `stats-wire-rides-first-or-follows` (D8).** *Context*: B-8's exclusion in `usage_stats` is
compiler-forced into tranche 1; the wire half (schema 2, Swift optional, one golden) is not. Until it
lands, `stats --json` carries a zero `session` block for a weekly-only account — a fabricated reading
on a wire. *Options*: ride first (tranche 1 grows by roughly half a day: one Rust reshape, one Swift
optional, one fixture, one golden; no queue contention — the `stats` version is independent of the
status minor); follow (accept the interim wire defect knowingly). *Recommendation*: **ride first** —
the cost is small and the defect is of the class the PRD calls "never". *Note*: riding first puts the first Swift edit and a `stats` schema bump on the urgent path — the class of work D10's own reasoning kept off it for S6 — so D8 and OAD-3 were settled as one question, not two. *Decision*: **ride first — settled by council on 2026-11-03 at the maintainer's delegation, with D8 as one question.** #1601 lands in tranche 1 in its own pull request after #1598's and before #1604, which it blocks; its cost is #1601's own estimate (1.5–2.5 working days), not the half day above; the first Swift edit and the `stats` bump ride the urgent path without the status minor's queue, and § 4's `stats --json` exception lapses. The maintainer may re-order to "deploy first, rebuild once #1601 merges" or revert to "follow" with the exception widened at no code cost.

## 12b. Alternatives Considered and Drawbacks

| Rejected approach | Why rejected |
|---|---|
| Fabricate `session = 0.0` as a parser-only urgent patch | lies on every measurement surface and writes an irreversible `"session":0.0` to the store; byte-identical to an idle window; the investigation's Q1 verdict and the PRD's § 3.3. Rejected for tranche 1 and after; the maintainer's decision of 2026-11-01 accepts a narrowed form as tranche 0 only (#1613) — a private flag marks the reading, no sample is stored for it, and tranche 1 removes both (§ 1b); the wider narrowing — consulting the flag at the wire and the event emitters too — was weighed and rejected, because each consultation is a hand-audited read on the urgent path while the store is the only irreversible sink (PRD § 3.3) |
| `session = weekly` | excludes a weekly-only spare at the viability floor; fires swaps attributed `reason=Session` |
| A flag beside an unchanged `f64` | must be consulted at every read or the fabricated value stands — the cost tranche 0's hot-fix accepts for a bounded time (§ 1b) |
| Reuse `session_pct: null` for "no window" | the contract defines `null` as "poll failed"; the Swift decoder documents the same; a 1.14 app renders it as the blind-style `n/a`; a rule the contract does not state |
| Read `/api/oauth/profile` for the plan name | the plan does not name the per-account window state; opens a second egress endpoint behind #737's gate |
| `Option<Window>` per window on `Usage` | the better end state, but it reshapes every `Usage` literal and the `Copy` decision type on the urgent path — recorded as a follow-up refactor (#1605), never a prerequisite |
| A versioned sample line | a larger change than the versionless `null`, and it fences nothing: no shipped build checks a version, so an older daemon skips and erases a versioned line exactly as a `null` one (the D6/D13 council's measurement, 2026-11-02); the D6/D13 ruling accepts four skew residuals (lines appended before the first fenced write; a rollup deleted or moved by hand; an older `reliability`'s report that leaves `null` lines out; every pre-#1598 build's own `stats` and compaction stopped for good) and the rollup fence instead |
| Hold ADR-0033's census UNKNOWN whenever a weekly-only account is present | hides the real session census of the accounts that have the window |
| A source lint over numeric defaults | false positives; the per-arm `None` test is the instrument |

**Known drawbacks of the approach taken**: the hot-fix's bounded fabrication until tranche 1 deploys (§ 1b); the transitional days during which a healthy no-window
account's session cell reads like a failed poll and its blind report arms are silent (B-6t); the
version-skew residuals an older daemon carries (D6, settled by council with a rollup fence: lines appended before the first fenced write, and a rollup deleted or moved by hand); a larger test diff (125 + 120 sites) whose
wrappers must be read for the `Some(0.0)` hazard; the `Option<Window>` refactor left on the table.

## 13. Quality Requirements

| Attribute | Target (PRD) | Where the design meets it | Validated |
|---|---|---|---|
| `REQ-DEP-Q-001` FleetReadingCoverage | 17/17 by the second cycle; FAIL any `n/a` after the third | B-1 + B-2 serve every reply (tranche 0's hot-fix serves the weekly-only ones first); B-12's check measures it — a recovery check at tranche 0, a no-regression check under tranche 1 | Cap-7 (manual) |
| `REQ-ARM-Q-001` ScheduleRateNeutrality | delta 0 | no cadence change; the stagger locks run unmodified | Cap-10 |
| `REQ-STL-Q-001` WireLockstep | 0 stale sites after the bump | B-6's sweep; `grep -c '"minor":<old>'` over `Fixtures.swift` prints exactly the line count of the pinned fixture B-6 adds at the outgoing minor (`0` where none was added), and the `swift` job's test cardinality | Cap-3 |
| `REQ-SMP-Q-001` SampleStoreContinuity | 0 `null` lines lost per 1 000 | B-7's round-trip and compaction tests | Cap-4 |
| `MET-T-003` NoFabricatedWindowValue | 0 surfaces from tranche 1's deploy; tranche 0's hot-fix is the bounded exception (§ 1b) | AD-4's four moves; Oracle B; one test per surface | Cap-1…Cap-5 |
| Latency / throughput | unchanged | one `Value` walk per poll | — |
| Availability | unchanged | no new failure mode on the poll loop (fail-open store) | — |

## 14. Risks and Open Questions

### Ratification and lock status

| Item | Standing |
|---|---|
| Readiness verdict | `dor_status: failed` (check 6). **Acknowledged by the `/scope` orchestrator on the operator's behalf, recorded ratification-pending; not the operator's acknowledgment.** The design proceeds PROVISIONAL |
| Intake, track selection, per-track summaries, enrichment, open-question handling | pipeline-decided at every stop of the design-solution command, each with the default it took: intake — the PRD accepted as input despite `dor_status: failed`, acknowledged on the operator's behalf; track selection — Technical Architecture, UX/IA in generative mode and Testing Architecture, with API Design and Data Architecture folded into Technical (§ 8); the per-track summaries — accepted as written; enrichment — the product and UX lenses dispatched and their concerns applied (below); open-question handling — D11–D15 added, each built for its author-chosen default as of the 2026-11-01 run (each settled by council since: D13 on 2026-11-02, the rest on 2026-11-03); the premortem — de-anchored, DP-1 to DP-6; the lock — left NOT LOCKED. The run's own record is machine-local scratch, not committed and not a citation; this row is the committed record of the stops |
| Product lens (`product-strategist` agent) | **APPROVE WITH CONCERNS** — "The design builds exactly the class and shape the operator's two messages ask for, covers all 46 PRD requirements (verified by ID set, not taken from the matrix), ships nothing the non-goals forbid, makes every scope extension compiler-forced and flagged, and returns the one appetite change (OAD-3) to the PRD; the concerns are about what the operator must see beside the author-chosen rows before ratifying, and none changes the work items Stage 3 would write." Six non-blocking concerns, all applied in this revision (below) |
| UX lens (`ux-architect` agent) | **BLOCKING** on the first pass — "The structure, flows and D0 routing are sound and invent nothing on the panel, but § 9a's inventory misses one state S6 forces on the panel at compile time — the blind held block for a weekly-only blind active — so Stage 3 would scope S6 with an unclassified render decision inside it, the exact failure REQ-CEL-C-001 and the register rule exist to prevent; the fix is a row in § 9a and a sentence in OAD-1, and tranche 1 is unaffected." Revised as prescribed (§ 9a fifth cell, § 15, OAD-1, B-6, § 11); seven non-blocking concerns applied; re-dispatched once — second pass **APPROVE WITH CONCERNS**: "The blocking concern is resolved — § 9a now inventories the blind held block as the fifth RenderCell, § 15, OAD-1, AD-24, B-6 and § 16 route it to D0 under D15, and § 11 hands its spoken phrase over — so every panel state S6 forces is classified-pending rather than unrouted; one narrower gap remains in how S6's 'decode half lands, render half waits' split behaves while D0 is open, and it is a sentence to add, not a decision to make." That one non-blocking concern is applied (B-6 interim, § 9b (iii), DP-1, R-D9); all seven first-pass concerns confirmed addressed |
| Maintainer decisions of 2026-11-01 | Three, quoted verbatim in AD-23: tranche 1's budget re-set and a hot-fix added as tranche 0 (#1613); that hot-fix to happen as soon as possible, with the local daemon rebuilt and restarted; the runbook text (#1603) moved to tranche 2, after the deploy. **Decided by the maintainer, not ratification-pending**; the decisions D0–D15, AD-4b and AD-13 are settled by council at the maintainer's delegation (§ 12); every other decision of this design stays pending |
| Lock | **NOT LOCKED** — no load-bearing question stands: D6 and D13 were settled by council on 2026-11-02, D0–D5, D7–D12, D14 and D15 on 2026-11-03; the brief carries `status: draft` |

### Lens concerns and their disposition

**Product lens** (six, all non-blocking). (1) AD-6 / B-6t — the omitted block is no regression against the bundle that ran before tranche 0, and the interim cost lands on the operator's active account → both facts in B-6t row 5 and AD-6; R-D5 raised to MEDIUM. (2) B-6t / AD-23 — name the signpost when tranche 1's week passes (re-based to the end of tranche 1's budget by the 2026-11-01 decision, AD-23) → B-6t row 5, AD-6, R-D5. (3) AD-13 — `cap_hits` / `time_at_cap_secs` and the `caps` / `t@cap` columns would read `0` for an account with no session cap → both take the absence treatment (AD-13, B-8). (4) AD-22 — the landing order of #1537 is the operator's call → flagged author-chosen. (5) B-3 rows 1, 10, 13 — the weekly-less arms are design-derived by symmetry → AD-4b, flagged. (6) OAD-3 — riding first puts the first Swift edit and a `stats` bump on the urgent path → noted in OAD-3; D8 and OAD-3 ratified as one question.

**UX lens** (one blocking, seven non-blocking). (B) § 9a misses the blind held block S6 forces on the panel → the fifth RenderCell in § 9a and § 15, OAD-1 names it beside D15, B-6 states that S6's panel render half waits on D0 while its decode half does not. (1) the Stats-tab spoken row summary needs the same classification → D15 and § 11 Accessibility. (2) B-6t under-states the blind active's cost → B-6t row 5 rewritten, DP-1 and R-D5 updated. (3) the healthy weekly-only row's cost is smaller than stated, because a failed poll nulls both windows → § 6a flow 2's discriminator and its limit. (4) flows 3–4 lack error paths → written. (5) the `stats` `—` means UNKNOWN, not observed absence, and the signal, velocity, runway and cap cells degrade alike → B-8 and § 9a. (6) option (ii)'s row-geometry cost, and the `status` table's own `EXPIRY_GAP` precedent for option (i)'s CLI half → § 9b. (7) B-6t row 4 cited `resetIn`, a test-only helper → the per-meter reset label is cited instead. **Second pass** (APPROVE WITH CONCERNS, one non-blocking): S6's decode half cannot land alone (a present block missing a required field throws in the Swift decoder) and the panel needs a non-inventing interim for a decoded percent-less block while D0 is open → B-6 states the interim (render it as no block, today's two-meter `n/a` row) and its cost; § 9b (iii), DP-1 and R-D9 carry it. The lens confirmed all seven first-pass concerns addressed.

### Stage premortem (run at design entry, de-anchored; the items the PRD's P-rows did not name)

| ID | It shipped and failed because… | Verdict | Routed to |
|---|---|---|---|
| DP-1 | the transitional state omitted the `blind_active` block and the operator never learned a weekly-only active had been blind for an hour | surface | AD-6's stated cost (B-6t row 5: indistinguishable from a failed-poll row on both surfaces until S6); the signpost: S6 (#1599) merges and #1608 deploys it within 14 calendar days of the tranche-1 deploy record on #1604 (the comment that closes it) — tranche 2's own one-to-two-week appetite (PRD § 3.1), measured from the day the transitional state first serves; S6 is exempt from tranche 2's ship-what-is-done breaker, because REQ-STL-B-001 forbids the transitional state outliving it. Before that day passes, the operator records on #1590 either #1599's merge commit and #1608's deploy record, or a comment naming the cause and re-ratifying D10 by council with the interim's observed cost in hand and a new dated bound (D10, settled by council on 2026-11-03); `blind_enter` / `blind_exit` events still record the episode. After S6 and before #1594 builds the held block as D0/D15 classified it, the panel alone keeps that blindness invisible (a percent-less block renders as no block, B-6) while the CLI footer shows the duration and verdict — "days" become "until #1594" on the panel (R-D9) |
| DP-2 | a #1598 build compacted the store and wrote the format-2 rollup envelope, and an older `stats` binary in `target/` refused the whole rollup with `UsageRollupMalformed` | decide | B-7's loud-failure statement — under the council's rollup fence that refusal is the intended outcome for every fenced rollup, whatever its buckets hold, and the ruling's C6 makes the message expected text; B-12's no-older-daemon rule names stray `target/` builds and says the store files are never deleted or moved |
| DP-3 | `absent_windows` collided with a key #1588 adds to the same struct | test | the rebase of whichever lands second; Cap-3's decoder tests |
| DP-4 | the `stats` panel tab crashed on a `nil` session dimension | decide | B-8 decodes `nil` without edit to the render path; D15 for the rendering |
| DP-5 | the weekly-less account arrived and `Sample.weekly` was still `f64` | decide | AD-10's symmetric shape |
| DP-6 | the oracles were "proven RED" by a `--include-ignored` run that re-sealed a frozen fixture | decide | AD-18's filtered command; § 11 gate 7 |

### Feasibility summary

| Component | Verdict | Evidence |
|---|---|---|
| B-1 parser | **FEASIBLE** | the investigation's prototype of this exact shape: `9 passed; 0 failed` against Oracles A–D |
| B-2 model | **FEASIBLE** | compiler-measured consumer set (33 + 17 production errors, lower bounds) |
| B-3 arms | **FEASIBLE** | every arm is a pure function or a tested method; the moves are one-liners per site; no new state |
| B-4 fixtures, B-5 oracles | **FEASIBLE** | mutation evidence in hand; the RED proof is a filtered run |
| B-6 wire + B-6t | **FEASIBLE** — coordination, not a spike | the `rotated` precedent for the reshape; the lockstep counts measured at `fd8640b` |
| B-7 store | **FEASIBLE** | the four optional siblings already use `serde(default)`; the fold is local |
| B-8 stats | **FEASIBLE** | one reshape, one Swift optional, one golden emitter |
| B-9, B-10 records | **FEASIBLE** | in-place amendment precedent (ADR-0006, 0012, 0013, 0017, 0020) |
| B-11 render | **FEASIBLE once D0 lands** (the verdict of 2026-11-01; D0 was settled by council on 2026-11-03); not a spike — a decision | the `—` token and the expiry-line precedent exist; the gates are known |
| B-12 deploy | **FEASIBLE** | the runbook exists; the identity preflight is its own first step |

Gate decision: **PROCEED** — no UNCERTAIN or INFEASIBLE Must-Have; no spike required. The one
unknown that would have been a spike (does a second reply shape exist among the 14 uncaptured
accounts?) is cheaper as the PRD's first decision gate than as a probe this run cannot make (the probe
reads the keychain).

### Risk register (design level; L × I)

| ID | Risk | L | I | Score | Mitigation | Owner |
|---|---|---|---|---|---|---|
| R-D1 | Open-PR contention: #1537 conflicts textually with B-1; #1588 with B-6's minor and goldens; #1538 with B-3 row 7 | 3 | 2 | 6 MEDIUM | AD-22: order stated, rebase rule stated; nothing re-scoped; the transitional state needs no minor | implementer |
| R-D2 | A second `minor: 15` on `main` | 2 | 3 | 6 MEDIUM | AD-9: the minor is chosen at landing; the future-frame fixture moves with it | implementer |
| R-D3 | An older daemon stalls on the fenced rollup (silent: it discards the compaction error) and erases `null` lines only in the first two of the residuals the D6/D13 ruling accepts: lines appended before the first fenced write, and a rollup deleted or moved by hand; an older `stats` refuses the fenced rollup (loud, by design) | 2 | 3 | 6 MEDIUM | AD-10/AD-11 state both; B-12's no-older-daemon rule and ancestry check; the rollup fence #1598 builds; D6 residual recorded | operator |
| R-D4 | Tests "fixed" by `Some(0.0)` wrappers; a consumer defaults silently (P-1, P-11) | 2 | 3 | 6 MEDIUM | AD-4; per-arm `None` tests (Cap-2); `MET-T-003`; review reads every wrapper | implementer, reviewer |
| R-D5 | The transitional days render a weekly-only blind active as an ordinary failed-poll row on both surfaces (duration and percent lost; PRD § 11.1's Stale cell unmet) and silence its report arms; the healthy weekly-only row costs less than first stated, because a failed poll nulls both windows and the weekly cell discriminates (§ 6a flow 2) | 3 | 2 | 6 MEDIUM | AD-6 documents and dates it; the signpost: S6 (#1599) merges and #1608 deploys it within 14 calendar days of the tranche-1 deploy record on #1604 (the comment that closes it) — tranche 2's own one-to-two-week appetite (PRD § 3.1), measured from the day the transitional state first serves; S6 is exempt from tranche 2's ship-what-is-done breaker, because REQ-STL-B-001 forbids the transitional state outliving it. Before that day passes, the operator records on #1590 either #1599's merge commit and #1608's deploy record, or a comment naming the cause and re-ratifying D10 by council with the interim's observed cost in hand and a new dated bound (D10, settled by council on 2026-11-03) | operator (D10, settled by council 2026-11-03) |
| R-D6 | Goldens regenerated with `--include-ignored`, re-sealing the frozen migration fixture | 1 | 3 | 3 LOW | AD-18; § 11 gate 7 says never | implementer |
| R-D7 | Appetite overrun on tranche 1 against its re-set budget — the human effort-day estimate the coverage gate sized (PRD § 3.1), no longer one calendar week (one compile unit across ~15 files plus 245 test sites) | 2 | 2 | 4 MEDIUM | AD-23's cut order; the hot-fix (tranche 0, #1613) means an overrun no longer leaves the fleet unreadable; "extend with justification" by one small batch, never a fabricating shortcut beyond tranche 0's bounded one (§ 1b) | operator |
| R-D8 | A second reply shape among the 14 uncaptured accounts | 1 | 2 | 2 LOW | the PRD's first decision gate (capture quota fields, add a fixture) | operator |
| R-D9 | #1594 never lands (D0 itself landed by council on 2026-11-03; the score is the snapshot of 2026-11-01, given when the risk read "D0 never lands"): the transitional `n/a` becomes permanent by default and, once S6 lands, the panel keeps rendering a percent-less `blind_active` block as no block — the blind duration and verdict stay invisible on the panel alone (the CLI footer shows them) | 2 | 2 | 4 MEDIUM | S11 is a tracked waiting item (#1594) with a named owner; OAD-1 records the ruling, which names the held block explicitly | maintainer |

No HIGH risk remains in this design's own register (R-D1 to R-D9). The PRD's four HIGH risks (PRD § 6, R1 to R4)
are the problems this design is built to close, and each sits at MEDIUM or below here once its mitigation is
in place: R1, open-PR and schema-minor contention, as R-D1 and R-D2 under AD-22 and AD-9; R2, an invented session
value acted on, by the four named moves on every decision arm, with R-D4 guarding the tests; R3, an invented
value written to disk, by B-7's `null` line, which leaves nothing invented to write; R4, silent loss on the
sample-line change, as R-D3 under the rollup fence and B-12's no-older-daemon rule. Rabbit-hole scan (10× test): if B-6's lockstep takes 10× longer, tranche 1 still
ships the sample line and an honest `session_pct: null` under B-6t, the fleet's weekly readings having recovered at tranche 0 — it is not load-bearing; if B-3's arm sweep takes 10× longer, the
whole compile unit waits, which is why B-5 lands first and the arms are enumerated here rather than
discovered at the compiler. Gate: **PASS**.

### Open questions — none stand (every one was load-bearing; the brief carries each with its context and its record)

D1, D2, D3, D4, D5, D8, D9, D10 — once standing as PRD § 8 stated them — were settled by council on 2026-11-03 at the maintainer's delegation, and each PRD § 8 row records its ruling and its record (D1 and D2 on #1596; D3 and D4 on #1602; D5 on #1600; D8 on #1601; D9 and D10 on #1597). D11 (the wire key's shape), D12
(`Sample.weekly` optional), D14 (event-field absence), the weekly-less arms (AD-4b), the `stats` field set (AD-13) and the #1537 landing order (AD-22) — once the author's own defaults in § 12 — were settled the same day, and their § 12 Flag cells record the rulings: D11 ratified in shape and amended in text (a comment on #1599); D12 as a faithful application of D6 (a comment on #1598); D14 with AD-14's text amended by the value-domain half of the 13 → 14 bump and the bucket's admission rule, and B-3 row 15's `BlindGateEligible` clause conditioned on D3 (a comment on #1602); AD-4b ratified on the #1614 Answer 2 premise with D3 and D4 (a comment on #1602); AD-22 with D9 (a comment on #1597); AD-13 as amended 2026-11-01, with D8 (a comment on #1601). D6 and D13 are no longer open: a council settled both on 2026-11-02 at the maintainer's delegation (a comment on #1598), ratifying each default and amending D6 with the rollup fence and the corrected deployment rule (B-7, B-12) and D13 with the per-axis `samples` count (AD-11). D0 and D15 are no longer open either: a council settled both on 2026-11-03 at the maintainer's delegation (a comment on #1594), classifying the no-session-window account under *Decided to be nothing*, with no track in the Session meter or in the held block (OAD-1, B-11). D7 is no longer open either: a council settled it on 2026-11-03 at the maintainer's delegation (a comment on #1602), keeping the ranking `0.0` gave a spare with no session window, replacing the number with the named move pass, and amending the EMA fold to reset the slot on every served reading with no session window (AD-16, B-3 row 12). The one product-scope item once returned to the PRD rather than decided
here — whether S8's wire half rides in tranche 1 (OAD-3) changes the appetite of § 3.1, which the PRD
owns — was settled by D8 (a comment on #1601): ride first, #1601 in tranche 1 before #1604.

### Open items the design does not own (recorded here, tracked elsewhere)

The `Option<Window>` refactor (§ 12b), tracked as #1605; the per-axis sample count in the tiers
(AD-11), #1610, absorbed into #1598 by the council ruling of 2026-11-02; the accessibility label of an absent meter (§ 9b, handed to D0), carried
by #1594; PR #1537's D9, carried by that pull request. The follow-ups filed while scoping are each Related to #1590 and none is its child: the `usage_gap` event's missing cause (C2), #1609; slow-polling a peer whose polls keep ending in a rejection (C3), #1612; severity from `limits[]` beneath a top-level window (C7), #1606; ADR-0013's drifted code pointers, #1607; keeping a viable peer visible to swap-target selection when its usage poll fails, #1611; a window the reply carries but cannot read, which the hot-fix and B-1 serve as no window, #1614; the `Tunables.poll_secs` doc comment's sweep cadence, #1615; one identifier per account on every event-log line, #1616. Tranche 2's deploy, #1608, is a child of #1590, not a follow-up.

## 15. Glossary

| Canonical term (PRD § 9) | Definition | Technical Architecture alias | Testing alias | UX/IA alias |
|---|---|---|---|---|
| **UsageReply** | one HTTP 200 body per poll | the JSON `body` / `root: Value` in `parse_usage` | the fixture body | — |
| **QuotaWindow** | one quota meter the plan carries, or does not | the per-window `Option` (fraction, reset, severity); `WindowKind` on the wire | "window present / absent" | the Session meter / the Weekly meter |
| **Reading** | the two windows the daemon decides on | `UsageReport` (raw), `Usage` (decision), `PolledReading` (with sample-only fields), `LastGood` / `BlindAnchor` (retained) | "a reading whose window is `None`" | — |
| **DecisionArm** | a rule that reads a reading | the fifteen rows of B-3 | Cap-2's per-arm tests | — |
| **StatusLine** | one account's row on the wire | `AccountStatusLine`, its `BlindActive` block, `absent_windows` | the golden rows | the panel row, the CLI row |
| **Sample** | one durable line per successful poll | `Sample`; the `HourBucket` / `DayBucket` tiers it folds into | the round-trip and compaction fixtures | — |
| **StatsReport** | one `stats --json` document | `StatsWire` / `AccountWire` / `DimWire` / `Band` | `wire-stats-basic.json` | the Stats tab, the `stats` table |
| **DesignRecord** | a written premise the code implements | the four ADRs, the parser doc, the warm-up PRD + design, HQ `REQ-USG-B-001` | Cap-6 (non-testable) | — |
| **Oracle** | an executor-uneditable judge | the four `#[ignore]`d tests of B-5 | Cap-8 | — |
| **Fixture** | a body the parser tests feed | the `r#"…"#` literals of B-4 | Cap-1 | — |
| **Deployment** | the bundle serving on the operator's machine | the runbook's procedure (B-12) | Cap-7 | — |
| **RenderCell** | where the operator sees the state | B-11; the `—` gap sentinel D0 classified | Cap-9 | the Session meter slot, the blind held block (bar, percent slot, caption, spoken phrase), the SESSION%/RESET cells, the Stats-tab cell and its spoken summary |
| *transitional wire state* | the `1.14` behaviour tranche 1 ships before S6 | B-6t | Cap-3's transitional tests | the `n/a` the operator sees for days |
| *absent window* | a window the reply did not carry — distinct from a failed poll and from a measured `0 %` | `None`; `absent_windows`; `"session":null`; an omitted `DimWire` | the `None` case of every Cap | the state D0 classifies |

No conceptual mismatch and no missing entity across the three tracks: every entity the technical
track names is an alias of a PRD object, and the two terms the design introduces (*transitional wire
state*, *absent window*) are states of PRD objects (StatusLine, QuotaWindow), not new entities.
Cross-track consistency: Technical ↔ Testing — every building block maps to a capability and every
capability to building blocks (§ 11, § 16); Technical ↔ UX/IA — the wire carries the state the
cells will render, and the transitional `n/a` is the same bytes both tracks describe; UX/IA ↔ UI —
no UI track ran (the mock authors no frame; D0). No conflict to resolve.

## 16. Requirement-to-Track Coverage Matrix

| PRD Req | Track(s) | Section | Capability | Status |
|---|---|---|---|---|
| REQ-RPL-B-001 | Technical, Testing | § 5 B-1, B-5 (Oracle A/B) | Cap-1, Cap-8 | covered |
| REQ-QW-B-001 | Technical, Testing | § 5 B-1 (`dimension` unchanged) | Cap-1 | covered |
| REQ-RPL-C-001 | Technical, Testing | § 4.2, § 5 B-3 row 15, B-6, B-7, B-8 | Cap-1…Cap-5 | covered |
| REQ-RPL-B-002 | Technical, Testing | § 5 B-1, B-5 (Oracle D) | Cap-1, Cap-8 | covered |
| REQ-RPL-B-003 | Technical, Testing | § 5 B-1 (D1), B-5 (Oracle C) | Cap-1, Cap-8 | covered — D1 (a) settled by council, 2026-11-03 |
| REQ-RPL-B-004 | Technical, Testing | § 5 B-1 (D2), B-4 | Cap-1 | covered — D2 identical, settled by council, 2026-11-03 |
| REQ-RDG-B-001 | Technical | § 4.1, § 5 B-2 | Cap-2 | covered |
| REQ-RDG-C-001 | Technical, Testing | § 4.2, § 5 B-3 (rows 1–15) | Cap-2 | covered |
| REQ-ARM-B-001 | Technical | § 5 B-3 row 1 | Cap-2 | covered |
| REQ-ARM-B-002 | Technical | § 5 B-3 row 1 | Cap-2 | covered |
| REQ-ARM-B-003 | Technical | § 5 B-3 rows 3–5 | Cap-2 | covered |
| REQ-ARM-B-004 | Technical | § 5 B-3 row 9 | Cap-2 | covered |
| REQ-ARM-B-005 | Technical | § 5 B-3 row 6 | Cap-2 | covered |
| REQ-ARM-B-006 | Technical | § 5 B-3 row 6 | Cap-2 | covered |
| REQ-ARM-B-007 | Technical | § 5 B-3 row 7, B-6t, B-6 | Cap-2, Cap-3 | covered (surfacing half: #1599 § 2a's fourth report arm, settled by council, 2026-11-04); arming half OAD-2, settled (a) by council 2026-11-03 with its reopen evidence |
| REQ-ARM-B-008 | Technical | § 5 B-3 row 8 | Cap-2 | covered — D4 ratified by council, 2026-11-03 |
| REQ-ARM-B-009 | Technical | § 5 B-3 row 10 | Cap-2 | covered |
| REQ-ARM-B-010 | Technical | § 4.5, § 5 B-3 row 11 | Cap-2 | covered |
| REQ-ARM-B-011 | Technical | § 5 B-3 (closing paragraph), § 6b | Cap-2 | covered |
| REQ-ARM-B-012 | Technical | § 5 B-3 rows 1, 12 | Cap-2 | covered — settled by council (D7) |
| REQ-ARM-C-001 | Technical | § 4.5, § 5 B-3 row 14 | Cap-2 | covered |
| REQ-ARM-Q-001 | Technical, Testing | § 11 Performance, § 13 | Cap-10 | covered |
| REQ-FIX-B-001 | Testing | § 5 B-4 | Cap-1 | covered |
| REQ-ORC-B-001 | Testing | § 5 B-5, § 11 | Cap-8 | covered |
| REQ-STL-I-001 | Technical | § 5 B-6, § 8 | Cap-3 | covered — key shape D11, settled by council 2026-11-03 |
| REQ-STL-I-002 | Technical | § 5 B-6 (the reshape) | Cap-3 | covered — wire and decode halves; the held block's panel render half is #1594's (D15, settled by council 2026-11-03) |
| REQ-STL-C-001 | Technical, Testing | § 5 B-6 (lockstep), § 4.6 | Cap-3 | covered |
| REQ-STL-B-001 | Technical | § 5 B-6t | Cap-3 | covered — D10 settled by council, 2026-11-03 |
| REQ-STL-Q-001 | Testing | § 13 | Cap-3 | covered |
| REQ-SMP-B-001 | Technical | § 5 B-7 | Cap-4 | covered |
| REQ-SMP-B-002 | Technical, Testing | § 5 B-7 (reader, compaction, tiers) | Cap-4 | covered |
| REQ-SMP-C-001 | Testing | § 5 B-7 (tests that move), § 11 | Cap-4 | covered |
| REQ-SMP-Q-001 | Testing | § 5 B-7, § 13 | Cap-4 | covered |
| REQ-STS-B-001 | Technical, UX/IA | § 5 B-8, § 9a | Cap-5 | covered — wire timing settled ride first (D8, council 2026-11-03); Stats-tab cell D15 |
| REQ-STS-C-001 | Technical | § 4.5 | Cap-5 | covered (no change, by construction) |
| REQ-REC-B-001 | Technical | § 5 B-9 | Cap-6 | covered — non-testable (documentation) |
| REQ-REC-B-002 | Technical | § 5 B-10 | Cap-6 | covered — non-testable (documentation) |
| REQ-REC-B-003 | Technical | § 5 B-10 (parser doc) | Cap-6 | covered — non-testable (documentation) |
| REQ-REC-B-004 | Technical | § 5 B-10 (HQ row) | Cap-6 | covered — non-testable; the amendment itself is the operator's |
| REQ-REC-C-001 | Technical | § 5 B-10 (ordering) | Cap-6 | covered — non-testable (ordering constraint, reviewed) |
| REQ-CEL-C-001 | UX/IA | § 9, § 5 B-11 | Cap-9 | covered — the constraint holds by construction (nothing is built against an unclassified state: D0 and D15 classified the no-session-window cells and the held block S6 forces by council on 2026-11-03, and #1594 builds them as classified) |
| REQ-CEL-B-001 | UX/IA, Testing | § 9b | Cap-9 | **deferred with explicit tracking** — D0 settled by council on 2026-11-03 at option (i); the S11 waiting item (#1594) builds the cells after #1599, and the Stats cells after #1601; the backward row stands |
| REQ-DEP-B-001 | Technical | § 5 B-12 | Cap-7 | covered — non-testable (procedure); manual; the text half (#1603) is in tranche 2, after the deploy |
| REQ-DEP-C-001 | Technical | § 5 B-12 (no-older-daemon rule), B-7 | Cap-7 | covered — binds from tranche 1's deploy, which writes the first `null` line |
| REQ-DEP-B-002 | Technical, Testing | § 5 B-12 (fleet recovery check) | Cap-7 | covered — manual, operator-run; a recovery check at tranche 0, a no-regression check under tranche 1 |
| REQ-DEP-Q-001 | Testing | § 13 | Cap-7 | covered — manual; first met by tranche 0's deploy, a no-regression check under tranche 1 |

**46 of 46 addressed: 45 covered, 1 deferred with explicit tracking (REQ-CEL-B-001 → the S11
waiting item, #1594, after #1599 and, for the Stats cells only, after #1601; D0 settled by council on 2026-11-03). No UNCOVERED entry.** Seven requirements are classified non-testable by
code (documentation and procedure) and are graded by the doc gates and the runbook's manual check;
the Testing track records that classification rather than binding them to a code capability.

## 16b. Element-to-Requirement Backward-Coverage Matrix

| Design element | Type | Traces to | Provenance | Status |
|---|---|---|---|---|
| B-1 per-window parser composition; D1 (a); D2 identical | mechanism | REQ-RPL-B-001…B-004, REQ-QW-B-001 | PRD § 10.1–10.2 | traced |
| Invariants I-1, I-2 + their tests | test | REQ-RDG-C-001 | PRD § 10.3 (no read site defaults) | traced — design-derived, flagged |
| B-2 `Option<f64>` on four reading types | mechanism | REQ-RDG-B-001 | PRD § 10.3 (recommended mechanism) | traced |
| B-3 rows 1–15 (the moves) | mechanism | REQ-ARM-B-001…B-012, REQ-ARM-C-001, REQ-RDG-C-001 | PRD § 10.3 | traced |
| B-3 row 15 event-field typed absence (D14) | mechanism | REQ-RDG-C-001 (events clause) | PRD § 10.3 | traced — settled by council (D14), 2026-11-03 |
| AD-4b weekly-less arms (B-3 rows 1, 10, 13) | mechanism | REQ-RPL-B-002 (a weekly-less reading is served), REQ-ARM-B-012 (symmetry) | PRD § 10.2, § 10.3; message 2 | traced — settled by council (AD-4b), 2026-11-03 |
| AD-13 session-cap counts omitted (`cap_hits`, `time_at_cap_secs`, the `caps` / `t@cap` cells) | wire contract | REQ-STS-B-001; MET-T-003 | PRD § 10.8, § 4 | traced — ratified by council 2026-11-03 |
| B-4 fixture bodies | artifact | REQ-FIX-B-001 | PRD § 10.4 | traced |
| B-5 Oracles A–D, `#[ignore]`d, RED-proven | test | REQ-ORC-B-001 | PRD § 10.5 | traced |
| B-6 `absent_windows` key (D11) | wire contract | REQ-STL-I-001 | PRD § 10.6 ("the key's name is Stage 2's") | traced — shape settled by council (D11), 2026-11-03 |
| B-6 `last_known_session_pct` optional — the account row's, and `recent_blind_preempt_swap`'s (#1599 § 2b), which restores the notice B-6t withholds for a Retry-After swap off a session-less anchor | wire contract | REQ-STL-I-002 | PRD § 10.6 | traced |
| B-6 minor + lockstep + two new golden rows | artifact | REQ-STL-C-001, REQ-STL-Q-001 | PRD § 10.6 | traced |
| B-6t transitional state; no `blind_active` block (AD-6 sub-default) | wire contract | REQ-STL-B-001, REQ-ARM-B-007 | PRD § 10.6 (D10), § 10.3 | traced — settled by council (D10), 2026-11-03 |
| B-7 `Sample.session` optional, `null` written | artifact | REQ-SMP-B-001 | PRD § 10.7 | traced |
| B-7 `Sample.weekly` optional (D12) | artifact | REQ-RDG-B-001 (both windows), REQ-RPL-B-002, REQ-SMP-B-001 by construction | PRD § 10.3, § 10.2, § 3.2 coupling 1 | traced — settled by council (D12), 2026-11-03 |
| B-7 tier `Option<HourStat>` / `Option<DayStat>` (D13) | artifact | REQ-SMP-B-002 (fold without a fabricated statistic) | PRD § 10.7 | traced — settled by council (D13) |
| B-7 canary move + round-trip + compaction tests | test | REQ-SMP-C-001, REQ-SMP-Q-001 | PRD § 10.7 | traced |
| B-8 session-figure exclusion; census exclusion (D5) | mechanism | REQ-STS-B-001; ADR-0033 amendment (REQ-REC-B-002) | PRD § 10.8, § 8 D5 | traced |
| B-8 wire: `session`/`band` omittable, schema 2, Swift optional, golden | wire contract | REQ-STS-B-001 (representation is Stage 2's) | PRD § 10.8 | traced — settled by council (AD-13, with D8), 2026-11-03 |
| B-8 CLI `stats` table `—` reuse | UX surface | REQ-STS-B-001 | PRD § 10.8; `stats`' own UNKNOWN vocabulary | traced |
| D15 Stats-tab cell, its spoken summary and the blind held block routed to D0 | decision | REQ-CEL-C-001 (register rule) | PRD § 10.11 | traced — settled by council (D15, with D0), 2026-11-03 |
| B-9 warm-up amendment | documentation | REQ-REC-B-001 | PRD § 10.9 | traced |
| B-10 four ADR amendments, README rows, parser doc, HQ note | documentation | REQ-REC-B-002, B-003, B-004, C-001 | PRD § 10.10 | traced |
| B-11 waiting item (three options, gates) | UX surface | REQ-CEL-C-001, REQ-CEL-B-001 | PRD § 10.11 | traced |
| B-12 runbook amendment: no-older-daemon rule; fleet recovery check | procedure | REQ-DEP-B-001, C-001, B-002, Q-001 | PRD § 10.12 | traced — commands design-derived; the text half is tranche 2 (AD-23) |
| AD-22 PR composition | decision | PRD R1, § 7 | PRD § 6, § 7 | traced |
| AD-23 tranche / cut order, with the tranche-0 hot-fix | decision | PRD § 3.1, § 3.2, A-7, A-8 | PRD § 3; the maintainer's decisions of 2026-11-01 | traced — appetite and tranche 0 set by those decisions |

**27 of 27 traced. No PHANTOM element.** The invariants I-1 and I-2 (AD-3) stay a design-derived extension of a requirement (flagged in the status column) and are surfaced as a decision to ratify in § 12 rather than absorbed silently; the others once flagged were settled by council on 2026-11-03 and carry their rulings in the status column; none traces to nothing.

## Design Lock

**NOT LOCKED — provisional.** `status: draft` on the brief.

| Gate | Verdict |
|---|---|
| Forward coverage (§ 16) | 46 / 46 addressed; 0 UNCOVERED; 1 deferred with explicit tracking (REQ-CEL-B-001 → S11, #1594, which builds the cells D0 classified) |
| Backward coverage (§ 16b) | 27 / 27 traced; 0 PHANTOM |
| Load-bearing open questions | **none stand** — D6 and D13 were settled by council on 2026-11-02, D0–D5, D7–D12, D14 and D15 on 2026-11-03 (§ 14); the design stays NOT LOCKED and `final` is forbidden |
| Feasibility (§ 14) | PROCEED — no UNCERTAIN, no INFEASIBLE, no spike |
| Risk (§ 14) | PASS — no HIGH risk |
| Dual-lens ratification | product lens APPROVE WITH CONCERNS (six, all applied); UX lens BLOCKING on the first pass, revised as prescribed, re-dispatched once — second pass APPROVE WITH CONCERNS (one non-blocking concern, applied; § 14); the design's author supplied neither verdict |
| Readiness | `dor_status: failed`, acknowledged by the orchestrator on the operator's behalf — the operator's own ratification is still owed |

What the operator should read first: § 12's Flag cells, each carrying its council ruling and date (AD-4b's weekly-less arms, AD-6's omitted block and its dated signpost, AD-7's key, AD-10's weekly half, AD-13's field set with the cap counts, AD-14's events, AD-22's landing order), the three settled stubs, and D15 — the question this design found that the PRD did not carry, settled with D0 and covering the Stats-tab cell, its spoken summary and the blind held block.
