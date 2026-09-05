---
title: Daemon Diagnostic Integrity — Solution Design
scope: daemon-diagnostic-integrity
created: 2026-09-04
status: draft
source: docs/requirements/daemon-diagnostic-integrity.md
---

# Solution Design: Daemon Diagnostic Integrity

## 1. Goals and Drivers

Make the daemon's self-report usable as evidence. Three independent, additive changes, plus one
decision that belongs to a neighbouring PRD and is settled here because this run is its Stage 2.

1. **Attributable** (PRD R-1, R-7) — a log line can be tied to the build that wrote it, from the log
   alone. Measured cost today: **3 out-of-band artifacts**, two of which need the process alive.
2. **Truthful** (PRD R-2, R-3) — `near_limit_poll_coverage` reports what the scheduler applied
   (≈37.5 s at the live configuration), not the 60 s cap that draw did not reach.
3. **Deployable** (PRD R-4, R-5) — a runbook for the procedure that actually replaces the running
   daemon.
4. **The observation-gap readout** — `active-account-observation-continuity` R-4/R-5, whose § 6b
   says *"which readout carries it is a Stage 2 decision"*. Settled in § 7 below.

## 2. Constraints

| Constraint | Source | Consequence |
|---|---|---|
| MSRV `1.87.0`, stable pinned `1.96.0` | `Cargo.toml:8`, `ci.yml:18,20` | Any mechanism must compile on both; the `msrv` job re-runs build+test on the older toolchain |
| Rate neutrality | `ADR-0012` Decision 3 | D-2 corrects a *report*; it must not move the schedule. `near_limit_poll_secs` is **not** touched — that is `#1458` |
| Four independent schema wires | `CLAUDE.md` § Schema versions | D-4 bumps **`reliability`'s own** `JSON_SCHEMA_VERSION` only. No `STATUS_SCHEMA_VERSION`, no Swift edit, no **status/watch** golden — but the three `build/fixtures/cli-renders/reliability-*.txt` renders **do** move, and owe a trailer (§ 7) |
| `src/**` is in the `rust` CI filter | `ci.yml` `changes` filter | D-1 and D-2 owe `test`, `msrv` **and** `deny` — not just the five local gates |
| Single operator, one machine, by hand | PRD § 1 | "Deployable" means a runbook, not a pipeline |

## 3. Context and Scope

All four changes touch surfaces the daemon already owns. Nothing crosses into the menu-bar app:
`grep -rn 'near_limit_poll_coverage'` finds one emitter, one unit test and one daemon-side assertion,
and **no Swift surface**; `observation_gap_*` likewise appears only in `src/`.

## 4. Solution Strategy

### D-1 — Build identity: stamp what the process can observe about itself, not a build-time SHA

**Decision: emit at startup from runtime-observable facts — `CARGO_PKG_VERSION`, the resolved
`current_exe()` path, and that file's size and mtime. Do NOT add a `build.rs` in this increment.**

The obvious design is a `build.rs` baking `git rev-parse HEAD`. Three reasons it loses here, and one
reason the alternative is not merely cheaper but *more correct*:

- **A SHA lies on a dirty tree.** The operator builds from a working checkout. A `build.rs` stamps
  the last commit regardless of uncommitted changes, so the one case where attribution matters most —
  "was this the build with my fix in it?" — is exactly the case a bare SHA answers wrongly. It
  produces a confident, false attribution, which is worse than none. (The PRD premortem raised this;
  it is the deciding argument, not a caveat.)
- **It is not what actually worked.** § 2.1 of the PRD records that the successful proof was the
  `lsof` binary probe — the artifact's *identity*, not a commit. Path + size + mtime reproduces the
  proof that worked, from the log, after the process is gone.
- **It stays out of the build system.** `CARGO_PKG_VERSION` is a compile-time env var Cargo always
  provides; no `build.rs`, no `cargo:rerun-if-changed` staleness trap (a cached build script silently
  serving a stale SHA is a failure mode with no detector), no MSRV exposure, no `deny` surface.

**A SHA is not foreclosed.** If a later increment wants one, it is additive to the same event and
must carry a dirty-tree marker beside it. Recorded as an open question (§ 11), not designed away.

**Shape.** One durable event at startup, in the established `event=` line vocabulary:

```
ts={ts} event=daemon_build version={v} exe={path} exe_size={bytes} exe_mtime={rfc3339}
```

