---
title: Credential Grant Survival
scope: credential-grant-survival
created: 2026-08-14
status: draft
dor_status: passed-with-findings
source: session investigation 2026-08-14 (/investigate then /scope), live daemon event log (untracked runtime state), operator statements including the machine-B natural experiment
appetite: big batch — 2 weeks, with a hard 2-day sub-appetite on diagnosis + spike
formulation: {technical-architecture: complete, testing-architecture: complete, infrastructure: n/a}
artifacts:
  design-doc: docs/design/credential-grant-survival-solution-design.md
  design-brief: docs/briefs/2026-08-15-design-credential-grant-survival.md
  requirements-brief: docs/briefs/2026-08-14-requirements-credential-grant-survival.md
features:
  death-diagnosis: {stage: design, tracks: {technical-architecture: complete}}
  writer-attribution: {stage: design, tracks: {technical-architecture: complete}}
  recovery-discipline: {stage: design, tracks: {technical-architecture: complete}}
  health-truthfulness: {stage: design, tracks: {technical-architecture: complete}}
  fleet-observability: {stage: design, tracks: {technical-architecture: complete}}
  operator-runbook: {stage: design, tracks: {technical-architecture: complete}}
  poll-pressure: {stage: design, tracks: {technical-architecture: complete}}
  provenance-hygiene: {stage: design, tracks: {technical-architecture: complete}}
---

# PRD — Credential Grant Survival

> **Provenance warning, read before acting.** This PRD was authored by an AI pipeline
> (`/scope` Stage 1) from a live-log forensic investigation. The **13-item scope membership was
> explicitly ratified by the operator**; the requirement *text* below is pipeline-authored and
> carries `Origin` + `Ratification` per requirement. The **root cause is not established** — § 1c
> records what is evidence and what is hypothesis, and R-1/R-2 exist precisely because the cause is
> open. Do not read a requirement here as a claim that its mechanism is confirmed.

## 1. Problem

Sessiometer's promise is that the operator never thinks about credentials. Since **2026-08-11** it
has been producing the exact toil it exists to remove: roughly **one manual `claude /login` per
day**, on a six-account fleet.

**The decision variable — accounts that refresh unaided, per day (of 6):**

| Date | 08-06 | 08-07 | 08-08 | 08-09 | 08-10 | 08-11 | 08-12 | 08-13 | 08-14¹ |
|---|---|---|---|---|---|---|---|---|---|
| accounts OK | 6 | 6 | 5 | 6 | 5 | 5 | **3** | 4 | **2** |
| refresh attempts | 13 | 19 | 15 | 16 | 15 | **108** | **932** | 353 | 31 |

¹ to 09:00Z. Source: `~/Library/Logs/Sessiometer/sessiometer.log`, 26,401 lines, 2026-07-01 → 2026-08-14.

**`credential_unrecoverable` has fired three times in four days on three different accounts —
and not once in the preceding 41 days.** 08-11T22:08 (`.fr`), 08-12T21:20 (`.eu`),
08-14T04:14 (`.com`).

**Do not quote the per-attempt success rate.** It collapses 100% → 0.5%, but the denominator is
inflated by the recovery loop's own retries — 932 attempts against a ~15/day baseline is a **62×
amplification**, not 62× more work being attempted. The honest measure is accounts-alive-without-
intervention, above. Optimising the attempt rate would make a retry throttle look like a cure.

### The death that defines the problem

`oleksii@pelykhconsulting.com` — the operator relogged it at 08-13T11:56:48Z
(`restash … grant_replaced=true`). It was dead at 08-14T04:03:58Z. **It survived 16 h 07 m.**

At death, neither token had expired:

- access token valid to `2026-08-14T12:03:37Z` — **8 h remaining**, unchanged on every subsequent
  attempt from 04:05:45Z through 08:57:54Z
- refresh token valid to `2026-08-29T06:06:48Z` — **15 days remaining**

The *grant* was invalidated while both tokens were live. **This is not expiry**, and the daemon
cannot currently tell that it is not — see R-3.

### Two death shapes, not one

| Shape | Signature | Observed |
|---|---|---|
| **A — live grant invalidated** | 401 with substantial access-token life remaining | `.com`, 08-14 |
| **B — token lapsed, refresh cannot revive it** | `expires_before=2026-08-14T02:06:53Z`, still dead at 08:08 | `.consulting`, 08-14 |

