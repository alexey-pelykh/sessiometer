---
type: architecture-decision-record
number: 19
title: "Out-of-rotation (exhausted) peers are slow-polled on a widened, reset-aware cadence"
date: 2026-07-15
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0019: Out-of-rotation (exhausted) peers are slow-polled on a widened, reset-aware cadence

## Status

**Accepted** — 2026-07-15. Records the design behind the **issue #537** poll-cadence
change, so a contributor does not reverse-engineer why an exhausted peer polls less
often than `poll_secs` — and, in particular, why it does so through a **second**
per-account window rather than the existing rate-limit back-off. Like ADR-0009 (whose
per-account-cadence framing this extends) it records a **shipped** behavior change,
enforced in `src/daemon.rs`.

## Context

sessiometer keeps one live Claude Code credential active by rotating across a roster,
polling **each** enabled, non-quarantined account's usage endpoint once per staggered
`poll_secs` sweep (#5, #80, #366). A peer that is **out of rotation** — weekly- or
session-exhausted (`weekly >= weekly_trigger` / `session >= session_trigger`) — is
still polled at that full cadence.

But an out-of-rotation peer's usage number can only change two ways: its server-side
window **resets** (a time the daemon **already knows** — `Usage.weekly_resets_at` /
`Usage.session_resets_at`, retained per-account in `last_readings`), or Anthropic resets
it server-side **out of band** (rare). Re-polling it every `poll_secs` is a wasted
`curl` + HTTPS GET + keychain read per account per cycle, and adds to the continuous-poll
footprint that hq research flags as an "unusual traffic" signal.

A per-account poll **skip mechanism already exists** (ADR-0009): `tick` drops a scheduled
index via `poll_idx.filter(|&i| !self.account_backing_off(i))`, and `account_backing_off`
reads `AccountHealth.poll_backoff_until`. But that back-off is a **rate-limit** window: it
is armed only by a `429` / `5xx` and **cleared on ANY success** (`note_account_backoff`),
and it drives the durable `usage_backoff` / `usage_backoff_cleared` events plus the
exponential streak. An **exhausted** account is an HTTP-200 **success**. So the existing
mechanism neither fires for exhaustion nor could be reused for it without corruption.

This is therefore net-new **policy** (skip exhausted peers) on the existing **mechanism**
(per-account poll skip).

## Decision

**After a successful poll of a NON-active peer whose reading is out of rotation, arm a
per-account `exhausted_poll_until` window and skip that account's poll until it elapses.**

1. **A separate `AccountHealth.exhausted_poll_until: Option<Instant>`** — NOT a reuse of
   `poll_backoff_until`. Overloading the back-off field would (a) self-clear on the very
   HTTP-200 success that reads exhaustion, and (b) fire spurious `usage_backoff` rate-limit
   events + advance the 429 streak. The two windows are consulted **together** at the skip
   site: `poll_idx.filter(|&i| !self.account_backing_off(i) && !self.exhausted_slow_polling(i, active))`.

2. **The window is reset-aware:**
   `until = now + min(exhausted_poll_secs, max(soonest_applicable_resets_at - now, floor))`,
   where the "applicable" reset is the **soonest** `resets_at` among the dimensions
   actually exhausted (weekly's when weekly-exhausted, session's when session-exhausted, the
   sooner when both), and it falls back to `now + exhausted_poll_secs` when no applicable
   reset is known. Rationale: the hourly `exhausted_poll_secs` ceiling bounds worst-case
   blindness for the **rare** server-side early reset; a **known** `resets_at` — which the
   daemon already retains — pulls the next poll **earlier**, so a window that elapses sooner
   than an hour is caught promptly. The `floor` is **`poll_secs`**: a slow-polled peer must
   never re-poll **faster** than the normal cadence, and the floor guards the degenerate
   `resets_at <= now` case (a server late to reset) from a busy re-poll every tick.

3. **The active account is EXEMPT** — peers only. The active account's swap-away trigger is
   time-sensitive and must stay observable at full cadence. `note_exhausted_poll` never arms
   the active account (and clears any window it inherited by being promoted), and
   `exhausted_slow_polling` exempts it at the consult too (belt-and-suspenders against a
   stale window on an account just promoted via `use`). This mirrors the #453 active-vs-peer
   asymmetry in the rate-limit back-off.

4. **A new tunable `exhausted_poll_secs`** (default 3600, range `poll_secs..=86400`), the
   window ceiling, hand-emitted per **ADR-0005**. The lower bound is `poll_secs` (see the
   floor above); the 24 h ceiling is a sanity bound.

5. **Exhausted accounts stay IN `poll_schedule` / `rotation_len` (N)** — they are simply
   skipped on ticks where `exhausted_poll_until` has not elapsed, reusing the proven skip
   path. The aggregate rate math is unchanged. Removing them from N is a deliberate
   **non-goal** (YAGNI, out of scope for #537).

6. **One edge-triggered durable event pair** — `exhausted_slow_poll` (ENTER, carrying the
   armed window) / `exhausted_slow_poll_cleared` (EXIT) — mirroring `usage_backoff` /
   `usage_backoff_cleared`. ENTER fires only on the normal→slow transition (not on each
   re-arm while the peer stays exhausted — it never left the widened cadence); EXIT only
   when a window was actually armed. Redacted to **uuid + window** only (issue #15).

The window is cleared when a later poll reads the account **viable again**, or when
`now >= exhausted_poll_until` (the window elapses and the account is re-polled on its next
scheduled tick — catching the rare server-side early reset within ≤ the ceiling).

## Alternatives considered

1. **Overload the existing `poll_backoff_until` (ADR-0009) for exhaustion.**
   - **Pros**: one skip field, one consult site; no new `AccountHealth` state.
   - **Cons**: `note_account_backoff` **clears** the back-off on any success, and an
     exhausted reading **is** a success — so the very poll that reads exhaustion would clear
     the window it needs to arm. And `poll_backoff_until` drives the `usage_backoff` 429
     events + exponential streak, so arming it on exhaustion would emit **spurious
     rate-limit** signals and pollute the "429 count" the events exist to make queryable.
   - **Why rejected**: it conflates two orthogonal reasons to skip a poll (rate-limited vs
     out-of-rotation) whose lifecycles are opposite — one cleared by success, the other
     **entered** by it.

2. **Blind hourly slow-poll — always `now + exhausted_poll_secs`, ignore `resets_at`.**
   - **Pros**: the simplest window; no reset arithmetic.
   - **Cons**: the daemon **already retains** `resets_at`, and an exhausted window commonly
     resets sooner than an hour. Ignoring it leaves a peer that became viable 20 min ago
     dark for up to another 40 min — wasted runway and a slower return to a full rotation.
   - **Why rejected**: the reset-aware ceiling is strictly better at no real cost — the reset
     is already in hand, and the fallback covers the unknown-reset case.

3. **Remove exhausted accounts from `rotation_len` (N).**
   - **Pros**: N would track only genuinely-pollable accounts.
   - **Cons**: N is the tick divisor for the staggered aggregate rate (#80/#366); changing
     it re-derives the whole rate math and the per-source floor for a saving the skip already
     delivers (no request is made for a skipped account either way).
   - **Why rejected**: YAGNI — out of scope for #537; the skip already removes the request.

## Consequences

### Positive

- **Fewer wasted requests.** An out-of-rotation peer is polled at most once per
  `exhausted_poll_secs` (or sooner, at a known reset) instead of every `poll_secs` — cutting
  the continuous-poll footprint for accounts whose number cannot change until they reset.
- **The active account is never slowed.** Exemption keeps the swap-away trigger fully
  observable; only spare peers — which are just swap targets, ranked by reset — widen.
- **Reuses the proven skip path (ADR-0009).** Same `tick` filter, same carry-`last_readings`
  behavior — so the all-exhausted relief (`all_exhausted_relief`, which folded in the former
  `soonest_weekly_reset` per #665) still computes from the retained reading, and the aggregate
  rate math is untouched.
- **Diagnosable.** The edge-triggered `exhausted_slow_poll` / `_cleared` pair brackets each
  slow-poll episode in `sessiometer.log`, secret-free by construction (uuid + window).

### Negative / trade-offs

- **A second per-account skip window** — one more `AccountHealth` field and one more consult
  term at the skip site. Accepted: it is the correct model (the two windows have opposite
  lifecycles), and it mirrors the existing per-account back-off state.
- **Bounded blindness to a RARE out-of-band reset.** If Anthropic resets a slow-polled
  peer's window early and out of band, the daemon does not see it until the window elapses
  (≤ `exhausted_poll_secs`, or the known `resets_at`). Accepted: this is exactly what the
  hourly ceiling bounds, and a peer is only a swap *target* — its staleness never blinds the
  active account.

## Related

- Issues: **#537** (this ADR). Prior art: **#293 / ADR-0009** (the per-account poll-skip
  mechanism this policy layers on), **#453** (the active-vs-peer asymmetry the exemption
  mirrors), **#11 / #37** (the all-exhausted relief + `all_exhausted_relief`, formerly
  `soonest_weekly_reset` (#665), that consumes the retained `resets_at`), **#5** (per-account
  usage quota, both dimensions + `resets_at`),
  **#80 / #366** (the staggered schedule + `rotation_len` this leaves unchanged), **#15**
  (the redaction meter the new events stay clean under).
- Out of scope (deliberate non-goals): the menu-bar "de-prioritized / next-poll-at" surface
  (a `phase:2-ui` follow-up); removing exhausted accounts from `rotation_len` N.
- Code: `src/daemon.rs` — `AccountHealth.exhausted_poll_until`, `exhausted_slow_polling`,
  `note_exhausted_poll`, `exhausted_poll_window`, the `tick` skip filter, the post-poll arm
  call. `src/config.rs` — the `exhausted_poll_secs` tunable (parse / validate / render /
  origin). `src/observability.rs` — `Event::ExhaustedSlowPoll` / `ExhaustedSlowPollCleared`.
- ADR-0005 (config parsed by crate, emitted by hand — the new tunable's render);
  ADR-0009 (the per-account back-off scoping this extends).
