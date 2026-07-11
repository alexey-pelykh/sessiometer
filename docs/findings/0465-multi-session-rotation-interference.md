# Finding #465 — multi-session refresh-rotation interference on the shared credential (spike)

The measurement half of the **#463** umbrella: *why do only a few of many concurrent sessions hit
the scrub, and how often?* This settles the mechanism and the exposure **denominator** from real
daemon logs, and honestly scopes the one part that needs a captured episode.

**Verdict.** The scrub is a **cross-process refresh race on the always-rotating shared item**, not a
server-side revocation. The refresh token rotates on **every** exchange (daemon logs: **141**
`rotated=true`, **0** `rotated=false`), so a dead prior token is continuously produced; Claude Code scrubs
when such a dead token is the canonical item's *current* value at the instant a refresh hits it and no
fresh token has landed (the CAS guard decoded in [#466](../../build/version-compat.md)). Only the few
sessions whose read→refresh *straddles a rotation boundary* **and** whose failing refresh finds the
dead token still stored are exposed — the intersection of two low-probability conditions — and the
**first** such session scrubs the item for the whole fleet. A cross-process refresh **lease that
external `claude` sessions honor is infeasible** (CC exposes no lock/hook at the scrub site — #466);
the daemon can only shrink its own share of shared-item churn (#468, with the #476 trade-off) and
**recover fast** (#467). Prevention alone cannot close the window.

## Data-availability boundary (read first)

This spike depends on the #464 instrumentation (`diag=canonical`, `event=canonical_scrubbed` /
`canonical_restored`). #464 merged to `main` at `a3c2618` on 2026-07-11 14:59Z; the daemon **running**
during this analysis (PID 82060, started 04:27Z) **predates that merge by ~10 h** — and a merge to
`main` does not swap an already-running process, so even the post-merge tail of the log carries
**zero** `diag=canonical` and **zero** `canonical_scrubbed` lines (grep-confirmed). Consequences,
applied throughout:

- What is **settled from real data**: the rotation model, the rotation/exposure **rate by source**,
  the concurrency churn density, and the two visible death episodes.
- What is **capture-pending** (needs the #464 instrumentation to run through a live scrub): the direct
  per-episode correlation (`canonical_scrubbed` ↔ the specific preceding swap / keep-warm / session
  refresh) and the scrub **numerator** (scrubs per active-hour, per swap). These are marked
  🟡 *capture-pending* below, never asserted as measured.

The scrub is **structurally invisible** in this pre-instrumentation log — which is itself the finding
that motivated #464: the two `credential_dead` events present are the daemon's own 401-quarantine
edge, **not** the scrub (see § Death episodes). Deploying the #464 build (rebuild + restart the
daemon) is the prerequisite for the capture-pending items; it is an operational step outside this
analysis, tracked as the follow-up in § Downstream.

## Results at a glance

| # | Acceptance question | Verdict | Basis |
|---|---|---|---|
| **1** | Trigger: rotated-out/dead token is canonical's *current* token at refresh time (vs server-side revocation)? | ✅ **confirmed mechanism**; revocation **refuted** as driver | rotation model (real data) + CC CAS/race guard (#466) |
| **1′** | Per-episode `scrubbed ↔ preceding rotation` correlation | 🟡 **capture-pending** | needs #464 live through a scrub |
| **2** | Rate — how often per active-hour / per swap; dominant rotation source | ⚠️ **denominator measured; numerator capture-pending** | 95 canonical writes/10.12 d; swap is the dominant canonical-touching source |
| **3** | Concurrency model + cross-process guard viability | ✅ **characterized; true lease infeasible** | churn density (real data) + CC lock model (#466) + daemon swap-lock (#64) |
| — | *Why only a **few** of many concurrent sessions?* | ✅ **answered + bounded** | straddle ∧ CAS-still-dead intersection |

## (1) Trigger — confirmed race, revocation refuted

**The rotation model is real-data-confirmed.** Across the 10.12-day window **no** cycle ever reports
`rotated=false`: every `refresh` (86) and `keep_warm` (31) carries `rotated=true`, as do 24 of 49
`poll_refresh` (the other 25 predate the #279 rotation field) — **141** rotations, **0** non-rotations.
So each exchange retires the prior refresh token server-side immediately, and a
*dead* prior token is a continuous by-product of normal operation — no server-side revocation is
required to produce one.

**The scrub condition (from #466's decode of CC 2.1.207).** CC's first-party refresh POSTs the stored
refresh token once; on `invalid_grant` it clears both tokens **only** if, at failure time, (a) no
concurrent writer already landed a fresh access token (`accessToken` race-check →
`tengu_oauth_token_refresh_race_recovered`) **and** (b) the stored refresh token is **still the dead
one it just used** (CAS `refreshToken===c`). So the trigger is exactly *"a rotated-out/dead token is
the canonical item's current value when a refresh hits it, with no fresh replacement landed"* —
**confirmed**. Server-side revocation of a still-current token is **refuted** as the driver: it is
unnecessary (rotation already supplies dead tokens) and unevidenced (the only deaths in-window are
401-quarantine, not refresh-token revocation).

**Which rotation source leaves a dead token as the canonical's current value** (mechanism; the direct
attribution is 🟡 capture-pending):

- **A concurrent `claude` session** — the dominant, log-invisible writer. Session A refreshes and
  writes a new token; session B, which read the old token microseconds earlier, refreshes it after
  A's rotation. This is the unguarded cross-process race (§ 3).
- **`swap`** installs whichever token the target account's stash holds (`rotated=0` — a swap promotes,
  it does not itself refresh). If that stash is stale (holds a rotated-out token — the #477 shape),
  the swap lands a dead token in the canonical. *Not observed in-window* (all stash writers
  re-stashed fresh — see § 2), but mechanistically live.
- **`keep_warm`** rotates the active token and promotes the fresh one to the canonical
  (`refreshed_not_restashed` is its normal success outcome, 31/31 here). It keeps the canonical
  *fresh* (protective — the basis of #476), but each cycle still retires the prior active token, so a
  session that read that prior token and refreshes in the gap is exposed.

## (2) Rate — exposure denominator measured; scrub numerator capture-pending

Real-data rotation rate over the window (2026-07-01 12:19Z → 2026-07-11 15:06Z; 10.12 days; 153
distinct active hour-buckets):

| Source | Writes | Rotates canonical? | Per day | Per active-hour |
|---|---|---|---|---|
| `refresh` (parked stash, #106) | 86 | no — writes the stash | 8.5 | 0.56 |
| `swap` (#11/#42) | 64 | **yes** — repoints the item | 6.3 | 0.42 |
| `poll_refresh` (parked stash, #162) | 49 | no — writes the stash | 4.8 | 0.32 |
| `keep_warm` (active, #282) | 31 | **yes** — promotes to the item | 3.1 | 0.20 |
| **Canonical-touching total** | **95** | | **9.4** | **0.62** |

- **Dominant canonical-touching source: `swap`** (64), then `keep_warm` (31). Only these two daemon
  paths write the *canonical item*; `refresh`/`poll_refresh` write the isolated per-account **stash**
  (the #253 active-exclusion), so they create no fleet-wide dead-token window directly — they matter
  only via the swap-installs-stale-stash path above, which did **not** fire in-window (0
  `refreshed_not_restashed` among the 135 stash writers; all `refreshed`).
- **Swap reasons**: 55 `session`, 6 `weekly`, 2 `manual`, 1 `forced` — churn is overwhelmingly
  autonomous session-driven rotation, not operator action.
- **The scrub numerator is not yet measurable** (🟡 capture-pending; pre-instrumentation log). The
  expected rate is *low* — the scrub is the CAS-surviving subset of straddling refreshes (§ 3) — which
  is consistent with the umbrella's field observation (a ~4 h window with live "Not logged in"
  lockouts yet **zero** `credential_dead`). The **denominator above bounds the exposure**: the
  canonical is rotated ~9.4×/day by the daemon *plus* once per session refresh (log-invisible, and
  in a busy fleet the dominant term).

### The two rates, separately: yank-rate vs scrub-rate (#475)

The live forensics split the single "Not logged in" symptom into **two modes of opposite severity and
remedy**, which this finding reports **as two distinct rates** — the conflation [#475](https://github.com/alexey-pelykh/sessiometer/issues/475)
removes. Each is now its own `grep`-able signal in the daemon's own log (the #475 `mode=` instrumentation,
landing with this refinement), so the split is directly measurable going forward — not inferred by
cross-correlating fingerprint deltas against swap/keep-warm lines by hand:

| Mode | Trigger | Signal (#475) | Rate | Remedy |
|---|---|---|---|---|
| **rotation-yank** (frequent, RECOVERABLE) | the shared canonical ROTATES while a session is mid-flight — a daemon `swap` / `keep_warm`, or a concurrent `claude` session refresh — so a session pinned to the outgoing token gets a 401 while the item stays live | `mode=yank prev=<fp>` on the per-poll `diag=canonical` line (a Present→Present fingerprint delta) | **measured: ≥ 9.4 canonical rotations/day** (0.62/active-hour) by the daemon alone (§ 2 table), **plus** one per concurrent session refresh (log-invisible, dominant in a busy fleet) | none — sessions self-recover on `continue` |
| **invalid_grant-scrub** (rare, UNRECOVERABLE) | a rotated-out/dead token is the canonical's *current* value when a refresh hits it AND no fresh token landed (the CAS-still-dead straddle, § 1 / § 3) — CC empties the item on the first `invalid_grant` | `event=canonical_scrubbed mode=scrub` (a durable Present→empty edge) | **numerator 🟡 capture-pending** (pre-instrumentation log carried zero scrubs); modeled *low* — the CAS-surviving subset of straddling refreshes | every session needs `claude /login` |

The two signals sit on **different log sinks**, by the #475 design: `mode=scrub` is a durable
`event=` line in `sessiometer.log`, while `mode=yank` rides the per-poll `diag=canonical` line on the
**`-v` diagnostic channel** (as this finding's own methodology already runs the daemon — § Provenance).
This asymmetry is deliberate: a yank needs no *new* durable event. Every canonical rotation the daemon
can attribute is already durably logged — its own writes as `swap` / `keep_warm` (§ 2), an
externally-authored rotation it detects as `re_stash` / `uncaptured_login` (§ 3) — while the sub-poll
session refreshes that dominate the churn are invisible to *any* daemon event alike. A durable
`canonical_yanked` would therefore only duplicate the former while still missing the latter; `mode=yank`
instead labels the observable rotation series on the channel this analysis already reads.

The **yank-rate is the canonical-rotation rate** measured in § 2: the #475 marker fires on every
observed Present→Present rotation, so its count *is* that rotation series — and the yank-*rate* is
quantified from the durable § 2 rotation counts (swap/keep-warm `event=` lines), so it holds even
without `-v` (whether a given rotation actually stranded a live session is client-side and unobservable
to the daemon — the rate bounds exposure, § "Why only a *few* …"). The **scrub-rate is the
`canonical_scrubbed mode=scrub` count** — an
edge-triggered *subset* of the yanking rotations (only those that straddle a rotation *and* survive CC's
CAS guard), which is why it is rare in absolute terms yet fleet-wide in blast radius. The two are **not
independent**: every scrub is preceded by a yank-eligible rotation; almost no yank becomes a scrub.

## (3) Concurrency model + cross-process guard

**Who writes the one shared `Claude Code-credentials` item:** every local `claude` session (on its own
refresh — CC's behavior), plus the daemon's `swap` and active-account `keep_warm`. One shared item ⇒
one global token ⇒ every writer races every other writer.

**What is guarded vs unguarded:**

- **Daemon-vs-daemon: guarded.** The daemon serializes its own canonical writes behind the
  single-writer swap lock (#64). The tightest observed gap between two daemon canonical writes is
  **4.0 s** (6 pairs within 60 s, 15 within 5 min), but these are *sequential lock-holders*, not torn
  concurrent writes.
- **Single process's own concurrent refreshes: guarded by CC.** CC de-duplicates via the CAS +
  `tengu_oauth_token_refresh_race_recovered` (#466).
- **Session-vs-session and session-vs-daemon: unguarded.** CC holds **no cross-process lock** on the
  shared item; the only lock at the scrub site is `ELOCKED` keychain-write contention
  (`maxRetries:5`), not a refresh lease (#466). N sessions + the daemon are mutually unprotected —
  exactly the umbrella's "no cross-process lock" observation.

**Can Sessiometer add or emulate a cross-process guard? A true lease: no.** CC owns the shared item
(`g.claudeAiOauth`) and will not acquire an external Sessiometer lease before its first-party refresh
— there is no hook or knob at the scrub site to make it wait (#466 established the knob-absence). So a
lease that *external sessions honor* is infeasible; the daemon's own #64 lock only covers the daemon's
writes. What remains actionable:

1. **Shrink the daemon's share of shared-item churn** — gate proactive keep-warm of the *active*
   account (#468). Weigh against #476: keep-warm landing a fresh token in the canonical is precisely
   what keeps a straddled window *recoverable*, so this trades churn-cut against scrub-exposure rather
   than being a pure win.
2. **Keep the canonical fresh** so a straddling session more often reads a live token (the protective
   side of keep-warm).
3. **Recover fast** — autonomous adopt-target of a scrubbed/empty canonical (#467). Because prevention
   is bounded by CC's un-lockable refresh, recovery is the load-bearing mitigation.

### Why only a *few* of many concurrent sessions hit the scrub

1. **Most of the time the canonical holds a live token** (the steady state *between* rotations, i.e.
   the vast majority of wall-clock). A session that reads and refreshes succeeds. Scrub-eligibility
   exists **only** in the narrow window straddling a rotation (old token dead, new token not the one
   this session holds).
2. **Of the sessions that do straddle and refresh with the now-dead token, CC scrubs only the subset
   where the item still stores that dead token at failure time** (the CAS guard). If any concurrent
   writer already landed a fresh token, CC race-recovers and does **not** scrub.
3. So the scrub is the **intersection of two low-probability conditions** — *straddle* ∧
   *CAS-still-dead* — which is why only a few of many sessions hit it, why it is **rare in absolute
   terms yet fleet-wide in blast radius** (the first such session empties the item for everyone,
   edge-triggered), and why it is bounded by the measured canonical rotation rate × a small
   straddle-and-survive probability.

## Death episodes (why the scrub is invisible here)

Both `credential_dead` events are the **401-quarantine** edge — `monitor_401` `consecutive=1,2,3` →
`credential_dead` (confirming `monitor_401_n=3`), account-A, at 2026-07-03 22:46Z (→ `emergency_swap`)
and 2026-07-10 20:19Z. These are the *access-token* streak, **not** the refresh-token scrub, which
fires on the **first** `invalid_grant` and leaves **no** `credential_dead` (the umbrella's asymmetry).
The captured log therefore cannot show a scrub even though the mechanism is confirmed — the direct
evidence gap #464 exists to close.

## Downstream

- **#467 (autonomous adopt-target) — confirmed necessary and load-bearing.** Prevention is bounded by
  CC's un-lockable refresh, so recovery carries the resilience.
- **#468 (gate proactive keep-warm) — confirmed a partial mitigation, not a pure win.** It reduces the
  daemon's canonical-churn share (measured 31 keep-warm writes = 33% of the 95 daemon canonical
  writes) but trades against recoverability (#476). Quantifying that trade is #476's job.
- **#475 (distinguish rotation-yank from scrub) — landed with this refinement.** This finding now
  reports the two as separate rates (§ "The two rates, separately"): the *measured* rotation-yank
  (swaps + keep-warm + session refreshes, frequent, recoverable, `mode=yank` on `diag=canonical`) vs
  the *modeled* scrub (rare, unrecoverable, `event=canonical_scrubbed mode=scrub`). The `mode=`
  classification makes each mode a first-class `grep` axis rather than a hand-correlated fingerprint delta.
- **#477 (stash staleness)** — the swap-installs-stale-stash trigger path is mechanistically live but
  **did not fire in-window** (0 `refreshed_not_restashed` stash writes); worth confirming under load.
- **Follow-up capture (prerequisite for the 🟡 items):** rebuild + restart the daemon on the #464
  build, then over an active window correlate each `canonical_scrubbed` with the preceding
  `diag=canonical` fingerprint change and the swap/keep-warm/session refresh that produced it — giving
  the scrub numerator and the per-episode attribution this spike scopes but cannot yet measure.

## Provenance

Analysis of the daemon's own structured event log
(`~/Library/Logs/sessiometer/sessiometer.log`), 973 events over 2026-07-01 → 2026-07-11
(script: `.tmp/analyze-465.py`, session-local). **No credentials read or written; no network call.**
Public-safety (#463): this characterizes **Sessiometer's own daemon behavior** from its own logs;
observable Claude Code behavior is cited from #466/#470, not re-derived. Operator account labels
(which the operator chose email-shaped) are **redacted to `account-A…`** here — the log itself is
`#15`-clean (handles / enums / counts / timestamps only; no token-shaped material present). Rates are
descriptive of one operator's ~10-day window, not a fleet-general constant. Cross-checks:
[ADR-0018](../adr/0018-shared-credential-scrub-multi-writer-lockout.md) (owns the observable model),
[#466](../../build/version-compat.md) (CC scrub decode + knob-absence), #464 (the instrumentation this
spike consumes).

Daemon log 2026-07-01 → 2026-07-11 · sessiometer #465 · umbrella #463.