`exe` is a filesystem path, not a credential; nothing about the roster, an account UUID, or a token
appears. Emitted on **every** start, including a restart after a crash — which is precisely when the
question is asked, and which additionally makes restart boundaries visible in the log. That second
effect is not incidental: D-4 depends on it (§ 7).

### D-2 — Truthful sub-interval: correct the value, and carry the cap beside it

**Decision: emit the effective interval, and add the cap as a second field. Do not gate the event.**

Two sites, both in `src/daemon.rs`:

| Site | Today | After |
|---|---|---|
| `Event::NearLimitPollCoverage` (emit) | `sub_interval_secs: self.near_limit_poll_secs` — the cap | the interval `next_subinterval` applied, **carried** to this site |
| `next_subinterval` (compute) | `base.min(Duration::from_secs(self.near_limit_poll_secs))` | unchanged in value; must now **publish** what it applied |

**The fix is to CARRY the applied value from the compute site, never to re-derive it at the emit
site** — and that is a constraint, not a style preference. `next_subinterval` obtains `base` from
`next_poll_interval`, which **draws** from the poll strategy against `&mut self.rng`; `poll_secs` is
the one tunable that jitters by default (σ ≈ 20% of the default), and `next_subinterval`'s own doc
comment states that *each sub-interval draws a fresh full interval before dividing*. So calling it
again at the emit site would advance the RNG and perturb the schedule — a rate change R-3 forbids —
and would in any case yield a *different* draw than the one actually applied. The applied value must
be captured where it is computed and passed through.

**A second consequence of the same draw: `base` is a random variable, so the report needs
sub-second resolution.** `37.5 s` is the *mean* of `base` at the live configuration, not a fixed
observable, and `sub_interval_secs` is integral seconds — which cannot express `37.5` and would
truncate a truthful report to `37`. Carrying the applied interval faithfully therefore requires a
sub-second field (a `_ms` sibling, or a fractional value). Which of those, and whether the integral
field is retained for compatibility, is an open sub-decision for Stage 2.

**Gating the event on the cap actually binding was rejected**: the event is the only durable record
that the near-limit band was entered, so suppressing it when the cap does not bind — which, at the
live configuration, is *nearly always* — deletes the record entirely. Carrying both values instead is strictly more informative than either single
value, and it makes "did the cap bind?" answerable rather than inferable:

```
ts={ts} event=near_limit_poll_coverage acct={a} sub_interval_secs={effective} cap_secs={cap}
```

**Reader-compatibility note.** `sub_interval_secs` changes *meaning* while keeping its name. That is
acceptable only because the grep in § 3 found no consumer outside the emitter and its own tests — and
because leaving a field that has been wrong 624 times is not a compatibility guarantee worth
preserving. The doc comment on `near_limit_poll_secs` already states the `min(...)` form correctly —
and names `next_subinterval` as its applier — so the code's own
documentation moves toward the code rather than away from it.

### D-3 — Runbook: document the procedure, cite the script, do not paraphrase it

**Decision: a short runbook under `docs/`, citing `apps/menubar/scripts/release-macos.sh` rather than
restating its steps.** Nothing in this repo reconciles prose against a script, so a paraphrase rots
silently; a citation degrades visibly. It must state three things the operator's `cargo build` mental
model gets wrong:

1. The daemon is an `SMAppService` agent **inside the app bundle** (`Contents/Helpers/sessiometer`,
   parent `org.sessiometer.menubar`). There is **no** `~/Library/LaunchAgents` plist to re-point, and
   `cargo build` into `target/` replaces nothing that runs.
2. The procedure: `cd apps/menubar && ./scripts/release-macos.sh --sign-only`, then relaunch the app
   (which fires `reconcileDaemonAgentRegistration()`) **or**
   `launchctl kickstart -k gui/$(id -u)/org.sessiometer.agent`.
3. **The hazard**: the script `rm -rf`s the running bundle's directory before rebuilding.

**Verification step (PRD R-5)** — read D-1's `event=daemon_build` line back from the event log and
confirm it names the build just deployed. This is why R-5 depends on R-1: without the stamp the check
degrades to inference, which is the failure the whole PRD is about.

## 5. Building Blocks

| Element | File | Change |
|---|---|---|
| B-1 | `src/observability.rs` | new `daemon_build` event variant + render arm |
| B-2 | daemon startup path | emit B-1 once, after the exe path resolves |
| B-3 | `Event::NearLimitPollCoverage` in `src/daemon.rs` | report the effective interval; add `cap_secs` |
| B-4 | the `near_limit_poll_coverage` arm in `src/observability.rs` | render arm for B-3's second field |
| B-5 | `docs/` | the runbook (D-3) |
| B-6 | `src/reliability.rs` | `first_sight` block, schema `12 → 13` (D-4, § 7) |

