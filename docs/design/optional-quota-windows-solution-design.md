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
**Components**: S1–S12 (PRD § 0). **Open decisions**: D0–D10 (PRD § 8) plus five this design adds
(D11–D15, § 12); D6 and D13 were settled by council on 2026-11-02 (§ 14).

> **Provenance and standing.** Authored by an AI pipeline (`/scope` Stage 2, headless) from the PRD,
> the investigation report and the scope seed it names in its frontmatter `source` (both machine-local
> scratch; everything relied on is quoted in-band). **The readiness gate's `failed` verdict was
> acknowledged by the `/scope` orchestrator on the operator's behalf, not by the operator**, so this
> design is carried as **PROVISIONAL** — ratification-pending in every decision it makes, and
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
- No flag beside an unchanged `f64`; no reuse of `session_pct: null` for "no window".
- **One exception: tranche 0, the hot-fix #1613 (AD-23).** The maintainer decided on 2026-11-01 that a hot-fix
  path exists; the mechanism below is the scoping pipeline's design under that decision, ratification-pending.
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
    line is written for such a reading: a gap, never a stored zero. That is the only place the flag is
    consulted, so every other reader sees `0.0`; on the swap decision that reads as "not session-blocked",
    which is what an account with no session window needs. #1596 (B-1) removes the weekly-only branch and
    both flags (its § 7, AC-17); #1598 (B-7) replaces the sample skip with a `"session":null` line (its
    § 8, AC-16).
  - *What it costs, until tranche 1 retires it — except the last cost below, which stands until #1614 is ruled.* The status/watch wire and the panel show 0% session for
    a weekly-only account. Event-log lines that carry a session token (swap, `usage_velocity`, the blind
    events) read 0, so `reliability` over the hot-fix window reads them as idle sessions. No usage sample
    exists for such an account while it serves, so `stats` shows a gap, never a zero. The usage-gap tracker
    counts such a poll as a reading (`maintain_stats_store` keys on `polled.is_err()`), so the account's gap
    streak clears and no `usage_gap` event is logged while no sample is stored. A session window the reply
    carries but the parser cannot read is served as no window, like an absent one; #1614 puts that to the
    maintainer.
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
| The panel's register forbids building against an unclassified axis; the mock authors no no-window row | `apps/menubar/design/README.md:1289-1291`; `apps/menubar/design/menubar-preview.html:1121-1126` | S11 waits on D0 and is designed as an item carrying its options and gates (§ 9) |
| Sample lines are versionless; an older daemon skips a line it cannot parse and its compaction erases it (an older `stats` or `reliability` only reads) | `read_samples` at `src/usage_store.rs:449-467`; `read_samples` … `write_samples` at `src/usage_store.rs:535-544`; the one production caller of `compact_and_roll` is the daemon's `stats_events_for_poll` (`src/daemon.rs:839`) | No pre-#1598 build runs as the daemon against a store that carries a `null` line (D6 as the council ruled it on 2026-11-02, `REQ-DEP-C-001`); the rollup fence #1598 builds stalls an older daemon's compaction before it rewrites, and an older `stats` fails loudly on it (§ 5 B-7) |
| Open PRs on the same seam: #1537 (the `dimension` lookups at `src/usage.rs:507-508`), #1588 (minor 15, the five goldens, `src/usage.rs`), #1538 (the blind seam, ADR-0017) | PRD R1; live `gh` read 2026-11-01 (§ 4.6) | Composition stated per PR; none re-scoped |
| Gates every code item owes: the five `test` steps, `msrv`, both `deny` checks, `swift`, an **armed** `panel-goldens` run where the panel changes, and the three golden/gate trailers where they apply | `CLAUDE.md` § Before you push | § 11 Quality gates |
| Platform-neutral; Linux is landed but un-gated in CI | `CLAUDE.md` § What this repo is | § 4.7 states the assumption |
| `cargo build` deploys nothing; the serving daemon is whatever bundle was last launched — built 2026-09-05 when the scope began, the #1613 emergency build since 2026-11-01 | `docs/runbooks/replacing-the-running-daemon.md` § Why `cargo build` deploys nothing (the mechanism); the build dates are the investigation's and the scope brief's readings of the serving bundle, not the runbook's | S12 amends that runbook (B-12) |
| Appetite: tranche 0 (the hot-fix #1613) deployed as soon as it is built; tranche 1 sized to the human effort-day estimate the coverage gate computed (PRD § 3.1), no longer one calendar week; tranche 2 one to two weeks; S11 unsized until D0 | PRD § 3.1, A-9, as re-set by the maintainer's decision of 2026-11-01 (AD-23) | § 12 AD-23 adds the hot-fix as tranche 0 and orders the rest so the cut order is the tranche order reversed |

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
                                                   └──► B-6 status/watch wire ──► CLI table · Swift panel (B-11, waits on D0)
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
| BC-7 | Rendering (panel row, CLI cells, Stats tab) | RenderCell | where the operator sees the state | conformist to BC-3/BC-4; blocked on D0 |

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

`REQ-RDG-C-001` forbids the fifth move — substitute a number — at every site, including tests:
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
in the next pull request; then B-12's live half (#1604, the deploy). B-7's placement is a choice, not a
compiler-forced one: the compile unit keeps #1613's sample skip, re-keyed to skip a reading with either fraction
`None`, so it compiles without B-7, and that interim never runs on the operator's machine, because the deploy
waits on B-7. The wire ships the documented transitional state (D10, B-6t).
Tranche 2: B-6 (the key, the minor, the lockstep), B-8 (the `stats` wire — D8 decides whether it
rides earlier; § 12 OAD-3 recommends it does), B-9 (the warm-up design amendment), and B-12's runbook
text (#1603), which the maintainer moved here from tranche 1 and which lands after the deploy; tranche 2
deploys as #1608. Waits: B-11.

### 4.5 What does not move

`classify_poll` (`src/daemon/classify.rs:55-62`) — a served reply is `Live` with no edit.
`should_keep_warm_retry` (`src/daemon/keep_warm.rs:44`) reads liveness only. `reliability`'s rules
(`REQ-STS-C-001`): a weekly-only account produces no `reason=session` swaps, `relief_of` reads
`session_blocked` false (`src/usage_stats.rs:1055-1057`), and `compute_landing` (`src/reliability.rs:2138`) treats an absent reading as unmeasured. The `log --json` wire
(`JSON_SCHEMA_VERSION` at `src/log.rs:167`) carries no per-window reading that changes meaning. The
`reliability --json` wire (`JSON_SCHEMA_VERSION` at `src/reliability.rs:264`) does: its blind-window
census must give a session-less `blind_window` line a disclosed bucket to keep the #484 exact
partition, an always-present key, so it moves 13 → 14 (§ 5 B-3, #1602 Specification § 3).

### 4.6 Composition with the three open pull requests (read live on 2026-11-01; the #1588 row re-read 2026-11-02)

| PR | State read | Seam shared | Disposition |
|---|---|---|---|
| **#1537** `fix(usage): let an active weekly_scoped cap outvote the all-models window` | OPEN, MERGEABLE, `CHANGES_REQUESTED`, touches only `src/usage.rs` (+135/−1); its hunk is `@@ -450,10 +450,20 @@ fn parse_usage` and keeps both `?` lookups | the same lines B-1 rewrites | **This design lands first and #1537 rebases onto it.** The two are textually conflicting and semantically independent: B-1 makes each window optional, #1537 overrides a present weekly fraction with a higher active scoped cap. After rebase #1537's override applies to `weekly: Some(w)`; whether it may also fill a `None` weekly from a scoped cap is **D9**, left with #1537 (C6). Nothing here depends on #1537 |
| **#1588** `(feat) storage: add file-backed Claude credentials and token-only accounts` | OPEN, `CONFLICTING`, 60 files; moves `STATUS_SCHEMA_VERSION` 1.14 → 1.15 and rewrites all five goldens and the Swift fixtures; `src/usage.rs` +461/−99 (a header-derived usage path for token-only accounts) | the minor, the goldens, the Swift fixtures, and two new production constructions of the lifted types: a `Usage {` in `parse_probe_headers` (`src/usage.rs`) and a `LastGood {` in `src/daemon.rs`, plus a `Usage {` test literal under `src/daemon.rs`'s `mod tests` (read from `gh pr diff 1588` on 2026-11-02; the hunk header names `mod tests`) | **Queue, not block.** S6 takes the next free minor at landing, read from `main` and the open claimants then — 1.15 is the lowest unclaimed today — with the lockstep counts re-derived then (`REQ-STL-C-001`). Whichever lands second rebases; #1588's new `Usage` and `LastGood` constructions meet B-2's `Option` shape at the compiler, so none is missed silently. `parse_probe_headers` returns an error when the five-hour header is missing; whether a header-derived reading may carry an absent session is #1588's question and the parser's, not this design's. If #1588 lands first, #1597's sweep baselines grow by these sites. The transitional state (B-6t) needs no minor and does not contend |
| **#1538** `(fix) daemon: the blind-preempt swap also fires on the #584 velocity projection` | OPEN, MERGEABLE, `CHANGES_REQUESTED`; edits `blind_gate_armed`, `blind_swap`, `records.rs`, one `BlindActive` doc line and ADR-0017 in place | the blind seam B-3 touches; ADR-0017, which B-10 also amends in place | **Composes.** Both read the anchor's session value; under B-3 that value is `Option<f64>` and both arms read false on `None`. Whichever lands second rebases; the two ADR-0017 `**Amended**` paragraphs are additive and order by date. No re-scoping of #1538's velocity arm |

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
why `weekly_scoped` and the 25 null top-level keys are already inert (`REQ-QW-B-001`). `parse_usage`
(`src/usage.rs:503-537`) stops using `?` on the two `dimension` lookups at `src/usage.rs:507-508` and composes:

```rust
let session = dimension(&root, "five_hour", &["session", "five_hour"]);
let weekly  = dimension(&root, "seven_day", &["weekly_all", "seven_day", "weekly"]);
if session.is_none() && weekly.is_none() {
    // D1 (a), pipeline default, ratification-pending — Oracle C pins whichever D1 the operator keeps.
    return Err(Error::UsageParse("no quota window (neither five_hour nor seven_day)".into()));
}
Ok(UsageReport { session: session.map(|w| w.0), session_resets_at: session.and_then(|w| w.1), … })
```

- **D1 (a)**: a 200 reply carrying no applicable window stays a hard `UsageParse`, so a 200 reply
  with nothing to decide on maps to `PollOutcome::Transient` exactly as today and the loop skips,
  never swaps. Option (b) — `Ok` with both windows `None` — is a one-line change plus Oracle C's
  assertion; nothing durable prevents reversal.
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
| 12 | Velocity EMA and the tie-break (D7) | `usage_velocity` at `src/daemon/classify.rs:121-125`; `velocity_rate` at `src/daemon/selection.rs:135-143` | skip / pass | `usage_velocity` returns per-axis `Option<i16>`; the EMA fold skips an absent axis; on a reset tie an absent EMA reads `0.0`, "the safest (lowest-velocity) landing" — the pipeline default, ratification-pending, watched by `MET-R-001` | `REQ-ARM-B-012`: a weekly-only spare and a two-window spare rank on the same soonest-weekly-reset axis |
| 13 | Weekly-exhausted verdict on the wire | `weekly_exhausted` at `src/daemon/snapshot_build.rs:439-440` | pass | `is_some_and(\|usage\| usage.weekly.is_some_and(…))` | a weekly-less account is never `weekly_exhausted` |
| 14 | Keep-warm | `should_keep_warm_retry` at `src/daemon/keep_warm.rs:44` | — (unchanged) | reads liveness only | `REQ-ARM-C-001` |
| 15 | Event fields | `Swap` … `session_pct: u8` at `src/observability.rs:1359`; `UsageVelocity` … `session_delta_pct` at `src/observability.rs:2039`; `BlindWindow` at `src/observability.rs:2074`; `session_high_water_pct` at `src/observability.rs:2108`; `BlindEnter` at `src/observability.rs:2131`; `BlindExit` at `src/observability.rs:2171`; `BlindGateEligible` at `src/observability.rs:2203` | name | every **reachable** per-window event field becomes `Option`, rendered as an **omitted token** on the log line (the `session_high_water_pct` precedent: an absent token means "none", never "unknown"); `BlindGateEligible.session_pct` stays `u8`, because its only emitter runs past `blind_gate_armed`, which is false on `None`, so the field never meets an absent window (#1602 Specification; the scope brief's D14). `reliability`'s swap handlers already read `session_pct` optionally, but its blind-projection parser does not — `record_blind_projection` (`src/reliability.rs:1248-1262`) counts a missing `session_pct` as `malformed` — so the implementer gives the `blind_*` parsers a disclosed bucket for the absent token, `n_without_session_window`, rather than tolerance, keeping the #484 exact partition. `log`'s `JSON_SCHEMA_VERSION` does not move (`LogWire` at `src/log.rs:865` carries the line's own tokens); `reliability`'s moves 13 → 14 for that bucket (§ 4.5, AD-14, § 8) | `REQ-RDG-C-001`: an event for a weekly-only account carries no fabricated session percent and `reliability` decodes the emitted shape |

Nothing durable remembers the state: no persisted flag, no config key, no new field in the daemon's
state file (`REQ-ARM-B-011`). A window that disappears stops acting as a current reading because every
consumer re-derives per poll (rows 4, 7, 9, 12); a window that reappears is a fresh window because the
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

**The key** (D11, author-chosen default, ratification-pending): one omittable per-account key

```json
"absent_windows": ["session"]
```

- Rust: `absent_windows: Vec<WindowKind>` on `AccountStatusLine` (`src/daemon/snapshot.rs:737`),
  `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, `WindowKind` serialised lowercase
  (`session`, `weekly`). Emitted only from a **served** reading whose window is `None`; a failed poll
  has no reading and emits nothing, so "no reading" stays `session_pct: null` without the key and "no
  window" is `session_pct: null` **with** it — distinguishable by bytes (`REQ-STL-I-001`).
- Why a list rather than a boolean `no_session_window`: message 2's weekly-less account rides the
  same key with `["weekly"]` and no second minor; the omit-when-absent precedents — `blind_active`,
  added at 1.4; the daemon-level `canonical_scrub`, added at 1.5; `rotated`, reshaped at 1.14 (the
  1.10 → 1.11 `systemic_refresh_source` and 1.11 → 1.12 `expiry` bumps followed the same pattern), in
  the history the `STATUS_SCHEMA_VERSION` doc comment keeps at `src/daemon/snapshot.rs:538-541` — are
  all keys that are absent when nothing applies. Rejected: an enum per window (`session_window: "absent"`) — two keys,
  two bumps.
- Swift: `let absentWindows: [String]` decoded with `decodeIfPresent … ?? []` beside `sessionPct` (`apps/menubar/Sources/WireModel.swift:648`); beyond this read and the `UInt8?` reshape below, production `WireModel.swift` needs no decode edit (the support gate is major-only, `decodeIfPresent` at `apps/menubar/Sources/WireModel.swift:704`); the panel renders the state only under D0 (B-11).

**The reshape**: `BlindActive.last_known_session_pct: u8` (`src/daemon/snapshot.rs:167`) becomes
`Option<u8>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`, exactly the `rotated` reshape (`src/daemon/snapshot.rs:141-142`): a weekly-only blind active keeps its `blind_secs` and
`auto_protection_degraded` and carries no fabricated percent (`REQ-STL-I-002`). Swift
`lastKnownSessionPct: UInt8` (`apps/menubar/Sources/WireModel.swift:499`) becomes `UInt8?` via `decodeIfPresent` — the decode half of S6's Swift lockstep.

**The render half is a panel state the mock never authored.** `BlindMeter` reads the percent as a value for its held bar, band and text (`lastKnownSessionPct` at `apps/menubar/Sources/StatusPanelRoster.swift:911-924`) and the row's spoken phrase says "last-known session N percent" (`apps/menubar/Sources/StatusPanelFormat.swift:2197`), so what the held block shows for a weekly-only blind active is classified with D0 (D15, § 9a's fifth cell), and S6's panel render half waits on it exactly as S8's does.

**Why the two halves land together.** `blindActive` is decoded with `decodeIfPresent(BlindActive.self, …)` (`apps/menubar/Sources/WireModel.swift:712`), and `BlindActive`'s doc says a present block missing a required field throws rather than mis-reads (`apps/menubar/Sources/WireModel.swift:486-493`), so a frame carrying a percent-less block fails to decode on an un-updated app, and the decode half ships with S6. In the other direction, `lastKnownSessionPct: UInt8?` does not compile until `BlindMeter`'s `utilSeverity(_:)` and `Double(_:)` reads (`lastKnownSessionPct` at `apps/menubar/Sources/StatusPanelRoster.swift:911-922`) change with it: both take a non-optional `UInt8`. The spoken interpolation (`lastKnownSessionPct` at `apps/menubar/Sources/StatusPanelFormat.swift:2197`) compiles against an optional with a warning only — `apps/menubar/project.yml` sets no warnings-as-errors — and is changed with them so that it never prints `Optional(87)`.

**The panel interim, and its cost.** The one non-inventing option `REQ-CEL-C-001` leaves: while D0 is open, the panel renders a decoded percent-less block **as no block**, today's two-meter `n/a` row, the mock-authored state B-6t already ships, until D0 classifies the held block. On the panel the blind duration and verdict the wire now carries stay invisible until D0 (the CLI footer shows them), so if D0 stalls, DP-1's days become "until D0" on the panel alone (R-D9).

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

Pipeline default, ratification-pending, with one author-chosen sub-default:

| Element | Transitional behaviour under `1.14` | Retired by B-6 |
|---|---|---|
| A served weekly-only reading | `session_pct: null`, `session_resets_at: null`, `weekly_pct` and `weekly_resets_at` present, `weekly_exhausted` from the weekly line | the same line gains `absent_windows` |
| `session_pct` doc comment (`src/daemon/snapshot.rs:758-763`) | amended: "`null` if the last poll failed — or, transitionally until `absent_windows` lands (S6), if the served reading carried no session window" | reverted to the steady-state meaning (`REQ-STL-B-001`) |
| CLI SESSION%/RESET cells (`pct` at `src/cli.rs:4595-4598`, `reset_cell` at `src/cli.rs:4365-4368`) | `n/a` / `n/a`, exactly as today — the failed-poll token, which the mock calls a misreport for an observed absence; accepted for days, documented | B-11 under D0 |
| 1.14 Swift app | decodes without edit; the Session meter is uncoloured `n/a` (`sessionSeverity` at `apps/menubar/Sources/StatusPanelFormat.swift:1755`) beside a normal Weekly meter; each meter carries its own reset label (`UsageMeter` … `reset:` at `apps/menubar/Sources/StatusPanelRoster.swift:443-446`), so the session slot shows the failed-poll `n/a` and the weekly slot its real reset | B-11 under D0 |
| **A weekly-only active that goes blind** (author-chosen sub-default) | **no `blind_active` block is emitted**: `blind_active_view` returns `None` when the anchor's session is `None`, because under `1.14` the block cannot be emitted without a `u8` and a `0` would be the fabrication this design forbids. Cost, stated precisely: blindness reaches both surfaces only through that block — the CLI `health_cell` (`src/cli.rs:3937`) carries the auth rollup alone, and the panel's blind treatment keys solely on `blindActive` (`apps/menubar/Sources/HonestStateMachine.swift:386`) — so for those days such an account is indistinguishable from an ordinary failed-poll row, duration lost as well as percent (the pre-#479/#485 presentation), PRD § 11.1's Stale "Required" cell is unmet, the footer makes no auto-protection claim (`REQ-ARM-B-007`, D3's surfacing half), and the Retry-After and velocity report arms are silent for it. Two bounds: it is no regression against the bundle that ran before tranche 0, because no weekly-only account had ever been read, so none had ever held a `last_good` and the view emitted no block for it (the tranche-0 bundle holds such an anchor's session as `0.0`, § 1b, so a block it emitted would carry a 0 percent — the fabrication this design declines to carry); and the captured reply is the operator's active account (PRD § 2.2), so the interim cost lands on the active account. Signpost: S6 lands by the end of tranche 1's budget (AD-23), or is pulled into tranche 1 under the extend-with-justification breaker, or D10 is re-ratified (R-D5) | the block returns with `last_known_session_pct` omitted; the blind row, its duration and its verdict return with it |
| **A `Retry-After` swap off a session-less anchor** (D10's analogue; decided by the scoping party, ratification-pending; #1602 Specification § 4) | the swap still fires and its `swap` event-log line records it with `session_pct=` omitted, but **no `BlindPreemptSwapRecord` is built**, so no `recent_blind_preempt_swap` notice is served. The #582 `Retry-After` arm fires whatever the anchor's session (the `server_retry_after_holding` arm of the blind gate in `src/daemon.rs`), and every link of the notice is a `u8` under `1.14`: the record's field (`src/daemon/records.rs:322`), `recent_blind_preempt_swap_view` (`src/daemon.rs:597-614`), which copies it unconditionally, and the always-present wire key (`src/daemon/snapshot.rs:204`). A `0` there would be the fabrication this design forbids. Cost, stated precisely: for such a swap the CLI `status` prints no `swapped off {from} (blind @ last-known {pct}%) → {to}` narration and no `use <from>` undo hint (`src/cli.rs:3261-3272`); the menu-bar app does not decode the notice, so the panel loses nothing; the swap, its log line and the #582 walk record are unchanged, and a two-window anchor's notice is unchanged. Rejected: lifting the record to `Option<u8>` in tranche 1, which would make the wire key omittable under `1.14`, a reshape that belongs to B-6's minor | the record and `BlindPreemptSwap.last_known_session_pct` become `Option<u8>`, the key is omitted for a session-less anchor, and the notice returns with its labels and its undo hint (#1599 § 2b) |

What the transitional state is not: a redefinition of `null`. The steady-state contract stays "the
last poll failed"; the transitional meaning is named, dated and tied to the item that retires it.

### B-7 — the sample store carries `"session":null` and every reader copes (S7)

- Tranche 0 meets this block first (§ 1b): the hot-fix returns early from `append_sample_for_poll`
  (`src/daemon.rs`) when `session_window_absent` is set, so no sample line exists for such a
  reading. #1598 replaces that skip with the `"session":null` line below (its § 8, AC-16). The first `null`
  line is written at tranche 1's deploy, which is when B-12's both-halves rule starts to bind.
- `Sample.session: f64` (`src/usage_store.rs:135-166`) becomes `Option<f64>` with
  `#[serde(default)]` and **no** `skip_serializing_if`, so the key is always written — `null` for an
  absent window (`REQ-SMP-B-001`; #1580's exact-key-set oracle is green on `null` and RED on
  omission). **D12** (author-chosen, compiler-forced by S2, ratification-pending): `Sample.weekly`
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
  a word, rewrites nothing and erases no `null` line — a stall of compaction on that store, which
  ends when a #1598+ build runs again. Of the residuals the ruling accepts (PRD R4 records the
  risk), an older daemon erases `null` lines in only these: lines appended before the first fenced
  write, and a rollup deleted or moved by hand. The first poll after the deploy appends, then
  compacts and writes the fence, so only a failed first pass leaves lines unfenced, for up to one
  compaction cadence, an hour. After a rollup is deleted or moved by hand, an older build reads the
  store as new and its compaction erases every `null` line (C6: the store files are never deleted or
  moved). An older `stats` or `reliability` only reads and deletes nothing: `stats` **loudly**
  refuses every fenced rollup with `UsageRollupMalformed` ("malformed usage rollup"), whatever its
  buckets hold, and `reliability`, which never reads the rollup, leaves the `null` lines out of its
  report in silence. That "malformed usage rollup" message from an older build is expected after
  #1598 (C6). The ruling's other accepted residuals are `reliability`'s silent omission and, on each
  machine that has run #1598, every pre-#1598 build's own `stats` and compaction stopping for good,
  the fence's accepted cost. The store's compaction resumes as soon as a #1598+ build runs again,
  while the older binary's own compaction never does. Loud is the better failure.
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
- **The panel's Stats tab** renders `account.session.mean/peak` through `statsSessionMeanPeak` (`apps/menubar/Sources/StatusPanelFormat.swift:2332`; the cell at `apps/menubar/Sources/StatusPanelStats.swift:197`). A `nil` session dimension there is an
  unauthored state on a panel surface — the register rule reaches it. **D15** (design-found): D0's
  classification should name the treatment for this cell and for the row's spoken summary (`accessibilityLabel` at `apps/menubar/Sources/StatusPanelStats.swift:216-224` reads "Session mean N percent, peak N percent" and the cap-hit count) too; until it does, the Swift half of B-8
  decodes `nil` without crashing and the cell's rendering is B-11's (§ 9).

### B-9 — the warm-up design reads the fraction (S9)

`docs/requirements/session-warmup-on-reset.md` (§ 3 at `docs/requirements/session-warmup-on-reset.md:95`;
R-2 at `docs/requirements/session-warmup-on-reset.md:122-124`) and
`docs/design/session-warmup-on-reset-solution-design.md` (C-1 at
`docs/design/session-warmup-on-reset-solution-design.md:45`; the predicate
`session_resets_at == None … clock stopped` at `docs/design/session-warmup-on-reset-solution-design.md:57`;
its own R-3 at `docs/design/session-warmup-on-reset-solution-design.md:90`) are amended in place so
C-1 reads two states instead of one: `session.is_none()` is **no window, nothing to warm** (R-2 never
matches); `session.is_some() && session_resets_at.is_none()` is the **stopped-clock state** R-1's
spike (#1231, closed GO on 2026-08-11; its AC-3 confirmed the stamp-versus-zero split) already
established. No shipped code changes; R-1's spike result is untouched (`REQ-REC-B-001`).

### B-10 — the records say "every dimension the account has" (S10)

In-place amendments on the repo's own practice — keep the original `date:`, add an
`**Amended 2026-11-… (#NNN)**` paragraph (the `**Amended 2026-07-21 (#665)**` form ADR-0013 already
carries), update the index row in `docs/adr/README.md` to the `Accepted (amended … by #NNN — …)` form
its rows 0006, 0012 and 0020 already use:

| Record | Passage | Amendment sentence (draft, operator's to ratify) |
|---|---|---|
| ADR-0013 | "Sound because **both** windows hard-reset" (`docs/adr/0013-session-floor-default-on-reserve-emergency-exempt.md:134`) | "…sound because every window the account has hard-resets; an account with no session window has no session floor to reserve against, and `pick_target`'s session gates pass it vacuously." |
| ADR-0017 | the gate's constants `T=300s` / `risk_band=60%` (`docs/adr/0017-bounded-blindness-preemptive-swap-not-header-observation.md:22`); "Never fires on a `None`/missing reading. It keys off the retained `last_good`" (`docs/adr/0017-bounded-blindness-preemptive-swap-not-header-observation.md:189`) | "…an anchor whose session window does not apply is such a `None`: the gate reads false, `status` claims no auto-protection on its strength, and whether a weekly anchor should arm the swap is an open amendment (OAD-2)." |
| ADR-0025 | "at least as strict as the negation of the release predicate on BOTH dimensions" (`docs/adr/0025-weekly-trigger-ceiling-semantics.md:48-50`) | "…on every dimension the account has; a dimension the account lacks is neither released nor acquired on." |
| ADR-0033 | "The census reports what it observed and refuses to interpolate" (`docs/adr/0033-weekly-saturated-session-low-stays-unknown.md:54-58`) | D5: "A structurally absent session window is excluded from the session census — it is not an unobserved reading, and the census is over the accounts that have the window." |
| Parser doc comment | `UsageParse` at `src/usage.rs:473-474` | "Each window is independently optional: a window the reply does not carry is reported as `None`, never a fabricated `0`; a reply carrying no window at all is a hard `Error::UsageParse` (D1), so the loop skips, not swaps." |
| Private HQ `REQ-USG-B-001` | "both dimensions" (quoted in PRD § 2.4) | **owed by the operator**, recorded as owed in PRD § 2.4 / § 7 and here (`REQ-REC-B-004`); not reachable from a clone |

Ordering (`REQ-REC-C-001`): the ADR and parser-doc amendments land with or before the commit that
moves `a_missing_dimension_is_a_parse_error_not_a_fabricated_zero` (`src/usage.rs:959`).

### B-11 — the render cells, as a waiting item (S11; blocked on D0)

Specified in § 9. Nothing is built until a register entry exists.

### B-12 — deployment and live verification (S12)

Amend `docs/runbooks/replacing-the-running-daemon.md`, never add a second runbook: under
*§ Prerequisites* add the **no-older-daemon rule** — from the store's first `"session":null` line, no
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
2. **Read a weekly-only account's row** — `status` shows its weekly percentage and reset; the session cell shows `n/a` transitionally (B-6t; `0%` under tranche 0's hot-fix until tranche 1 deploys, § 1b), then the D0 token (B-11). `status --json` carries `absent_windows` after B-6. Discriminator during the transition: a failed poll nulls *both* windows, so a row reading `n/a | n/a | 90% | 5d` — an uncoloured Session meter beside a coloured Weekly meter — cannot occur today, and the weekly cell tells no-window from no-reading before B-6 lands. Its limit is the error path: a weekly-only active that goes blind (B-6t row 5) is indistinguishable from a failed-poll row on both surfaces until S6.
3. **Switch to a weekly-only account by hand** — `sessiometer use <label>` without `--force` returns the weekly verdict (B-3 row 10); no `UsageParse` abort. Error path: a weekly-exhausted account returns `WeeklyExhausted` instead of switching, exactly as a two-window account does, and `--force` overrides it as today (`REQ-ARM-B-009`).
4. **Read the record** — `stats` shows the account's weekly figures and `—` for its session figures (B-8); `stats --json` omits its `session` block under schema 2. Error paths: an older `stats` run against a store carrying `null` lines refuses the fenced rollup loudly, and an older `reliability` leaves the lines out of its report — neither deletes anything; only an older daemon's compaction erases, which the runbook's no-older-daemon rule and the rollup fence exist for (B-7, D6); and if OAD-3 resolves to "follows", `stats --json` keeps carrying a zero `session` block for such an account until B-8's wire half lands.

### 6b. System interactions

```
poll ─► parse_usage ─► Ok(UsageReport{session:None, weekly:Some(0.90), …})     [D1 (a): neither → Err(UsageParse)]
     ─► classify_poll → Live ─► last_reading = Some(usage); last_good (active) = Some(LastGood{session:None,…})
     ─► append_sample_for_poll → {"ts":…,"session":null,"weekly":0.9,"weekly_resets_at":…}
     ─► tick: decide (weekly axis only) · velocity_swap (held) · note_exhausted_poll (weekly line) · blind gate (false)
     ─► snapshot: session_pct:null · weekly_pct:90 · [absent_windows:["session"] after B-6] · blind_active per B-6/B-6t
     ─► watch frame / status reply ─► CLI cells (B-6t → B-11) · Swift decode (B-6t: no edit; B-6: the key read, `UInt8?`) · render (D0)
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
| `status` reply + both `watch` frames | `absent_windows: [WindowKind]`, omitted when empty; `blind_active.last_known_session_pct` optional, omitted when absent | `STATUS_SCHEMA_VERSION` minor → next free (queue rule) | five goldens regenerated + two new rows; 27 + 14 + 1 Swift fixture sites; Swift decode (`decodeIfPresent`); CLI consumers of the reshaped field |
| `status` reply, tranche 0 (hot-fix #1613, `1.14`) | `session_pct: 0` beside present weekly fields for a served weekly-only reading, with no session reset stamp; no sample line is written for it (§ 1b) | none | none — replaced by the transitional row below at tranche 1's deploy |
| `status` reply, transitional (`1.14`) | `session_pct: null` beside present weekly fields for a served weekly-only reading; no `blind_active` block for a weekly-only blind active; no `recent_blind_preempt_swap` notice for a `Retry-After` swap off a session-less anchor; doc comment amended | none | none — retired by the row above |
| `stats --json` | `accounts[*].session`, `band`, `cap_hits` and `time_at_cap_secs` omittable (AD-13); in the same bump #1601 also makes `accounts[*].weekly` omittable (the weekly symmetry) and `velocity.session_pct_per_min` nullable, so a weekly-only account's weekly rate and runway reach the wire — both widenings pipeline-recommended, ratification-pending | `JSON_SCHEMA_VERSION` 1 → 2 | `wire-stats-basic.json` regenerated; the Swift `StatsWire` mirror optional for every omittable key; `statsBasic` fixture at schema 2 |
| sample line `usage-samples.jsonl` | `"session":null` / `"weekly":null` written; the key never omitted | none (versionless) | readers tolerant; deploy order (B-12) |
| rollup tiers `usage-rollup.json` | per-axis `HourStat`/`DayStat` omitted when the bucket has no reading on that axis; every present `HourStat`/`DayStat` records `samples: Option<u32>`, the number of samples it covers (D13 as ruled) | store generation 1 → 2 under #1598's `ROLLUP_FORMAT`: `usage-rollup.json` is written as `{"format":2,"rollup":{…}}`; a file with no `format` key reads as the legacy generation 1; every pre-#1598 build refuses the envelope before it rewrites either store file | deploy order (B-12) |
| event log lines | per-window tokens omitted when the window is absent | none for `log` (`JSON_SCHEMA_VERSION` at `src/log.rs:167`); `reliability` (`JSON_SCHEMA_VERSION` at `src/reliability.rs:264`) moves 13 → 14 for the census's `n_without_session_window` | `reliability` parsers tolerate the absent token; the census keeps its exact partition |
| `log --json` | unchanged: records carry the line's own tokens (`LogWire` at `src/log.rs:865`), so an omitted token reshapes nothing | none (`JSON_SCHEMA_VERSION` at `src/log.rs:167` stays 2) | — |
| `reliability --json` | the blind-projection block gains `n_without_session_window` beside `n_malformed`, an always-present key, so the census keeps its exact #484 partition | `JSON_SCHEMA_VERSION` 13 → 14 (`src/reliability.rs:264`) | the three `build/fixtures/cli-renders/reliability-*.txt` goldens regenerated; `RELIABILITY_USAGE`'s `schema:` token; no Swift surface |
| `/api/oauth/usage` | unchanged in kind and rate; no second endpoint | — | the sole-egress oracle stays green |

## 9. UX Architecture

The UX/IA track ran in generative mode for the one surface the PRD touches — the no-session-window
account on the panel row and the CLI status cells — and returned **blocked on D0** with its
inventory, state coverage and options complete.

### 9a. Information architecture

| Surface | Element | Today | Under this design |
|---|---|---|---|
| Panel account row | the Session meter (`UsageMeter(label: "Session", …)` at `apps/menubar/Sources/StatusPanelRoster.swift:443`; `AccountRow` at `apps/menubar/Sources/HonestStateMachine.swift:361`) | every one of the mock's 98 rows carries one; `nil` percent renders uncoloured `n/a` (a failed poll) | renders the state D0 classifies; until then the transitional `n/a` (B-6t) |
| Panel account row, blind held block | the `BlindMeter` held bar, band and percent text, which read `lastKnownSessionPct` as a value (`apps/menubar/Sources/StatusPanelRoster.swift:911-924`), and the row's spoken phrase "last-known session N percent" (`apps/menubar/Sources/StatusPanelFormat.swift:2197`) | every mock blind frame carries a percent; the row's blind treatment keys solely on `blindActive` (`apps/menubar/Sources/HonestStateMachine.swift:386`) | S6 makes the percent optional (B-6), so a weekly-only blind active is a panel state the mock never authored — the fifth RenderCell, classified with D0 (D15); S6's panel render half waits on D0 exactly as S8's does, its decode half (`UInt8?`) does not |
| CLI `status` table | SESSION% and RESET cells (`pct` at `src/cli.rs:4595-4598`; `reset_cell` at `src/cli.rs:4365-4368`) | `n/a` for a failed poll, pinned by the nine `status-*` render goldens under `build/fixtures/cli-renders/` that carry the token | the token D0 names for the same state; `n/a` transitionally |
| Panel Stats tab | the per-account `StatCell(label: "Session m/pk", …)` (`apps/menubar/Sources/StatusPanelStats.swift:197`) and the row's spoken summary "Session mean N percent, peak N percent … cap hits" (`accessibilityLabel` at `apps/menubar/Sources/StatusPanelStats.swift:216-224`) | always a figure | D15 — classified with D0, cell and spoken summary alike |
| CLI `stats` table | the session columns, and the SIGNAL, velocity, runway and cap columns keyed on the session series (`signal_cell` at `src/stats.rs:2498-2504`; the `cap_hits` and `time_at_cap_secs` columns at `src/stats.rs:3509-3527`) | figures | the surface's own `—` UNKNOWN sentinel (B-8) — reuse, not invention; the table then does not distinguish "no window" from "never observed" (only the wire does; G-4 holds on the wire) |

The state matrices of PRD § 11 are the state coverage for these views. This design adds one state the PRD's matrices did not carry — the blind held block of a weekly-only blind active, forced on the panel by S6's reshape — and routes it to D0 under D15 rather than rendering it. No new navigation, control or surface exists, and no task flow changes — the operator reads a cell that today misreports and tomorrow reports.

### 9b. The waiting item's specification (what S11 builds once D0 lands)

| D0 option | Panel Session meter | CLI SESSION% / RESET | Cost and gates |
|---|---|---|---|
| (i) **decided to be nothing** — the expiry line's `—` OBSERVED-ABSENCE token, which the mock already says "is deliberately NOT `n/a`" (`apps/menubar/design/menubar-preview.html:1121-1126`), and which the CLI `status` table already spends on the same meaning as `EXPIRY_GAP`, "DELIBERATELY not the `n/a`" of a failed poll (`src/cli.rs:4372-4385`) | `—` in the percent slot, the track empty, and the per-meter reset label (`UsageMeter` … `reset:` at `apps/menubar/Sources/StatusPanelRoster.swift:443-446`) empty rather than today's `n/a`; the blind held block likewise — bar empty, `—` in its percent slot, spoken phrase without a percent; register row under *Decided to be nothing* naming this reasoning; row geometry unchanged | `—` / `—` — the same table's own `EXPIRY_GAP` precedent, reuse not invention | one register entry; a new panel fixture + an **armed** `panel-goldens` run (`TEST_RUNNER_SESSIOMETER_PANEL_GOLDEN_GATE=1`) + `Panel-Goldens-Rebaselined:` trailer; a new CLI render fixture + `emit_cli_render_goldens_status` (`src/cli.rs:17139`) + `CLI-Goldens-Rebaselined:` trailer |
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

No new control. Three spoken strings change meaning and are **a Gap handed to D0** (§ 9b), recorded here so the register entry carries them: the label of an absent Session meter; the blind held block's "last-known session N percent" (`lastKnownSessionPct` at `apps/menubar/Sources/StatusPanelFormat.swift:2197`) when there is no percent; and the Stats-tab row summary's "Session mean N percent, peak N percent" (`accessibilityLabel` at `apps/menubar/Sources/StatusPanelStats.swift:216-224`).

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
| Cap-9 | the render cells show the classified state | B-11 | `REQ-CEL-C-001`, `REQ-CEL-B-001` — **blocked on D0** |
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
no property-based tool.

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

Each decision is **ratification-pending** (the design is provisional). Rows marked **author-chosen**
had no pipeline recommendation in the PRD and are the ones to read first.

| ID | Decision | Rationale | Alternatives considered | Flag |
|---|---|---|---|---|
| AD-1 | Per-window `Option<f64>` on `UsageReport`, `Usage`, `LastGood`, `BlindAnchor`; `Copy` kept on the three that carry it (`UsageReport` is `Clone`) | the compiler enumerates every consumer; no flag can be forgotten; no sentinel can be mistaken for an idle window (§ 4.1) | a flag beside `f64` (rejected: 93 reads to audit); `Option<Window{fraction, resets_at, severity}>` per window (cleaner, but reshapes every `Usage` literal and the `Copy` decision type on the urgent path — deferred as a later refactor, § 12b, tracked as #1605) | pipeline-recommended |
| AD-2 | `dimension` unchanged; `parse_usage` composes per-window Options; D1 (a) error only when neither window; D2 identical | the smallest parser change that is also the model change; D1 (a) keeps a 200-with-nothing on the skip path | D1 (b) `Ok` with both `None` (one line + Oracle C); D2 different (a second fixture and a documented reason, for a difference no code reads) | pipeline defaults |
| AD-3 | Invariants I-1 (stamp ⇒ fraction) and I-2 (a served reading has ≥ 1 window under D1 (a)), each pinned by a test | consumers may read a stamp without re-checking the fraction; the high-water fold's second `?` is unreachable alone | state I-1 in a doc comment only (rejected: a later `dimension` edit could break the tuple with nothing red); drop I-2 (rejected: D1 (a) is what makes it hold, and the test is what would catch D1 (b) landing unnoticed) | design-derived |
| AD-4 | Four named moves per consumer; numeric default forbidden at every site including tests (B-1 … B-12; tranche 0 is the one bounded exception, § 1b) | P-1 and P-11 are the failure modes a grep cannot catch; a per-arm `None` test is the instrument | a source-lint test over `unwrap_or(0.0)` (rejected: false positives on legitimate non-window floats) | design-derived |
| AD-4b | The weekly-less arms, by symmetry: a weekly-less account is never weekly-blocked (B-3 row 1), `use` returns `Viable` (row 10), it is never `weekly_exhausted` (row 13) | compiler-forced once `Usage.weekly` is `Option` (S2, message 2's future case); extends `REQ-ARM-B-012`'s symmetry to an account class with no observable instance today | refuse a weekly-less reading at the parser (rejected: contradicts message 2 and `REQ-RPL-B-002`); a new `Unknown` verdict for `use` (a rule nobody ratified) | **author-chosen** — surfaced for ratification; no instance exists to measure against |
| AD-5 | `blind_gate_armed` and `plausible_anchor_session` take `Option<f64>`; false / `None` on an absent anchor; the single-source rule survives | the predicate's two callers must not drift (the gate's own comment), and `blind_swap`'s own comparison against the config thresholds reads false on `None` with them; D3's surfacing half | caller-side skips at each site (rejected: the drift the gate was built to prevent) | pipeline default (D3 surfacing) |
| AD-6 | Transitional `1.14` state (D10): `session_pct: null` beside present weekly fields; doc comment amended; **no `blind_active` block for a weekly-only blind active**; a signpost at the end of tranche 1's budget (AD-23) | tranche 1 deploys days sooner (the fleet's weekly readings already recover at tranche 0); a `0` in `last_known_session_pct` would be the fabrication; the cost is stated in full in B-6t row 5 (indistinguishable from a failed-poll row on both surfaces, duration and percent lost, report arms silent) and bounded by two facts — no regression against the bundle that ran before tranche 0, which could not read such an account at all (the tranche-0 hot-fix, serving since 2026-11-01, does read it and holds its anchor's session as `0.0`), and the cost lands on the operator's active account | S6 in tranche 1 (no misreport; the 1.15 queue and 27 + 14 + 1 fixture edits on the urgent path) | **author-chosen sub-default** (the omitted block); ratify D10 and the signpost together |
| AD-7 | `absent_windows: [WindowKind]`, omitted when empty, emitted only from a served reading | one key for both windows; the omit-when-absent precedents; bytes distinguish no-window from no-reading | boolean `no_session_window`; per-window enum `session_window: "absent"` (two keys, two bumps) | **author-chosen (D11)** |
| AD-8 | `BlindActive.last_known_session_pct: Option<u8>` omitted when absent | the `rotated` reshape precedent; a weekly-only blind active keeps `blind_secs` and `auto_protection_degraded` | drop the block for such an account permanently (rejected: loses the `Retry-After` and velocity report arms for good; B-6t accepts that only transitionally) | pipeline-recommended |
| AD-9 | The next free minor at landing under the queue rule; goldens regenerated once; two new golden rows; the `"minor":15` future frame moves one ahead | `REQ-STL-C-001`; #1588 and three issues claim 15 | pin 1.15 now (rejected: a second `minor: 15` on `main` is premortem P-3) | pipeline-recommended |
| AD-10 | `Sample.session` **and** `Sample.weekly` → `Option<f64>`, `#[serde(default)]`, key always written as `null` | gap honesty reserves "no line" for a failed poll; #1580's key-set oracle; the weekly axis is compiler-forced by S2 | omit the key (RED on #1580); a versioned sample line (rejected by the D6/D13 council of 2026-11-02 as well: no shipped build checks a version, so an older daemon skips and erases a versioned line exactly as a `null` one); a number plus an "absent" marker (an older daemon's rewrite strips the marker and leaves a fabricated reading); a new sample-file name (splits the store's history) | **author-chosen (D12, the weekly half)**; the session half ratified by council on 2026-11-02, amended with the rollup fence (B-7) |
| AD-11 | Rollup tiers: per-axis `HourStat`/`DayStat` → `Option`, omitted when no reading on that axis; counts stay counts; an older `stats` fails loudly on such a rollup, an older daemon discards the compaction error and stops compacting in silence (the ruling's C7 adds an `eprintln!`). **Amended by the D6/D13 council of 2026-11-02**: every present `HourStat`/`DayStat` also records `samples: Option<u32>`, the number of samples it covers, set at fold time; the same-key merge weights each axis by it when both operands carry one; #1610 is absorbed into #1598 | `max_of` over an empty slice is `-∞`, which `serde_json` writes as `null` and the next `read_rollup` then refuses with `UsageRollupMalformed` (P-12); no production reader of the per-axis stats exists to move; the per-axis subset's size exists only at fold time, because a folded day's raw samples are deleted | `HourStat` with `Option` fields (same skew, uglier; `cap_hits` ambiguous between 0 and unknown); an explicit `null` in buckets (two spellings of "no reading" in the lifetime tier); a count without the per-axis `Option` (a statistic over zero samples must invent a `max` and a `mean`) | **settled by council (D13)**, default ratified and amended |
| AD-12 | The canary's `null` decode arm moves to `Ok(None)`; its non-finite decode arms stay refused; its encode arm moves to assert that a non-finite `Some` is refused at serialisation by a field serializer, because `serde_json` writes a non-finite `f64` as `null`, which would read back as "no window"; the round-trip test gains the `null` case for both axes | `REQ-SMP-C-001`; `null` on disk keeps one meaning | delete the canary (forbidden by the requirement's BUT NOT); accept `null` for a non-finite `Some` and assert `None` on the read side (rejected: two meanings for `null` on disk; #1598 states it as the alternative) | pipeline-recommended |
| AD-13 | `stats`: session figures over present samples; census excludes (D5); wire `session`, `band`, `cap_hits` and `time_at_cap_secs` omittable under schema 2; Swift optional; CLI table reuses `—` for the session, signal, velocity, runway and cap columns; panel Stats tab (cell and spoken summary) waits on D15/D0. Amended 2026-11-01: #1601 widens the field set in the same bump — `weekly` omittable (symmetry with `Sample.weekly`, D12) and `velocity.session_pct_per_min` nullable (the velocity object is emitted when either axis has a rate) — ratification-pending | a zero series rendered as a reading is the fabrication on a third surface, and a `0` cap count reads as "never hit its session cap" for an account with no cap (`MET-T-003`); the reshape is a bump by the repo's own rule | a sibling marker beside always-present zeros (rejected: still renders zeros); exclusion with no wire change (impossible: the block must hold something); counts kept at `0` (rejected after the product lens: a fabricated value on a surface) | **author-chosen field set; timing is OAD-3** |
| AD-14 | Event fields typed absence, omitted token on the line; `reliability` parsers tolerant; no `log` version move; `reliability` moves 13 → 14 for the census's no-session-window bucket (the #484 partition kept; no-bucket arm rejected) | `REQ-RDG-C-001`; the `session_high_water_pct` absent-token precedent | emit `session_pct=0` (the fabrication); skip the event (loses the weekly half) | **author-chosen (D14)** |
| AD-15 | `use` serves the weekly verdict; a weekly-less reading is `Viable` | `REQ-ARM-B-009`; no line to exhaust | abort as today (the symptom); a new `Unknown` verdict for a weekly-less reading (AD-4b's alternative, rejected: a rule nobody ratified) | pipeline-recommended |
| AD-16 | Velocity: per-axis `Option<i16>` deltas; EMA fold skips an absent axis; tie-break reads an absent EMA as `0.0` (D7) | the "safest landing" rule already in `velocity_rate`; `MET-R-001` watches it | a neutral rule for absent EMAs (a new rule nobody ratified) | pipeline default (D7) |
| AD-17 | ADR-0033 census excludes a structurally absent window (D5) | an absent window is not an unobserved reading | hold the census UNKNOWN whenever a weekly-only account is present (blunter; hides the roster's real session census) | pipeline default (D5) |
| AD-18 | Oracles A–D as named in B-5, `#[ignore]`d, RED-proven by a filtered `--ignored` run | the repo's RED-oracle rule; `--include-ignored` rewrites a frozen fixture | land un-ignored (cannot merge); prove RED by a recorded run with no committed test (rejected: nothing executable then grades the fix); land the oracles inside the fix pull request (rejected: the executor could edit them) | pipeline-recommended |
| AD-19 | In-place amendments (B-10), README rows updated, HQ amendment recorded as owed — the ADR amendments before the pinned test moves (#1593, its own pull request); the parser doc rewritten in the commit that moves the test, which pins that very contract (B-10's ordering rule: with or before) | the repo's ADR practice; `REQ-REC-C-001` | a new ADR superseding four (rejected: the outcomes do not change, only their premise's wording — amend in place) | pipeline-recommended |
| AD-20 | Warm-up design C-1 reads two states from the fraction and the stamp (B-9) | contradiction 3 of the investigation; P-4 | leave #1232's design and fix at build time (rejected: the design is where its predicate lives; the feature is unbuilt) | design-derived form |
| AD-21 | Runbook amended with the both-halves rule and the fleet recovery check (B-12); the text is #1603, tranche 2, after the deploy (AD-23) | `REQ-DEP-B-001`, `C-001`, `B-002` | a second runbook (rejected: the seed's step already exists as this runbook) | design-derived commands |
| AD-22 | PR composition per § 4.6: this design first and #1537 rebases (D9 stays its own); S6 takes the next free minor relative to #1588; #1538 composes | read live 2026-11-01; PRD R1 says coordinate, never re-scope | absorb #1537 (R1 grows; D9 is not this design's to decide) | **author-chosen** — the landing order of another author's open PR is the operator's call; this row recommends it and does not order it |
| AD-23 | **Tranche 0 first, outside the twelve blocks: the hot-fix #1613** (§ 1b, § 4.4). Then the tranche order is the cut order reversed: B-5 → B-4 (which reads B-5's bodies by name) ‖ B-10 records (#1593) → B-1+B-2+B-3 (the compile unit, carrying B-10's parser doc) → B-7 with B-8's reader half (#1598 with #1600) → B-6t → B-12 live half (#1604) ‖ B-6 → B-8 → B-9 ‖ B-11, with B-12's runbook text (#1603) in tranche 2, after the deploy. Tranche 1's budget is the human effort-day estimate the coverage gate sized (PRD § 3.1), no longer one calendar week. #1603 moves from Must (tranche 1) to Should (tranche 2); #1604 follows #1603's text where it has merged and the runbook's current text otherwise | The maintainer's decisions of 2026-11-01. Asked whether tranche 1 can stay a small batch inside one calendar week when the coverage gate sized it at a much larger human effort-day estimate, the maintainer answered: "Bigger budget, but we need a known path to have a hot-fix for the issue as rugiht now sessiometer is unsuable". Then: "The hotfix must happen ASAP and local daemon rebuilt-restarted so that you can continue working". Asked whether the runbook text #1603 must gate the tranche-1 deploy #1604: "Yes, after deploy (Recommended)". The compile unit cannot be split; the store (B-7, with B-8's reader half) lands in the next pull request by the pipeline's choice, because the compile unit keeps #1613's sample skip re-keyed on a `None` fraction and so compiles alone, and no running daemon carries that interim; B-10's records half lands first so the record precedes the moved test; with the hot-fix deployed, weekly readings recover at tranche 0 rather than at tranche 1's deploy | ship the fabricating shortcut first (never, § 1b) — except tranche 0's narrowed, bounded form, which the maintainer chose; keep tranche 1 inside one calendar week (set aside by the maintainer's answer); #1603 as a Must in tranche 1, landing before the deploy (the earlier placement) | **maintainer-decided** for tranche 0, the budget and #1603's placement; the cut order itself pipeline-recommended |
| AD-24 | S11 specified as a waiting item with three options and its gates (§ 9b); D15 extends D0 to the Stats-tab cell, its spoken summary and the blind held block | the register rule | pick the `—` token now (forbidden: inventing against the register); build S11 against option (i) and re-baseline if D0 picks (ii) (rejected: a committed golden consumed by the parity gate turns OAD-1's two-way door one-way) | design-derived |
| AD-25 | Platform-neutral; no `cfg`; Linux un-gated and said so | § 4.7 | a `cfg(target_os)` guard on the parse path (rejected: nothing target-specific exists to guard, and the guard would be the first); a Linux CI job inside this scope (rejected: #964 owns it) | design-derived |

**Reversibility of the decisions above.** Every AD row is a two-way door until the first deploy that writes or serves its shape, and most stay two-way after it. A code-only decision (AD-1 … AD-5, AD-4b, AD-15 … AD-20, AD-24, AD-25) reverses by a code change with no data to migrate. A wire decision (AD-7, AD-8, AD-9, AD-13, AD-14) reverses by a further bump of its own wire with the same lockstep, so its cost is one more minor or schema and one more fixture sweep. A store decision (AD-10, AD-11, AD-12) reverses for lines not yet written and for every line and bucket already written, because a later reader reads tranche-1 lines and buckets unchanged — the D6/D13 council of 2026-11-02 found that "a ruling for another shape needs a migration of data already written" is false for both decisions — and what a deploy makes irreversible is only information never recorded (the per-axis `samples` count, which is why the ruling records it) and lines an older daemon erased. D1 (b), D10's transitional state, OAD-1 and OAD-2 state their own reversibility in B-1, B-6t and the stubs below. AD-21, AD-22 and AD-23 are procedure and order, reversed by a later decision at no code cost.

### Open architecture decision stubs (context and options filled; decision left open)

**OAD-1 — `render-no-window-classification` (D0 + D15).** *Context*: the no-session-window account
has no authored state on the panel row (the Session meter, and — once S6 makes `last_known_session_pct` optional — the blind held block's bar, percent slot, caption and spoken phrase), the panel's Stats tab (the cell and the row's spoken summary), or the CLI `status` cells; the register
rule forbids inventing one. *Options*: (i) decided to be nothing — the `—` OBSERVED-ABSENCE token;
(ii) as authored — a mock frame; (iii) genuinely open. *Evidence that discriminates*: whether the
maintainer wants the row to read as "one window, by design" (i) or as a distinct account kind (ii).
*Reversibility*: a register row and a fixture — a two-way door until a golden is committed and
consumed. *Decision*: **open — maintainer**. *Owner of the row*: the maintainer who ratifies mock
frames. S11, the panel render half of S6 and the Swift render half of S8 wait on it; S6's and S8's decode halves do not.

**OAD-2 — `blind-gate-weekly-only-active` (D3, the arming half).** *Context*: ADR-0017's preemptive
swap keys on the retained session anchor against `BLIND_GATE_RISK_BAND` (`src/daemon.rs:401`); a
weekly-only active has no such anchor, so it is unprotected while blind. *Options*: (a) leave it
unprotected and say so (this design's surfacing half); (b) arm on a weekly anchor against a weekly
band — a new constant, a new ADR-0017 amendment, and a swap that fires on the slower-moving axis.
*Evidence that discriminates*: a weekly-only active observed blind past `BLIND_GATE_SECS` while
climbing on the weekly axis (event evidence `MET-R-003` would surface). *Reversibility*: (b) adds a
constant and an arm — reversible by removing both. *Decision*: **open — operator**, outside this fix.

**OAD-3 — `stats-wire-rides-first-or-follows` (D8).** *Context*: B-8's exclusion in `usage_stats` is
compiler-forced into tranche 1; the wire half (schema 2, Swift optional, one golden) is not. Until it
lands, `stats --json` carries a zero `session` block for a weekly-only account — a fabricated reading
on a wire. *Options*: ride first (tranche 1 grows by roughly half a day: one Rust reshape, one Swift
optional, one fixture, one golden; no queue contention — the `stats` version is independent of the
status minor); follow (accept the interim wire defect knowingly). *Recommendation*: **ride first** —
the cost is small and the defect is of the class the PRD calls "never". *Note*: riding first puts the first Swift edit and a `stats` schema bump on the urgent path — the class of work D10's own reasoning kept off it for S6 — so the operator ratifies D8 and OAD-3 as one question, not two. *Decision*: **open — operator**.

## 12b. Alternatives Considered and Drawbacks

| Rejected approach | Why rejected |
|---|---|
| Fabricate `session = 0.0` as a parser-only urgent patch | lies on every measurement surface and writes an irreversible `"session":0.0` to the store; byte-identical to an idle window; the investigation's Q1 verdict and the PRD's § 3.3. Rejected for tranche 1 and after; the maintainer's decision of 2026-11-01 accepts a narrowed form as tranche 0 only (#1613) — a private flag marks the reading, no sample is stored for it, and tranche 1 removes both (§ 1b); the wider narrowing — consulting the flag at the wire and the event emitters too — was weighed and rejected, because each consultation is a hand-audited read on the urgent path while the store is the only irreversible sink (PRD § 3.3) |
| `session = weekly` | excludes a weekly-only spare at the viability floor; fires swaps attributed `reason=Session` |
| A flag beside an unchanged `f64` | must be consulted at every read or the fabricated value stands — the cost tranche 0's hot-fix accepts for a bounded time (§ 1b) |
| Reuse `session_pct: null` for "no window" | the contract defines `null` as "poll failed"; the Swift decoder documents the same; a 1.14 app renders it as the blind-style `n/a`; a rule the contract does not state |
| Read `/api/oauth/profile` for the plan name | the plan does not name the per-account window state; opens a second egress endpoint behind #737's gate |
| `Option<Window>` per window on `Usage` | the better end state, but it reshapes every `Usage` literal and the `Copy` decision type on the urgent path — recorded as a follow-up refactor (#1605), never a prerequisite |
| A versioned sample line | a larger change than the versionless `null`, and it fences nothing: no shipped build checks a version, so an older daemon skips and erases a versioned line exactly as a `null` one (the D6/D13 council's measurement, 2026-11-02); D6 accepts the skew residual and the rollup fence instead |
| Hold ADR-0033's census UNKNOWN whenever a weekly-only account is present | hides the real session census of the accounts that have the window |
| A source lint over numeric defaults | false positives; the per-arm `None` test is the instrument |

**Known drawbacks of the approach taken**: the hot-fix's bounded fabrication until tranche 1 deploys (§ 1b); the transitional days during which a healthy no-window
account's session cell reads like a failed poll and its blind report arms are silent (B-6t); the
version-skew residual an older daemon carries (D6, settled by council with a rollup fence); a larger test diff (125 + 120 sites) whose
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
| Intake, track selection, per-track summaries, enrichment, open-question handling | pipeline-decided at every stop of the design-solution command, each with the default it took: intake — the PRD accepted as input despite `dor_status: failed`, acknowledged on the operator's behalf; track selection — Technical Architecture, UX/IA in generative mode and Testing Architecture, with API Design and Data Architecture folded into Technical (§ 8); the per-track summaries — accepted as written; enrichment — the product and UX lenses dispatched and their concerns applied (below); open-question handling — D11–D15 added, each built for its author-chosen default; the premortem — de-anchored, DP-1 to DP-6; the lock — left NOT LOCKED. The run's own record is machine-local scratch, not committed and not a citation; this row is the committed record of the stops |
| Product lens (`product-strategist` agent) | **APPROVE WITH CONCERNS** — "The design builds exactly the class and shape the operator's two messages ask for, covers all 46 PRD requirements (verified by ID set, not taken from the matrix), ships nothing the non-goals forbid, makes every scope extension compiler-forced and flagged, and returns the one appetite change (OAD-3) to the PRD; the concerns are about what the operator must see beside the author-chosen rows before ratifying, and none changes the work items Stage 3 would write." Six non-blocking concerns, all applied in this revision (below) |
| UX lens (`ux-architect` agent) | **BLOCKING** on the first pass — "The structure, flows and D0 routing are sound and invent nothing on the panel, but § 9a's inventory misses one state S6 forces on the panel at compile time — the blind held block for a weekly-only blind active — so Stage 3 would scope S6 with an unclassified render decision inside it, the exact failure REQ-CEL-C-001 and the register rule exist to prevent; the fix is a row in § 9a and a sentence in OAD-1, and tranche 1 is unaffected." Revised as prescribed (§ 9a fifth cell, § 15, OAD-1, B-6, § 11); seven non-blocking concerns applied; re-dispatched once — second pass **APPROVE WITH CONCERNS**: "The blocking concern is resolved — § 9a now inventories the blind held block as the fifth RenderCell, § 15, OAD-1, AD-24, B-6 and § 16 route it to D0 under D15, and § 11 hands its spoken phrase over — so every panel state S6 forces is classified-pending rather than unrouted; one narrower gap remains in how S6's 'decode half lands, render half waits' split behaves while D0 is open, and it is a sentence to add, not a decision to make." That one non-blocking concern is applied (B-6 interim, § 9b (iii), DP-1, R-D9); all seven first-pass concerns confirmed addressed |
| Maintainer decisions of 2026-11-01 | Three, quoted verbatim in AD-23: tranche 1's budget re-set and a hot-fix added as tranche 0 (#1613); that hot-fix to happen as soon as possible, with the local daemon rebuilt and restarted; the runbook text (#1603) moved to tranche 2, after the deploy. **Decided by the maintainer, not ratification-pending**; every other decision of this design stays pending |
| Lock | **NOT LOCKED** — load-bearing questions stand (D0–D5, D7–D12, D14, D15; D6 and D13 were settled by council on 2026-11-02); the brief carries `status: draft` |

### Lens concerns and their disposition

**Product lens** (six, all non-blocking). (1) AD-6 / B-6t — the omitted block is no regression against the bundle that ran before tranche 0, and the interim cost lands on the operator's active account → both facts in B-6t row 5 and AD-6; R-D5 raised to MEDIUM. (2) B-6t / AD-23 — name the signpost when tranche 1's week passes (re-based to the end of tranche 1's budget by the 2026-11-01 decision, AD-23) → B-6t row 5, AD-6, R-D5. (3) AD-13 — `cap_hits` / `time_at_cap_secs` and the `caps` / `t@cap` columns would read `0` for an account with no session cap → both take the absence treatment (AD-13, B-8). (4) AD-22 — the landing order of #1537 is the operator's call → flagged author-chosen. (5) B-3 rows 1, 10, 13 — the weekly-less arms are design-derived by symmetry → AD-4b, flagged. (6) OAD-3 — riding first puts the first Swift edit and a `stats` bump on the urgent path → noted in OAD-3; D8 and OAD-3 ratified as one question.

**UX lens** (one blocking, seven non-blocking). (B) § 9a misses the blind held block S6 forces on the panel → the fifth RenderCell in § 9a and § 15, OAD-1 names it beside D15, B-6 states that S6's panel render half waits on D0 while its decode half does not. (1) the Stats-tab spoken row summary needs the same classification → D15 and § 11 Accessibility. (2) B-6t under-states the blind active's cost → B-6t row 5 rewritten, DP-1 and R-D5 updated. (3) the healthy weekly-only row's cost is smaller than stated, because a failed poll nulls both windows → § 6a flow 2's discriminator and its limit. (4) flows 3–4 lack error paths → written. (5) the `stats` `—` means UNKNOWN, not observed absence, and the signal, velocity, runway and cap cells degrade alike → B-8 and § 9a. (6) option (ii)'s row-geometry cost, and the `status` table's own `EXPIRY_GAP` precedent for option (i)'s CLI half → § 9b. (7) B-6t row 4 cited `resetIn`, a test-only helper → the per-meter reset label is cited instead. **Second pass** (APPROVE WITH CONCERNS, one non-blocking): S6's decode half cannot land alone (a present block missing a required field throws in the Swift decoder) and the panel needs a non-inventing interim for a decoded percent-less block while D0 is open → B-6 states the interim (render it as no block, today's two-meter `n/a` row) and its cost; § 9b (iii), DP-1 and R-D9 carry it. The lens confirmed all seven first-pass concerns addressed.

### Stage premortem (run at design entry, de-anchored; the items the PRD's P-rows did not name)

| ID | It shipped and failed because… | Verdict | Routed to |
|---|---|---|---|
| DP-1 | the transitional state omitted the `blind_active` block and the operator never learned a weekly-only active had been blind for an hour | surface | AD-6's stated cost (B-6t row 5: indistinguishable from a failed-poll row on both surfaces until S6); the signpost at the end of tranche 1's budget (AD-23); `blind_enter` / `blind_exit` events still record the episode. After S6 and before D0, the panel alone keeps that blindness invisible (a percent-less block renders as no block, B-6) while the CLI footer shows the duration and verdict — "days" become "until D0" on the panel (R-D9) |
| DP-2 | the rollup gained an omitted dimension and an older `stats` binary in `target/` refused the whole rollup with `UsageRollupMalformed` | decide | B-7's loud-failure statement — after the council's rollup fence that refusal is the intended outcome, and the ruling's C6 makes the message expected text; B-12's no-older-daemon rule names stray `target/` builds and says the store files are never deleted or moved |
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
| B-11 render | **FEASIBLE once D0 lands**; not a spike — a decision | the `—` token and the expiry-line precedent exist; the gates are known |
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
| R-D3 | An older daemon stalls on the fenced rollup (silent: it discards the compaction error) and erases `null` lines only in the accepted residuals — lines appended before the first fenced write, or a rollup deleted or moved by hand; an older `stats` refuses the fenced rollup (loud, by design) | 2 | 3 | 6 MEDIUM | AD-10/AD-11 state both; B-12's no-older-daemon rule and ancestry check; the rollup fence #1598 builds; D6 residual recorded | operator |
| R-D4 | Tests "fixed" by `Some(0.0)` wrappers; a consumer defaults silently (P-1, P-11) | 2 | 3 | 6 MEDIUM | AD-4; per-arm `None` tests (Cap-2); `MET-T-003`; review reads every wrapper | implementer, reviewer |
| R-D5 | The transitional days render a weekly-only blind active as an ordinary failed-poll row on both surfaces (duration and percent lost; PRD § 11.1's Stale cell unmet) and silence its report arms; the healthy weekly-only row costs less than first stated, because a failed poll nulls both windows and the weekly cell discriminates (§ 6a flow 2) | 3 | 2 | 6 MEDIUM | AD-6 documents and dates it; the signpost: S6 lands by the end of tranche 1's budget (AD-23), or is pulled into tranche 1 under the extend-with-justification breaker, or D10 is re-ratified | operator (D10) |
| R-D6 | Goldens regenerated with `--include-ignored`, re-sealing the frozen migration fixture | 1 | 3 | 3 LOW | AD-18; § 11 gate 7 says never | implementer |
| R-D7 | Appetite overrun on tranche 1 against its re-set budget — the human effort-day estimate the coverage gate sized (PRD § 3.1), no longer one calendar week (one compile unit across ~15 files plus 245 test sites) | 2 | 2 | 4 MEDIUM | AD-23's cut order; the hot-fix (tranche 0, #1613) means an overrun no longer leaves the fleet unreadable; "extend with justification" by one small batch, never a fabricating shortcut beyond tranche 0's bounded one (§ 1b) | operator |
| R-D8 | A second reply shape among the 14 uncaptured accounts | 1 | 2 | 2 LOW | the PRD's first decision gate (capture quota fields, add a fixture) | operator |
| R-D9 | D0 never lands: the transitional `n/a` becomes permanent by default and, once S6 lands, the panel keeps rendering a percent-less `blind_active` block as no block — the blind duration and verdict stay invisible on the panel alone (the CLI footer shows them) | 2 | 2 | 4 MEDIUM | S11 is a tracked waiting item with a named owner; OAD-1 carries the question; the D0 ask names the held block explicitly | maintainer |

No HIGH risk remains in this design's own register (R-D1 to R-D9). The PRD's four HIGH risks (PRD § 6, R1 to R4)
are the problems this design is built to close, and each sits at MEDIUM or below here once its mitigation is
in place: R1, open-PR and schema-minor contention, as R-D1 and R-D2 under AD-22 and AD-9; R2, an invented session
value acted on, by the four named moves on every decision arm, with R-D4 guarding the tests; R3, an invented
value written to disk, by B-7's `null` line, which leaves nothing invented to write; R4, silent loss on the
sample-line change, as R-D3 under the tolerant reader and the both-halves rule. Rabbit-hole scan (10× test): if B-6's lockstep takes 10× longer, tranche 1 still
ships the sample line and an honest `session_pct: null` under B-6t, the fleet's weekly readings having recovered at tranche 0 — it is not load-bearing; if B-3's arm sweep takes 10× longer, the
whole compile unit waits, which is why B-5 lands first and the arms are enumerated here rather than
discovered at the compiler. Gate: **PASS**.

### Open questions (every one load-bearing; the brief carries each with its context)

D0 (+ D15), D1, D2, D3, D4, D5, D7, D8, D9, D10 — standing as PRD § 8 states them, each with
the default this design applied where the PRD recommended one; D11 (the wire key's shape), D12
(`Sample.weekly` optional), D14 (event-field absence), the weekly-less arms (AD-4b) and the #1537 landing order (AD-22) — the author-chosen defaults of § 12. D6 and D13 are no longer open: a council settled both on 2026-11-02 at the maintainer's delegation (a comment on #1598), ratifying each default and amending D6 with the rollup fence and the corrected deployment rule (B-7, B-12) and D13 with the per-axis `samples` count (AD-11). One product-scope item is returned to the PRD rather than decided
here: whether S8's wire half rides in tranche 1 (OAD-3) changes the appetite of § 3.1, which the PRD
owns.

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
| **RenderCell** | where the operator sees the state | B-11; D0's token | Cap-9 | the Session meter slot, the blind held block (bar, percent slot, caption, spoken phrase), the SESSION%/RESET cells, the Stats-tab cell and its spoken summary |
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
| REQ-RPL-B-003 | Technical, Testing | § 5 B-1 (D1), B-5 (Oracle C) | Cap-1, Cap-8 | covered — default D1 (a), ratification-pending |
| REQ-RPL-B-004 | Technical, Testing | § 5 B-1 (D2), B-4 | Cap-1 | covered — default identical, ratification-pending |
| REQ-RDG-B-001 | Technical | § 4.1, § 5 B-2 | Cap-2 | covered |
| REQ-RDG-C-001 | Technical, Testing | § 4.2, § 5 B-3 (rows 1–15) | Cap-2 | covered |
| REQ-ARM-B-001 | Technical | § 5 B-3 row 1 | Cap-2 | covered |
| REQ-ARM-B-002 | Technical | § 5 B-3 row 1 | Cap-2 | covered |
| REQ-ARM-B-003 | Technical | § 5 B-3 rows 3–5 | Cap-2 | covered |
| REQ-ARM-B-004 | Technical | § 5 B-3 row 9 | Cap-2 | covered |
| REQ-ARM-B-005 | Technical | § 5 B-3 row 6 | Cap-2 | covered |
| REQ-ARM-B-006 | Technical | § 5 B-3 row 6 | Cap-2 | covered |
| REQ-ARM-B-007 | Technical | § 5 B-3 row 7, B-6t, B-6 | Cap-2, Cap-3 | covered (surfacing half); arming half OAD-2 |
| REQ-ARM-B-008 | Technical | § 5 B-3 row 8 | Cap-2 | covered — default D4, ratification-pending |
| REQ-ARM-B-009 | Technical | § 5 B-3 row 10 | Cap-2 | covered |
| REQ-ARM-B-010 | Technical | § 4.5, § 5 B-3 row 11 | Cap-2 | covered |
| REQ-ARM-B-011 | Technical | § 5 B-3 (closing paragraph), § 6b | Cap-2 | covered |
| REQ-ARM-B-012 | Technical | § 5 B-3 rows 1, 12 | Cap-2 | covered — default D7, ratification-pending |
| REQ-ARM-C-001 | Technical | § 4.5, § 5 B-3 row 14 | Cap-2 | covered |
| REQ-ARM-Q-001 | Technical, Testing | § 11 Performance, § 13 | Cap-10 | covered |
| REQ-FIX-B-001 | Testing | § 5 B-4 | Cap-1 | covered |
| REQ-ORC-B-001 | Testing | § 5 B-5, § 11 | Cap-8 | covered |
| REQ-STL-I-001 | Technical | § 5 B-6, § 8 | Cap-3 | covered — key shape D11, author-chosen |
| REQ-STL-I-002 | Technical | § 5 B-6 (the reshape) | Cap-3 | covered — wire and decode halves; the held block's panel render half waits on D0 (D15) |
| REQ-STL-C-001 | Technical, Testing | § 5 B-6 (lockstep), § 4.6 | Cap-3 | covered |
| REQ-STL-B-001 | Technical | § 5 B-6t | Cap-3 | covered — conditional on D10 |
| REQ-STL-Q-001 | Testing | § 13 | Cap-3 | covered |
| REQ-SMP-B-001 | Technical | § 5 B-7 | Cap-4 | covered |
| REQ-SMP-B-002 | Technical, Testing | § 5 B-7 (reader, compaction, tiers) | Cap-4 | covered |
| REQ-SMP-C-001 | Testing | § 5 B-7 (tests that move), § 11 | Cap-4 | covered |
| REQ-SMP-Q-001 | Testing | § 5 B-7, § 13 | Cap-4 | covered |
| REQ-STS-B-001 | Technical, UX/IA | § 5 B-8, § 9a | Cap-5 | covered — wire timing OAD-3; Stats-tab cell D15 |
| REQ-STS-C-001 | Technical | § 4.5 | Cap-5 | covered (no change, by construction) |
| REQ-REC-B-001 | Technical | § 5 B-9 | Cap-6 | covered — non-testable (documentation) |
| REQ-REC-B-002 | Technical | § 5 B-10 | Cap-6 | covered — non-testable (documentation) |
| REQ-REC-B-003 | Technical | § 5 B-10 (parser doc) | Cap-6 | covered — non-testable (documentation) |
| REQ-REC-B-004 | Technical | § 5 B-10 (HQ row) | Cap-6 | covered — non-testable; the amendment itself is the operator's |
| REQ-REC-C-001 | Technical | § 5 B-10 (ordering) | Cap-6 | covered — non-testable (ordering constraint, reviewed) |
| REQ-CEL-C-001 | UX/IA | § 9, § 5 B-11 | Cap-9 | covered — the constraint holds by construction (nothing built; the held block S6 forces is routed to D0 rather than rendered) |
| REQ-CEL-B-001 | UX/IA, Testing | § 9b | Cap-9 | **deferred with explicit tracking** — blocked on D0; the S11 waiting item carries the three options and the gates; the backward row stands |
| REQ-DEP-B-001 | Technical | § 5 B-12 | Cap-7 | covered — non-testable (procedure); manual; the text half (#1603) is in tranche 2, after the deploy |
| REQ-DEP-C-001 | Technical | § 5 B-12 (both-halves rule), B-7 | Cap-7 | covered — binds from tranche 1's deploy, which writes the first `null` line |
| REQ-DEP-B-002 | Technical, Testing | § 5 B-12 (fleet recovery check) | Cap-7 | covered — manual, operator-run; a recovery check at tranche 0, a no-regression check under tranche 1 |
| REQ-DEP-Q-001 | Testing | § 13 | Cap-7 | covered — manual; first met by tranche 0's deploy, a no-regression check under tranche 1 |

**46 of 46 addressed: 45 covered, 1 deferred with explicit tracking (REQ-CEL-B-001 → the S11
waiting item, blocked on D0). No UNCOVERED entry.** Seven requirements are classified non-testable by
code (documentation and procedure) and are graded by the doc gates and the runbook's manual check;
the Testing track records that classification rather than binding them to a code capability.

## 16b. Element-to-Requirement Backward-Coverage Matrix

| Design element | Type | Traces to | Provenance | Status |
|---|---|---|---|---|
| B-1 per-window parser composition; D1 (a); D2 identical | mechanism | REQ-RPL-B-001…B-004, REQ-QW-B-001 | PRD § 10.1–10.2 | traced |
| Invariants I-1, I-2 + their tests | test | REQ-RDG-C-001 | PRD § 10.3 (no read site defaults) | traced — design-derived, flagged |
| B-2 `Option<f64>` on four reading types | mechanism | REQ-RDG-B-001 | PRD § 10.3 (recommended mechanism) | traced |
| B-3 rows 1–15 (the moves) | mechanism | REQ-ARM-B-001…B-012, REQ-ARM-C-001, REQ-RDG-C-001 | PRD § 10.3 | traced |
| B-3 row 15 event-field typed absence (D14) | mechanism | REQ-RDG-C-001 (events clause) | PRD § 10.3 | traced — author-chosen form, flagged |
| AD-4b weekly-less arms (B-3 rows 1, 10, 13) | mechanism | REQ-RPL-B-002 (a weekly-less reading is served), REQ-ARM-B-012 (symmetry) | PRD § 10.2, § 10.3; message 2 | traced — design-derived by symmetry, flagged |
| AD-13 session-cap counts omitted (`cap_hits`, `time_at_cap_secs`, the `caps` / `t@cap` cells) | wire contract | REQ-STS-B-001; MET-T-003 | PRD § 10.8, § 4 | traced — author-chosen, flagged |
| B-4 fixture bodies | artifact | REQ-FIX-B-001 | PRD § 10.4 | traced |
| B-5 Oracles A–D, `#[ignore]`d, RED-proven | test | REQ-ORC-B-001 | PRD § 10.5 | traced |
| B-6 `absent_windows` key (D11) | wire contract | REQ-STL-I-001 | PRD § 10.6 ("the key's name is Stage 2's") | traced — author-chosen shape, flagged |
| B-6 `last_known_session_pct` optional — the account row's, and `recent_blind_preempt_swap`'s (#1599 § 2b), which restores the notice B-6t withholds for a Retry-After swap off a session-less anchor | wire contract | REQ-STL-I-002 | PRD § 10.6 | traced |
| B-6 minor + lockstep + two new golden rows | artifact | REQ-STL-C-001, REQ-STL-Q-001 | PRD § 10.6 | traced |
| B-6t transitional state; no `blind_active` block (AD-6 sub-default) | wire contract | REQ-STL-B-001, REQ-ARM-B-007 | PRD § 10.6 (D10), § 10.3 | traced — author-chosen sub-default, flagged |
| B-7 `Sample.session` optional, `null` written | artifact | REQ-SMP-B-001 | PRD § 10.7 | traced |
| B-7 `Sample.weekly` optional (D12) | artifact | REQ-RDG-B-001 (both windows), REQ-RPL-B-002, REQ-SMP-B-001 by construction | PRD § 10.3, § 10.2, § 3.2 coupling 1 | traced — compiler-forced extension, flagged |
| B-7 tier `Option<HourStat>` / `Option<DayStat>` (D13) | artifact | REQ-SMP-B-002 (fold without a fabricated statistic) | PRD § 10.7 | traced — settled by council (D13) |
| B-7 canary move + round-trip + compaction tests | test | REQ-SMP-C-001, REQ-SMP-Q-001 | PRD § 10.7 | traced |
| B-8 session-figure exclusion; census exclusion (D5) | mechanism | REQ-STS-B-001; ADR-0033 amendment (REQ-REC-B-002) | PRD § 10.8, § 8 D5 | traced |
| B-8 wire: `session`/`band` omittable, schema 2, Swift optional, golden | wire contract | REQ-STS-B-001 (representation is Stage 2's) | PRD § 10.8 | traced — field set author-chosen, flagged |
| B-8 CLI `stats` table `—` reuse | UX surface | REQ-STS-B-001 | PRD § 10.8; `stats`' own UNKNOWN vocabulary | traced |
| D15 Stats-tab cell, its spoken summary and the blind held block routed to D0 | decision | REQ-CEL-C-001 (register rule) | PRD § 10.11 | traced — design-found extension of D0, flagged |
| B-9 warm-up amendment | documentation | REQ-REC-B-001 | PRD § 10.9 | traced |
| B-10 four ADR amendments, README rows, parser doc, HQ note | documentation | REQ-REC-B-002, B-003, B-004, C-001 | PRD § 10.10 | traced |
| B-11 waiting item (three options, gates) | UX surface | REQ-CEL-C-001, REQ-CEL-B-001 | PRD § 10.11 | traced |
| B-12 runbook amendment: both-halves rule; fleet recovery check | procedure | REQ-DEP-B-001, C-001, B-002, Q-001 | PRD § 10.12 | traced — commands design-derived; the text half is tranche 2 (AD-23) |
| AD-22 PR composition | decision | PRD R1, § 7 | PRD § 6, § 7 | traced |
| AD-23 tranche / cut order, with the tranche-0 hot-fix | decision | PRD § 3.1, § 3.2, A-7, A-8 | PRD § 3; the maintainer's decisions of 2026-11-01 | traced — appetite and tranche 0 set by those decisions |

**27 of 27 traced. No PHANTOM element.** 10 elements are design-derived extensions of a requirement (flagged in the status column) and are surfaced as decisions to ratify in § 12 rather than absorbed silently; none traces to nothing.

## Design Lock

**NOT LOCKED — provisional.** `status: draft` on the brief.

| Gate | Verdict |
|---|---|
| Forward coverage (§ 16) | 46 / 46 addressed; 0 UNCOVERED; 1 deferred with explicit tracking (REQ-CEL-B-001 → S11, blocked on D0) |
| Backward coverage (§ 16b) | 27 / 27 traced; 0 PHANTOM |
| Load-bearing open questions | **open questions stand** (D0–D5, D7–D12, D14, D15; D6 and D13 were settled by council on 2026-11-02) — `final` is forbidden |
| Feasibility (§ 14) | PROCEED — no UNCERTAIN, no INFEASIBLE, no spike |
| Risk (§ 14) | PASS — no HIGH risk |
| Dual-lens ratification | product lens APPROVE WITH CONCERNS (six, all applied); UX lens BLOCKING on the first pass, revised as prescribed, re-dispatched once — second pass APPROVE WITH CONCERNS (one non-blocking concern, applied; § 14); the design's author supplied neither verdict |
| Readiness | `dor_status: failed`, acknowledged by the orchestrator on the operator's behalf — the operator's own ratification is still owed |

What the operator should read first: § 12's **author-chosen** rows (AD-4b's weekly-less arms, AD-6's omitted block and its signpost, AD-7's key, AD-10's weekly half, AD-13's field set with the cap counts, AD-14's events, AD-22's landing order), the three open stubs, and D15 — the one question this design found that the PRD did not carry, now covering the Stats-tab cell, its spoken summary and the blind held block.