The enumerated scope carried shape A only. Both are in scope; a fix verified against one is not
evidence about the other.

### 1b. Boundaries

**Appetite**: big batch — 2 weeks. Hard 2-day sub-appetite on `death-diagnosis` + the
`writer-attribution` spike, which gate the interpretation of everything downstream.
**Circuit-breaker**: if the spike cannot discriminate inside its box, ship `recovery-discipline` and
`fleet-observability` regardless — they are valuable under every hypothesis and must not wait on a
verdict.

**In scope**: diagnosis instrumentation; writer attribution; recovery-loop discipline; health-signal
truthfulness; fleet observability and its push surface; the operator runbook; poll-pressure
measurement; provenance hygiene (CC range, #1000, binary-resolution churn).

**Out of scope**:
- **Eliminating manual `claude /login`.** It is the irreducible remedy of last resort. This work makes
  it rarer and better-signposted; it does not remove it.
- **A cross-process or cross-machine credential lease.** #466 probed for a Claude Code knob and found
  none; #613 put cross-machine sync explicitly outside the product boundary. If the spike concludes
  the second writer is another machine, that conclusion routes to a **decision for the operator**,
  not to an implementation task inside this appetite.
- **Changing Anthropic's token-endpoint behaviour.** Rotation-on-every-exchange is a given (#262).
- **Menubar redesign.** R-17's cue is additive to the existing panel, not a re-layout.

**Standing negative**: no `user-stories` document — requirements are EARS + OOUX here; story-shaped
work lands as tracked items typed `story`.

### 1c. What is evidence, and what is hypothesis

This section exists because a previous pass of this investigation asserted a cause that the operator
falsified in one sentence. Read it before building on any mechanism.

**Established (A — self-verifying, from the log or the source):**

1. Fleet collapse 08-11 → 08-14, quantified above.
2. `credential_unrecoverable` 3× in 4 days; 0× in the preceding 41 days.
3. Both death shapes A and B occurred on 08-14.
4. **A second writer rotates these credential families ~2×/day, chronically since 2026-07-29.**
   All **26 of 26** `grant_replaced` observations are `true` (the refresh-token bytes differed);
   **24 of 26** carry a deadline delta of −2…+2 s, which is a *rotation*, not a login — a `/login`
   moves the refresh-token deadline ~26–29 days, and exactly one of the 26 does that.
5. Retry amplification: 932 attempts on 08-12 against a ~15/day baseline.
6. The daemon captures no 401 / `invalid_grant` sub-reason anywhere; revocation and expiry are
   therefore indistinguishable in every artifact it produces.
7. `ExpiryProvenance::ExternalChange` is, by the daemon's own documentation
   (`own_refresh_since_expiry_observation` in `src/daemon.rs`), *"a residual category rather than a positive finding"* — it means
   only *this process did not refresh*. It cannot gate anything in its present form.
8. `reason=unresolved` and refresh-binary re-resolution churn both cluster on 08-11…08-13
   (binary re-resolutions: ≈1/day baseline → 6, 16, 10).

**Falsified:**

- **CC version is not the cause.** Four Claude Code builds landed in the four collapse days, all
  outside the verified range 2.1.181–2.1.217, which made a compelling temporal correlation. The
  operator falsified it with a natural experiment: **a second machine runs CC 2.1.231 — also outside
  the range — with an older sessiometer, and is healthy.** Recorded because the correlation is
  seductive and will be re-derived by the next reader otherwise.

**Falsified — H2a, the cross-machine writer (2026-08-15, two-machine log comparison):**

The operator supplied the second machine's daemon log (24,281 lines, 2026-06-29 → 2026-08-15) and
config. The comparison is as controlled as this problem allows:

| | Machine A (this one) | Machine B |
|---|---|---|
| Accounts | the same six labels, same twelve account-uuid fragments | identical |
| `config.toml` | — | **byte-identical** after stripping comments |
| `[refresh] enabled` / `poll_secs` | `true` / `300` | `true` / `300` |
| Claude Code | 2.1.227→2.1.232 | 2.1.231 |
| Refresh outcomes 08-04…08-15 | collapsed to 0–2% on 08-12/13 | **979 of 979 `refreshed` — zero `no_change`, zero `dead`, zero `error`** |

**Same accounts, same grants, same server, same days — one machine at 100%, the other at 2%.** That
eliminates the accounts, the token endpoint, Claude Code's version, and the configuration. **The
fault is local to machine A.**

**H2a is separately refuted by direct correlation.** Of machine A's **77** foreign-write observations
(`external_change` + `restash`), **0 fall within ±60 s of any machine-B write event**. At ±300 s the
hit rate is 11% against a **9% chance baseline** computed over 2,000 uniformly-sampled times — i.e.
indistinguishable from coincidence. The other machine is not the writer this daemon observes.

**Open — narrowed to one prime suspect:**

- **H2b — same-machine concurrent `claude`** is now the leading hypothesis: finding #465's exact
  documented mechanism, at a scale its mitigations (#467, #468) were never sized for. Machine A runs
  `/do-all` worktree batches; at the time of writing **three batches are live declaring 9 concurrent
  streams, across 60 worktrees**, each subprocess a `claude -p` that reads and rotates the ACTIVE
  account's shared credential. Machine B runs no such batches. This is the one structural difference
  the comparison did not eliminate.
- **Counter-evidence, deliberately not suppressed.** Worktree *creation* does not cleanly predict
  failure. 08-11 created 18 worktrees (the peak) as success fell to 12% — but **08-15 created 14 and
  refreshed 4 of 4 successfully**, and 08-09/08-10 created 6–7 at 93–100%. So either creation count
  is a poor proxy for *concurrent* writer pressure (likely — a worktree opened Monday may still be
  running Wednesday), or a further factor is in play. Historical concurrency cannot be reconstructed
  from directory birthtimes, which is precisely why R-4 becomes a forward measurement rather than a
  retrospective one.

**Consequence for R-4**: the spike is no longer "which of two writers" but a **measurement** —
instrument concurrent `claude -p` count against refresh outcome going forward. The 24 h
single-machine pause is **superseded**: the two-machine comparison already answered what it would
have, without the wait.

## 2. Object Model (OOUX)

| Object | Core content | Key relationships |
|---|---|---|
| **Grant** | the OAuth credential family: refresh token (rotating), access token, family deadline | belongs to one *Account*; rewritten by ≥1 *Writer*; dies with a *Death* |
| **Account** | roster entry: uuid, label, park/active state, quarantine flag | holds one *Grant*; carries a *HealthState* |
| **Writer** | any process that writes the canonical keychain item — this daemon, a local `claude`, another machine's daemon, an operator `/login` | rewrites a *Grant*; currently only *inferred*, never identified |
| **RefreshAttempt** | one isolated-refresh cycle: outcome (`refreshed`/`no_change`/`dead`/`error`), window_secs, rotated | targets an *Account*; belongs to a *RecoveryEpisode* |
| **RecoveryEpisode** | the bounded sequence of attempts after an account is quarantined | groups *RefreshAttempt*s; must terminate |
| **Death** | an account transitioning to unusable: shape A or B, with a captured cause | ends a *Grant*; opens a *RecoveryEpisode* |
| **HealthState** | the per-account signal surfaced to CLI/menubar | derived from *Account* + last *RefreshAttempt*; must not derive from poll reachability |
| **FleetCapacity** | count of accounts alive without operator intervention | aggregate over *Account*s; the SLI |

### CTA inventory

| Object | Operator CTA | Surface |
|---|---|---|
| Death | see that it happened, and why | menubar cue + `status` |
| Death | know what to do next | runbook |
| FleetCapacity | see the floor approaching before the last account dies | push notification |
| Writer | learn which writer is rewriting a grant | `status` / log |
| RecoveryEpisode | see that recovery gave up, and when | `status` |

## 3. Requirements

### Feature: death-diagnosis

**R-1** — WHEN an account's credential operation returns HTTP 401 or an OAuth error body, the daemon
SHALL record the error's machine-readable sub-reason (e.g. `invalid_grant`, `invalid_token`,
`expired_token`) in the event log alongside the existing outcome.
*Origin*: scope item 7 (scope-ratified). *Ratification*: membership ratified; text pipeline-authored.

**R-2** — The daemon SHALL classify every `Death` as shape **A** (grant invalidated while the access
token had remaining life) or shape **B** (access token lapsed), and SHALL record the classification
and the remaining-life value that decided it.
*Origin*: evidence-derived (both shapes observed 08-14) + premortem P7. *Ratification*: pipeline-authored.

**R-3** — IF the sub-reason required by R-1 is not observable at the daemon's vantage point — because
`claude -p` does not surface it — THEN the work SHALL record that negative finding explicitly and
SHALL NOT substitute an inferred cause.
*Origin*: premortem P2. *Ratification*: pipeline-authored.
> This is the instrument-validity gate. R-1 assumes a capture point exists; R-3 is what happens when
> it does not, and it forbids the failure mode where an inference is logged in a field a reader will
> take for an observation.

### Feature: writer-attribution

**R-4** — The daemon SHALL record the count of concurrent local `claude` processes contending for a
given account's credential at the time of each refresh attempt, such that the correlation between
local concurrency and refresh outcome becomes measurable.
*Origin*: scope item 1 (scope-ratified), **reframed twice by evidence** — see § 1c.
*Ratification*: membership ratified.
> **H2a is refuted and this requirement no longer tests it** (0/77 correlation against a 9% chance
> baseline). The remaining question is local and quantitative, so the requirement is instrumentation,
> not an experiment. **Per premortem P1 the committed action is named**: a confirmed correlation
> re-opens finding #465's mitigation set (#467, #468) as under-sized for `/do-all` concurrency.
> The previously-specified 24 h single-machine pause is **withdrawn as superseded**.

**R-5** — WHERE a foreign write to a canonical credential is detected, the daemon SHALL distinguish a
**rotation** (refresh-token bytes changed, family deadline within ±5 s) from a **new grant**
(deadline moved by more than one day), and SHALL surface the two distinctly.
*Origin*: evidence-derived — the 24-of-26 vs 1-of-26 split is exactly this discriminator, computed
by hand for this PRD and currently computed nowhere in the product. *Ratification*: pipeline-authored.

**R-6** — The daemon SHALL strengthen `ExpiryProvenance::ExternalChange` from a residual category to
a positive finding, or SHALL document why it cannot be — such that a downstream consumer may act on
it without re-deriving the caveat at `own_refresh_since_expiry_observation` in `src/daemon.rs`.
*Origin*: evidence-derived (item 9). *Ratification*: pipeline-authored.

**R-7** — The work SHALL reconcile finding #465's verdict clause — *"not a server-side revocation"* —
against death shape A, and SHALL amend the finding or file a sibling.
*Origin*: contradiction C1 (scope item 9). *Ratification*: pipeline-authored.

### Feature: recovery-discipline

**R-8** — WHILE an account is quarantined, the daemon SHALL bound its refresh attempts by an
explicit, measurable rate, and the observed rate SHALL be asserted by a test.
*Origin*: scope item 2 (scope-ratified). *Ratification*: membership ratified.

**R-9** — The work SHALL resolve contradiction C2: `src/refresh_tick.rs:377-382` asserts the recovery
bypass *"cannot degenerate into the sub-poll retry storm ADR-0007 decided against"* and fires
*"at most once per idle period (poll cadence)"*, while quarantined `.com` was observed at **11
attempts in 27 m 34 s, mean spacing 2.6 min, against `poll_secs = 300`**. Either the code violates
its own invariant or "idle period" legitimately recurs faster than `poll_secs`. The work SHALL
measure which, and fix the code or the claim.
*Origin*: contradiction C2, evidence-derived. *Ratification*: pipeline-authored.
> Neither side may be assumed. The comment is a durable claim a future reader will trust.

**R-10** — WHEN a `RecoveryEpisode` exhausts its bounded attempts, the daemon SHALL enter a terminal,
operator-visible state and SHALL cease high-frequency retries, WITHOUT ceasing low-frequency
liveness re-probing.
*Origin*: scope item 2 (scope-ratified). *Ratification*: membership ratified.
> **The split is load-bearing, and it is evidenced.** `.eu` went `credential_unrecoverable` on
> 08-12T21:20, and at 08-13T07:09:53Z it recovered under `provenance=my_refresh` with
> `delta_secs=2089344` — the daemon's *own* retry re-anchored the family deadline 24 days forward,
> with no operator relogin. A hard circuit-breaker would have stranded an account that the loop
> genuinely rescued. R-8's bound must therefore throttle the loop, never terminate it. Any change
> here must reconcile with **ADR-0007**, which records four already-rejected recovery mechanisms.

**R-11** — The daemon SHALL NOT report `outcome=dead` and `outcome=no_change` alternately for the
same account within one `RecoveryEpisode` without recording what differed between the two attempts.
*Origin*: evidence-derived — `.com` hour 04 produced 5 `dead` and 6 `no_change` on one credential.
*Ratification*: pipeline-authored.

### Feature: health-truthfulness

**R-12** — The daemon SHALL derive `HealthState` from credential validity only, and SHALL NOT report
a credential-health value that is a function of usage-poll reachability.
*Origin*: scope item 3 (scope-ratified) + contradiction A2. *Ratification*: membership ratified.
> Measured 08-14: `.fr` produced **9 `healthy` and 9 `unknown`** transitions, each `unknown` paired
> with a `usage_backoff class=rate_limited` → `blind_enter`. The credential was never in question.

**R-13** — IF an account is returning 401 on its credential, THEN the daemon SHALL NOT report that
account as `healthy`.
*Origin*: scope item 3 (scope-ratified). *Ratification*: membership ratified.
> Measured: `.com` reported `state=healthy` at 08-14T05:08:37Z while 401ing continuously since
> 03:59:37Z, and produced five distinct health states in one day.

### Feature: fleet-observability

**R-14** — The daemon SHALL expose a fleet SLI defined as *the count of accounts that completed a
successful unaided refresh within the last rolling 24 h*, and SHALL NOT define it as a per-attempt
success rate.
*Origin*: scope item 10 (scope-ratified) + premortem P3. *Ratification*: membership ratified.

**R-15** — WHEN the R-14 SLI falls below an operator-configurable floor, the daemon SHALL raise a
**push** notification, not a queryable field alone.
*Origin*: scope items 4 + 10 (scope-ratified) + premortem P5. *Ratification*: membership ratified.
> The collapse ran seven days and was noticed by the operator, not by the system. A `--json` field
> nobody polls reproduces that failure exactly.

**R-16** — WHEN fleet capacity reaches one remaining live account, the daemon SHALL notify before the
last account dies.
*Origin*: premortem P6. *Ratification*: pipeline-authored.

**R-17** — The menubar SHALL carry a glance-level cue for `credential_unrecoverable`, distinct from
the lapsed-token cue tracked by #923.
*Origin*: scope item 13 (scope-ratified). *Ratification*: membership ratified.
> #923 is adjacent but scoped to the wrong axis — a *lapsed* token (shape B), not a *revoked* grant
> (shape A). Reuse its surface; do not reuse its semantics.

### Feature: operator-runbook

**R-18** — The repository SHALL carry an operator runbook answering *"an account died — what now"*,
covering both death shapes, the relogin procedure, and how to tell a transient failure from a
terminal one.
*Origin*: scope item 11 (scope-ratified). *Ratification*: membership ratified.

### Feature: poll-pressure

**R-19** — The work SHALL determine whether the usage-poll cadence is itself a material contributor
to rate-limit pressure on an account, and SHALL state the measurement that decided it.
*Origin*: scope item 12 (scope-ratified). *Ratification*: membership ratified.
> Context: 19 `usage_backoff class=rate_limited` episodes on `.com` in the 16 h between its relogin
> and its death. Correlation only — the direction of causation is unestablished, and the requirement
> is to measure it, not to assume it.

### Feature: provenance-hygiene

**R-20** — The work SHALL re-verify the isolated-refresh lifecycle assumptions (#101 AC-1…AC-6)
against the current Claude Code build and widen `CC_SUPPORTED_MAX` in `build/version-compat.md`, or
record why it cannot be widened.
*Origin*: scope item 5 (scope-ratified). *Ratification*: membership ratified.
> **Premortem P4 binds here.** #714's canary re-checks only the #100 keychain-service derivation. A
> green canary is **not** evidence about the refresh lifecycle, and widening the range on its
> strength would be a rubber stamp — a gate passing on a subject it never evaluated. This requires
> its own empirical walk.

**R-21** — Issue #1000 SHALL be discharged: the #262 refresh-token reuse observations recorded as a
finding, with the n=1 "no family revocation observed" verdict re-stated against the 2026-08-14 data.
*Origin*: scope item 6 (scope-ratified) + contradiction A1. *Ratification*: membership ratified.

**R-22** — The work SHALL determine why refresh-binary resolution churned during 08-11…08-13
(≈1/day → 6, 16, 10 re-resolutions) and whether it correlates with `reason=unresolved`.
*Origin*: scope item 8 (scope-ratified). *Ratification*: membership ratified.
> The original framing — *"stale `claude` binary path across a symlink re-point"* — is a hypothesis,
> not a finding. The resolved path was `/Users/alexey-pelykh/.local/bin/claude` on all 37 events.

## 4. Acceptance Criteria

**AC-1 (R-1, R-2)** — GIVEN an account whose grant has been invalidated server-side, WHEN the daemon
observes the failure, THEN the event log carries the OAuth sub-reason and a shape A/B classification
with its deciding remaining-life value. **BUT NOT** an inferred cause presented in the same field as
an observed one.

**AC-2 (R-3)** — GIVEN `claude -p` does not surface the sub-reason, WHEN the work concludes, THEN the
negative finding is recorded in `docs/findings/`. **BUT NOT** a partial implementation that logs a
placeholder sub-reason.

**AC-3 (R-4)** — GIVEN a week of ordinary `/do-all` batch activity with concurrency instrumentation
in place, WHEN the refresh outcomes are read against the recorded concurrent-`claude` count, THEN the
correlation is stated with its strength and its falsifier, and the committed action fires if
confirmed (re-open #465's mitigation set as under-sized). **BUT NOT** a verdict of "inconclusive,
cause is external" with no action — and **BUT NOT** a correlation claimed without a chance baseline,
which is what made the H2a refutation readable.

**AC-4 (R-5)** — GIVEN a foreign write, WHEN the daemon classifies it, THEN rotation and new-grant are
distinguished by the ±5 s / >1 day deadline rule and surfaced distinctly. **BUT NOT** a single
`external_change` tag covering both, which is the present behaviour.

**AC-5 (R-8, R-9, R-10)** — GIVEN a quarantined account, WHEN a `RecoveryEpisode` runs, THEN attempts
are bounded at a rate asserted by a test, the episode terminates into an operator-visible state, and
low-frequency liveness re-probing continues. **BUT NOT** a hard stop that strands a recoverable
account — `.eu` was rescued by the daemon's own retry 10 h after being marked unrecoverable.

**AC-6 (R-9)** — GIVEN contradiction C2, WHEN the work concludes, THEN either
`src/refresh_tick.rs:377-382` is corrected or the observed 2.6-min spacing is explained against
`poll_secs = 300`. **BUT NOT** the comment left standing unexamined.

**AC-7 (R-12, R-13)** — GIVEN an account 401ing on its credential WHILE its usage poll is
rate-limited, WHEN health is computed, THEN it reports neither `healthy` nor a value derived from
poll reachability. **BUT NOT** `unknown` used to mean "the poll is backed off".

**AC-8 (R-14, R-15, R-16)** — GIVEN fleet capacity falling from 6 to 2 over four days, WHEN the SLI
is evaluated, THEN a push notification fires at the configured floor and again at one remaining live
account. **BUT NOT** a `--json` field with no push surface.

**AC-9 (R-17)** — GIVEN an account in `credential_unrecoverable`, WHEN the operator glances at the
menubar, THEN a cue distinct from #923's lapsed-token cue is visible. **BUT NOT** a panel-only
disclosure requiring the panel to be opened.

**AC-10 (R-18)** — GIVEN an account has just died, WHEN the operator opens the runbook, THEN both
death shapes, the relogin procedure, and the transient-vs-terminal test are covered. **BUT NOT** a
runbook that assumes shape A only.

**AC-11 (R-20)** — GIVEN `CC_SUPPORTED_MAX` is to be widened, WHEN the widening lands, THEN it is
backed by a refresh-lifecycle walk, not by #714's keychain-derivation canary. **BUT NOT** a widening
justified by a green canary that never evaluated the lifecycle.

**AC-12 (R-21, R-22)** — GIVEN #1000 and the binary-churn question, WHEN the work concludes, THEN
#1000 is closed by a committed finding and the churn question is answered or explicitly deferred with
its signpost. **BUT NOT** closed by restating the hypothesis as a conclusion.

## 5. Quality Attributes (Planguage)

**QA-1 — Fleet survival**
- **SCALE**: accounts completing a successful unaided refresh within a rolling 24 h, of 6
- **METER**: `event=refresh outcome=refreshed`, distinct accounts per day
- **PAST**: 6 (08-06, 08-07, 08-09) · **NOW**: 2 (08-14 to 09:00Z) · **MUST**: ≥ 5 · **WISH**: 6

**QA-2 — Operator credential interventions**
- **SCALE**: manual `claude /login` events per week
- **METER**: `restash` with `grant_replaced=true` AND deadline delta > 1 day
- **PAST**: ~1 (07-31 was the only such event in 44 days) · **NOW**: ~7/week · **MUST**: ≤ 1/week

**QA-3 — Time to operator awareness of a fleet decline**
- **SCALE**: elapsed time from SLI breach to operator notification
- **METER**: notification timestamp minus first breaching sample
- **PAST**: 7 days (operator-noticed, not system-noticed) · **MUST**: ≤ 1 h · **WISH**: ≤ 15 min

**QA-4 — Recovery amplification**
- **SCALE**: refresh attempts per day at fleet level
- **METER**: `count(event=refresh)` per day
- **PAST**: 13–19 · **NOW**: 932 peak (08-12) · **MUST**: ≤ 60 · **WISH**: ≤ 30

## 5b. Feature Completeness

All 13 ratified scope items map to a feature; no feature exists without a ratified item.

| # | Scope item | Feature | Requirements |
|---|---|---|---|
| 1 | spike: why is a live grant revoked | writer-attribution | R-4, R-5 |
| 2 | recovery loop replays without back-off | recovery-discipline | R-8, R-9, R-10, R-11 |
| 3 | health oscillates; `healthy` while 401ing | health-truthfulness | R-12, R-13 |
| 4 | 7-day collapse produced no signal | fleet-observability | R-15 |
| 5 | CC outside verified range | provenance-hygiene | R-20 |
| 6 | #1000 record #262 observations | provenance-hygiene | R-21 |
| 7 | cannot distinguish revocation from expiry | death-diagnosis | R-1, R-2, R-3 |
| 8 | `reason=unresolved` clusters | provenance-hygiene | R-22 |
| 9 | #465 mitigations insufficient | writer-attribution | R-6, R-7 |
| 10 | fleet SLI + alert threshold | fleet-observability | R-14, R-15 |
| 11 | operator runbook | operator-runbook | R-18 |
| 12 | poll cadence as 429 driver | poll-pressure | R-19 |
| 13 | no glance-level unrecoverable cue | fleet-observability | R-17 |

**Premortem items folded in, not added** (membership stays at 13): P4 → R-20; P7 → R-2; P1 → R-4;
P2 → R-3; P3 → R-14; P5 → R-15; P6 → R-16.

## 6. Success Criteria

**North Star**: the operator performs at most one credential intervention per week (QA-2).

| Indicator | Type | Target |
|---|---|---|
| Accounts alive unaided (QA-1) | lagging | ≥ 5 of 6, sustained 7 days |
| Manual relogins per week (QA-2) | lagging | ≤ 1 |
| Time to awareness (QA-3) | leading | ≤ 1 h |
| Refresh attempts/day (QA-4) | leading | ≤ 60 |
| Deaths carrying a recorded sub-reason | leading | 100% of deaths after R-1 lands |
| Writer attributed | binary | H2a or H2b named with falsifier |

The leading indicators are the ones that move first: R-1 and R-4 are diagnostic, and QA-1 cannot be
expected to recover until the cause is known.

## 7. Cross-Cutting & Non-Functional

- **Security** — no requirement here may log, print, or persist a token value. The existing contract
  (`refresh_token` is *"only ever emptiness-checked or byte-compared, never logged"*) binds R-5's
  byte comparison and every diagnostic added under R-1.
- **Privacy** — operator account labels are already redacted per #463; new surfaces inherit that.
- **Reliability** — R-10 must not strand a recoverable account (`.eu` recovered after being marked
  unrecoverable). Reconcile with **ADR-0007**.
- **Observability** — R-1's sub-reason is the enabling capability for every other diagnosis here.
- **Portability** — macOS only; no cross-platform obligation (`CONTRIBUTING.md`).
- **Schema** — R-14's SLI and R-2's classification are new wire fields. Additive-with-
  `skip_serializing_if` implies no bump; a field always present requires one. Four independent
  wires exist — `STATUS_SCHEMA_VERSION`, and `JSON_SCHEMA_VERSION` separately in `src/log.rs`,
  `src/stats.rs`, `src/reliability.rs`. Design must name which it touches.
- **Compatibility** — R-17 crosses into Swift. `STATUS_SCHEMA_VERSION` changes require the Swift
  fixture sweep.

## 8. Ratification Ledger

| Item | Status | Note |
|---|---|---|
| 13-item scope membership | **ratified** | operator selected "all enriched (13)"; binding — none added, none dropped |
| The symptom (relogin too frequent) | **ratified** | operator-stated, twice |
| CC version is not the cause | **ratified** | operator-supplied natural experiment |
| R-1…R-22 text | pipeline-authored | each requirement's `Origin` records whether it traces to a ratified item |
| R-3, R-5, R-6, R-7, R-9, R-11, R-16 | **pipeline-authored, no ratified parent item** | derived from evidence or premortem rather than from the 13. They elaborate ratified items rather than extending scope, but a reviewer should confirm that reading |
| Appetite (2 weeks; 2-day diagnosis sub-appetite) | pipeline-authored | § 1b |
| QA targets (MUST/WISH values) | pipeline-authored | § 5 — thresholds are proposals, not operator-set |

**No operator action outstanding.** The 24 h single-machine pause previously recorded here is
**withdrawn** — the two-machine log comparison (§ 1c) answered what it would have, without the wait.
R-4 is now passive instrumentation over batch activity that already happens; it requires no
behavioural change from the operator.

## 9. Assumption Registry

| # | Assumption | Importance | Evidence | Verdict | Cheapest test | Signpost |
|---|---|---|---|---|---|---|
| A-1 | A capture point for the OAuth sub-reason exists at the daemon's vantage | **high** — R-1 and most of the PRD rest on it | none | **test** | probe one 401 through `claude -p` and inspect what surfaces | R-3 fires if absent |
| A-2 | ~~The second writer may be the other machine~~ — **RESOLVED, refuted**: 0/77 correlation vs a 9% chance baseline; it is local | high | two-machine log comparison | **decided** | — | — |
| A-3 | Rotation-on-every-exchange still holds | medium | #262: 141 `rotated=true`, 0 false | defer | re-count `rotated=` in the current log | a `rotated=false` appears |
| A-4 | Local `claude -p` concurrency drives the failure | high — now the leading hypothesis | 08-11 worktree peak coincides with onset; **but 08-15 contradicts the dose-response** | **test** | R-4's concurrency instrumentation | correlation absent over a week of measurement |
| A-5 | Shape B (lapsed token) shares a root cause with shape A | medium | both appeared 08-14 | test | classify per R-2 over a week | the two rates diverge |
| A-6 | ~~`.eu`'s recovery was an operator relogin~~ — **RESOLVED, and false**: it recovered at 08-13T07:09:53Z under `provenance=my_refresh`, `delta_secs=2089344`. The daemon's own retry rescued it | high — it constrains R-10 | log, verified | **decided** | — | — |
| A-7 | 2 weeks is enough given the cause is unknown | medium | none | **hedge** | — | circuit-breaker in § 1b |

## 10. Source Traceability

| Requirement | Source |
|---|---|
| R-1, R-2, R-3 | scope item 7; premortem P2, P7; log — no sub-reason field exists |
| R-4 | scope item 1; operator machine-B experiment; log — 26 foreign writes |
| R-5 | log — 24/26 rotation vs 1/26 login deadline split |
| R-6 | `own_refresh_since_expiry_observation` in `src/daemon.rs` residual-category caveat |
| R-7 | `docs/findings/0465-multi-session-rotation-interference.md` verdict clause; contradiction C1 |
| R-8, R-10, R-11 | scope item 2; log — 932 attempts 08-12; `.com` hour-04 dead/no_change alternation |
| R-9 | `src/refresh_tick.rs:377-382`; `docs/adr/0007-decided-against-credential-recovery-options.md`; contradiction C2 |
| R-12, R-13 | scope item 3; log — `.fr` 9 healthy/9 unknown; `.com` `healthy` at 05:08:37Z while 401ing |
| R-14, R-15 | scope items 4, 10; premortem P3, P5 |
| R-16 | premortem P6 |
| R-17 | scope item 13; issue #923 (OPEN) |
| R-18 | scope item 11 |
| R-19 | scope item 12; log — 19 rate-limit episodes in `.com`'s final 16 h |
| R-20 | scope item 5; `build/version-compat.md`; issues #714, #716, #101 |
| R-21 | scope item 6; issue #1000 (OPEN); issue #262 |
| R-22 | scope item 8; log — 37 `refresh_binary_resolved`, all resolving the same path |