## 6. Runtime View

D-1 fires once per process start. D-2 fires at near-limit band entry, as today, with corrected
content. Neither adds a request, a timer, or a poller — `ScheduleRateNeutrality` (PRD § 6) is
satisfied **by construction**, not by measurement, because no code path that decides *when* to poll is
touched.

## 7. The observation-gap readout — closing the neighbouring PRD's Stage-2 deferral

`active-account-observation-continuity` R-4/R-5 are `must` requirements — **pipeline-authored and
ratification-pending**, not operator-ratified: that PRD's own § 11 files them under *Derived from
measured evidence* and marks the whole row outstanding operator action, and its § 1b ratifies only
that *"the surfacing half is sized separately"* — which R-5 **is**. Treating them as ratified would
be exactly the conflation this PRD refuses for its own requirements. Its § 6b deferred only
*which readout carries them* — the row **this commit** leaves at `NEAR-COMPLETE`, having decided the readout home while the
censoring argument in *this* section shows the `GOAL` percentile has no source — and *that* PRD's own
solution design, in its separate § 7, states the instrument is **"log-only in its first
increment"**. This section is that deferred decision.

**Decision (D-4): a `first_sight` block on `ReliabilityWire`, `JSON_SCHEMA_VERSION` `12 → 13`,
additive, mirroring `BlindEpisodesWire`.**

**Why `reliability` and not `status` / `stats`.** The metric is a *distribution over a window* —
`active-account-observation-continuity` § 6's `PostSwapFirstSightLatency` asks for p50 and p95 over
7 days — a shape only a windowed-SLI wire carries, whatever the censoring problem below does to
*which* of those two figures this source can honestly support. `status` carries instantaneous state and
`stats` carries per-account facts; `reliability` is the wire whose entire purpose is windowed SLIs
derived from the event log, and it already carries a family of them. Cost is also decisive: this bumps
`reliability`'s own constant, so there is **no** Swift surface, **no** Swift fixture lockstep and
**no** status/watch golden — where a `StatusResponse` field would obligate all three.

**One golden class does move, and it is not optional.** `reliability`'s human render and its JSON
wire are built from one report struct: its `blind_episodes` field feeds both the `render_human`
renderer and the `BlindEpisodesWire` map, all three in `src/reliability.rs`, and the template block
named below renders into `build/fixtures/cli-renders/reliability-full.txt:33`. So D-4 owes
`cargo test -- --ignored emit_cli_render_goldens` **and** a `CLI-Goldens-Rebaselined:` trailer
(`scripts/check-cli-golden-rebaseline.sh`); and because `build/fixtures/**` sits in both the `rust`
and `swift` path filters in `ci.yml`, the jobs those two gate are owed as well — `test`, `msrv` and
`deny` off `rust`, and `swift` plus `panel-goldens` off `swift`. Note there is **no** `panel-goldens`
*filter*: that job is gated on the `swift` filter's output, which is why re-baselining a fixture
that touches no `apps/menubar/**` path still owes it. The precedent is
`df9796b` — the same commit § 2.1 uses as its attribution proof — which moved all three
`reliability-*.txt` renders and carried exactly that trailer.

**Why `BlindEpisodesWire` is the template rather than a fresh shape.** `observation_gap_enter` /
`observation_gap_exit` is structurally the same enter/exit pair as `blind_enter` / `blind_exit`, and
that block already solves every pathology this one faces — each of which is a real hole, not a
hypothetical:

| Pathology | `BlindEpisodesWire` field | Why it recurs here |
|---|---|---|
| A `--since` cutoff or log rotation severs the pair | `n_exit_without_enter` | Same log, same rotation |
| A daemon restart loses the in-memory anchor | `n_anchor_lost` | the gap anchor `observation_gap` in `src/daemon.rs` is in-memory too |
| Still open at the horizon | `n_never_recovered` | A gap that never closed is the worst case, not a missing sample |
| Unparseable line | `n_malformed` | Same parser class |

**The restart case is where D-1 pays off a second time.** `n_anchor_lost` exists because a restart
severs an episode and must not be counted as a recovery. Today a restart is invisible in the log; with
D-1's `daemon_build` line it is a marker the reader — and the parser — can see. D-1 and D-4 are
independent changes that compose; neither blocks the other.

