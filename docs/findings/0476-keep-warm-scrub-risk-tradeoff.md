# Finding #476 — the scrub-risk cost of gating proactive keep-warm (spike)

The decision half of the **#463** umbrella's keep-warm question: **#468** wants to gate proactive
keep-warm of the active account to cut shared-token churn — but keep-warm's freshness is also what
keeps a straddled window *recoverable* ([#465](0465-multi-session-rotation-interference.md) § 1),
so gating it is not obviously a win. This note quantifies **both arms** and hands #468 a predicate.

**Verdict.** The tradeoff is **neither neutral nor a pure win — it is severity-conditional, and
[#467](https://github.com/alexey-pelykh/sessiometer/issues/467) has already flipped the condition.**
Gating proactive keep-warm cuts a **measured ~44 %** of the daemon's canonical-rotation churn *in the
regime it runs* (32 of 73 canonical writes over the **~6.9 d it has been enabled**; 4.6/day; **every
one `rotated=true`** — each a rotation-yank window) in exchange for a **modeled** rise in
scrub-*eligibility* (the active canonical reaches
expiry unrefreshed before the reactive backstop re-warms it, widening the dead-token window the
scrub straddles). **Before #467 that was a veto** — the scrub was fleet-wide and *unrecoverable*
(every session `claude /login`), so keep-warm's freshness was worth its churn. **#467 (autonomous
adopt-target of a scrubbed canonical, merged at `20038fa`) changed the calculus**: the residual
scrub is now daemon-recovered within a poll cycle and sessions recover on `continue` — the *same
mild, recoverable outcome* as the yank the churn produces. With the severe arm de-fanged, the
frequent **measured** churn cut dominates the rare **modeled** scrub increase. **Recommendation:
#468 should gate proactive keep-warm off on the active axis, leaning on the reactive backstop
(`should_keep_warm_retry`) + #467 for recovery** (§ Recommendation, predicate **C**). The scrub
*numerator* stays capture-pending — the running daemon still predates #464 — so this recommends a
**direction** from a measured churn arm plus a mechanism-settled scrub arm, not from an observed
scrub rate.

## Data-availability boundary (read first)

This spike inherits [#465](0465-multi-session-rotation-interference.md)'s boundary **unchanged**. The
direct correlation the acceptance asks for — *scrub events (via #464 instrumentation) against
canonical freshness at event time* — depends on the #464 signals (`diag=canonical`,
`event=canonical_scrubbed`, the #475 `mode=`). #464 merged to `main` at `a3c2618` (2026-07-11
14:59Z) and #475 at `51e2968`; the daemon **running** during this analysis is **PID 82060, started
2026-07-11 04:27Z** — the *same* process #465 identified, still predating both merges by hours, and
a merge to `main` does not swap an already-running process. Grep-confirmed over the full log: **0**
`diag=canonical`, **0** `canonical_scrubbed`, **0** `canonical_restored`, **0** `mode=yank`, **0**
`mode=scrub`. Consequences, applied throughout:

- **Settled from real data**: the churn arm — the proactive keep-warm rotation count, its
  proactive-vs-reactive split, and its share of daemon canonical churn.
- **Capture-pending** (needs the #464 build deployed — rebuild + restart the daemon — then a live
  scrub straddled): the scrub *numerator* (scrubs per active-hour), the per-episode
  `canonical_scrubbed ↔ preceding freshness` correlation, and the **redundant fraction** of
  proactive mints (those a live session would have re-warmed anyway). These are marked 🟡 below,
  never asserted as measured.

The scrub is **structurally invisible** in this pre-instrumentation log — the same gap that
motivated #464. What #476 *adds over #465* is the churn arm broken out **by keep-warm trigger** (the
axis #468 actually gates) and the **severity re-basing by #467** — neither of which needs the
capture-pending signal.

## Results at a glance

| # | Acceptance question | Verdict | Basis |
|---|---|---|---|
| **1** | Does a fresher canonical (proactive keep-warm) measurably reduce **scrub** incidence? | ✅ **protective by mechanism**; scrub-incidence delta 🟡 **capture-pending** | before/after keep-warm onset: active account *died on expiry* pre-keep-warm (07-03), then 32 proactive mints / **0** reactive-backstop firings / **no** active death after (real data) + CC CAS/race guard (#466); the scrub delta itself uncorrelated (no scrub observed) |
| **2** | Net effect of gating: churn ↓ **vs** scrub exposure ↑ — both arms | ⚖️ **churn measured (~44 %, 4.6/day in the enabled regime); scrub modeled, numerator capture-pending** | keep-warm trigger split (real data) + dead-token-window mechanism (#465 § 1/§ 3) |
| **3** | A middle setting that cuts churn without leaving the canonical holding a dead token | ✅ **predicate recommended (C); two alternatives evaluated (A keep as fallback, B rejected)** | code-grounded on `keep_active_warm` gates + #467 severity re-base |
| — | *Is the tradeoff a veto on #468?* | ✅ **no — #467 removed the veto** | scrub severity re-based from unrecoverable → `continue`-recoverable |

## (1) Is proactive keep-warm protective? — yes by mechanism; the scrub delta is capture-pending

**Real-data signal — a before/after contrast at the keep-warm onset.** The log's **first** proactive
keep-warm mint is **2026-07-04 20:15Z**; no `keep_warm` event precedes it. Splitting the window
there:

- **Before (pre-keep-warm):** the **then-active** account aged into a 401 and *died* — a
  `monitor_401 consecutive=1,2,3 → credential_dead → emergency_swap` streak on **2026-07-03 22:46Z**,
  on the account the preceding swap had promoted to active. With no pre-emptive mint the active
  canonical ran to expiry and quarantined; the **#42 emergency swap** (the `[refresh]`-off fallback)
  recovered it. This is the same episode #465 catalogs (its § Death episodes).
- **After (keep-warm-enabled, ~6.9 d):** **32 proactive mints, *zero* reactive-backstop firings, and
  no active-account death.** (The one later `credential_dead`, 07-10 20:19Z, was a **parked** account
  — a *different* account was active then — so it is not an active-expiry event.) The reactive
  backstop (`should_keep_warm_retry`, the active-only #282 complement — `src/daemon.rs:2630`) fires
  only on an observed active 401; that it *never* fired while proactive keep-warm fired 32 times is
  direct evidence the pre-emptive mint is **absorbing the active-expiry load** in the enabled
  sub-window — the active 401 that killed the account *before* keep-warm has **no** counterpart
  *after* it.

That is exactly keep-warm's protective purpose: keep the item holding a live token so a straddling
session reads (or CAS-recovers against) a fresh value instead of scrubbing (#466's decode; #465 § 1).
The before-case is a single pre-keep-warm episode, not a controlled A/B — it is *consistent with*,
not proof of, the protective mechanism.

**Why the scrub-incidence *delta* is nonetheless 🟡 capture-pending.** "Protective" is a mechanism
claim, not a measured scrub-rate reduction: **no scrub was observed in-window** (the boundary above),
so there is no `canonical_scrubbed` count to compare with-vs-without keep-warm. And the 32 mints do
**not** prove each was *necessary* — some fraction fired in near-expiry windows a live session would
have re-warmed on its own next use (the redundant fraction #468 targets). Separating
"proactive-did-the-work" from "proactive-pre-empted-a-session-that-would-have" needs the #464
canonical-fingerprint series (did the canonical rotate *externally* between near-expiry onset and
the proactive mint?) — capture-pending. So: **protective in direction, magnitude unmeasured.**

## (2) The two arms

### Churn arm — measured

Proactive keep-warm has only been firing since **2026-07-04 20:15Z** (§ 1), so its churn is measured
over the **keep-warm-enabled sub-window** it actually operates in — the regime #468 governs — not
diluted across the pre-enable days. Real-data canonical-write breakdown, enabled window
2026-07-04T20:15Z → 2026-07-11T17:56:00Z (**6.90 d**):

| Source | Writes | Rotates canonical? | Trigger | Per day |
|---|---|---|---|---|
| `swap` (#11/#42) | 41 | **yes** — repoints the item | — | 5.9 |
| `keep_warm` (#282) | **32** | **yes** — promotes to the item | **100 % `proactive`** | **4.6** |
| **Canonical-touching total** | **73** | | | **10.6** |

- **All 32 keep-warm writes are `trigger=proactive`, `outcome=refreshed_not_restashed`,
  `rotated=true`** — i.e. the axis #468 gates is the *entire* keep-warm churn here, and each firing
  genuinely rotated the live shared token (a rotation-yank window per #475). **Zero** were
  `trigger=reactive`.
- **Proactive keep-warm = 32 / 73 = ~44 % of the daemon's canonical churn in the enabled regime.**
  Gating it off removes **up to** that share — the **upper bound**, realized only if *none* of it
  would be replaced by a session/reactive refresh. The true net cut is ≤ 44 % by the redundant
  fraction (🟡 capture-pending, § 1); the reactive backstop that replaces some of it fires **only on
  an actual 401**, so it rotates strictly *less* often than the pre-emptive path it replaces.
- Over the **full 10.23 d** log the counts are `swap` 65, `keep_warm` 32 (= 32 / 97 = 33 %), but that
  dilutes the keep-warm numerator across the ~3.3 d before it was enabled; the **~44 %** enabled-window
  share is the regime-relevant figure. The parked-stash writers below are unchanged either way.
- `refresh` (parked stash, #106) = **88** and `poll_refresh` (parked stash, #162) = **49** over the
  full window — non-canonical (they write the isolated per-account **stash**, the #253
  active-exclusion), so they are **not** in the churn arm (unchanged from #465).

### Scrub arm — modeled, numerator capture-pending

Gating proactive keep-warm **widens the dead-token window** the scrub straddles:

1. With no pre-emptive mint, the active canonical runs to **near-full expiry**.
2. At/after expiry the item holds a soon-/already-dead token until *something* re-mints it: a live
   session's own next-use refresh (a yank — recoverable) **or** a daemon poll's reactive backstop
   (fires on the 401, then promotes + re-polls).
3. The **scrub-eligible window is between the token going dead and a fresh token landing** (#465
   § 3 / § "Why only a few"). Proactive keep-warm *shrinks* that window (lands a fresh token before
   expiry); gating it *widens* it → more straddling session-refreshes find a dead token as the
   canonical's **current** value → higher scrub-eligibility (the CAS-still-dead subset — #466).

**Magnitude is modeled, not measured.** The increase is bounded — the reactive backstop re-warms on
the next poll after the 401, so the widened window is ~one reactive cycle per active-token expiry,
not an open-ended gap — but the scrub *numerator* (how many straddles land in it and survive CC's
CAS guard) is **🟡 capture-pending** (0 scrubs instrumented). The arm's **direction is certain, its
rate is not** — stated as modeled, never as a measured value (house style).

## (3) Recommendation — predicate C, because #467 removed the veto

The arms above weigh **frequent measured churn** (4.6 rotation-yanks/day, all recoverable) against a
**rare modeled scrub increase**. What breaks the tie is **severity, and #467 re-based it**:

| | Before #467 | After #467 (merged, `20038fa`) |
|---|---|---|
| Scrub outcome | canonical emptied → **every session `claude /login`** (unrecoverable, fleet-wide) | daemon **autonomously adopt-targets** a live spare into the canonical within a poll cycle → sessions recover on **`continue`** |
| Yank outcome (the churn arm) | item stays live → sessions recover on `continue` | unchanged — recover on `continue` |

Post-#467 the two arms **converge in operator-visible severity** (both → `continue`); the scrub
merely adds one autonomous daemon adopt-target cycle. So the calculus inverts from #465's era: the
**rare, now-recoverable** scrub no longer vetoes cutting the **frequent, measured** yank-churn.

**Predicate C (recommended) — gate proactive keep-warm off on the active axis; lean on the reactive
backstop + #467.** Concretely: suppress the proactive `keep_active_warm` promotion path
(`src/daemon.rs:2702`) for the active account, and rely on the layered fallbacks that already exist —
`should_keep_warm_retry` (active-401 in-place re-warm, fires *exactly* when a session needs it), and,
for the residual dead-token window, #467's autonomous adopt-target. **Keep parked-account keep-warm
unchanged** (the #106 stash sweep — no live sessions there, so no scrub/yank exposure, and parked
stashes still need freshness). This satisfies #468's acceptance ("proactive keep-warm no longer
rotates the active shared token except under a tightened condition; the reactive backstop still
prevents active-token expiry mid-use").

**Fallback layering by `[refresh].enabled` (ADR-0015).** Predicate C operates *within*
`[refresh].enabled = true`: `should_keep_warm_retry` — the #282 reactive keep-warm backstop C leans on
— is itself **`[refresh].enabled`-gated** (ADR-0015 § Decision step 4), *not* the "reactive
unconditional" path (that is the separate **#162 parked-account** refresh, hoisted out of the toggle).
So with `[refresh]` **off**, neither proactive nor reactive keep-warm fires and a dead active token
recovers via the **#42 emergency swap** to a live spare — exactly the fallback that recovered the
07-03 pre-keep-warm death (§ 1). C therefore *tightens the active-proactive axis #468 scopes and
nothing else*: it removes the pre-emptive live-canonical rotation while leaving every existing
recovery layer (reactive keep-warm when refresh is on, emergency swap when off, #467 for a scrub)
intact — consistent with ADR-0015's posture that every **live**-canonical rotation stays opt-in.

**Alternatives evaluated:**

- **A — shrink the near-expiry horizon (keep as fallback, do not ship first).** Instead of gating
  proactive off, fire it only *very* close to expiry (a horizon << `cadence + stagger`), so a busy
  account's sessions win the re-warm race in most windows and proactive fires only as a last-resort
  when truly nobody refreshed. Cuts churn on in-use tokens while retaining a short-lead freshness
  backstop. Rejected as the *primary* only because #467 makes the simpler full gate-off's residual
  scrub cheap; **A is the documented fallback** if post-deployment capture (§ below) shows #467's
  recovery latency too slow or the scrub numerator above the modeled-low expectation.
- **B — fingerprint-change throttle (rejected).** "Skip proactive if the #464 canonical fingerprint
  rotated within the last cadence." Largely **redundant with the existing near-expiry gate**: any
  external rotation (session/swap/reactive) resets the canonical's `expiresAt`, so *reaching*
  near-expiry already implies the fingerprint has been stable for ≈ (TTL − horizon). The throttle
  would rarely fire differently from the gate already in place — added state, negligible new
  suppression. Not worth the complexity.

**The one caveat that must travel to #468:** the reactive backstop fires *post-401*, so a residual
scrub-eligible window remains before it re-warms. It is bounded by #467, **not eliminated**. Ship C
with #467 deployed; do not gate proactive off in a build lacking #467's autonomous recovery, or the
umbrella's original unrecoverable-scrub severity returns.

## Downstream

- **#468 — unblocked with predicate C.** This note is the gating report; #468 may ship the active
  proactive gate, leaning on `should_keep_warm_retry` + #467, parked keep-warm untouched.
- **#467 — confirmed load-bearing (again).** #465 flagged recovery as carrying the resilience; #476
  shows it also **re-bases the prevention tradeoff** — gating proactive keep-warm is only safe
  *because* #467 recovers the scrub it makes marginally likelier.
- **Follow-up capture (closes the 🟡 items for BOTH #476 and #465):** rebuild + restart the daemon on
  the #464/#475 build, then over an active window measure (a) the scrub numerator, (b) per-episode
  `canonical_scrubbed ↔ preceding freshness`, and (c) the **redundant fraction** of proactive mints
  (external canonical rotations in the near-expiry window). (c) directly measures how much of the
  ~44 % churn cut is free vs. replaced by session/reactive refreshes — the one number that would
  upgrade this recommendation from *direction* to *magnitude*.

## Provenance

Analysis of the daemon's own structured event log (`~/Library/Logs/sessiometer/sessiometer.log`),
~1040 events, analysis cut-off **2026-07-11T17:56:00Z** (full window from 2026-07-01T12:19:18Z; the
live daemon keeps appending, so the total drifts, but the cited canonical-write counts are stable at
that cut-off; `grep`/`uniq` over the `event=`/`trigger=` fields; no credentials read or written; no
network call). Public-safety (#463): this characterizes
**Sessiometer's own daemon behavior** from its own logs; observable Claude Code behavior is cited
from #466/#470, not re-derived. Operator account labels (operator-chosen, email-shaped) are not
reproduced here (counts/enums/timestamps only; the log is `#15`-clean — no token-shaped material).
Rates are descriptive of one operator's ~10-day window, not a fleet-general constant. The
churn-arm figures are **measured**; the scrub-arm rate is **modeled** and the numerator marked
capture-pending — never presented as measured. Cross-checks:
[#465](0465-multi-session-rotation-interference.md) (the shared boundary + rotation model),
[ADR-0015](../adr/0015-reactive-refresh-unconditional-proactive-gated.md) (reactive/proactive gate
policy), [ADR-0018](../adr/0018-shared-credential-scrub-multi-writer-lockout.md) (the observable
scrub model), [#466](../../build/version-compat.md) (CC scrub decode + CAS guard), #467 (the
autonomous recovery this recommendation leans on), #464 (the instrumentation this spike still owes a
capture to). Code cited: `src/daemon.rs` — `keep_active_warm` (proactive gate + throttle),
`should_keep_warm_retry` (reactive backstop), `keep_warm_and_promote` (the shared promote core).

Daemon log 2026-07-01 → 2026-07-11 · sessiometer #476 · umbrella #463.
