---
type: architecture-decision-record
number: 9
title: "Rate-limit back-off is scoped per-account, not endpoint-global"
date: 2026-07-04
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0009: Rate-limit back-off is scoped per-account, not endpoint-global

## Status

**Accepted** — 2026-07-04. Records the design behind the **#293** back-off
change (shipped at `3d08218`), so a contributor does not reverse-engineer why a
`429` on one account no longer backs off the whole poll loop. It **supersedes the
endpoint-global framing of #76** — a premise that was carried in #76's back-off
code but **never formalised in an ADR**, so there is no prior ADR to mark
`Superseded`; this record simply documents the corrected model.

Like ADR-0008 (and unlike ADR-0007's deliberate non-decisions), this ADR records
a **shipped** behavior change, now enforced in `src/daemon.rs`.

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a
roster of accounts, and the daemon polls **each** enabled account's usage
endpoint once per staggered cycle for its per-account quota (#5). #76 added a
**rate-limit back-off**: a `429` (rate-limited) or a `5xx` / network transient
widens the account's effective poll spacing — an exponential climb honouring a
server `Retry-After` as a minimum — instead of re-polling at the fixed interval.

#76 implemented that back-off as **endpoint-global**: a single
`DecisionState::poll_backoff_streak` counter, applied by widening the **whole
loop's** next wait. That rested on an unstated premise — **one usage endpoint for
the roster ⇒ one shared rate-limit bucket** — so throttling *anywhere* was treated
as throttling *everywhere*.

**That premise is wrong. The endpoint is per-account.** Each account's token
resolves to its **own Anthropic org**, so the `429` rate-limit buckets are
**independent** — verified by probing a **distinct `anthropic-organization-id` per
account**, and corroborated in-repo by `build/version-compat.md` **H2**, which
resolved that the **token carries auth/quota** while `oauthAccount` is
**display-only** and orthogonal (the token, not any shared config, is the quota
identity). The doc-comment on `note_account_backoff` records the same fact: "the
`429` is per-Anthropic-org (independent buckets)" (`src/daemon.rs:3273`).

The endpoint also emits **no `anthropic-ratelimit-*` headers**; the only
server-advised back-off signal is `Retry-After` (delta-seconds) on a `429`. The
poller captures exactly that — `curl`'s `write-out` appends only
`%header{retry-after}` after the status (`src/usage.rs:387`), parsed by
`parse_retry_after` in delta-seconds form (`src/usage.rs:428`) and honoured as a
minimum wait.

The cost of the wrong premise: a **spare** (non-active) account's `429` backed off
the **whole staggered loop**, silencing the **active** account's usage monitoring
— the daemon's core job — for the entire back-off (up to the ~1 h cap, or a server
`Retry-After`). One throttled spare blinded the one reading that matters.

## Decision

**Scope the rate-limit / transient back-off per-account.** A throttled account
backs off only **its own** next poll while the active account and every other
account keep polling on their normal cadence. #76's exponential climb and
`Retry-After`-as-minimum are **preserved**, just moved per-account; the loop no
longer globally waits for a rate-limit.

1. **Back-off state moves onto the account.** The single
   `DecisionState::poll_backoff_streak` is replaced by two fields on
   `AccountHealth`: `poll_backoff_streak` (`src/daemon.rs:1013`) and a
   `poll_backoff_until` deadline on the monotonic `Clock` (`src/daemon.rs:1021`) —
   mirroring the #282 `last_keep_warm_attempt` monotonic-deadline idiom.

2. **`tick` skips a backing-off account.** A scheduled index still inside its
   window is dropped before the poll body —
   `poll_idx.filter(|&i| !self.account_backing_off(i))` (`src/daemon.rs:1554`,
   predicate at `src/daemon.rs:3251`). No usage request is made, the account
   carries its last reading, and the schedule slot is consumed — so the throttle
   stays scoped to that one account.

3. **`note_account_backoff` folds the outcome into that account's own window**
   (`src/daemon.rs:3276`). A `429` / `5xx` / network transient advances the
   account's exponential streak — the freshly-drawn, jittered poll interval
   (inheriting #38's decorrelation) times `2^min(streak, POLL_BACKOFF_MAX_SHIFT)`,
   clamped to `POLL_BACKOFF_CAP` = 3600 s (~1 h, `src/daemon.rs:198`), never below
   a server `Retry-After` — and arms `poll_backoff_until = now + widened`. **Any
   other outcome** (success / `401` / `403`) clears both the streak and the window.

4. **The rate-limit / transient path no longer widens the whole loop.** It never
   sets the loop-level `next_wait`, which stays `None` in the normal tick
   (`src/daemon.rs:896`, `src/daemon.rs:1648`). The **only** remaining whole-loop
   wait is the #13 locked-keychain tick — a locked keychain genuinely blocks the
   entire roster, so that one is correctly global.

5. **Transient (`5xx` / network) is scoped per-account too.** Under a genuine
   endpoint outage every account fails its **own** poll and arms its **own** window
   anyway, so a single per-account path is the simplest correct design and needs no
   separate global case (documented on `note_account_backoff`). A `checked_add`
   guards `poll_backoff_until` against an astronomically-large `Retry-After`
   overflowing the monotonic instant — a panic the long-running daemon must never
   hit (`src/daemon.rs:3300`); **bounding a pathological value as policy stays
   #294's concern**, distinct from this arithmetic-safety floor.

## Alternatives considered

1. **Keep #76's endpoint-global back-off** — one loop-wide
   `poll_backoff_streak`, one whole-loop wait, on the "one endpoint ⇒ one bucket"
   premise.
   - **Pros**: a single counter; the simplest possible model **if** the premise
     held.
   - **Cons**: the premise is **false** — the `429` buckets are per-org
     (independent), so a **spare** account's throttle needlessly darks the
     **active** account's monitoring (the daemon's core job) for the whole back-off
     (up to the ~1 h cap or a server `Retry-After`). It penalises the entire roster
     for one account's rate-limit.
   - **Why rejected**: it mis-models the endpoint. Scoping per-account is what the
     independent buckets actually are, and it keeps the one reading that matters —
     the active account's — alive while a spare backs off.

2. **A separate global case for transient outages** — scope only the `429`
   per-account, but keep a whole-loop wait for `5xx` / network transients, on the
   reasoning that a *real* endpoint outage is genuinely roster-wide.
   - **Pros**: superficially matches intuition — one outage, one global back-off.
   - **Cons**: it adds a **second** back-off path (global transient **plus**
     per-account `429`) to prove, test, and keep consistent. Under a real outage
     every account already fails its **own** poll and arms its **own** window, so
     roster-wide back-off **emerges** as the sum — the global case is redundant.
   - **Why rejected**: one per-account path is the simplest **correct** design; the
     extra global case doubles the surface for no behavioural gain (the emergent
     per-account sum already backs the whole roster off under a true outage).

## Consequences

### Positive

- **The active account's monitoring is never silenced by a spare's throttle.** A
  `429` on a non-active account backs off only its own next poll; the active
  account — and every other account — keeps polling on its normal cadence. The
  daemon's core job survives one account being rate-limited.
- **#76's back-off intent is preserved, just scoped.** Sustained `429` / `5xx` on
  an account still widens **that account's** effective spacing (exponential,
  `Retry-After` as a minimum, capped at ~1 h); the difference is only *whose*
  cadence widens.