**Percentile shape: nullable, matching the swap-SLI siblings — not the plain counts
`RefreshTokenLossWire` uses.** That block's own doc comment states the discriminator: a zero there is
*"a real, meaningful reading"*. Here it is not — a window with no change of active is an **empty
subject**, and reporting `p95 = 0` for it would assert perfect latency where nothing was measured.
Withhold the figure and publish the sample count beside it.

**Derivation**: `observation_gap_exit` lines where `was_active=true` and `swapped_away=false`,
p50/p95 of `elapsed_secs`, bounded to the active `--since` window. `swapped_away=true` is excluded
and counted separately — it is a gap that ended by the account being parked, not by being observed,
so folding it in would flatter the metric.

**This is NOT the § 6 METER, and the difference decides what D-4 may claim.** The event pair is
**edge-triggered past the bound**: the `ObservationGapEnter` arm fires only when `elapsed >
threshold`, and its own comment says the strictness is deliberate — *at* the bound the guarantee is
met, so an episode opened there would report a breach the requirement does not consider one. Every
emitted episode is therefore **left-censored at `2 · poll_secs / N`**, and a within-bound first
sight emits nothing at all. The daemon's own emitter documentation states the consequence, above
the `observation_gap_exit` render arm in `src/observability.rs`:

> the exits form the tail `{ latency > 2 · poll_secs / N }`, never the whole first-sight
> distribution. A p50 over these lines is therefore the median of BREACHES, and the healthier the
> fleet the smaller and worse that population reads — a within-bound first sight emits nothing at
> all. The `FAIL`-side criterion (any single occurrence beyond the bound) and the breach tail are
> what this line supports honestly; a p50 of first-sight latency needs a source that also records
> the non-breaches, which no event here is.

**One clause of that comment is loose, and an implementer meets it before meeting this design.**
"any single occurrence beyond the bound" reads against the *entry* edge `T`, because that is the
threshold the emitter itself applies. § 6's `FAIL` is `> 2T` — **twice** it. § 6 is authoritative;
build the detector at `2T`. Quoted unelided here for exactly that reason: the sentence is the one a
reader of `src/observability.rs` hits first, and taken at face value it licenses a detector that
fires at half the ratified threshold.

Two consequences follow, and the second is disqualifying for half the Planguage tag:

1. **`p50` over this population is the median of breaches**, not of first-sight latency — and it
   reads *worse* as the fleet gets healthier, because the within-bound sights that would pull it
   down are precisely the ones not recorded.
2. **`GOAL p95 ≤ 2 · poll_secs / N` is unreachable by construction.** Every sample exceeds the
   bound, so `p95 > GOAL` whenever the sample count is non-zero — and at zero, D-4b withholds the
   figure. The GOAL-met state is not representable at all, which is the same success-vs-no-data
   ambiguity D-4b exists to prevent.

**So D-4 bounds the `FAIL` criterion from above, and does not reach the `GOAL` percentile at all.**
Get the two thresholds the right way round, because they differ by a factor of two and an
implementer builds against whichever one this section names. Write `T = 2 · poll_secs / N` (75 s at
the live configuration). Then:

- The **entry edge** is `T` — `observation_gap_threshold` returns `poll_secs · 2 / rotation`, so the
  emitted set is `E = { elapsed > T }`.
- § 6's **`GOAL`** is `p95 ≤ T`.
- § 6's **`FAIL`** is `any single occurrence > 2T` — **twice** the entry edge, not the entry edge.

`2T > T`, so no post-swap first sight that would trip `FAIL` is missing from `E`: **filtering `E` on
`elapsed_secs > 2T` cannot produce a false negative.** An empty result is therefore conclusive — no
`FAIL` occurred.

**A non-empty result is not conclusive, and this is the limit to write down rather than round off.**
`E` is not confined to post-swap first sights. The entry anchor is
`observed.max(designated)`, so an account that has been active a long time anchors on its *last
observation*, and a mid-tenure observation gap opens an episode carrying `was_active=true` and
exiting with `swapped_away=false` — passing the derivation filter above verbatim. § 6's `SCALE` is
*"seconds from a change of active designation to the first completed observation of the new
active"*: a mid-tenure gap is not an interval of that kind at all. So `E` filtered at `2T` is an
**upper bound** on `FAIL` occurrences — sound as a negative, over-reporting as a positive.

That is sufficient for #1488, whose value is making the tail durable rather than certifying a
verdict, and this design deliberately claims no more than it. **Open sub-decision, not an assertion
that it works:** the two anchor branches are distinguishable at the emit site — the `match` already
separates them — so `observation_gap_enter` could carry which one fired, and a consumer could then
separate post-swap first sights from mid-tenure gaps. Whether that is worth an event field belongs
to #1488's implementation, and nothing here should be read as having decided it.

