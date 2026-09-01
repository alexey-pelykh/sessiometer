---
title: Credential Grant Survival — Solution Design
scope: credential-grant-survival
created: 2026-08-15
status: draft
source: docs/requirements/credential-grant-survival.md
workflow: /design-solution
---

# Solution Design: Credential Grant Survival

## 1. Goals and Drivers

Restore the property that the operator does not think about credentials. Concretely: accounts alive
without intervention back to ≥5 of 6 (QA-1), manual relogins ≤1/week (QA-2), fleet decline noticed by
the system within 1 h rather than by the operator after 7 days (QA-3).

**The governing constraint is that the root cause is not known**, and § 1c of the PRD is authoritative
on what is established. This design is therefore organised so that **the first increment buys
knowledge, not a guess** — everything after it is either cause-independent or gated on a measurement.

### 1a. What the design must not do

Encode a cause it does not have. Two hypotheses have already died under evidence (Claude Code's
version; the cross-machine writer), and a third is weakened *by evidence gathered during this design*
— see § 14 Risk R-1. A design that hard-codes "concurrency is the problem" would be the third
casualty.

## 2. Constraints

| Constraint | Source | Consequence for design |
|---|---|---|
| The daemon never sees the token endpoint | `src/refresh.rs` — CC performs the exchange inside `claude -p` | Token-endpoint errors need subprocess output capture or they are unobservable |
| `claude -p` stdout/stderr are **nulled at the spawn site** | `Stdio3::Null` in `src/isolated_spawn.rs` | Capturing them is a deliberate change with a token-exposure surface |
| Nothing reads any HTTP error body today | verified — no `.text()` / `error_for_status` / `json::<` in `src/usage.rs`, `src/use_account.rs` | Capturing the usage-API 401 detail is a new capability |
| A token value may never be logged, printed, or persisted | repo contract: `refresh_token` is *"only ever emptiness-checked or byte-compared, never logged"* | Every new diagnostic emits a **classified enum**, never raw text |
| Four independent schema wires | `CLAUDE.md` § Schema versions | A new field that is always present forces a bump; an omittable one does not |
| No cross-process or cross-machine lease is available | #466 (no CC knob), #613 (out of boundary) | Contention can be **detected**, not prevented by locking |
| macOS only | `CONTRIBUTING.md` | No portability obligation |

## 3. Context and Scope

```
   operator ──/login──┐
                      ▼
  ┌───────────────────────────────────┐        ┌──────────────────┐
  │ canonical keychain item           │◀──────▶│ concurrent        │
  │ (active account's live credential)│ writes │ `claude` processes│
  └───────────────────────────────────┘        └──────────────────┘
            ▲          ▲
            │          │ observes change → restash
            │          │
     ┌──────┴──────────┴───────┐   spawns    ┌────────────────────┐
     │ sessiometer daemon      │────────────▶│ `claude -p` in an  │
     │  · usage poller (401s)  │  isolated   │ isolated CONFIG_DIR│
     │  · isolated refresh     │◀────────────│ (CC does the OAuth │
     │  · swap engine          │  read-back  │  exchange itself)  │
     └─────────────────────────┘             └────────────────────┘
```

**The two 401s are different events and the design must keep them apart** — the codebase already
says so at `Viability::Quarantined` in `src/use_account.rs`: *"a 401 never sees the refresh token, so the remedy is a
refresh … not a re-login."*

| | Where it happens | Daemon sees it? |
|---|---|---|
| **Usage-API 401/403** — the access token was rejected | daemon's own HTTP call | **Yes** — response is in hand, body currently discarded |
| **Token-endpoint `invalid_grant`** — the *grant* was rejected | inside `claude -p` | **No** — output nulled at spawn |

Conflating them is what makes shape A (live grant invalidated) and shape B (token lapsed)
indistinguishable today.

## 4. Solution Strategy

**Three increments, gated. Increment 1 is an instrument, not a fix.**

