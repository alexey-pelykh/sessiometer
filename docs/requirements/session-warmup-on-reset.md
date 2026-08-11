---
title: Session Warm-Up on Reset — Recovering Idle-Clock Loss on the Five-Hour Window
scope: daemon-session-window-anchoring
created: 2026-08-11
status: draft
dor_status: passed-with-findings
source: operator idea (session, 2026-08-11), re-grounded by first-party measurement of the local daemon usage store over 2026-07-28 -> 2026-08-11; every figure is replicated in-band in § 2 because the sample store is machine-local runtime state, not a committed artifact
parent-requirements: private HQ, `strategy/design-swap-latency.md` (D-LAT-3 / §16.6 / §90-91) — not dereferenceable from a clone; the claims this PRD relies on are quoted in-band in § 1 and § 6
formulation: {}
features:
  poke-weekly-cost-spike: {stage: work-items, tracks: {spike: 'issue #1231'}}
  session-warmup-on-reset: {stage: work-items, tracks: {build: 'issue #1232'}}
artifacts:
  design-doc: docs/design/session-warmup-on-reset-solution-design.md
---

# PRD — Session Warm-Up on Reset

> **Provenance.** Authored by an AI pipeline (`/scope` Stage 1) from an operator idea plus first-party
> measurement the operator's own daemon produced. The **mechanism** claim in § 2.1 is the operator's,
> supplied verbatim mid-session and then tested; it is **confirmed**. The **cost** claim that gates the
> whole feature (§ 4) is **unmeasured** — R-1 exists to measure it, and R-2..R-6 are explicitly
> conditional on its verdict. No requirement here is ratified upstream; all are pipeline-authored from
> the measurement and are surfaced as such.

## 1. Problem Statement

Anthropic's **five-hour session window is anchored at first use**, not on a wall-clock cadence. An
account that has reset but is not touched has **no window running at all** — its next reset instant
does not exist yet, and is created only when the account is next used. Leaving an account idle
therefore pushes its next reset **further into the future**, one idle interval at a time.

The daemon does not touch a parked account merely because its window ended. It swaps on demand. So
across a rotating fleet, accounts accumulate idle time in which their session clock is stopped.

This is a **throughput** property of the fleet, not a phasing one: an account used continuously
completes one session window per 5 h; an account that idles `D` between windows completes one per
`5 h + D`.

**Prior art this does not contradict.** Issue #726's spike (verdict 2026-07-28) returned NO-GO on a
session-axis "stagger" lever. That lever was *delaying* first use to disperse anchors, rejected on
cost — its own words, quoted so this document stands alone: *"There is no surplus to fund the idle.
The only daemon lever on a session anchor is not touching an otherwise-viable account."* This PRD
proposes the **opposite** lever — *advancing* first use — whose cost is one cheap spawn rather than a
forfeited window. #726 measured the **spread between accounts** (median 160 min of a 300 min window;
residual +16 min vs perfect uniform phasing) and never measured **idle-clock intervals**, because the
delay lever gave it no reason to. § 2 supplies that missing measurement.

## 2. Evidence

Corpus: the operator's live daemon usage store, **15 784 samples, 6 accounts,
2026-07-28 00:36Z → 2026-08-11 11:40Z (14.5 d)** — the window immediately following #726's frozen
corpus (2026-07-13 → 2026-07-27), with no overlap. Weekly transitions de-duplicated at ±5 s to
suppress the reset-twin artifact #726 documents.

### 2.1 The session mechanism is CONFIRMED

| Measure | Value |
|---|---|
| Intervals with **no `session_resets_at` at all** for ≥30 min (clock stopped) | **242** |
| Samples carrying `session = 0` **and** no reset instant | **3 214** |
| Idle-clock duration | median **2.17 h**, p90 **3.76 h**, max **7.36 h** |
| Total idle-clock time | **582.52 h** |
| On re-arm, new `session_resets_at` ahead of the re-arm sample | median **4.73 h** (min 3.74, max 4.99) |

A re-arm offset of ~5 h measured **from the re-arm** is the signature of a clock that starts at first
use. The median sits below 5 h by poll-cadence lag (the re-arm sample lands up to one poll interval
after true first use), which biases the estimate **down**, not up.

**Magnitude.** 582.52 h against 6 accounts × 14.5 d = 2 088 account-hours ⇒ **27.9 % of all
account-time had no session window running**.

### 2.2 The weekly window does NOT behave this way — REFUTED

| Measure | Value |
|---|---|
| Weekly anchor transitions observed | **12** (6 accounts × 2) |
| Transitions at exactly **7.0000 d** | **12 / 12** |

