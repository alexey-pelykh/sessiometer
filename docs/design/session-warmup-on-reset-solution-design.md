---
title: Session Warm-Up on Reset — Solution Design
source: docs/requirements/session-warmup-on-reset.md
created: 2026-08-11
status: draft
tracks:
  technical-architecture: complete
  testing-architecture: complete
  ux-ia: not-applicable
  ui-visual: not-applicable
---

# Solution Design: Session Warm-Up on Reset

**Input PRD**: `docs/requirements/session-warmup-on-reset.md` — `dor_status: passed-with-findings`
(the finding is the gating unknown in its § 4, which R-1 exists to close).
**Tree**: `7489e83`. **Requirements**: R-1 … R-6. **Items**: 2 — #1231 (spike, gating) and #1232 (build, blocked).

> **Ratification status.** Authored analytically by a single actor; no independent ratifying lens was
> dispatched. The operator is the ratifying authority. Recorded as **single-actor ratification**.

## 1. Goals and Drivers

Recover the idle-clock loss measured in the PRD § 2.1 — 242 stopped-clock intervals, 582.52 h,
27.9 % of account-time — **if and only if** buying it does not cost more on the weekly axis than it
returns on the session axis.

The design is therefore deliberately **two-staged**: a measurement item that can return NO-GO and
delete the second item, and a build item that is small because the machinery already exists.

## 2. Constraints