| Increment | Requirements | Depends on | Ships |
|---|---|---|---|
| **1 — Instrument** | R-1, R-2, R-4 | nothing | immediately |
| **2 — Cause-independent repair** | R-8…R-13, R-14…R-18, R-20…R-22 | nothing | in parallel with 1 |
| **3 — Cause-directed fix** | R-5, R-6, R-7, R-19 | Increment 1's measurement | only after data |

Increment 2 does **not** wait on Increment 1. Per the PRD's circuit-breaker, throttling a retry storm,
telling the truth about health, and alerting on fleet decline are correct under every surviving
hypothesis.

## 5. Building Blocks

### 5.1 `CredentialFailure` — the classified-cause type (R-1, R-2)

A single enum carried on every credential failure event. **Values are classified at the observation
site; raw text never leaves it.**

| Variant | Source | Meaning |
|---|---|---|
| `AccessRejected { http }` | usage-API 401/403 | access token bad — *not* proof of grant death |
| `GrantInvalid` | token endpoint `invalid_grant` | the grant itself is gone — **shape A** |
| `TokenExpired` | token endpoint `expired_token`, or a lapsed local deadline | **shape B** |
| `Transport` | network / timeout | not a credential fact |
| `Unobservable` | the sub-reason could not be seen | **R-3's honest value** |

`Unobservable` is a first-class variant, not a fallback for laziness: it is what R-3 requires the
system to say rather than infer a cause it did not observe.

### 5.2 Two capture sites

**(a) Usage-API detail** — `src/usage.rs` / `src/use_account.rs`. On a 401/403, read the response
body and extract **only** an allowlisted error-code field, mapping it to `CredentialFailure`. The raw
body is dropped unread into the log. *New capability: nothing reads an error body today.*

**(b) Token-endpoint detail** — `src/refresh.rs`. Change the spawn config to pipe `claude -p`'s
stderr, match it against a fixed pattern set, emit only the classified variant, and **discard the
buffer**. See § 11 Security — this is the design's principal new exposure surface.

### 5.3 `refresh_contention` — measuring the hazard, not a proxy (R-4)

**Design finding that changes the meter.** The obvious instrument — counting observed canonical
changes — is wrong, and the evidence says so: foreign-write counts are *equal* across the two
machines (restash A=70 / B=82; keep_warm A=75 / B=82), yet one machine is healthy and the other
collapsed. The reason is **coalescing**: the daemon samples the canonical item periodically, so N
rapid foreign writes between two polls are observed as one. The existing signal structurally cannot
distinguish 2 writes from 50.

So measure the **race** directly instead:

> Read the canonical refresh-token bytes before the isolated refresh (`T0`). After the refresh, before
> writing back, read them again (`T1`). **`T1 != T0` means another writer landed inside our window** —
> the exact condition under which we would restash a superseded token and invite a replay.

Emitted as `refresh_contention` with the outcome it accompanied. This is a **direct observation of the
hazard**, immune to coalescing, and it reuses the byte-comparison `src/daemon/canonical.rs` already
performs. A coarse secondary meter — `local_claude_n`, a process-table count at attempt time — ships
alongside, because the PRD's counter-evidence means we do not yet know which correlates.

### 5.4 Recovery episode bounding (R-8…R-11)

`RecoveryEpisode` becomes explicit state: attempt counter, first-failure timestamp, and a decaying
cadence. **Throttle, never terminate** — `.eu` was rescued by the daemon's own retry 10 h after being
marked unrecoverable (`my_refresh`, `delta_secs=2089344`). Terminal state suppresses the *fast* loop
and drops to a low-frequency liveness probe; it never stops probing.

R-11's alternation (`dead` and `no_change` on one credential in one hour) is resolved by carrying
`CredentialFailure` on each attempt — the two outcomes then differ visibly or are revealed identical.

### 5.5 Health derivation (R-12, R-13)