- **One correct path for both `429` and transient.** No separate global-outage
  case to maintain; under a genuine endpoint outage roster-wide back-off emerges as
  each account arming its own window — the simplest design that is also correct.
- **Reuses the established monotonic-deadline idiom.** `poll_backoff_until` sits on
  the same `Clock` as the #282 keep-warm deadline, so the whole back-off remains
  unit-testable over the timing seams with **no real clock or network** (#76's
  test discipline, `src/daemon.rs`).

### Negative / trade-offs

- **Back-off state grows per-account** — two `AccountHealth` fields instead of one
  loop-global counter, a marginally larger health struct. Accepted: it is the
  correct model and mirrors the existing per-account keep-warm fields (#282).
- **A genuine endpoint-wide outage is discovered per-account**, not once globally:
  in the first cycle of an outage each of N accounts earns its own `429` / `5xx`
  before arming its window, so the roster issues a few more requests than a single
  global back-off would before it quiets. Accepted: the exponential climb converges
  quickly, and the alternative (a global transient case) doubles the surface for a
  negligible saving.
- **An astronomically-large `Retry-After` is bounded, not honoured literally.** The
  `checked_add` safety floor clamps a monotonic-instant overflow to
  `POLL_BACKOFF_CAP` rather than panicking; a deliberate **policy** cap on
  pathological server values is deferred to **#294** (open). Accepted: the
  long-running daemon must never panic on overflow, and policy bounding is a
  separate, tracked concern.

## Related

- Issues: **#297** (this ADR). Shipped as **#293** (scope the back-off per-account
  — closed at `3d08218`). Supersedes the endpoint-global framing of **#76** (poll
  cadence + jitter + rate-limit back-off — closed), which was never formalised in
  an ADR. **#294** (cap the honoured `Retry-After` — **open**; the policy bound the
  `checked_add` arithmetic-safety floor defers to). Prior art: **#282** (the
  monotonic-deadline keep-warm idiom this back-off mirrors — closed), **#38**
  (parameterized jitter the widening inherits — closed), **#13** (the one remaining
  whole-loop wait: a locked keychain — closed), **#5** (per-account usage quota,
  both dimensions — closed).
- Code: `src/daemon.rs` — `AccountHealth.poll_backoff_streak` (~L1013),
  `AccountHealth.poll_backoff_until` (~L1021), `account_backing_off` (~L3251),
  `note_account_backoff` (~L3276), the `tick` skip
  `poll_idx.filter(|&i| !self.account_backing_off(i))` (~L1554), `next_wait` no
  longer widened by a rate-limit (~L896, ~L1648), `POLL_BACKOFF_CAP` = 3600 s
  (~L198), `POLL_BACKOFF_MAX_SHIFT` = 6 (~L193). `src/usage.rs` — the `curl`
  `write-out` capturing only `%header{retry-after}` (~L387), `parse_retry_after`
  (delta-seconds) (~L428), the `429` → `UsageStatus::RateLimited` classification
  (~L335).
- `build/version-compat.md` — **H2** (keychain vs `oauthAccount`: token =
  auth/quota, config = display, orthogonal), corroborating that the quota — and so
  the `429` bucket — is a **per-token, per-org** fact.
- ADR-0003 (the canonical-item / per-request credential re-read the polling rests
  on); ADR-0007 (the `AccountHealth` health-machine vocabulary this back-off state
  extends).