What is **not** computable is the `GOAL` statistic itself. `GOAL` is a `p95` over the *whole*
first-sight distribution; `E` omits every observation at or below `T` by construction, so `p95(E)` is
not `p95(whole)` and no filtering recovers it. That needs a source recording within-bound first
sights, and none exists today: **OQ-3** in § 11. Not closed by this design, and not to be recorded as
closed.

This is what #1488 delivers, and it is real: today neither the `FAIL` criterion nor the breach tail
reaches any JSON wire, so nothing moves when a fix lands.

**Scope bound.** `record_usage_sample` stays inside the `poll_idx` guard, so the usage-sample store
still cannot see a never-attempted poll. D-4 repairs the **event-log** readout only. Stating this is
the point: a reader who assumes both surfaces were fixed will trust the wrong one.

## 8. Crosscutting Concepts

**Security.** D-1 emits a filesystem path, a version, a size and an mtime. No credential, token,
account UUID or roster content — the PRD's `BUT NOT` clause is the acceptance condition, and § 5's
GWT states it. Version/build disclosure in a log the operator may share is accepted deliberately.

**Observability.** This design *is* the observability lane; the constraint that governs it is that
none of it may move runtime behaviour (PRD R-3).

**Error handling.** `current_exe()` and the `metadata()` call behind size/mtime can both fail. D-1
must degrade rather than refuse to start: emit the event with the fields it could resolve and an
explicit marker for the ones it could not. **A daemon that will not start because it cannot describe
itself is a strictly worse outcome than an unattributable log line.**

**Master test plan.**

| ID | Covers | Test | RED against pre-change tree |
|---|---|---|---|
| T-1 | R-1, R-7 | a startup emits exactly one `daemon_build` line carrying a version and an exe path | **Yes** — no such event exists |
| T-2 | R-1 security | the line carries no token, account UUID or roster content | Vacuous pre-change (no line) — a *guard*, not a RED oracle, and recorded as such |
| T-3 | R-2 | at a configuration where `poll_secs / N` ≠ `near_limit_poll_secs`, the emitted `sub_interval_secs` equals the applied interval | **Yes** — today it emits the cap |
| T-4 | R-3 | the existing `#80`/`#366` stagger locks still pass | Passes pre-change; a regression guard, not an oracle |
| T-5 | R-6 | T-1 and T-3 are demonstrated RED before either fix is accepted | — |
| T-6 | D-4 | a fixture log with a severed pair, a restart orphan and a malformed line produces the four census counts, and an empty window withholds the percentiles rather than reporting 0 | **Yes** — no such block exists |

T-2 and T-4 are named as guards rather than oracles deliberately: a suite where some tests cannot
redden is fine, but calling them all oracles would misrepresent what T-5 proves.

## 9. Architecture Decisions

| ID | Decision | Alternative rejected | Why |
|---|---|---|---|
| D-1 | Runtime-observable build identity (version + exe path/size/mtime) | `build.rs` baking a git SHA | A SHA lies on a dirty tree — the case that matters most. Also avoids build-script cache staleness and MSRV exposure |
| D-2 | Correct `sub_interval_secs`; add `cap_secs` | Gate the event on the cap binding | Gating deletes the only durable record that the band was entered |
| D-2b | Reuse the field name with corrected meaning | New field, deprecate the old | No consumer outside the emitter (§ 3); preserving a value that has been wrong 624 times is not a guarantee worth keeping |
| D-3 | Runbook cites the script | Runbook restates the steps | A paraphrase rots silently; a citation degrades visibly |
| D-4 | `first_sight` on `ReliabilityWire`, schema 13 | A `StatusResponse` field | Obligates golden regeneration + Swift fixture lockstep for a windowed distribution that does not belong on an instantaneous-state wire |
| D-4b | Nullable percentiles + sample count | Plain counts (`RefreshTokenLossWire` shape) | A zero-sample window is an empty subject; `p95 = 0` would assert perfect latency where nothing was measured |

## 10. Quality Requirements

PRD § 6's three Planguage tags carry over unchanged. `BuildAttributionCost` GOAL 0 is met by D-1 iff
the emitted line is greppable by one stable token (`event=daemon_build`) — which is PRD R-7, and is
why R-7 is a requirement rather than a nicety.

## 11. Risks and Open Questions