`HealthState` is computed from credential facts only. Usage-poll reachability moves to a **separate,
explicitly-named** signal. Today they are fused: `.fr` produced 9 `healthy`/9 `unknown` transitions in
one day, every `unknown` merely a rate-limited poll.

Invariant: **an account with a live `AccessRejected` or worse can never compute to `healthy`.**

### 5.6 Fleet SLI and push surface (R-14…R-17)

SLI = count of accounts with ≥1 successful unaided refresh in a rolling 24 h. Never a per-attempt
rate — that denominator is inflated 62× by the retry loop. Two thresholds: a configurable floor, and
a hard "one account remaining" trip. Delivery is a **push** notification, reusing the menubar's
existing notification presenter; R-17's glance cue reuses #923's surface with distinct semantics.

## 6. Runtime View — the first increment's measurement loop

```
refresh due for account A
   │
   ├─▶ read canonical refresh-token bytes            → T0
   ├─▶ sample process table                          → local_claude_n
   ├─▶ seed isolated dir, spawn `claude -p` (stderr PIPED)
   │      └─ on failure: classify stderr             → CredentialFailure
   ├─▶ read canonical bytes again                    → T1
   ├─▶ T1 != T0 ?  ──yes──▶ emit refresh_contention
   └─▶ emit refresh { outcome, cause?, local_claude_n, contended? }
```

After a few days of ordinary batch work this yields the correlation R-4 exists to produce, with no
pause, no operator action, and no behavioural change to the refresh itself.

## 7. Interface Contracts (wire/schema)

**Increment 1 requires no schema bump, by construction.** Every new field is emitted through
`skip_serializing_if` and is therefore omittable — the condition `CLAUDE.md` names as the one under
which additive means no bump (`log` bumped precisely because its field was always present).

| Wire | Constant | Touched by |
|---|---|---|
| `log --json` | `JSON_SCHEMA_VERSION` (`src/log.rs`) | new **omittable** event fields → **no bump** |
| `status` + `watch` | `STATUS_SCHEMA_VERSION` | Increment 2 only, if the SLI is surfaced → bump + **Swift fixture sweep** |
| `stats --json` | `JSON_SCHEMA_VERSION` (`src/stats.rs`) | untouched |
| `reliability --json` | `JSON_SCHEMA_VERSION` (`src/reliability.rs`) | untouched |

Consequence: **Increment 1 does not cross into Swift at all** — no fixture sweep, no menubar change.
That is what makes it genuinely small.

## 8. Crosscutting Concepts

### Security — the design's principal new exposure

Piping `claude -p`'s stderr (§ 5.2b) is the one place this design widens what the daemon can see. Three
mitigations, all structural rather than procedural:

1. **Classify at the boundary.** The buffer is matched against a fixed pattern set and reduced to a
   `CredentialFailure` variant inside the capture function. The raw buffer is never returned to a
   caller, never stored, never logged.
2. **Allowlist, not denylist.** Only known error signatures map to a variant; anything unmatched
   becomes `Unobservable`. A denylist would leak whatever it failed to anticipate.
3. **Same rule for the HTTP body** (§ 5.2a): extract one allowlisted error-code field, drop the body.

The event log's existing safety was verified empirically for this design, not assumed from the code
comment: across 26,401 lines — zero `sk-ant` prefixes, zero bearer tokens, zero JWT-shaped strings,
zero base64 runs ≥60 chars, and no credential-valued field keys. The new fields must preserve that
property, which the classified-enum design guarantees by construction (no free text can reach the log).

### Observability

The whole first increment *is* the observability work. Beyond it: `refresh_contention` rate and the
QA-1 SLI are the two series worth alerting on.

### Error Handling

`Unobservable` propagates as a value, never as a silent default. A failure whose cause could not be
seen must be visibly a failure-of-unknown-cause.

### Master Test Plan

**Risk surface (ACC), abbreviated for a single-binary daemon:**

