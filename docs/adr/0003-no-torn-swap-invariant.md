---
type: architecture-decision-record
number: 3
title: "No-torn-swap invariant"
date: 2026-07-02
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0003: No-torn-swap invariant

## Status

**Accepted** — 2026-07-02. Records an invariant already enforced across
`src/swap.rs` and `src/daemon.rs`; this ADR captures the rationale (see #201,
#195/A3; originating #6, #12).

## Context

The daemon swaps the active credential **out-of-band** — while a Claude Code session
may be mid-flight. The target app re-reads the canonical `Claude Code-credentials`
keychain item **per request** (#12).

- A swap that lands mid-session must therefore present a **clean cut**: a concurrent
  reader must always observe either the outgoing account's credential or the incoming
  one — **never an empty, half-written, or torn blob**, and never a window in which
  the canonical item is absent.
- The swap is **multi-step** (read outgoing → re-stash outgoing → write incoming →
  co-write display → confirm; `src/swap.rs` module docs), and several writers can race
  the item: the swap engine, a concurrent `claude /login`, and Claude Code's own
  in-place token refresh.

## Decision

Hold a **no-torn-swap invariant**: a credential swap is **complete-or-abort**, and a
reader of the canonical item sees old-or-new and nothing in between. It is enforced by
four mechanisms, not one:

1. **Atomic commit point.** The canonical write is `security add-generic-password -U`,
   an atomic in-place update (old-or-new, never empty/torn — see ADR-0002). The swap
   sequence is ordered so this single atomic write is the commit point: steps 1–3
   (read outgoing, re-stash outgoing, write incoming) are the swap proper and must
   succeed; any failure aborts **before or at** the atomic write, leaving the outgoing
   account safely re-stashed and the canonical item un-torn. Steps 4–5 (co-write
   `~/.claude.json` `oauthAccount`, confirm re-read) are best-effort display /
   diagnostic.
2. **Single-writer swap lock.** `SwapLock` takes a kernel advisory `flock`,
   **fail-closed**: if it cannot be acquired within the bound, the swap aborts with
   **zero** writes (`Error::SwapLockBusy`) rather than writing without the lock and
   reopening the torn-write race (`SwapLock::acquire` in `src/swap.rs`).
3. **Swap only between ticks.** SIGINT/SIGTERM is observed only *between* ticks, so an
   in-flight swap always runs to completion — #6's no-half-swap (`src/daemon.rs`
   § graceful shutdown).
4. **Self-healing reconcile.** The canonical item is re-read every cycle (never
   cached); reconcile-on-start plus keychain-first ordering make a crash- or
   third-writer-induced `oauthAccount`↔canonical mismatch self-healing
   (`Daemon::reconcile_on_start`).

## Alternatives considered

1. **Non-atomic delete-then-add** (remove the old item, then add the new).
   - **Pros**: conceptually simple; works even with APIs lacking an atomic upsert.
   - **Cons**: opens a window in which the canonical item is **absent** mid-swap — a
     concurrent reader gets `CredentialNotFound`. This is exactly the forbidden window
     the swap's absent-read assertions are written to catch (`src/swap.rs` tests).
   - **Why rejected**: it violates the invariant on its face; the atomic `-U` upsert
     exists specifically to avoid it.
2. **Best-effort swap without a single-writer lock.**
   - **Pros**: no lock contention or wait.
   - **Cons**: two overlapping swaps (or a swap racing another writer with no
     coordination) can interleave writes and reopen the torn-write race.
   - **Why rejected**: fail-closed locking is what makes "never torn" hold under
     contention.
3. **Cache the active credential in the daemon** to avoid re-reading each cycle.
   - **Pros**: fewer keychain reads.
   - **Cons**: a third writer (a concurrent `/login` or a token refresh) changes the
     item under the cache, so the daemon would act on stale state.
   - **Why rejected**: re-read-each-cycle + reconcile keeps the daemon convergent with
     the authoritative keychain.

## Consequences

### Positive

- **Mid-turn correctness (#12).** A swap landing mid-turn presents a clean cut: the
  in-flight request is unaffected and the next request picks up the incoming account.
  Proven in CI by the `tests::mid_turn_live` oracle — a concurrent reader re-reading
  across a forced swap sees outgoing, then incoming, and never anything between; the
  assertions fail on a torn read or a *reproducibly*-absent item — a real
  delete-then-add recurs on every swap, whereas a lone `CredentialNotFound` is
  the file-keychain's benign cross-process artifact, not the forbidden window
  (issue #457).
- The keychain token is the single **authoritative bearer**, so the best-effort
  display co-write (`oauthAccount`) may be clobbered and simply self-heals on the next
  reconcile (last-writer-wins) — the invariant does not depend on it.
- Failure modes **degrade safely**: a lock-busy or an early-step error aborts the
  whole swap with the outgoing account intact, never leaving a partial credential.

### Negative / trade-offs

- Rests on a **platform property** — `security -U` being an atomic in-place update —
  that is macOS/keychain-specific and **empirically verified** rather than
  contractually guaranteed (`build/version-compat.md`); it is re-checked by the
  deferred live oracles on Claude Code auth bumps.
- **Fail-closed locking means a swap can be skipped** under sustained contention
  (correctness over liveness for that tick); the next cycle retries.
- One **live-only tail** — the in-flight request's at-most-one transparently-retried
  401 — is the target app's own retry, verified manually rather than in CI
  (`src/swap.rs` § deferred live checks).

## Related

- ADR-0002 (keychain via CLI): the atomic `-U` write depends on the CLI-not-SDK
  decision.
- ADR-0001 (current-thread runtime): the swap lock's cooperative async wait depends on
  the single-threaded runtime.
- Code: `src/swap.rs`, `src/daemon.rs`.
