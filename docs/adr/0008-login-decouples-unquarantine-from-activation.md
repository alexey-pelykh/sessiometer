---
type: architecture-decision-record
number: 8
title: "login decouples un-quarantine from activation"
date: 2026-07-04
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0008: `login` decouples un-quarantine from activation

## Status

**Accepted** — 2026-07-04. Records the design behind a `login` behavior change
shipped across three merged PRs — **#274** (gate the canonical re-point), **#275**
(the `restored` control command), **#276** (wire `login` to signal it) — so a
contributor does not reverse-engineer why `login <B>` no longer steals the active
slot yet still clears `B`'s quarantine at once.

Unlike ADR-0007 (a set of deliberate non-decisions, landing no code), this ADR
records a **shipped** invariant now enforced across `src/capture.rs`,
`src/daemon.rs`, `src/daemon/socket.rs`, and `src/daemon/run_loop.rs`.

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a
roster of accounts; the target app re-reads the canonical `Claude Code-credentials`
keychain item per request (ADR-0003, #12). `login <X>` captures `X`'s
freshly-authenticated credential in an **isolated** engine that leaves the shared
credential byte-for-byte untouched (#132), then **reconciles** the capture into the
roster (`reconcile_login`, `src/capture.rs`).

Two independent problems met at the `login` verb:

- **Activation was unconditional.** `login <X>` always re-pointed the canonical
  item to `X` in the reconcile phase — so logging in **any** account stole the
  active slot, even when the operator only wanted to add or revive a *parked*
  account while a *different* account stayed live. The capture engine never touched
  the shared credential; the activation was **entirely** the reconcile phase's
  canonical re-point (plus its `~/.claude.json` `oauthAccount` co-write).

- **Clearing a parked account's quarantine needs an un-quarantine trigger.** A
  dead account is quarantined (#42): the daemon stops polling and selecting it, and
  surfaces a durable "needs re-login" status (`AccountHealth.quarantined`). Re-login
  should clear that at once — but the daemon's un-quarantine paths were **coupled to
  activeness**:

  | Path | Site | Un-quarantines because… | Active? |
  |------|------|-------------------------|---------|
  | `reconcile_canonical_change` (#107) | `src/daemon.rs` ~L1861 | the canonical item was re-pointed to it | **becomes** active |
  | `note_poll_outcome` Live-recovery (#42) | `src/daemon.rs` ~L2026 | `monitor_recovery_m` consecutive `Live` polls of the account the daemon is actively polling — the stuck active — heal it in place | **is** the active |
  | `apply_refresh_restore` (#106) | `src/daemon.rs` ~L2981 | the refresh sweep's isolated re-probe answered | **no** change |

  Only the **RESTORE primitive** (#106, `apply_refresh_restore`) un-quarantines with
  **no** canonical write and **no** active-account change — it flips
  `quarantined = false`, resets `recovery_successes`, and emits
  `Event::CredentialRestored`. But it was reachable **only** from the periodic #106
  refresh sweep, which is **starved** (#260) — so a re-logged-in parked account would
  wait, indefinitely, for a sweep that may never touch it.

So `login <B>` on a parked `B` (while `A` stays active) needs both: **(a)** the
canonical re-point gated off, and **(b)** a *reliable, on-demand* un-quarantine that
does **not** activate — i.e., the #106 RESTORE primitive reached directly rather
than through the starved sweep.

## Decision

`login` **decouples un-quarantine from activation**, in two coupled parts:

1. **Gate the canonical re-point on the captured account's identity (#274).**
   `reconcile_login` reads the current active identity — `oauthAccount.accountUuid`
   in `~/.claude.json`, the honest-display pair of the canonical token (the keychain
   blob carries no uuid) — and **activates** the login (re-points the canonical item
   and co-writes `~/.claude.json`) **only** when the captured account **is** that
   active one (re-auth in place) **or** there is no readable active identity
   (bootstrap). When a **different** account is active, both writes are skipped and
   the account is stashed + rostered **without touching the active slot**. The
   verdict is a pure `should_activate(active_uuid, captured_uuid)` helper
   (`src/capture.rs:623`) unit-tested over its three branches; the real identity read
   stays in `reconcile_login` and the verdict is passed into the pure core. An
   unreadable / absent `~/.claude.json` reads as "no active account" →
   bootstrap-activate, the safe default for an operator who just ran `login`. `captured` +
   `activate` ride together in a `HarvestedLogin` struct so the run-login functions
   stay within the repo's 7-argument clippy bound.

2. **Un-quarantine a revived parked account via a dedicated authenticated control
   command, decoupled from any canonical change (#275, #276).** Expose the existing
   #106 RESTORE primitive rather than adding a new revive path:

   - **#275** adds `{"cmd":"restored","uuid":"<X>"}` in `control_reply`
     (`src/daemon/socket.rs:129`), gated on `peer_is_same_user` (#64) exactly like
     `manual-swapped` / `roster-reload` — **auth is checked first**, so a stranger
     never learns the request's well-formedness, and a `restored` with no `uuid` is
     refused as malformed. It carries `ControlSignal::Restored(uuid)` (an owned
     payload, so the enum drops `Copy`); the run loop breaks its idle to call the
     existing `apply_refresh_restore(&uuid)`, best-effort logs the
     `CredentialRestored`, and re-ticks so `status` reflects the un-quarantine within
     the poll cadence. An unknown / already-non-quarantined uuid returns `None`: an
     **idempotent** silent no-op.

   - **#276** wires `login`'s reconcile to **send** that command after a
     **non-activating REVIVE** — the pure `should_signal_restored(activate, outcome)`
     helper (`src/capture.rs:639`), `!activate && Revived`, unit-tested over all four
     combinations. The best-effort `notify_daemon_restored` fires right after the
     existing `roster-reload` notify. An **activating** revive is skipped (its
     canonical re-point already un-quarantines via #107); an **onboard** is skipped (a
     brand-new account was never quarantined, so `restored` would be a daemon-side
     no-op).

Net: **un-quarantine ≠ make-active.** `login B` while `A` is active keeps `A` active
**and** clears `B`'s quarantine immediately, using the one un-quarantine primitive
(#106 RESTORE) that does not activate — reached on demand, independent of the starved
#106 sweep (#260).

## Alternatives considered

1. **Reject `login` when a *different* account is active** — refuse the verb (or
   require an explicit swap first), so `login` keeps a single meaning: "log in **and
   activate**."
   - **Pros**: no identity gate; `login` never has a state-dependent effect on the
     active slot.
   - **Cons**: needs **two verbs** for the motivating intent — "log in / revive `B`
     without disturbing `A`" becomes a swap-or-deactivate dance around the login, and
     a parked account cannot be revived at all without a slot change.
   - **Why rejected**: gating on identity lets a **single** `login` verb cover both
     "log in and activate" (it *is* the active / bootstrap account) and "add or revive
     without stealing the slot" (a different account is active) — better ergonomics
     for exactly the operator intent #274 exists to serve.

2. **A second, non-isolated re-login / spawn path for the revive case** — instead of
   reusing the daemon's existing RESTORE primitive over a control command,
   un-quarantine by driving another (non-isolated) login of the parked account.
   - **Pros**: superficially folds "revive" into a single re-login action.
   - **Cons**: introduces a **second credential-capture spawn path** beyond the
     isolated #132 engine, each such path needing its **own** redaction / safety
     proof, and (for a non-active account) it re-opens the canonical / activation
     question this ADR is closing.
   - **Why rejected**: the daemon **already had** the exact primitive
     (`apply_refresh_restore` — un-quarantine with no canonical write, no active
     change). Exposing it as an authenticated control command reuses it with **no new
     spawn path**, folds the new channel into the existing #15 redaction-meter corpus,
     and keeps the credential-adjacent surface small (the CONTRIBUTING.md
     minimal-surface line).

## Consequences

### Positive

- **The primary intent holds**: `login B` keeps `A` active, and a re-logged-in
  parked `B` clears its "needs re-login" **at once** — not after the starved (#260)
  #106 sweep eventually reaches it.
- **On-demand un-quarantine is independent of the #260 starvation bug**: even while
  #260 is unfixed, a re-login recovers a parked account reliably, because the
  `restored` command reaches `apply_refresh_restore` directly instead of waiting on
  the sweep.
- **No new credential-capture spawn path.** #275 reuses `apply_refresh_restore`; the
  isolated #132 engine stays the single spawn path, and the new control channel is
  exercised (request + every reply) by the #15 redaction-meter corpus with a
  cardinality assertion keeping the clean verdict non-vacuous — the
  credential-adjacent surface stays small.
- **Hermetic decision cores.** The two branch decisions are pure helpers
  (`should_activate`, `should_signal_restored`) unit-tested exhaustively; the impure
  identity read and daemon notify stay in `reconcile_login` and hand a verdict into
  the pure core.
- **Safe to invoke and safe to retry.** The command is authenticated
  (`peer_is_same_user`, auth-first) and idempotent (unknown / already-healthy uuid →
  silent no-op), so `login` — or an operator — may send it directly and repeatedly.

### Negative / trade-offs

- **`login` now has a state-dependent effect on the active slot** — activate-in-place
  / bootstrap when it is the active (or no) account, stash-without-activation when a
  different account is live. One verb, two outcomes. Mitigated by the durable
  `status` reflecting the un-quarantine within a poll cadence, and by README + help
  documenting the active-preservation semantics (#277).
- **The un-quarantine signal is best-effort.** If the daemon is down or wedged,
  `login` still writes the stash + roster (the **authoritative** state) but the
  quarantine is not cleared until the daemon next reconciles; `notify_daemon_restored`
  logs and swallows a missing daemon rather than failing the `login` verb.
- **A small new authenticated control-channel surface** (the `restored` command +
  `ControlSignal::Restored`). Accepted — gated exactly like the existing
  `manual-swapped` / `roster-reload` commands and covered by the #15 redaction corpus.
- **The gate trusts the display pair.** Active identity is inferred from
  `~/.claude.json` `oauthAccount.accountUuid`, not the canonical keychain blob (which
  carries no uuid); an unreadable / absent file degrades to bootstrap-activate.
  Accepted as the safe default — and consistent with ADR-0003's self-healing
  reconcile, which keeps that display pair convergent with the canonical token.

## Related

- Issues: **#278** (this ADR). Shipped as **#274** (gate the canonical re-point —
  closed), **#275** (the `restored` control command — closed), **#276** (`login`
  signals the daemon — closed). **#260** (the #106-sweep starvation fix — **open**;
  the reason the on-demand command is needed). Prior art: **#107** (canonical-change
  un-quarantine — closed), **#42** (quarantine / health-machine origin — closed),
  **#106** (the refresh RESTORE primitive — closed), **#132** (the isolated capture
  engine), **#15** (the redaction meter), **#64** (`peer_is_same_user` control auth),
  **#277** (documented the active-preservation semantics).
- Code: `src/capture.rs` — `reconcile_login` (~L654), `should_activate` (~L623),
  `should_signal_restored` (~L639), `HarvestedLogin` (~L474),
  `notify_daemon_restored` (~L232); `src/daemon/socket.rs` — `control_reply` (~L129),
  the `restored` command (~L173), `ControlSignal::Restored` (~L61); `src/daemon/run_loop.rs`
  — the `Idle::Restored` → `apply_refresh_restore` dispatch (~L526); `src/daemon.rs` —
  `apply_refresh_restore` (~L2976) and the three un-quarantine sites
  (`reconcile_canonical_change` #107 ~L1861, `note_poll_outcome` Live-recovery #42
  ~L2026, `apply_refresh_restore` #106 ~L2981).
- ADR-0003 (no-torn-swap invariant — the canonical item + self-healing reconcile the
  identity read rests on); ADR-0007 (the recovery-rails model this extends — the
  #106 / #107 / #42 vocabulary); CONTRIBUTING.md (minimal credential-adjacent surface).