| Cap | Component | Attribute | Risk |
|---|---|---|---|
| Cap-1.1 | stderr classifier | secure | a token reaches the log |
| Cap-1.2 | stderr classifier | correct | a real `invalid_grant` classifies as `Unobservable` |
| Cap-2.1 | contention detector | correct | false negative under coalescing — the defect it exists to avoid |
| Cap-3.1 | recovery episode | correct | throttle degenerates into a hard stop, stranding a recoverable account |
| Cap-4.1 | health derivation | correct | `healthy` computed over a live `AccessRejected` |
| Cap-5.1 | SLI | correct | per-attempt denominator reintroduced |

**Pyramid**: unit-dominant, matching the repo. The classifier is a pure function over fixtured
strings — including a **negative fixture containing a token-shaped string, asserting it does not
appear in the emitted event** (Cap-1.1's oracle). Contention detection is testable by injecting a
byte change between the two reads. The recovery-episode cadence is asserted against a clock, which is
what R-8 requires and what makes contradiction C2 resolvable.

**Gates**: the repo's five (`fmt`, `clippy -D warnings`, `doc`, `build`, `test`) plus `msrv` — every
increment touches `src/**`. No Swift job for Increment 1 (§ 7).

## 9. Architecture Decisions

| ADR | Decision | Alternatives rejected |
|---|---|---|
| **D-1** | Measure contention by **before/after byte comparison**, not by counting observed canonical changes | Change-counting — *falsified during design*: equal foreign-write counts on a healthy and a collapsing machine, because sampling coalesces |
| **D-2** | Pipe and **classify** `claude -p` stderr rather than leave it nulled | Leave nulled + record R-3's negative finding — rejected because the sub-reason is the single highest-value unknown; the exposure is containable by classification |
| **D-3** | `Unobservable` is a first-class variant | Defaulting to a guessed cause — this is precisely R-3's prohibition |
| **D-4** | New log fields **omittable** via `skip_serializing_if` | Always-present fields, which would force a `JSON_SCHEMA_VERSION` bump and inflate a small increment |
| **D-5** | Terminal recovery state **throttles**, never terminates | Hard circuit-breaker — evidence-refuted: `.eu` self-rescued 10 h post-`unrecoverable` |
| **D-6** | Ship **both** contention and process-count meters | One meter — we do not know which correlates, and the PRD records counter-evidence against the intuitive one |
| **D-7** | Increment 2 proceeds in parallel, not gated on the measurement | Serialising everything behind the spike — the PRD's circuit-breaker forbids it |

**Author-chosen defaults requiring ratification** (per `decision-surfacing`): the SLI floor value; the
recovery-episode attempt bound and decay curve; the low-frequency liveness probe interval. All three
are thresholds with genuine alternatives, picked by this design and **not** operator-set.

## 10. Risks and Open Questions

### Feasibility Summary

| Component | Verdict | Note |
|---|---|---|
| Usage-API error-body capture | **FEASIBLE** | response in hand; body merely discarded today |
| stderr classification | **FEASIBLE-WITH-SPIKE** | must confirm CC actually emits a distinguishable signature on `invalid_grant`; if not → `Unobservable` and R-3 fires |
| Contention detection | **FEASIBLE** | reuses the existing byte comparison |
| Recovery-episode bounding | **FEASIBLE** | must reconcile with ADR-0007's four rejected mechanisms |
| Health derivation split | **FEASIBLE** | pure refactor of a computed value |
| Fleet SLI + push | **FEASIBLE** | notification presenter exists |
| Menubar glance cue | **FEASIBLE** | reuses #923's surface |

### Risk Register

| # | Risk | L×I | Mitigation |
|---|---|---|---|
| **R-1** | **The leading hypothesis is wrong.** Local concurrency is weakened by this design's own evidence — equal foreign-write rates, healthier machine slightly higher | 3×3 = **9 HIGH** | The design does not depend on it. Increment 1 measures rather than assumes; Increment 2 ships regardless. This is the mitigation, by construction |
| R-2 | CC emits no distinguishable `invalid_grant` signature → the highest-value unknown stays unknown | 2×3 = 6 MED | `Unobservable` + R-3's negative finding; usage-API capture (5.2a) still lands and is unaffected |
| R-3 | stderr capture leaks a token into the log | 1×3 = 3 LOW | Classify-at-boundary + allowlist + a negative test fixture asserting non-appearance (Cap-1.1) |
| R-4 | Throttle degenerates into the stranding a hard stop would cause | 2×3 = 6 MED | D-5; low-frequency probe never stops; `.eu`'s self-rescue is the regression fixture |
| R-5 | Measurement runs for days and the correlation is null | 2×2 = 4 MED | A null result is a real finding — it refutes the last standing hypothesis and redirects the search. Budget it as an outcome, not a failure |

**No unmitigated HIGH risks.** R-1 is HIGH and its mitigation is the design's core structure.

### Open Questions

- **Does Claude Code surface a distinguishable token-endpoint error on stderr?** Load-bearing —
  it decides whether R-1(b) is buildable or becomes R-3's negative finding. Cheapest test: force one
  failed refresh with stderr piped and inspect. *Impact if deferred*: Increment 1 ships at half value.
- **Which meter correlates — contention or process count?** Not load-bearing for building; it is the
  measurement's *output*. Both ship (D-6).

## 11. Requirement-to-Track Coverage (forward)

| Requirement | Track(s) | Section | ACC | Status |
|---|---|---|---|---|
| R-1, R-2, R-3 | Technical, Security | § 5.1, § 5.2, § 11 | Cap-1.1, Cap-1.2 | covered |
| R-4 | Technical | § 5.3 | Cap-2.1 | covered |
| R-5, R-6, R-7 | Technical | § 5.3, Increment 3 | Cap-2.1 | covered |
| R-8, R-9, R-10, R-11 | Technical, Testing | § 5.4 | Cap-3.1 | covered |
| R-12, R-13 | Technical | § 5.5 | Cap-4.1 | covered |
| R-14, R-15, R-16 | Technical, UX | § 5.6 | Cap-5.1 | covered |
| R-17 | UX | § 5.6 | Cap-5.1 | covered |
| R-18 | — (documentation deliverable) | Increment 2 | n/a — non-testable | covered |
| R-19 | Technical | Increment 3 | Cap-2.1 | covered |
| R-20, R-21, R-22 | — (provenance deliverables) | Increment 2 | n/a — non-testable | covered |

**No UNCOVERED entries.** R-18 / R-20 / R-21 / R-22 are documentation and provenance deliverables,
explicitly classified non-testable rather than left unbound.

## 11b. Element-to-Requirement Backward Coverage

| Element | Type | Traces to | Status |
|---|---|---|---|
| `CredentialFailure` enum | type | R-1, R-2, R-3 | traced |
| `Unobservable` variant | value | R-3 | traced |
| usage-API body capture | component | R-1 | traced |
| stderr classifier | component | R-1 | traced |
| `refresh_contention` event | event | R-4 | traced |
| `local_claude_n` field | field | R-4 | traced |
| `RecoveryEpisode` state | type | R-8, R-10, R-11 | traced |
| health/reachability split | refactor | R-12, R-13 | traced |
| fleet SLI + thresholds | component | R-14, R-15, R-16 | traced |
| menubar unrecoverable cue | UI | R-17 | traced |

**No PHANTOM entries** — every element traces to a ratified requirement.

## 12. Glossary

| Canonical | Definition |
|---|---|
| **Grant** | the OAuth credential family: rotating refresh token + access token + family deadline |
| **Contention** | another writer changed the canonical credential inside our read→write window |
| **Coalescing** | N rapid foreign writes observed as one, because observation is sampled — why change-counting fails |
| **Shape A / B** | grant invalidated while the access token lived / access token lapsed |
| **Increment 1** | the instrument: R-1, R-2, R-4 — buys knowledge, changes no behaviour |
