---
title: Daemon Diagnostic Integrity — Attributing, Trusting, and Deploying What the Daemon Reports
scope: daemon-diagnostic-integrity
created: 2026-09-04
status: draft
dor_status: passed-with-findings
source: investigation of "why we've missed so many 5h limits in the last days", run via `/investigate` at HEAD 9faa61b (2026-09-03T04:36:32Z); its six axis reports are machine-local scratch under `.tmp/`, not committed artifacts, so every figure they support is replicated in-band in § 2
parent-requirements: none — this PRD's requirements originate in the investigation named in `source`, not in an upstream requirement family
appetite: small batch — three independent, additive changes with no shared surface; operator-ratified 2026-09-04 as scope membership (option B, "all enriched")
formulation: {technical-architecture: complete, testing-architecture: complete, api-design: complete}
features:
  build-identity-stamp: {stage: design, tracks: {technical-architecture: complete, testing-architecture: complete}}
  truthful-sub-interval: {stage: design, tracks: {technical-architecture: complete, testing-architecture: complete}}
  deployment-runbook: {stage: design, tracks: {technical-architecture: complete}}
artifacts:
  design-doc: docs/design/daemon-diagnostic-integrity-solution-design.md
---

# PRD — Daemon Diagnostic Integrity

> **Provenance.** Authored by an AI pipeline (`/scope` Stage 1) from a `/investigate` forensic report
> plus first-party evidence the operator's own daemon produced. **One thing is operator-ratified**:
> the scope membership (§ 1b) — the operator selected these items on 2026-09-04. **Every requirement
> below is pipeline-authored and ratification-pending**; each carries an explicit `Origin` tag, and
> § 11 records the split. Every empirical claim in § 2 was re-verified against the tree at HEAD
> `9faa61b` during authoring rather than inherited from the investigation report — § 10 names which.
> **That pass was necessary and not sufficient**: an independent review round re-derived the same
> claims and found two of them wrong — `version_line()`'s return shape (§ 2.1) and `base` being a
> constant rather than a per-sub-interval jitter draw (§ 2.2). Both are corrected in place. Read a
> re-verification claim in this document as "checked twice", never as "cannot be wrong".

## 0. What this PRD owns, and what it deliberately does not

Three findings from the 5h-limits investigation share one root and are filed here; a fourth,
adjacent finding is **explicitly not** filed here, and saying so is the point of this section.

**The shared root**: the daemon's self-report is the only durable record of what it did — the
forensics in that investigation ran entirely off the event log, because nothing else survives a
restart. A self-report is load-bearing evidence, and three distinct properties of it are broken:

| Property | Broken how | Requirement |
|---|---|---|
| **Attributable** — a log line can be tied to the build that wrote it | The daemon stamps no version, commit or path, ever | R-1 |
| **Truthful** — an emitted value equals the value the code used | `near_limit_poll_coverage` emits the *cap* while the scheduler used the *effective* interval | R-2, R-3 |
| **Deployable** — the operator can put the build they intend into service | `cargo build` replaces nothing that runs; the real procedure is undocumented | R-4, R-5 |