The 582.52 h of idle in § 2.1 sits **inside this same corpus**, and no weekly anchor moved. Weekly is
a fixed hard-reset; it is not first-use-anchored, and warm-up cannot move it, cannot buy weekly quota,
and cannot relieve the Fri–Sun trough that #726 attributes 91 % of holds to. On fresh data this also
answers, for a **running** account, the question issue **#792** was opened to settle.

### 2.3 Reachable population

#726 recorded **80 of 143** capacity holds as `cause=session`, of which only 5 % had every spare
weekly-blocked — i.e. **~76 holds had weekly head-room and waited purely on a session window**. Those
are the holds an earlier session reset could shorten, by up to the idle-clock duration in § 2.1.

## 3. Objects (OOUX)

| Object | Core attributes | Relationships |
|---|---|---|
| **Account** | label, uuid, `session_pct`, `session_resets_at` (nullable = no window running), `weekly_pct`, `weekly_resets_at`, quarantined | belongs to Roster |
| **Session window** | anchored-at (first use), resets-at, running? | 0..1 per Account |
| **Warm-up attempt** | account, trigger instant, outcome, weekly cost observed | targets one Account |
| **Warm-up policy** | enabled?, eligibility predicate, per-account throttle | governs Roster |

## 4. The gating unknown

A warm-up spends **weekly** quota, and weekly is the binding axis (§ 2.2; #726: 16 of 18 weekly
windows peak ≥ 0.97). Warm-up therefore **trades the scarce resource to buy latency on the one that
is 27.9 % slack**.

- Per-poke weekly cost ≈ 0 ⇒ favourable on the ~76-hold population in § 2.3.
- Per-poke weekly cost material ⇒ **net loss**: it debits the axis causing 91 % of holds to relieve an
  axis that already has slack.

**This quantity is not in the corpus and cannot be derived from it.** R-1 measures it. Every other
requirement is conditional on R-1's verdict.

## 5. Requirements (EARS)

**R-1 — Measure the per-warm-up weekly cost.** *(spike; unblocks the rest)*
The system SHALL establish the weekly-quota cost of a single warm-up cycle against a parked account,
by observing `weekly_pct` and `weekly_resets_at` immediately before and after, on at least three
cycles across at least two accounts, and SHALL record a GO / NO-GO for R-2..R-6 against the § 2.3
benefit.
*Origin*: pipeline-authored from § 4. *Ratification*: operator ratified the spike-first shape (session, 2026-08-11).

**R-2 — Warm-up on reset.** WHEN a managed account's session window is observed to have ended and no
new window is running, the daemon SHALL initiate one warm-up cycle against that account.
*Conditional on R-1 = GO.*

**R-3 — Opt-in, off by default.** The warm-up behaviour SHALL be disabled unless explicitly enabled in
configuration, following the precedent of `[refresh].proactive_keep_warm`.

**R-4 — Never target the active account.** The daemon SHALL NOT warm the active account, and SHALL NOT
warm a quarantined account.

**R-5 — Throttled.** The daemon SHALL apply a per-account throttle so a repeatedly no-op warm-up cannot
spawn once per tick.

**R-6 — Observable.** Each warm-up attempt SHALL emit a durable event carrying account label, trigger
instant and outcome, redacted to non-secret handles, and the daemon SHALL expose a readout sufficient
to determine whether idle-clock time actually fell.

## 6. Out of scope — with the reason each is excluded

| Excluded | Why |
|---|---|
| Moving a **weekly** anchor | Refuted, § 2.2: 12/12 transitions at exactly 7.0000 d despite 582 h of idle |
| **Delaying** first use to disperse anchors | #726 Q1 NO-GO — *"There is no surplus to fund the idle"* |
| Cross-machine coordination | Rejected keystone (maintainer, 2026-07-18); the daemon has no shared backend |
| A provisioning advisory | #666 already ships one (`add an account` nudge) |
| Changing swap trigger / ceiling semantics | ADR-0023 territory; unrelated axis |

## 7. Success criteria

- R-1 returns a defensible per-warm-up weekly cost with its method stated, and a GO / NO-GO.
- If GO: measured idle-clock total falls materially below the 582.52 h / 27.9 % baseline in § 2.1,
  with the weekly-quota debit stated alongside it, so the trade is legible rather than assumed.

## 8. Open question owned by the operator

Whether a *favourable but small* R-1 result is worth the config surface and the new spawn path at all
is a judgement call, not a measurement. It is surfaced here rather than decided.
