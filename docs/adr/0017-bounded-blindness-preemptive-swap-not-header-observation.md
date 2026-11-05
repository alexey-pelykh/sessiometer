---
type: architecture-decision-record
number: 17
title: "Bounded-blindness preemptive swap-away, not header-based active-observation"
date: 2026-07-11
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0017: Bounded-blindness preemptive swap-away, not header-based active-observation

## Status

**Accepted** — 2026-07-11. Records the **#452** decision to swap the active
account away **preemptively on bounded blindness** — spending the *swap* lever when
fresh near-limit active usage is provably unavailable — and, importantly, to
**reject header-based active-observation** as the means of getting that freshness,
so the "just read the rate-limit headers" idea is not re-litigated each time the
blindness bites.

Like ADR-0009 and ADR-0012, this ADR records a **shipped** behaviour change, now
enforced in `src/daemon.rs` (**#452**), with constants `T=300s` / `risk_band=60%`
(**but see Amended 2026-11-02 (#1593)**: an active with no session window has no session window
for them to apply to) confirmed on two weeks of real telemetry — the **#451** premise-confirmation spike
validated the premise and the interim `T`, and **#484** ratified the conservative
60% band (the "fire-early-is-cheaper" asymmetry for unattended runs) over the
interim 65%. It **supersedes nothing**: it *affirms* [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)
(active-observation stays favoured), *extends the consequence* of
[ADR-0009](0009-rate-limit-backoff-per-account.md) for the active account, and
*cross-references* [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md).
No existing swap-eligibility predicate moves; the reactive and emergency paths are
untouched.

**Amended 2026-07-15 (#539)**: the velocity-projection arm this ADR had carried as a
deferred Future note is now **shipped** — the OBSERVED-overshoot peer of the reactive
session trigger (`observed + rate_ema × H >= trigger` off a FRESH reading + retained
per-account EMA velocity), closing the reaction-latency residual of **#363** that the
blind-window arm does not reach. Parameters validated by the **#538** spike; the
covered/poll-gap residual split with **#540** and the shipped mechanism are recorded in
Consequences and Related below. The core **#452** decision and the header-observation
rejection are unchanged.

**Amended 2026-07-17 (#584)**: the **REPORT** side of this ADR (the `status`
`auto_protection_degraded` projection, not the swap decision) gains a third arm. The
original arming predicate compares a *level to a level* — the frozen pre-blind anchor
against the `risk_band` — and so is blind to **velocity** and **blind-window length**:
on 2026-07-17 an account sat at anchor `0.29` (below the `0.60` band) while climbing
2.7–4.14 %/min, went blind past the interim `T`, and burned to exhaustion while `status`
reported `auto-protection OK`. At long blind windows there is no anchor low enough for the
level-vs-level comparison to stay safe, so re-tuning the band cannot fix it — the predicate
*shape* is wrong. The new arm projects the anchor forward over the blind window at the
retained **#539** EMA rate and reports DEGRADED when
`anchor + rate × k × blind_secs >= session_trigger`. Because the blind horizon runs 5–8×
past the #538-validated `H ≈ 150 s` envelope, it projects on a **bias-HIGH bound**, not the
point estimate: `k = BLIND_VELOCITY_RATE_INFLATION` (interim **1.75**, basis `4.14/2.7 ≈ 1.53`
rounded up, recorded on the const, ratification-pending on the **#583**-uncensored
distribution). It is **REPORT-ONLY** — the first `auto_protection_degraded` arm that fronts no
swap: acting on a deliberately-inflated projection off a stale anchor is a higher-confidence
decision than an honest status line needs, and the same swap-timing asymmetry that justifies
biasing the *report* HIGH argues against *acting* on it (a false swap thrashes a good target —
the cost the #582 breaker bounds). Whether the blind arm should ever *swap* on velocity is left
as a separate design. The core **#452** decision and the constants it ratified are unchanged.

**Amended 2026-07-17 (#586)**: the **bounded-blindness premise itself** — this ADR's
foundation, that the active blind window is capped at 120s by **#453**, its length set by the
daemon's *own* back-off and never a server `Retry-After` — is **falsified**. On 2026-07-17
`429`s on **active** accounts carried `retry_after_secs=3600`, the **first non-zero** values
among 581 logged back-offs (579 of which carried `0`); the window ran a self-renewing **hour**
(`consecutive=2` a full hour after the first), not 120s, and the constants this ADR calibrated
(`T=300`, `risk_band=60%`) were fitted to a distribution whose *maximum* window was the S1
spike's 755s. It was **not a decayed fact but an artifact of a dormant branch**: the active
`Retry-After` floor in `note_account_backoff` (`Some(ra) if is_active => widened.max(ra)`,
**#453**) reduces to `widened.max(0) == widened` (≤ `ACTIVE_POLL_BACKOFF_CAP` = 120s) whenever
`ra == 0`, which held for 579 of those 581 — so the un-clamped branch had **never once fired**.
The ADR generalised the 120s windows it had observed into "the window is bounded at 120s,"
calibrating on a sample that **structurally excluded** the failure mode; the branch woke and
the premise died the same second.

This **resolves the #453 record contradiction in this ADR's favour**: **#453's own body** calls
the 2026-07-10→11 window "Retry-After-dominated (server dictated 755s)," while this ADR records
all 181 of that window's `429`s at `retry_after_secs=0`. The event log settles it — 579/581
back-offs were `0`, the first non-zero was 2026-07-17 — so **this ADR read the telemetry
correctly and #453's body is the incorrect record**, which is exactly why the un-clamped floor
#453 shipped sat dormant and unvalidated for 581 back-offs. The guard is inverted where it
counts: **#294** (cap the honoured `Retry-After` so a pathological value can't dark the daemon
— closed, `f56f2f1`) clamps **peers only** (`POLL_BACKOFF_CAP` = 3600s), and #453 **exempts**
the active account, so every account is bounded except the one whose blindness matters — and
the peer clamp is **inert at this very value** (`widened.max(3600).min(POLL_BACKOFF_CAP = 3600)`
is `3600`), so at the server's chosen `3600` neither peer nor active is bounded. The throttle
**follows the active role**, not the poll rate — each `3600` landed within minutes of an account
taking the active slot, while a full day at `poll_secs=150` drew 267 `429`s and **zero** `3600`s.

**The decision stands — vindicated, not superseded.** A server-dictated hour-long blind window
makes "spend the swap lever, not the observation lever" *more* urgent and leaves the
header-observation rejection untouched, so nothing is reversed. Boundedness is **restored by
acting**, not by re-asserting the premise: **#582** treats a nonzero active `Retry-After` as a
swap-away signal in its own right — a swap needs no usage poll, so it fires while blind —
reusing `reason=blind_preempt` and the `session_blind_swap_secs` bound (whose kill-switch now
disables both arms) and changing **no** back-off arithmetic; the report side of the same
episode is the **#584** velocity-projection arm recorded above. Keeping the active floor
*honoured* rather than clamping it (as #582 weighed and rejected) is an engineering call, not
conformance: clamping does not restore sight — every clamped re-poll `429`s and re-clears the
reading, outcome-identical to doing nothing, and would invert #453's ratified absolute-floor AC
(the server is within spec — RFC 9110 §10.2.3 scopes `Retry-After` to 503/3xx as "ought to
wait," RFC 6585 §4 lets a `429` carry it). The core **#452** decision and its constants are
unchanged.

**Amended 2026-11-02 (#1593) — the retained anchor's session window may not exist: for an active
with no session window, "never fires on a `None`" covers a `None` that is structural rather than a
failed poll, and the constants `T=300s` / `risk_band=60%` have no session window to apply to.**
Since 2026-10-30 Anthropic has been removing the five-hour session window from verified Ultra
accounts, per account and automatically, so such an account's usage reply carries
`"five_hour": null` beside a present weekly window; Pro and Max accounts keep their session window,
so a fleet holds both shapes at once (the dated sources are replicated in
`docs/requirements/optional-quota-windows.md` § 2.2). This ADR's swap arm keys on the retained
pre-blind anchor's **session** fraction against `risk_band`: `blind_swap` (`src/daemon.rs`) compares
the plausibility-corrected anchor (`plausible_anchor_session`, `src/swap.rs`) with the config
tunable `session_blind_risk_band`, and the `status` projection's `blind_gate_armed`
(`src/daemon.rs`) compares the same anchor with the interim constants `BLIND_GATE_SECS` /
`BLIND_GATE_RISK_BAND`, default-equal to the tunable. Every passage in this record assumes that
fraction exists for every active. That premise is falsified on the wire; the decision stands and its
premise is rewritten.

**The decision in force, under open decision D3.** An anchor whose session window does not apply is
such a `None`. Under **D3** `blind-gate-weekly-only-active`
(`docs/requirements/optional-quota-windows.md` § 8) — the pipeline's default, pipeline-recommended,
ratification-pending, not an operator ruling — the gate reads false for such an active, because
there is nothing on the session axis to protect, and `status` claims no auto-protection on its
strength rather than reporting `auto-protection OK` over an account it cannot protect. A `None` is
therefore of two kinds: a failed poll, which this ADR always covered, and a window the account does
not have, which it did not. Whether a weekly anchor should arm the swap against a weekly band — a
new constant, a further amendment to this ADR and a swap that fires on the slower-moving axis — is
the arming half of the same question, left open as **OAD-2** `blind-gate-weekly-only-active` in
`docs/design/optional-quota-windows-solution-design.md` § 12 (§ Open architecture decision stubs),
the operator's to decide and outside #1590. The core **#452** decision, its constants, the
header-observation rejection and the **#582** `Retry-After` arm (which keys on no session anchor)
are unchanged; the **#584** velocity report arm projects the same session anchor, so for such an
active it has nothing to project and holds on an absent anchor (its rule is Rule 3 of
`docs/specs/decision-arms-absent-window.feature.md`).

**What the code does at this amendment's merge, and where the decision lands.** The tree does not
yet behave this way. The hot-fix #1613 (on `main` since 2026-11-02) serves a weekly-only reply with
the session fraction read as `0.0` and no session reset stamp, so the retained anchor of such an
active carries an idle session rather than an absent one: the gate reads false there by accident of
that `0.0` sitting under the band, not by this rule, and a blind weekly-only active renders as
`last-known session 0% — auto-protection OK` — the misreport D3's surfacing half exists to remove
(`docs/requirements/optional-quota-windows.md` § 12, premortem row P-5). The parser item #1596
replaces that read with `None`; the reading-model item #1597 lifts `LastGood` and `BlindAnchor`
(`src/daemon/records.rs`) to carry "does not apply"; the decision-arms item #1602 has
`blind_gate_armed` and `plausible_anchor_session` take the optional fraction and read false on
`None` (`docs/design/optional-quota-windows-solution-design.md` § 12 AD-5), with `blind_swap`'s own
comparison and the #584 arm's `blind_velocity_projected_armed` (`src/daemon.rs`) holding on an
absent anchor beside them (the same document's § 5 B-3 row 7; the spec above, Rule 3); and the
`status` half lands in two steps — until the status-wire item #1599 lands, the transitional state
the design's § 12 AD-6 records under D10 (author-chosen sub-default, ratification-pending) emits no
`blind_active` block for a weekly-only blind active, so `status` claims no auto-protection on its
strength at the cost the design's § 5 B-6t states (the row reads like a failed poll), and #1599 then
adds a report-only `blind_active_view` arm that reads degraded past the blind gate for a
session-less anchor — all under umbrella #1590. This record lands before them deliberately:
requirement `REQ-REC-C-001` (`docs/requirements/optional-quota-windows.md` § 10.10,
pipeline-authored and ratification-pending) asks that no commit reverse a pinned behaviour without
its record. This record takes D3's default as the premise those arms implement against; the arms are
#1602's and #1599's, and the ruling on D3 is the operator's.

**Provenance.** The operator's direction of 2026-11-01, quoted verbatim in
`docs/requirements/optional-quota-windows.md` § 2.1: "we need to make the parser more robust and
flexible, maybe eventually they will make some account w/o weekly limits e.g." — each quota window
independently optional, a weekly-less account the explicit future case; the request that opened the
work, from the same day, is quoted beside it. The corrected-premise sentence recorded above is the
solution design's draft (`docs/design/optional-quota-windows-solution-design.md` § 5 B-10); the rest
of this amendment was written for #1593 against that design and the requirements document. All of it
is pipeline-authored and ratification-pending under umbrella #1590; the operator has reworded
nothing and ratified no sentence here.

**Amended 2026-11-11 (#1692) — the header-observation rejection reaches every request this daemon
would send to a model endpoint itself, for every credential class and every purpose; a credential
the usage endpoint refuses for scope has no usage source and is to be refused at capture; disqualifier
(a)'s premise has decayed, and the rejection now stands on (b) alone.** Fork pull request #1588
proposes a second usage source. For a credential whose OAuth scopes include inference but not
profile access (the `claude setup-token` class, which the pull request registers from a token file
as a "token-only account"), the daemon would send one minimal request of its own to
`https://api.anthropic.com/v1/messages` and read the `anthropic-ratelimit-unified-5h-*` and `-7d-*`
response headers, selected automatically from the credential's scope list, at every poll of that
account, active or peer, and for the pre-swap canary. Its hunk on this record calls that an
"Experimental exception" and says the decision "remains unchanged for profile-scoped credentials".
The decision draws no such line: Alternatives considered item 2 names the mechanism, "a
minimal-inference probe with the subscription OAuth tokens", as disqualifier (b), and the Decision,
the Consequences ("`/oauth/usage` remains the **only** usage source — no header probe is added") and
the #539, #586 and #1593 amendments restate the rejection with no credential-class or purpose
qualifier. Re-labelling the probe a primary usage source for one credential class changes its
purpose, not the mechanism (b) rejects. **The decision stands; the pull request's exception does
not land.**

**The ruling, in mechanism terms.** (i) `GET /api/oauth/usage` stays the one usage source and the
one outbound request this process originates (README § Privacy: no telemetry; the capability guard
`the_sole_egress_endpoint_is_the_documented_usage_url` in `src/usage.rs`), and the daemon sends no
request that carries another client's name in its `User-Agent` (the proposed probe sends
`claude-code/2.1.0`). (ii) A request to a model endpoint spends the quota it would read, at the
documented cadence: under ADR-0012 the active account is polled about every `2·poll_secs/N` and a
peer about every `2·poll_secs·(N-1)/N`, `poll_secs` default 300, and near its limit
`near_limit_poll_secs` (default 60) caps the active account's poll spacing so it is re-polled at
least about every 120 s whatever the roster size (for a roster of five the base cadence is already
that). For a roster of five at the defaults that is about 720 model requests a day for the active
account and about 180 for each peer, before back-off and slow-poll, and most of them exactly when
the account's quota is scarcest. The #1231 spike measured that one short request through the official client (`claude -p "say
pong"`, the refresh engine's prompt) opens a rested account's five-hour window in 130 of 131 cycles, so an automatic probe is also an
unthrottled form of the opt-in, per-account-throttled warm-up that #1232 scopes. (iii) The one
request this daemon sends beyond the usage GET, the online canary, is off by default and sends
nothing while off; a probe keyed on a scope list the registration path writes itself would spend an
account's quota from the moment the account is registered, with no setting that says so. No form
of the probe lands under this ruling: not automatic, not a per-account opt-in, not canary-only. An
opt-in would bound what a user spends and change nothing about the request; a canary-only form is
still a model request this daemon sends itself.

**What an inference-only credential gets.** No usage source. The usage endpoint answers `403`
`OAuth token does not meet scope requirement user:profile` to a `claude setup-token` credential
(measured on 2026-06-19; issue #5 records the requirement to surface it), the daemon surfaces that as
`Error::UsageScopeMissing` with the re-capture remedy. No path on `main` registers such a
credential today; the pull request's token-file path would. The disposition this ruling sets is
that registration of such a credential is to be refused at capture with the same remedy, rather
than rostering an account the daemon can neither rank as a swap target nor protect while active (the blind arm keys on a `last_good` anchor such an
account never gets). Not the #1612 window: that proposal widens, and never stops, a poll whose
answer is already known, exempts the active account, and fits a credential already rostered whose
scope metadata is unknown. Not the #1590 no-window rendering: that state means a window the account
does not have, and an account the daemon cannot read is not an account without limits.

**Disqualifier (a)'s premise has decayed; the rejection stands on (b) alone.** This record says the
upstream request to persist the headers, anthropics/claude-code#55333, "is open" and that "Passive
reading is impossible". That issue was closed as completed on 2026-05-01, before this record was
accepted, and the official client's status line now hands a subscriber session
`rate_limits.five_hour.used_percentage`, `rate_limits.seven_day.used_percentage` and their
`resets_at` after the session's first response, each window independently absent (the status-line
documentation, read 2026-11-05; the field schema is in the 2.1.318 binary). Whether a
status-line-fed reading, which needs no request from this daemon, should become a usage source is a
separate decision this amendment does not take; it would amend Alternative 2(a), not (b). "Either
disqualifier alone is fatal" (Alternatives considered, item 2) is what carries the rejection today.

**Mechanism claims the pull request rests on, tagged.** That a setup-token credential has inference
scope without profile scope: read from dated provider documentation ("It can only make model
requests", read 2026-11-05); the pull request discovers no scope from the token: its registration path
writes `scopes: ["user:inference"]` into the stored credential from a prefix check, and its
selector then reads that list back. The `403` from the usage endpoint: measured once,
2026-06-19. That a minimal request under a setup-token credential is accepted and its reply carries
both utilization headers: unverified; the pull request defers it to "separate live acceptance
checks". The quota one probe costs, and whether a header percentage and the usage endpoint's windows
coincide: unverified; no record exists.

**Falsifier, and the measurement it waits on.** The ruling rests on this record and on the egress
contract, not on a measurement, so no probe run is owed. It is falsified by (i) `GET
/api/oauth/usage` answering `200` to a freshly minted `claude setup-token` credential, operator-run
with the daemon's own headers, repeating the 2026-06-19 request and sending no model request: the
class then has a usage source and the capture refusal lifts, with this decision untouched; or (ii)
the decision-maker narrowing (b) to the active-account freshness purpose or to interactive tokens,
which would take a new record. A second check, owed before any record restates (a), spends one model
request through the official client, since the status line populates only after the session's
first response: run the official client under `CLAUDE_CODE_OAUTH_TOKEN` with a status-line command
that saves its input, and read whether `rate_limits` is populated for a setup-token session and whether its values match
the usage endpoint's at the same moment on an account that holds both token types.

**Provenance.** Prepared on 2026-11-05 by a three-lens council under the maintainer's 2026-11-02
delegation of decisions of this kind to a council; the maintainer approved this text on 2026-11-11,
and that approval is the ruling. The pull request author is asked to open an issue for the storage
concern and to split the pull request by concern; the split drops the probe, the `User-Agent`
override, the widening of the egress guard, the README probe paragraphs and the hunk on this record.

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a
roster, deciding swaps on each account's **last-known** usage reading. Active
observation is deliberately favoured over the peers — [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)
interleaves the active account into the poll schedule so it is re-read more often
than accounts that are merely swap targets. The active account is the only one that
can reach its usage ceiling *while active*, so it is the only one whose observation
latency matters.

**The bounded-blindness gap (S1 spike, 2026-07-10→11).** The active account `429`'d
on its `/oauth/usage` poll at session usage `0.68`, went **blind for 755s**
(daemon-back-off-dominated: all 181 observed `429`s carried `retry_after_secs=0`,
so the blind window is the daemon's *own* back-off — per-poll-capped at 120s by
**#453** — not a server-dictated `Retry-After` — **but see Amended 2026-07-17 (#586)**), and
Claude Code burned it to `0.98`
before any fresh reading returned. The reactive `session_trigger=95` swap **never fired** — it keys
off an *observation*, and no observation crossed the trigger because the account was
blind across the entire swap-away band.

Two accepted decisions explain *why* the blindness is bounded but unpierceable by
more observation:

- **Per-account back-off ([ADR-0009](0009-rate-limit-backoff-per-account.md)).** A
  `429` on the active account's usage poll — correctly — backs off **only its own**
  next poll (`poll_backoff_until`, floored at the server `Retry-After`), and the
  account **carries its last reading**. That per-account scoping is right and
  unchanged; its *consequence for the active account* is a blindness window whose
  length is set by the daemon's own back-off — the S1 `429`s all carried
  `retry_after_secs=0`, so that floor contributed nothing and the window was the
  back-off policy's (capped at 120s per poll by **#453**), not the server's — but see
  **Amended 2026-07-17 (#586)** for the nonzero-`Retry-After` case the active floor is exempt
  from.
- **Interleave, not harder polling ([ADR-0012](0012-active-reobservation-via-schedule-interleave.md)).**
  Interleaving re-reads the active account more often, but it cannot re-read a
  *backing-off* account — and lowering `poll_secs` to poll harder was already
  rejected there (it re-opens the **#80** burst-`429` exposure). More observation
  budget does not pierce a back-off wall.

**Fresh near-limit active usage is provably unavailable by any safe local means:**

1. **Claude Code does not persist its usage.** The `anthropic-ratelimit-unified-5h-*`
   headers carry exact usage on every inference response, but Claude Code stashes
   nothing locally to harvest (verified by a local sweep 2026-07-11; upstream FR
   [anthropics/claude-code#55333](https://github.com/anthropics/claude-code/issues/55333)) —
   **but see Amended 2026-11-11 (#1692)**: that request was closed as completed on 2026-05-01, and
   the status line now carries the windows.
2. **Eliciting the headers means impersonating Claude Code.** The only way to draw a
   fresh header is a minimal-inference probe using the subscription OAuth tokens —
   i.e. impersonating Claude Code — which courts an **account ban**.
3. **`/oauth/usage` self-blinds under pressure.** The account's own usage endpoint
   `429`s exactly when polled hard enough to stay fresh — the failure mode above.

So the daemon cannot buy a fresh reading in time. The decision spends a different
lever: swap the active account **away** *before* the missing reading would have
tripped the trigger, keyed on the retained pre-blind anchor.

## Decision

**Add a gated preemptive swap-away path — separate from `emergency_swap` — that
fires on *bounded blindness* of the active account, keyed on the retained pre-blind
anchor, and do NOT pursue header-based observation to get a fresher reading.**

The path fires when **all** hold:

- `blind_elapsed > T` — the active account's last reading is stale beyond the
  threshold, **and**
- `last_good.session >= risk_band` — the retained pre-blind anchor (**#450**)
  was already near the band, **and**
- **a viable swap target exists** — a peer below `target_max_session_usage`
  ([ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md)).

Confirmed constants `T=300s`, `risk_band=60%` — the **#451** premise-confirmation
spike validated the premise and the interim `T` on two weeks of real telemetry, and
**#484** ratified the conservative 60% band over the interim 65%. New tunables
`session_blind_swap_secs` (`=T`, default `300`) and `session_blind_risk_band`
(default `60`), hand-emitted and cross-field-validated
(`target_max_session_usage <= effective session trigger`) per
[ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md). Setting
`session_blind_swap_secs` arbitrarily high **disables** the path — a config
kill-switch.

The path honours the **#369** cautions on a reactive fast-path:

- **Never fires on a `None`/missing reading.** It keys off the retained `last_good`
  anchor (**#450**) — *not* the `429`, *not* `last_readings` — so a
  genuinely-unknown account produces no spurious swap. **But see Amended 2026-11-02 (#1593)**: a
  `None` may also be structural — an active whose session window does not apply.
- **Not `emergency_swap`.** A separate *availability* path: it does not raise the
  target gate to infinity, does not bypass cooldown, and preserves the
  dead-vs-exhausted model (**#42**).
- **Respects the no-torn-swap invariant
  ([ADR-0003](0003-no-torn-swap-invariant.md)).**

## Alternatives considered

1. **Reactive-only — wait for `session_trigger=95` to observe the near-limit active
   account** (status quo, rejected).
   - **Why rejected**: the S1 spike proved the reactive trigger cannot fire through
     a blindness window. The reading that would trip `session_trigger=95` never
     arrives in time — the account `429`s, goes back-off-blind, and Claude Code
     burns it to exhaustion before any observation returns. Reacting to an
     observation that will not come is a guaranteed late swap.

2. **Header-based active-observation — read the `anthropic-ratelimit-unified-5h-*`
   headers** (rejected; recorded explicitly so it is not re-proposed).
   - The headers carry the **exact** usage on every inference response, which is
     genuinely fresher than anything `/oauth/usage` can offer near the limit. Two
     **independent** disqualifiers rule it out:
     - **(a) There is nothing to harvest.** Claude Code does not persist the
       headers — a local sweep (2026-07-11) found no stash; the upstream FR to
       persist them ([anthropics/claude-code#55333](https://github.com/anthropics/claude-code/issues/55333))
       is open. Passive reading is impossible. **But see Amended 2026-11-11 (#1692)**: closed as
       completed on 2026-05-01; the status line now carries the windows, so (a) no longer holds and
       the rejection stands on (b).
     - **(b) Eliciting them is impersonation.** The only way to draw a fresh header
       is a minimal-inference probe with the subscription OAuth tokens — i.e.
       impersonating Claude Code. For a **fleet-health** tool whose entire purpose is
       to keep accounts alive, courting an account ban to read a number is
       self-defeating and categorically disqualifying.
   - Either disqualifier alone is fatal; together they close the path.

3. **Escalate `/oauth/usage` polling — lower `poll_secs` / poll the active account
   harder to stay fresh** (rejected).
   - **Why rejected**: `/oauth/usage` **self-blinds** exactly when polled hard
     enough to stay fresh (the `429` *is* the failure), and lowering `poll_secs`
     globally was already rejected by
     [ADR-0012](0012-active-reobservation-via-schedule-interleave.md) — it re-opens
     the **#80** burst-`429` exposure the staggered round-robin exists to prevent.
     Polling harder buys **blindness**, not freshness.

4. **Preemptive swap-away on bounded blindness — spend the swap lever, not the
   observation lever** (chosen).
   - **Pros**: fires *before* self-exhaustion (fake-clock replay of the S1 trace
     swaps at ~`0.87–0.93`, before `0.98`); no impersonation and no account-ban
     risk; adds no new burst exposure; config-gated and kill-switchable; keys off
     the retained `last_good` anchor, so no spurious swap on a missing reading.
   - **Cons**: acts on a **stale** pre-blind anchor rather than fresh truth — a
     deliberate trade, since the fresh reading is provably unavailable. A **static**
     `risk_band` may over- or under-trigger; mitigated by the now-confirmed constants
     (validated on two weeks of real telemetry — **#451**, with **#484** ratifying
     the 60% band) and by the velocity-projection arm (now shipped as **#539**, the
     OBSERVED-overshoot peer — see Consequences).

## Consequences

### Positive

- **The active account swaps away before it self-exhausts**, closing the
  reaction-latency gap (**#363**) that
  [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)'s interleave
  alone cannot close through a back-off blindness window.
- **No account-ban risk.** The tool never impersonates Claude Code: it reads only
  `/oauth/usage` (the account's own endpoint) and acts on the retained anchor. This
  is the load-bearing reason header-observation is rejected, not merely deferred.
- **Bounded, gated, reversible.** Separate from `emergency_swap`; respects
  no-torn-swap ([ADR-0003](0003-no-torn-swap-invariant.md)); honours the
  swap-target reserve (`target_max_session_usage`,
  [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md)) because the
  new path is **not** emergency and so does **not** take ADR-0013's emergency
  exemption; disabled by a single config value.
- **The rejected header path is recorded**, so "just read the headers" is not
  re-proposed each time the blindness recurs.
- **The preemptive principle extends to the observed overshoot (#539).** The same
  "spend the swap lever, not the observation lever" choice now also fires on a FRESH
  reading whose retained per-account EMA velocity projects over the trigger within the
  horizon — a strict early-fire of the reactive swap (same jittered triggers, same
  reserve), closing the reaction-latency residual the blind-window arm does not reach
  (there the reading is present, the trigger merely latent between polls). Same
  discipline as the blind arm: config-gated + kill-switchable ([ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md)),
  honours cooldown and the swap-target reserve ([ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md),
  **not** emergency), routes through no-torn-swap ([ADR-0003](0003-no-torn-swap-invariant.md)),
  and NEVER fires on a missing or single-sample velocity (holds on the fresh reading
  rather than swap on a guess).

### Negative / trade-offs

- **Acts on a stale pre-blind anchor, not fresh truth.** It can swap away an account
  that (unobserved) recovered, spending a swap it did not strictly need. Bounded by
  `risk_band` (only near-band anchors qualify) and by requiring a viable target.
- **A static `risk_band` can still over- or under-trigger.** `T=300s` /
  `risk_band=60%` are now empirically confirmed (validated on two weeks of real
  telemetry — **#451**; the conservative 60% band ratified by **#484**). A static band
  cannot track per-account velocity — so the **velocity-projection arm has now shipped
  (#539)**, promoted from the deferred Future note this ADR carried. It is the OBSERVED
  peer of the blind-window path: where the active still reads live but the reactive
  session trigger is latent between the ~cadence polls, it projects `observed + rate_ema
  × H >= trigger` off the FRESH reading (not the stale anchor the blind arm keys off) and
  swaps early, closing the observed reactive-overshoot residual of **#363**. Parameters
  (validated by the **#538** spike): the horizon `H` tracks the active poll cadence
  (`session_velocity_horizon_secs`, default 120s; safe band `H ≤ 150s`); it projects only
  from a reading already at/over `session_velocity_min_project_above` (default 85%, the
  free guard, since the max reach `≤ ~14pp` at `H ≤ 150s` cannot cross from below it); and
  the rate is an EMA (`session_velocity_ema_alpha_pct`, default α≈0.5) over `≥ 2` blended
  intervals, so a single-interval spike never fires. The **#363** umbrella now splits by
  residual: **#539** owns the covered observed-overshoot swap (projected swap-out
  `P100 ≤ 98 / P50 ≤ 94`, measured by the **#455** reliability readout), while **#540**
  owns the residual near-limit poll-gap coverage (the full-trace `P100 < 99`) the
  projection is blind to across a near-limit poll gap. The kill-switch is per-arm:
  `session_blind_swap_secs` set high disables the blind arm; `session_velocity_horizon_secs
  = 0` disables the projective arm (the projection reduces to the observed reading, which
  the reactive path already held below the trigger).

## Related

- Issues: **#454** (this ADR). **#452** (the design recorded here — the
  bounded-blindness preemptive swap; implemented there, and the original home of the
  velocity-projection arm note, now shipped as **#539**). **#539** (the shipped
  velocity-projection preemptive trigger — the OBSERVED-overshoot peer of the reactive
  session trigger: `observed + rate_ema × H >= trigger` off a FRESH reading + retained
  per-account EMA velocity; horizon `H` ≈ active poll cadence, only-project-above-85%
  guard, `≥ 2`-sample EMA, all validated by the **#538** spike; adds the
  `session_velocity_*` tunables and the projected-swap-out-overshoot + false-projection
  SLIs). **#540** (the residual near-limit poll-gap coverage — the full-trace
  `P100 < 99` the projection is blind to across a near-limit poll gap; jointly owns the
  **#363** residual with **#539**, which owns the covered swap). **#538** (the spike
  that validated the projection parameters). **#451** (the
  premise-confirmation + constants finalisation gate for `T` / `risk_band`, now
  satisfied on two weeks of real telemetry). **#484** (ratified the conservative 60%
  `risk_band` over the interim 65%). **#453** (the active-account back-off — its 120s
  `ACTIVE_POLL_BACKOFF_CAP` bounds only the *exponential self-backoff* arm; a server
  `Retry-After` is an **un-clamped floor** for the active account, so the window is **not**
  120s-bounded when the server sends one. The S1 window was the daemon's own back-off only
  because all 181 of its `429`s carried `retry_after_secs=0`; #453's *body* misrecords that
  same window as "Retry-After-dominated (server dictated 755s)" — the incorrect record, per
  **Amended 2026-07-17 (#586)**). **#294** (cap the honoured `Retry-After` so a pathological
  value can't dark the daemon — closed; its `POLL_BACKOFF_CAP` = 3600s clamp binds **peers
  only**, #453 exempting the active account, so the one account whose blindness matters is
  unprotected — the inversion the 2026-07-17 amendment records). **#582** (the resolution — a
  nonzero active `Retry-After` now triggers swap-away, `reason=blind_preempt`, restoring
  boundedness by *acting* while blind; changes no back-off arithmetic). **#450** (the retained
  `last_good` pre-blind anchor the path keys
  off). **#363**
  (the reaction-latency umbrella). **#369** (the reactive fast-path open question
  whose cautions this honours). **#42** (the dead-vs-exhausted model the separate
  availability path preserves). **#455** (the reliability SLO readout for
  swap-out overshoot; its SLIs gated the velocity-projection arm, now shipped as
  **#539** with its own projected-swap-out-overshoot + false-projection readout at
  schema 3). **#80** (the burst-`429` exposure that rules out harder polling).
  [anthropics/claude-code#55333](https://github.com/anthropics/claude-code/issues/55333)
  (upstream FR: Claude Code does not persist its usage headers). **#15**
  (diagnostics stay secret-free — the swap keys off the account's own usage and
  label handles, never a token or email).
- Code (**#452**): the gated `blind_swap` path is a peer of `emergency_swap` in
  `src/daemon.rs`; the tunables `session_blind_swap_secs` /
  `session_blind_risk_band` in `src/config.rs`, hand-emitted and cross-field-validated
  (`target_max_session_usage <= effective session trigger`) per
  [ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md). `/oauth/usage` remains
  the **only** usage source — no header probe is added.
- Code (**#539**): the gated `velocity_swap` path in `src/daemon.rs`, called from
  `decide_action` exactly where the reactive path would HOLD (observed below the trigger),
  keying off the retained per-account `session_velocity` EMA (`note_session_velocity`
  folds each poll interval; the EMA is re-keyed in lockstep across a roster reconcile, and
  reset on a session-window drop). The tunables `session_velocity_horizon_secs` /
  `session_velocity_min_project_above` / `session_velocity_ema_alpha_pct` in
  `src/config.rs`, hand-emitted + range-validated per
  [ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md) (`horizon 0..=600` with `0`
  the kill-switch). The swap logs `reason=velocity_preempt` carrying the FRESH observed
  swap-out pct; `src/reliability.rs` folds it into the projected-swap-out-overshoot
  percentiles (`P100 ≤ 98 / P50 ≤ 94`) + the false-projection count (schema `2 → 3`). Same
  single `/oauth/usage` source — no header probe.
- Code (**#582**): the server-`Retry-After` swap-away arm folds into the existing `blind_swap`
  path in `src/daemon.rs` — the gate becomes `blind_elapsed > session_blind_swap_secs &&
  (anchor_armed || retry_after.is_some())`, keyed off the retained RAW pre-cap directive
  (`poll_backoff_retry_after`, armed/cleared in lockstep with `poll_backoff_until`; a `0`
  directive normalises to `None`). `server_retry_after_holding` is the single shared predicate
  for the swap AND the `status` projection, so they cannot drift. Reuses `reason=blind_preempt`
  and the `session_blind_swap_secs` bound (one kill-switch, both arms); **no** back-off
  arithmetic changes and no header probe is added.
- ADRs: [ADR-0009](0009-rate-limit-backoff-per-account.md) (per-account `429`
  back-off — **extended** here: the same per-account scoping that correctly bounds a
  throttle is what leaves the *active* account carrying a stale reading = bounded
  blindness; the per-account decision itself is unchanged).
  [ADR-0012](0012-active-reobservation-via-schedule-interleave.md)
  (active-observation via interleave — **affirmed**: the active-observation budget
  stays favoured; the preemptive swap covers only the residual the interleave cannot
  observe through). [ADR-0013](0013-session-floor-default-on-reserve-emergency-exempt.md)
  (`target_max_session_usage` swap-target reserve — **x-ref**: the "viable target"
  condition honours the reserve; the new path is **not** emergency, so it does
  **not** take ADR-0013's emergency exemption).
  [ADR-0003](0003-no-torn-swap-invariant.md) (no-torn-swap invariant — respected).
  [ADR-0005](0005-config-parsed-by-crate-emitted-by-hand.md) (config hand-emit +
  cross-field validation — the new tunables follow it). **None superseded.**