**Not owned here — the observation-gap readout.** The investigation also found that
`observation_gap_enter` / `observation_gap_exit` reach no JSON wire, so the post-swap first-sight SLI
has no durable readout. That is **already a requirement elsewhere** — `must`, but **pipeline-authored
and ratification-pending**, exactly like this PRD's own (that document's § 11 files R-4/R-5 under
*Derived from measured evidence*, outstanding operator action; its § 1b ratifies only that the
surfacing half is sized separately, and R-5 is the surfacing half):
`docs/requirements/active-account-observation-continuity.md` **R-4 and R-5**, whose
§ 6b recorded the gap as `NEAR-COMPLETE — the event is specifiable; which readout carries it is a
Stage 2 decision`. **This commit closes the readout-*home* half of that decision and leaves the row at `NEAR-COMPLETE`** — *this PR's* solution design, at its § 7, shows the chosen source cannot carry the `GOAL` percentile at all. Separately, the *neighbouring* PRD's own design document states that the instrument is *"log-only in its first increment"*. (Both documents have a § 7 and they are different sections; every § 7 named in this paragraph after this point is this PR's.) Re-stating it here would give one requirement two homes. It is scoped as a
work item by the same `/scope` run that produced this PRD. Its *requirement* stays in that
neighbouring PRD; its *design decision* lands in **this** PRD's solution design, at § 7 — the
`/scope` run that closed the deferral is this one, so this is where the reasoning is.

> **Cross-PRD citation discipline.** Requirement IDs are per-document. `R-4` below is *this* PRD's
> deployment requirement. The observation-continuity PRD's `R-4` is a different requirement in a
> different document, and is always cited here with its document name attached.

## 1. Problem Statement

**Current state.** On 2026-08-28..08-31 the fleet spent 11h51m and 9h18m of two consecutive days with
no serving capacity. Establishing *what code was running* during that window took an entire
investigation axis and three independent indirect proofs, because the daemon records nothing about
itself. Separately, a diagnostic emitted 624 times over the corpus asserts a poll tightening that
never occurred. And the operator's mental model of deployment — `cargo build` — does not replace the
binary that runs.

**Affected.** The operator, who is also the sole maintainer and the sole on-call. Every one of these
costs them at exactly the moment they can least afford it: during an incident.

**Why now.** The 5h-limits investigation is the second post-hoc investigation to hit the attribution
wall. The cost is not one-off; it is levied on every future investigation, and it compounds — the
older the incident, the weaker the indirect proofs.

### Problem framing — what was challenged

- **Observation vs interpretation.** *"We missed so many 5h limits"* was the operator's framing. The
  forensics falsified it: on 08-29 and 08-30 the session-caused no-capacity hold time was **zero** —
  the fleet was weekly-blocked. The problems filed here are the ones the investigation could
  *prove*, not the ones the original framing predicted. **No requirement here is derived from the
  original framing.**
- **Symptom or root cause.** All three are causes, not symptoms — but of *investigability*, not of
  the outage. None of them would have prevented the 08-29/08-30 capacity loss, and this PRD does not
  claim they would. They lower the cost of the next investigation and remove one active falsehood.
- **Prevention vs solution.** R-1 is pure prevention: it costs one log line at startup and retires a
  whole class of future forensic work. That asymmetry is why it leads.
- **Implicit constraint surfaced.** The operator deploys by hand, on one machine, from a checkout.
  There is no release train and no fleet — so "deployable" here means *one person, one Mac, at 2am*,
  not a rollout system.

## 1b. Boundaries

### Appetite

**Small batch.** Three independent additive changes sharing no code surface. Operator-ratified
2026-09-04 as scope membership; the sizing is pipeline-proposed and ratification-pending. Nothing
here is on the critical path of an outage fix, so the appetite is deliberately *not* elastic — if any
item grows past the batch, it is cut rather than funded.

### Out of Scope — with the reason each is excluded

| Excluded | Why |
|---|---|
| **Lowering `near_limit_poll_secs`** | That is `#1458`'s scope, conditional on operator-owned `{T}`/`{D}`. The cap is *shared* — lowering it tightens the whole tick, a rate change `ADR-0012` Decision 3 forbids buying silently. R-2/R-3 make the diagnostic *truthful*; they must not make the schedule *faster*. This is the sharpest boundary in the document. |
| **The capacity / provisioning question** | `#726` records Q1 as NO-GO and Q2 as provisioning-deficit-dominated, with `#792` as its named falsifier. No scheduling or diagnostic change reaches it. |
| **The observation-gap readout** | Already `active-account-observation-continuity` R-4/R-5 — see § 0. |
| **Peer-coverage repair on schedule invalidation** | `#1464`, already OPEN. Enriched with this investigation's measured evidence; not duplicated. |
| **Target-reading staleness bound** | `#1456`, correctly BLOCKED on operator-owned `{T}`. |
| **Session warm-up on reset** | `#1232`. Cannot buy weekly quota, and the outage was weekly. Its stale blocker banner is cleared as an edit, not scoped as work. |
| **The `exhausted_slow_polling` trade** | Settled in `ADR-0019`. The sibling `WEEKLY_TAIL_MARGIN` is a swap-decision constant (`src/swap.rs:218`, `= 0.01`) that ADR never mentions; it is equally out of scope, but on its own ground. Reopening either needs a new falsifier, which this investigation did not produce. |
| **Automated deployment** | R-4/R-5 are a *runbook*. A release pipeline for a single-operator single-machine daemon is not warranted, and proposing one would be scope the operator did not select. |

## 2. Evidence

Every figure below was re-derived from the tree at HEAD `9faa61b` during authoring. Where the
investigation report is the only source for a runtime figure, that is stated.

### 2.1 The daemon stamps no build identity — CONFIRMED at source

- There is **no** `build.rs` in the crate root, and no `vergen`-style dependency.
- `grep -rnE 'CARGO_PKG_VERSION|GIT_COMMIT|GIT_SHA|vergen' src/` returns **zero** matches that stamp
  the daemon's own identity — and it is emphatically **not** a zero-hit grep, so read the qualifier
  rather than the exit code. **The great majority of what it returns is noise**: `vergen` matches as
  a *substring* of `divergence`, `divergent`, `Divergence`, `KnownDivergence` and `convergence`,
  none of which has anything to do with build stamping. `GIT_COMMIT` and `GIT_SHA` are true zeros,
  and there is no `vergen` dependency. Every genuine hit is a `CARGO_PKG_VERSION` in `src/cli.rs`,
  and all of them are `--version` output. **No count is quoted here on purpose** — `src/` churns, so
  any tally would be stale on landing and would invite exactly the misreading this bullet exists to
  prevent; re-run the grep and read its shape. One of those hits is nearly reusable: `version_line()`
  in `src/cli.rs` (issue #175, and asserted by a test in that same file) wraps
  ``concat!("sessiometer ", env!("CARGO_PKG_VERSION"))`` — the value D-1 needs. **It is not
  directly reusable, and this is a real constraint on D-1:** `version_line()` returns a **two-line**
  `String`, appending `cc_version::supported_range_provenance()` after a `\n`. Emitting it verbatim
  would split one event across two log lines and break R-7's single-line contract. D-1 must take the
  `concat!` expression — or a shared constant factored out of it — never `version_line()` itself.
- There is **no** daemon-startup **event** at all — no `daemon_start`, no `startup`, no
  `daemon_started` among the event names rendered in `src/observability.rs`. Read that as the
  event-name claim it is, not as a token search: the literal string `startup` *does* occur there, in
  doc-comment prose about the near-limit fault preflight (*"startup preflight"*, *"startup probe"*),
  none of which emits anything at process start. The bullet above teaches this distinction for
  `vergen`; it applies here too.

**Consequence, measured.** Establishing that the running daemon predated the `#1451`..`#1455` series
required three independent indirect proofs: an `lsof` binary probe plus a `strings` canary; the
`reliability` wire reading schema `11` where `df9796b` bumps it to `12`; and zero `observation_gap_*`
lines across 45,655 log lines. **Three out-of-band artifacts to answer one question**, two of which
require the process to still be alive. The third does not, which is the only reason the question was
answerable at all — and it worked by accident, not by design.

### 2.2 `near_limit_poll_coverage` asserts a tightening that never happens — CONFIRMED at source

Two lines, both in `src/daemon.rs`:

- **What is emitted** (`Event::NearLimitPollCoverage`): `sub_interval_secs: self.near_limit_poll_secs`
  — the **cap**.
- **What the scheduler uses** (`next_subinterval`): `base.min(Duration::from_secs(self.near_limit_poll_secs))`
  — the **effective** interval, where `base = poll_secs / N`.

At the live configuration `poll_secs = 300`, `N = 8`, so `base` averages `37.5 s` and
`min(37.5, 60) = 37.5 s`. The event reports `sub_interval_secs=60`. The doc comment on
`near_limit_poll_secs` states the `min(...)` form correctly — **the code and its own documentation
are right; only the emitted value is wrong.**

**`base` is a random variable, not the constant 37.5.** `next_subinterval` divides the output of
`next_poll_interval`, which *draws* against the daemon's `rng`; `poll_secs` is the one tunable that
jitters by default (σ ≈ 20% of the default), and that function's own doc comment states each
sub-interval draws a fresh interval before dividing. So `37.5 s` is `base`'s **mean**, and the cap
binds whenever a draw exceeds `N × near_limit_poll_secs` = 480 s — far-tail, not never. Two
consequences carried into R-2/R-3: the report must be **captured at the compute site and carried**
(re-deriving it at the emit site advances the RNG and perturbs the schedule, which R-3 forbids), and
it needs **sub-second resolution**, since the integral `sub_interval_secs` cannot express `37.5` and
would truncate a truthful report to `37`.

**Magnitude.** The gap is ≈22.5 s, and at the live configuration the cap binds only on the far tail
of the draw. Emitted **624 times** over the investigated corpus *(count from the investigation's log sweep — the log is
machine-local and not a committed artifact)*.

**Why this is worse than a cosmetic defect.** It is emitted at band entry — the near-limit band, the
exact regime an operator inspects during an incident. A reader who trusts it concludes the daemon
tightened its polling when it did not, and stops looking. It is a diagnostic that lies at the one
moment diagnostics matter.

### 2.3 `cargo build` deploys nothing — CONFIRMED at source

The daemon runs as an `SMAppService` agent **inside the app bundle**: `managed_by
com.apple.xpc.ServiceManagement`, program identifier `Contents/Helpers/sessiometer`, parent bundle
`org.sessiometer.menubar`. There is **no** `~/Library/LaunchAgents` plist to re-point.

`apps/menubar/scripts/release-macos.sh` exists and accepts `--sign-only` (in its usage block and its
argument parser — *"signed
(--sign-only); skipping notarize."*). The working procedure is: rebuild and re-embed the bundle, then
either relaunch the app (which fires `reconcileDaemonAgentRegistration()`) or
`launchctl kickstart -k gui/$(id -u)/org.sessiometer.agent`.

**The hazard that must be written down**: the script `rm -rf`s the directory the running bundle sits
in — literally `rm -rf .build/Build/Products/Release`, relative to `apps/menubar/` — before
rebuilding. Naming the path matters because this is an acceptance criterion: *"the running bundle's
directory"* is an inference a reader has to make, and the operator needs the string. Anyone reasoning from `cargo build` alone will conclude their fix is live when the old
binary is still serving — which is precisely the confusion the 08-28..08-31 attribution problem
turned into a multi-hour question.

## 3. Object Model (OOUX)

| Object | Core concept | Key attributes | CTAs |
|---|---|---|---|
| `BuildIdentity` | Which artifact is running | version, commit, binary path, build profile | stamp, read-back |
| `DiagnosticEvent` | A durable line asserting a fact about daemon behaviour | event name, asserted value, the value the code used | emit, verify |
| `PollSubInterval` | The spacing the scheduler actually applied this tick | `base = poll_secs / N`, `cap = near_limit_poll_secs`, `effective = min(base, cap)` | compute, report |
| `DeployedDaemon` | The binary actually in service | bundle path, agent registration, running build identity | rebuild, re-register, verify |

## 4. Requirements (EARS)

| ID | Origin | Requirement | Priority |
|---|---|---|---|
| **R-1** | `[AI-inferred-expansion]`; scope membership `[operator-ratified 2026-09-04]` | WHEN the daemon starts, the system SHALL emit one durable event recording its own build identity — at minimum a version and a commit — **BUT NOT** requiring the process to still be running to read it, and **BUT NOT** emitting any credential, token, or roster content. | must |
| **R-2** | `[AI-inferred-expansion]`; scope membership `[operator-ratified 2026-09-04]` | WHERE `near_limit_poll_coverage` reports a sub-interval, the system SHALL report the interval the scheduler **actually applied** (`min(poll_secs / N, near_limit_poll_secs)`) — **BUT NOT** the configured cap when the cap did not bind. | must |
| **R-3** | `[AI-inferred-expansion]` | The change in R-2 SHALL NOT alter the poll schedule, the per-tick spacing, the aggregate request rate, or the `near_limit_poll_secs` value — **BUT NOT** by lowering the cap, which is `#1458`'s scope and a rate change `ADR-0012` Decision 3 forbids buying silently. | must |
| **R-4** | `[AI-inferred-expansion]`; scope membership `[operator-ratified 2026-09-04]` | The repository SHALL carry a deployment runbook stating the procedure that actually replaces the running daemon, **including** that the daemon is an `SMAppService` agent inside the app bundle and that `cargo build` alone replaces nothing — **BUT NOT** describing a release pipeline that does not exist. | must |
| **R-5** | `[AI-inferred-expansion]` | The runbook in R-4 SHALL state a **verification step** by which the operator confirms the intended build is the one now serving, and SHALL warn that the release script removes the running bundle's directory before rebuilding. | must |
| **R-6** | `[AI-inferred-expansion]` | Each of R-1, R-2 SHALL carry a test that **fails against the pre-change tree** — for R-2, one asserting the emitted value equals the effective interval under a configuration where cap and base differ. | must |
| **R-7** | `[AI-inferred-expansion]` | WHERE R-1's identity is emitted, it SHALL be greppable from the event log by a single stable token, so that attribution needs **no** out-of-band artifact. | should |

## 5. Acceptance Criteria (GWT + BUT NOT)

**R-1 / R-7 — the daemon says what it is**

> **Given** a daemon started from a known build,
> **When** its event log is read afterwards — including after that process has exited,
> **Then** a single line names the version and commit that produced it,
> **And** that line is reachable by one grep for a stable token.
> **BUT NOT** requiring `lsof` on a live process, `strings` on a binary, or a schema-constant
> cross-reference — the three out-of-band artifacts § 2.1 measured;
> **BUT NOT** carrying any credential, token, account UUID, or roster content;
> **BUT NOT** emitted only on a clean start — a restart after a crash is exactly when it is needed.

**R-2 / R-3 — the diagnostic tells the truth**

> **Given** a configuration where `poll_secs / N` and `near_limit_poll_secs` differ — the live
> configuration, where they average 37.5 s and 60 s — **and** the jitter draw pinned, so the applied
> interval is a decidable value rather than a fresh random one,
> **When** the daemon enters the near-limit band and emits `near_limit_poll_coverage`,
> **Then** the reported sub-interval equals the interval the scheduler applied on *that tick*, not
> the cap (60 s), at sub-second resolution,
> **And** the schedule, per-tick spacing, and aggregate request rate are unchanged.
> **BUT NOT** by lowering `near_limit_poll_secs`;
> **BUT NOT** by removing the event, which is the only durable record that the band was entered;
> **BUT NOT** silently changing what an existing log line means without the change being greppable in
> the diff.

**R-4 / R-5 — the operator can deploy what they built**

> **Given** an operator with a fix built locally,
> **When** they follow the runbook,
> **Then** the intended build is the one serving,
> **And** they can verify that from the daemon's own output rather than by inference.
> **BUT NOT** by a procedure that leaves `cargo build` looking sufficient;
> **BUT NOT** omitting that the release script `rm -rf`s the running bundle's directory;
> **BUT NOT** documenting a release pipeline that does not exist.

**R-6 — the oracles redden**

> **Given** the tree **before** each change,
> **When** each test runs,
> **Then** it **FAILS**.
> **BUT NOT** accepted as evidence if it passes pre-change — a detector that does not redden against
> the corpse falsifies the diagnosis rather than confirming it.

## 6. Quality Attributes (Planguage)

```
TAG:    BuildAttributionCost
SCALE:  number of out-of-band artifacts required to attribute a log line to the build that wrote it
METER:  count the artifacts consulted, replaying the 2026-08-28..08-31 attribution question
PAST:   3  (lsof binary probe + strings canary; reliability schema-constant cross-reference;
           absence of observation_gap_* across 45,655 lines) — two of the three require the
           process to still be alive
GOAL:   0  (answerable from the event log alone)
FAIL:   any > 0
```

```
TAG:    DiagnosticTruthfulness
SCALE:  absolute difference, in seconds, between the sub-interval a near_limit_poll_coverage line
        reports and the sub-interval the scheduler applied on that tick
METER:  a hermetic test at a configuration where cap and base differ; plus a sweep of the event log
PAST:   ~22.5 s (reports 60 s; applied min(base, 60) where base averages 300/8 = 37.5 s and is a
        fresh jitter draw each sub-interval), on 624 emissions
GOAL:   0
FAIL:   any > 0
```

```
TAG:    ScheduleRateNeutrality
SCALE:  aggregate /usage requests per hour, roster-wide, and per-tick spacing
METER:  the existing #80/#366 stagger locks, run pre- and post-change
PAST:   unchanged is the requirement, not an improvement target
GOAL:   delta == 0   (R-2 corrects a report; it must not move the schedule)
FAIL:   any change
```

## 6b. Feature Completeness

| Feature | Verdict | Gap |
|---|---|---|
| Build-identity stamp (R-1, R-7) | **NEAR-COMPLETE** | Requirement and oracle are exact. **The emission mechanism is now DECIDED, by the solution design shipping alongside this PRD** (D-1: runtime-observable facts, no `build.rs`) — this row was authored before it, and § 11 carries the current state. What remains genuinely open is narrower: whether the stamp also carries dirty-tree state (design § 11 Q-1). The crate has no `build.rs`, so a commit SHA would need one added (or the claim narrowed to a version plus binary path). Both options were viable; D-1 chose the second, here. |
| Truthful sub-interval report (R-2, R-3) | **COMPLETE** | Both sites identified in `src/daemon.rs` — `Event::NearLimitPollCoverage` emits, `next_subinterval` computes; the fix is to **carry** the value `next_subinterval` applied to the emit site, since re-deriving it there would advance the RNG (R-3 forbids the rate change). Two open sub-decisions land at Stage 2, and neither is merely cosmetic: the field needs sub-second resolution (integral seconds truncate 37.5 to 37), and whether to retain the integral field for compatibility. Gating the event on the cap binding is already rejected in the design. |
| Deployment runbook (R-4, R-5) | **NEAR-COMPLETE** | Procedure verified end to end. **R-5's verification step depends on R-1**: without a build stamp there is no first-party way to confirm which build is serving, so the runbook's verification degrades to inference. Sequencing consequence, recorded in § 8 A-3. |
| Pre-change oracles (R-6) | **COMPLETE** | R-2's RED state is exact and cheap — assert against a configuration where cap and base differ. |

## 7. Success Criteria

| # | Criterion | How it is measured | Why this one |
|---|---|---|---|
| S-1 | The next post-hoc investigation attributes its window to a build **from the event log alone** | § 6 `BuildAttributionCost` — count of out-of-band artifacts, target 0 | This is R-1's whole value, and it is falsifiable the first time it is needed. Leading indicator: the stamp appears in a fresh log. |
| S-2 | No `near_limit_poll_coverage` line disagrees with the applied interval | § 6 `DiagnosticTruthfulness`, from a log sweep | Binary, first-party, needs no new instrumentation beyond the fix. |
| S-3 | A deploy performed from the runbook ends with the intended build serving, confirmed by S-1's stamp | The runbook's own verification step | Lagging indicator, and the one that proves R-1 and R-4 compose rather than each being locally green. |

**Decision gate.** If S-2 cannot be shown from a post-change log sweep within one week of the change
landing, the fix did not reach the emission path and the item reopens.

## 8. Assumption Registry

| ID | Assumption | Origin | Confidence | Cheapest test | If false |
|---|---|---|---|---|---|
| A-1 | The 624-emission count and the 45,655-line corpus size are accurate | investigation log sweep — machine-local, not committed | 🟡 | Re-run the sweep on the live log | The magnitude changes; the defect does not — it is proven at source (§ 2.2), independent of any count |
| A-2 | A commit SHA can be stamped without disturbing the build | `[AI-inferred-expansion]` | 🟡 | Add a minimal `build.rs`, confirm `cargo build`/`clippy`/MSRV stay green | R-1 narrows to version + binary path, which still beats 3 out-of-band artifacts |
| A-3 | R-5's verification step will have R-1's stamp to read | derived from § 6b | 🟢 | Sequencing only | The runbook ships with an inference-based check and is amended when R-1 lands. **This is a dependency, not a blocker** |
| A-4 | Correcting the emitted value breaks no downstream consumer | `[AI-inferred-expansion]` | 🟡 | `grep -rn 'near_limit_poll_coverage'` across `src/` and the Swift app | If a consumer parses it, the fix carries that consumer — the grep at authoring time found only the emitter, one unit test, and one daemon-side assertion, and **no Swift surface** |
| A-5 | `poll_secs = 300`, `N = 8` are the live values, so the cap binds only on the far tail of the jitter draw (`base > 60 s` needs a draw `> 480 s`, ≈3σ) | investigation + config + `next_subinterval` | 🟡 | Read the live config; the jitter default is in `src/config.rs` | The cap is *not* dead: on a binding draw the event is truthful, so the defect is "truthful only sometimes" — still a defect, and a test pinned to a fixed 37.5 would be flaky against a live schedule |
| A-6 | The no-capacity hold-time figures are accurate — 11h51m (49.4%) and 9h18m (38.8%) on 2026-08-29/08-30, and the **0h00m** session-attributable share on both days | investigation hold-time analysis — machine-local, not committed | 🟡 | Re-run the hold-time sweep against the live event log; the fleet-state probe at 2026-08-29 12:00Z (4 of 7 accounts at weekly ≥ 0.98) is the independent corroboration | This PRD's largest **negative** decision inverts. The 0h00m share is the whole warrant for filing nothing against the capacity question (§ 0); if session-attributable hold time was material after all, the verdict was not "weekly-caused" and a session-side item may be owed. Registered separately from A-1 because a *negative* decision resting on an unverifiable figure is the one most likely to go unrevisited |

### Premortem (de-anchored — blind spots the ISO sweep cannot enumerate)

- **R-1 becomes a fingerprinting surface.** A build stamp in a log the operator may share while
  asking for help discloses their exact build. Low harm here (single-operator, own machine), but the
  requirement's `BUT NOT` clause already forbids credentials; the version/commit disclosure is
  accepted deliberately rather than overlooked.
- **R-2 is fixed in the wrong direction.** The cheapest-looking fix is to delete the misleading
  field. That destroys the only durable record that the band was entered — hence the explicit
  `BUT NOT` in § 5.
- **The runbook rots.** It documents a script's behaviour; the script can change. Nothing in this
  repo reconciles a runbook against a script, and this PRD does not add such a gate — the runbook
  cites the script rather than paraphrasing it, which bounds the rot without pretending to stop it.
- **All three land and the next incident is still unattributable** — because the operator was running
  a build from a branch that was never committed. R-1 mitigates this only if it stamps *dirty* state
  as well as a SHA. Called out as a Stage 2 input, not resolved here.

## 9. Cross-Cutting & Non-Functional Concerns

| # | Concern | Disposition |
|---|---|---|
| 9.1 | **Security** | R-1's emission is bounded by an explicit `BUT NOT`: no credential, token, account UUID, or roster content. Version and commit disclosure is accepted (see Premortem). No other item touches a credential surface. |
| 9.2 | **Compliance & Regulatory** | N/A — a single-operator local daemon with no regulated data. |
| 9.3 | **Reliability & Observability** | This PRD *is* the observability lane. R-3 constrains it: observability changes must not move runtime behaviour. |
| 9.4 | **Performance & Scalability** | R-1 is one line at startup. R-2 changes a value, not a code path. Cost is nil; `ScheduleRateNeutrality` (§ 6) is the gate that proves R-2 did not move the schedule. |
| 9.5 | **Operational** | R-4/R-5 are entirely operational. The pre-push and path-filtered CI gates in `CLAUDE.md` apply unchanged — `src/**` puts R-1/R-2 in the `rust` filter, so `test`, `msrv` and `deny` are all owed. |
| 9.6 | **Lifecycle** | No migration, no on-disk format change, no `FORMAT_VERSION` involvement. **No `STATUS_SCHEMA_VERSION` bump**: none of R-1..R-7 adds a field to `StatusResponse` or to a `watch` frame, so no status/watch golden, no Swift fixture sweep, no Swift edit. A-4's grep is the evidence for the Swift half. **No CLI-render golden either — on separate ground, not as a corollary of the above**: that class is keyed on *rendered output*, not on a schema constant. The three rendered verbs are `status`, `stats` and `reliability`, and no R-1..R-7 change reaches any of them (R-2's `near_limit_poll_coverage` has no consumer outside its emitter — `src/reliability.rs` never parses it; R-1's event is new). Where the two classes *do* come apart is D-4: see the design § 7. |

## 10. Source Traceability

| Requirement | Source | Re-verified at authoring? |
|---|---|---|
| R-1, R-7 | Investigation finding F12 | **Yes** — no `build.rs`, no `CARGO_PKG_VERSION`/`vergen` stamp, no startup event (§ 2.1) |
| R-2, R-3 | Investigation finding F-Obs | **Yes** — `Event::NearLimitPollCoverage` (emit) vs `next_subinterval` (compute), both `src/daemon.rs` (§ 2.2) |
| R-4, R-5 | Investigation Next Step 4 | **Yes** — `release-macos.sh:8,46` carries `--sign-only` (§ 2.3) |
| R-6 | `[AI-inferred-expansion]` — the project's own RED-oracle discipline | Convention, not a source claim |
| § 2.2 emission count (624), § 2.1 corpus size (45,655) | Investigation log sweep | **No** — machine-local runtime state; carried as A-1 |
| **The no-capacity hold-time figures — 11h51m and 9h18m on 2026-08-29/08-30, and above all the 0h00m session-attributable share** | Investigation hold-time analysis | **No** — machine-local runtime state, not re-derivable from a clone. Carried as **A-6**. Labelled explicitly because this PRD's largest *negative* decision rests on it: the 0h00m figure is the whole warrant for filing nothing against the capacity question. The least-checkable claim must not be the least-labelled one |
| The scope brief's remaining runtime figures (weekly-saturation share, `peak<10%` share, anchor-transition periods, the three consecutive-swap-gap populations, the warm-up cycle rate) | Investigation axis reports | **No** — same class as A-1 and A-6, machine-local. The brief marks its own two headline counts 🟡; this row covers the rest so the labelling is uniform rather than selective |

### A note on what the investigation got wrong, and this PRD does not inherit

The investigation's originating framing — *"we've missed so many 5h limits"* — was **falsified** by
its own forensics: session-caused no-capacity hold time on 2026-08-29 and 08-30 was zero, and the
fleet was weekly-blocked. No requirement in this PRD is derived from that framing. The items filed
here are the ones the forensics proved on their own terms.

## 11. Requirement Provenance (DoR check 6)

| Set | Members | Status |
|---|---|---|
| Operator-ratified | Scope membership: build-identity stamp, `near_limit_poll_coverage` truthfulness, deployment runbook — selected 2026-09-04 as option B, "all enriched" | **Ratified** |
| Pipeline-authored, ratification-pending | R-1 … R-7 in their specific form; the appetite; every Planguage GOAL and FAIL; S-1 … S-3 | **Pending** — the operator ratified *that these three things be worked*, not the precise form each requirement takes |

**Honest verdict**: `passed-with-findings`. Check 6 does not fully pass — the requirement *forms* are
pipeline-authored and have not been ratified one at a time. They are recorded as pending rather than
presented as ratified, which is what the check demands when granular ratification has not occurred.
The three items themselves trace to an explicit operator selection, so nothing here is a fabricated
requirement with no user ask behind it.

**Open findings carried into Stage 2** — and this list was written before the solution design that
ships alongside it, so two of its three entries are already **closed by that design**, not carried:

- R-1's emission mechanism — **DECIDED** (design D-1: runtime-observable facts, not a `build.rs` SHA).
- R-2's correct-vs-gate sub-decision — **DECIDED** (design § 4 rejects gating the event on the cap
  binding, and R-3's rate-neutrality constraint settles *how* the value reaches the emit site).
- Whether R-1 stamps dirty-tree state — **genuinely open** (design § 11 Q-1).

Two further open questions the design raises and this PRD inherits: the sub-second field shape for
R-2 (§ 2.2), and **OQ-3** — which source could grade `PostSwapFirstSightLatency`'s `GOAL` half at
all, on which `active-account-observation-continuity` R-4/R-5 remain `NEAR-COMPLETE`.