| Constraint | Source | Effect on design |
|---|---|---|
| Weekly anchors are immovable | PRD § 2.2 (12/12 at 7.0000 d) | No weekly-side component exists in this design at all |
| Must never touch the active account | `poke` contract (`Error::PokeTargetActive`), `keep_warm` active-exclusion complement (#253) | Eligibility predicate excludes active + quarantined |
| Shared canonical credential is scrub-prone | Finding #465 — refresh token rotates on **every** exchange; a cross-process race scrubs the shared item | Rules out the literal "swap to it briefly" actuation — see § 5 D-2 |
| Opt-in, off by default | PRD R-3; precedent `[refresh].proactive_keep_warm` | New config key defaults false; whole path inert until set |
| macOS-only target | `CONTRIBUTING.md` | No portability work implied |

## 3. Building Blocks

| # | Component | Responsibility | Reuses | Feasibility |
|---|---|---|---|---|
| C-1 | **Stopped-clock predicate** | From an existing poll reading, decide "this parked account has no session window running" | `Usage::session_resets_at: Option<i64>` (`src/usage.rs`; cited by symbol — this repo has a recorded line-ref drift, issue #1058) already polled every tick | **FEASIBLE — with one open sub-question**, see § 6 R-3 |
| C-2 | **Eligibility gate** | Exclude active, quarantined, throttled, and config-disabled accounts | The `keep_active_warm` gate ladder (`src/daemon/keep_warm.rs`) is the same shape, including `last_keep_warm_attempt` throttling | **FEASIBLE** — direct precedent |
| C-3 | **Warm-up actuator** | Perform one real use against a parked account | `crate::refresh` isolated engine (#102); `src/poke.rs` is already a thin caller over it | **FEASIBLE for the spawn; UNPROVEN for the effect** — see § 5 D-1 and § 6 R-1 |
| C-4 | **Config surface** | `enabled` flag + throttle interval | `[refresh].proactive_keep_warm` precedent | **FEASIBLE** |
| C-5 | **Event + readout** | Durable, redacted record per attempt; enough to tell whether idle-clock fell | Existing `Event` machinery; adjacent to #906 (durable Event for out-of-process `poke` refresh) | **FEASIBLE** |

## 4. Sequence

```
tick
 └─ poll account i  ──►  Usage { session_pct, session_resets_at, weekly_* }
       │
       ├─ C-1  session_resets_at == None  (and account parked)  ──► clock stopped
       ├─ C-2  not active ∧ not quarantined ∧ enabled ∧ throttle elapsed
       ├─ C-3  refresh-engine cycle against isolated CLAUDE_CONFIG_DIR   ← the "use"
       └─ C-5  Event::SessionWarmup { label, trigger_ts, outcome }
```

No new socket surface, no wire/schema change, no `STATUS_SCHEMA_VERSION` bump — C-1 consumes a field
already serialized, and C-5 adds an event, not a status field.

## 5. Decisions and alternatives considered

**D-1 — Actuate via the isolated refresh engine, not by making the account active.**

The operator's phrasing was *"briefly swapped to"*. This design deliberately does **not** swap.

| Option | Verdict |
|---|---|
| **Isolated `refresh`/`poke` cycle** (chosen) | Runs `claude -p` under an ephemeral `CLAUDE_CONFIG_DIR` against an isolated keychain item; never touches the live canonical item the active session reads |
| A real swap to the account and back | **Rejected.** Two swaps per warm-up injects churn into the shared canonical credential, which finding #465 shows is scrub-prone: *"the refresh token rotates on every exchange … the first such session scrubs the item for the whole fleet."* Paying a fleet-wide scrub risk to save session latency inverts the trade |
| Direct HTTP call to the provider bypassing `claude` | **Rejected.** Would need a second, independently-maintained request path and its own auth handling; the existing engine already solves this |
| Do nothing — rely on rotation (status quo, the #726 position) | **Rejected on evidence.** Rotation is the staggering mechanism for *spread*, but PRD § 2.1 shows it leaves 27.9 % of account-time with the clock stopped |
| Delay-based stagger | **Rejected.** #726 Q1 NO-GO — *"There is no surplus to fund the idle"* |
| Buy more accounts | **Out of scope.** #666 already nudges; adds weekly quota, does not address idle-clock |

**D-2 — Two items, hard-gated, not one.** R-1's result can invert the sign of the feature (PRD § 4),
so the build item is *blocked by* the spike rather than merely informed by it.

## 6. Risks

| # | Risk | Sev | Mitigation |
|---|---|---|---|
| R-1 | **The warm-up may not start the clock.** `poke`'s purpose is credential refresh; if its `claude -p` cycle does not make a billable inference call, it may refresh the token without opening a session window — in which case C-3 actuates nothing and the whole feature is void | **HIGH** | The spike measures **both** the weekly cost *and* whether `session_resets_at` appears after a cycle. This is why R-1 gates rather than informs. Cheap to falsify: one cycle against a parked account |
| R-2 | Weekly-quota debit on the binding axis | **HIGH** | R-1 quantifies it; NO-GO deletes the build item |
| R-3 | `session_resets_at == None` may not mean "no window". The corpus also holds 2 843 samples with `session = 0` **and** a reset instant present, which C-1 must not treat as stopped | **MEDIUM** | Fold into R-1: confirm the absent/present split against a known-idle account before C-1 is written |
| R-4 | **Warm-up synchronises rather than staggers.** Warming every account at its own reset preserves existing phase and could tighten co-exhaustion | **MEDIUM** | C-5's readout must report reset **spread** alongside idle-clock, so a regression is visible; per-account jitter is the lever if it appears |
| R-5 | Spawn cost / storm | **LOW** | C-2 throttle, precedent-identical to `keep_warm` |

## 7. Requirement → component traceability

| Req | Components | Item |
|---|---|---|
| R-1 measure per-warm-up weekly cost | — (spike; also closes § 6 R-1 and R-3) | **#1231** |
| R-2 warm on reset | C-1, C-3 | **#1232** |
| R-3 opt-in, off by default | C-4 | **#1232** |
| R-4 never active/quarantined | C-2 | **#1232** |
| R-5 throttled | C-2, C-4 | **#1232** |
| R-6 observable | C-5 | **#1232** |

## 8. Testing architecture

| Layer | Coverage |
|---|---|
| Unit | C-1 predicate over the three observed shapes (absent / present-with-0 / present-with-usage); C-2 gate ladder incl. active + quarantined exclusion, mirroring the existing `should_keep_warm_retry` tests |
| Integration | Daemon tick drives C-1 → C-2 → C-3 through the existing keep-warm seam (`with_keep_warm_engine`), asserting the active account is never targeted |
| Manual / operational | R-1's measurement is an operator-run procedure against the live fleet, not a test |

No Swift surface: `session_resets_at` is already carried, and this design adds no status field, so the
menubar fixtures and `STATUS_SCHEMA_VERSION` are untouched.