| ID | Risk | I×L | Disposition |
|---|---|---|---|
| K-1 | The operator runs a build from an uncommitted branch, so even D-1 cannot tie the log to a commit | 2×3=6 MEDIUM | **Accepted.** D-1 answers "which artifact", which is what § 2.1's successful proof answered. Tying to a *commit* needs the SHA-plus-dirty-marker increment in Q-1 |
| K-2 | `current_exe()` fails or resolves through a symlink to something unhelpful | 2×2=4 LOW | § 8 Error handling: degrade, never refuse to start |
| K-3 | D-2 changes a field's meaning and an unknown consumer breaks | 3×1=3 LOW | The § 3 grep is the evidence; it found none. Re-run it at implementation time — the grep is cheap and the tree moves |
| K-4 | The runbook rots when the script changes | 2×3=6 MEDIUM | Bounded by citing rather than paraphrasing. **Not eliminated** — nothing reconciles prose against a script, and this design does not add such a gate |
| K-5 | D-4's percentiles are computed over a window with too few samples to be meaningful | 3×2=6 MEDIUM | Publish `n` beside the percentiles; the `BlindEpisodesWire` census fields make the denominator visible rather than implied |
| K-6 | A reader takes D-4's `p95` for the § 6 `GOAL` figure and concludes the fleet is failing its SLI when it is not | 3×3=9 **HIGH** | **Mitigated by a named, buildable constraint, not by this prose alone** — Rule 1's `censored-population naming` note in `docs/specs/first-sight-readout.feature.md` is where #1488's implementer meets it: the wire field names must carry the censoring (a `breach_` prefix or equivalent), never a bare `p50` / `p95`. Recorded there because that spec is what the work item is built against, and a risk row no requirement or scenario reaches is not a mitigation. § 7 states the censoring in terms and names the population as the breach tail; the wire field names and the `reliability` render must repeat it at the point of use, not only here. OQ-3 is the real fix |

**Open questions**

- **Q-1** — Should a commit SHA be added later, and must it carry a dirty-tree marker? *Recommended:
  yes to both, as a separate increment; a SHA without the marker is worse than no SHA.* Not filed.
- **Q-2** — Should `daemon_build` also be emitted on config reload, not only process start? Deferred:
  the attribution question is per-process, and a reload does not change the binary.
- **OQ-3 — what source can grade `GOAL p95 ≤ 2 · poll_secs / N`? OPEN, and load-bearing.** § 7
  establishes that `observation_gap_*` is edge-triggered past the bound, so it can carry the `FAIL`
  criterion and the breach tail but can never represent the GOAL-met state. Grading the GOAL needs a
  source that records within-bound first sights — a per-swap first-sight sample emitted
  unconditionally, or a counter of within-bound sights to pair with the breach tail. Both are new
  instrumentation and neither is in this design's scope. **Consequence:**
  `active-account-observation-continuity` R-4/R-5 stay `NEAR-COMPLETE`: the readout *home* is decided
  (D-4), the `FAIL` criterion is **bounded from above** (the emitted set is a superset of it, so a
  filter at `2T` over-reports rather than misses — § 7 states the mid-tenure case that makes it a
  bound and not an equality), and the `GOAL` percentile has no source at all. Not filed —
  it needs the operator's call on whether the GOAL half is worth new instrumentation at all.

## 12. Forward Coverage — requirement to design element

| Requirement | Design element | Test | Status |
|---|---|---|---|
| R-1 | D-1, B-1, B-2 | T-1, T-2 | covered |
| R-2 | D-2, D-2b, B-3, B-4 | T-3 | covered |
| R-3 | D-2 (no scheduling path touched) | T-4 | covered |
| R-4 | D-3, B-5 | manual | covered |
| R-5 | D-3 verification step (depends on D-1) | manual | covered |
| R-6 | T-5 | T-5 | covered |
| R-7 | D-1's stable `event=daemon_build` token | T-1 | covered |
| `active-account-observation-continuity` R-4/R-5 | **D-4**, D-4b, B-6 | T-6 | covered — this closes that PRD's § 6b deferral |

## 13. Backward Coverage — design element to requirement

| Design element | Requirement | Status |
|---|---|---|
| D-1 / B-1 / B-2 | R-1, R-7 | traced |
| D-2 / D-2b / B-3 / B-4 | R-2, R-3 | traced |
| D-3 / B-5 | R-4, R-5 | traced |
| D-4 / D-4b / B-6 | `active-account-observation-continuity` R-4, R-5 | traced |

No orphan design elements.
