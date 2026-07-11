---
type: architecture-decision-record
number: 18
title: "Shared-credential scrub on the first invalid_grant: the multi-writer \"Not logged in\" lockout"
date: 2026-07-11
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0018: Shared-credential scrub on the first `invalid_grant`: the multi-writer "Not logged in" lockout

## Status

**Accepted** — 2026-07-11. Records the credential-lifecycle finding the **#463**
umbrella rests on — Claude Code (observed on the shipped **2.1.207** build) **empties**
the shared `Claude Code-credentials` keychain item on its **first** `invalid_grant`
refusal — together with the interaction that turns it fleet-wide, the observability
**asymmetry** it creates, and the **mitigation decisions** taken against it. It exists so
the finding is not re-derived from scratch each time the lockout recurs, and so the
mitigation rationale is traceable.

Like [ADR-0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md), this
ADR records decisions whose **implementation is pending** — the mitigation layers are
tracked in **#467** / **#468** / **#469**, and the **#466** knob-spike outcome is **open**
(recorded below as genuinely pending, not resolved). It **supersedes nothing**: it
*extends* [ADR-0007](0007-decided-against-credential-recovery-options.md) decision 4 by
carving out the scrubbed-canonical-**with**-a-live-target case that decision 4 did not
contemplate, *reuses* the honest-signal philosophy of
[ADR-0016](0016-dead-active-no-target-surfaced-not-relaxed.md), and *respects* the
no-torn-swap invariant of [ADR-0003](0003-no-torn-swap-invariant.md). This ADR publishes
**observable** Claude Code behavior only — no reverse-engineered internals.

## Context

sessiometer keeps **one** live Claude Code credential active by rotating across a roster.
Every local `claude` session reads the shared `Claude Code-credentials` keychain item
**per request** ([ADR-0003](0003-no-torn-swap-invariant.md), #12) — one shared item ⇒ one
global active account. Three observed facts combine into a hard, fleet-wide lockout.

**1. The refresh token rotates on every exchange.** Anthropic's token endpoint issues a new
refresh token and **invalidates the one just used** on each exchange — refresh-token
rotation, the RFC 9700 default for a public OAuth client. Observable in the daemon's own
logs: refreshes are uniformly `rotated=true`. The prior refresh token dies immediately
server-side.

**2. Claude Code scrubs the shared item to empty on the first `invalid_grant`.** Observed on
the shipped **2.1.207** build: when a refresh is refused with `invalid_grant`, Claude Code
responds by **emptying** the shared `Claude Code-credentials` item (tokens cleared) — **not**
by leaving the existing token in place. After that, every session shows **"Not logged in ·
Please run /login"** until an operator re-authenticates. Claude Code re-reads the item per
request and **self-heals a merely-stale *in-memory* token** by picking up a fresh token that
has landed in the canonical item — but it **cannot** self-heal from an **emptied** item
(there is nothing valid to re-read). The scrub is **guarded**: it does not clobber a fresh
token that already landed, so it bites specifically when a **dead token is the item's current
token with no valid replacement**.

**3. Many writers rotate the one shared, always-rotating credential.** Multiple `claude`
sessions **plus** the daemon (swaps + proactive keep-warm) all refresh/write the single
shared item. Because the refresh token rotates on every exchange (fact 1), there are windows
in which a **rotated-out (dead)** token is the item's *current* token; a refresh against it
returns `invalid_grant` (fact 2) and empties the item **for everyone**.

**The interaction**, then: one shared item + per-exchange rotation + multiple writers ⇒
windows where a dead token is current ⇒ scrub on the first `invalid_grant` ⇒ fleet-wide
"Not logged in." This is strictly worse than the two shared-item failure modes already
understood — the **self-healing multi-session refresh race** (the losing session re-reads
the winner's fresh token from the canonical and recovers) and the **off-canonical stranded
token** (fixed in the daemon by #253/#254: the active account is excluded from poll-refresh,
so its fresh token is never stranded off-canonical). Both of those leave *some* token in the
canonical to re-read; the scrub leaves **nothing**, so even the per-request self-heal cannot
fire.

**The asymmetry (an observability gap).** Claude Code scrubs on the **1st** `invalid_grant`;
the daemon declares an account DEAD and quarantines only after `monitor_401_n` **consecutive**
401s (`DEFAULT_MONITOR_401_N: u8 = 3`, `src/config.rs`). So the operator can hit "Not logged
in" with **no `credential_dead` event** in `sessiometer.log` — the shared item is already
empty while the daemon's edge-triggered health machine has not yet crossed its threshold
(observed: a ~4 h window with zero recorded deaths yet live, fleet-wide lockouts).

**The recovery gap today.** Recovery for a gone/scrubbed canonical (`adopt_target`) is
**`use --force`-gated** (`src/use_account.rs`: "Recovery requires `--force`"); the autonomous
daemon **never adopts** ([ADR-0007](0007-decided-against-credential-recovery-options.md)
decision 4). So recovery is **manual today** — the operator must re-login every stuck
session, even when a perfectly live account exists in the roster that the daemon could adopt.

## Decision

**Record the finding above as observable Claude Code behavior, and mitigate it with a
three-layer set that autonomously recovers the fleet when a live account exists, cuts the
daemon's own contribution to the rotation windows, and surfaces the scrubbed state even when
no `credential_dead` event fired — while preserving the manual-`/login` remedy for the
genuinely-all-dead case.**

The load-bearing distinction, which reconciles this with
[ADR-0007](0007-decided-against-credential-recovery-options.md) decision 4 and
[ADR-0016](0016-dead-active-no-target-surfaced-not-relaxed.md):

- **`ActiveDeadNoTarget`** (ADR-0007 d4) — the active account's token is **genuinely dead**
  and **no live spare exists**. There is no safe automated move (an in-place active refresh
  corrupts live sessions; a timer is a retry storm), so a manual `claude /login` is the only
  correct recovery. **Unchanged.**
- **Scrubbed-canonical-with-a-live-target** (this ADR) — the canonical item is **empty**
  (Claude Code scrubbed it), but a **live account exists** in the roster. This is *not*
  `ActiveDeadNoTarget`: there **is** a viable target, so the daemon can autonomously write a
  live credential into the emptied canonical and heal every session — no operator action
  required. Treating this as `ActiveDeadNoTarget` (telling the operator to re-login when the
  daemon could adopt an existing live account) would be the **#427 wrong-remedy lie**.

The mitigation set (implementation pending — tracked in the sub-issues):

1. **Autonomously adopt-target a scrubbed/empty canonical (#467).** When the canonical is
   **confirmed-empty** (`CredentialNotFound` — the scrubbed item, distinguished from a merely
   locked keychain, which is a safety abort) **and a live target exists**, the daemon adopts
   that target into the canonical **without** the `--force` gate — the narrow, safe carve-out
   from ADR-0007 decision 4. The adopt write is the same atomic `-U` canonical write the swap
   uses, so it honors the no-torn-swap invariant
   ([ADR-0003](0003-no-torn-swap-invariant.md)): a concurrent reader sees the empty item, then
   the adopted credential, never a torn blob. A genuinely-all-dead roster still has no target
   and correctly falls through to the manual-`/login` remedy.

2. **Gate proactive keep-warm of the active account (#468).** Proactive keep-warm is one of
   the daemon's writers that rotates the shared token
   ([ADR-0015](0015-reactive-refresh-unconditional-proactive-gated.md): reactive on-401
   refresh is unconditional; proactive maintenance is the gated arm). Gating keep-warm of the
   **active** account removes the daemon's own contribution to the rotation windows that make a
   dead token current — shrinking the exposure without touching the reactive path.

3. **Surface the scrubbed/dead canonical state (#469).** `status` and the menubar surface the
   scrubbed/empty canonical with the **re-login remedy**, closing the asymmetry observability
   gap: the operator sees the stuck state even when the daemon's `monitor_401_n` threshold has
   not yet fired a `credential_dead` event — the same honest-signal move
   [ADR-0016](0016-dead-active-no-target-surfaced-not-relaxed.md) made for the stranded
   out-of-capacity state.

**Open items (recorded as genuinely pending, not resolved):**

- **The #466 knob-spike outcome is unknown.** Whether Claude Code exposes a configuration
  knob to disable the `invalid_grant` scrub is being probed in #466; that spike has **not**
  concluded. If a knob exists it would attack the root cause upstream of all three mitigation
  layers; until #466 reports, this ADR does not assume one.
- **Reporting the behavior upstream is deferred, pending #466.** Whether to file an
  observable-behavior report with Claude Code depends on the #466 outcome (a documented knob
  makes a report unnecessary; its absence may warrant one). The report, if filed, would carry
  the **observable** surface only — the same public-safety constraint that governs this ADR.

## Alternatives considered

1. **Manual re-login as the sole remedy** (status quo, rejected as the *only* recovery).
   - **Why rejected**: recovery for a scrubbed canonical is `--force`-gated and the autonomous
     daemon never adopts (ADR-0007 d4), so today every stuck session needs a manual
     `claude /login` **even when a live account exists** that the daemon could safely adopt.
     For the common scrubbed-but-live-target case that is an unnecessary, fleet-wide outage.
     Kept **only** for the genuinely-all-dead `ActiveDeadNoTarget` case, where it remains the
     correct remedy.

2. **Eliminate the shared-item rotation windows entirely — give each session its own
   credential** (rejected).
   - **Why rejected**: the single shared `Claude Code-credentials` item, re-read per request,
     is intrinsic to Claude Code's model — it is *what* makes one global active account and the
     seamless out-of-band swap work. There is no safe way to give each session a private
     canonical without breaking that model. The keep-warm gating (#468) instead **reduces** the
     windows by removing the daemon's own contribution; it does not attempt to eliminate the
     intrinsic multi-session refresh race, which is Claude Code's to own.

3. **Disable the scrub via a Claude Code configuration knob** (deferred — #466, outcome
   pending).
   - If Claude Code exposes such a knob, it would remove the root cause upstream of the local
     mitigations and would be **preferable** to reactive recovery. But whether one exists is
     **unknown** until #466 concludes, so it cannot be relied on now. Recorded as the
     preferred-if-available lever, not a chosen mechanism.

4. **Treat every scrub as `ActiveDeadNoTarget` and require a manual login** (rejected).
   - **Why rejected**: this is the ADR-0007 d4 remedy applied to the wrong case. It is correct
     only when the roster is genuinely all-dead. Applied to a scrubbed canonical *with* a live
     target, it is the **#427 wrong-remedy lie** one axis over — telling the operator to
     re-login when the daemon could adopt an existing live account. The carve-out in decision 1
     exists precisely to keep the two cases distinct.

## Consequences

### Positive

- **The common lockout auto-recovers.** A scrubbed canonical with any live account in the
  roster is healed autonomously by the adopt-target path (#467) — no manual `/login` for the
  fleet-wide case that dominates in practice.
- **The daemon stops making it worse.** Gating proactive keep-warm of the active account
  (#468) removes one of the writers rotating the shared token, shrinking the windows in which
  a dead token is the item's current one.
- **The stuck state is visible even with no `credential_dead` event.** Surfacing the scrubbed
  canonical (#469) closes the asymmetry observability gap, so the operator is not left staring at
  "Not logged in" with a clean log.
- **Reconciles cleanly with the existing recovery model.** ADR-0007 decision 4 is *narrowed,
  not overturned*: the genuinely-all-dead `ActiveDeadNoTarget` case still requires a manual
  `/login`; only the scrubbed-**with**-a-live-target case is newly automated. No swap-eligibility
  predicate moves, and the adopt write honors the no-torn-swap invariant
  ([ADR-0003](0003-no-torn-swap-invariant.md)).
- **The finding is durable and observable-only.** A contributor gets the behavior, the
  interaction, and the asymmetry from this record instead of re-deriving them from a live
  incident, with no reverse-engineered internals published.

### Negative / trade-offs

- **The intrinsic multi-session refresh race is not eliminated.** Keep-warm gating removes only
  the *daemon's* contribution; multiple live `claude` sessions can still race a refresh and put
  a rotated-out token in the canonical. The mitigations shrink and recover from the exposure;
  they do not close it. Only an upstream change (the #466 knob, if it exists) or a change to
  Claude Code's scrub behavior would.
- **Adopt-target recovery is reactive, not preventive.** It heals *after* the scrub — there is
  still a brief lockout window between Claude Code emptying the item and the daemon observing the
  empty canonical and adopting a live target. It bounds the outage; it does not prevent the
  first "Not logged in."
- **The #466 knob-spike outcome is unknown.** Whether a cleaner upstream fix exists is
  genuinely open; this ADR commits to the local mitigations without assuming one, and will be
  revisited if #466 finds a knob.
- **Rests on observed 2.1.207 behavior.** The scrub-on-first-`invalid_grant` finding is
  empirical and version-specific. A future Claude Code release could change it (leave the token
  in place, scrub differently, or add a knob), silently invalidating the premise the mitigations
  target. Guarded by the deferred-live re-check trigger recorded in `build/version-compat.md`
  (the [ADR-0002](0002-keychain-via-security-cli-zero-ffi.md) / ADR-0003 pattern): re-verify on
  a Claude Code auth bump.

## Related

- Issues: **#470** (this ADR). **#463** (the umbrella — shared-credential "Not logged in" scrub
  resilience). _Understand_: **#464** (instrument the canonical credential — per-poll snapshot +
  scrub event), **#465** (characterize multi-session rotation interference + measure the rate —
  spike), **#466** (probe Claude Code for a knob to disable the scrub — spike; **outcome
  pending**). _Fix_: **#467** (autonomously adopt-target a scrubbed/empty canonical), **#468**
  (gate proactive keep-warm of the active account), **#469** (surface the scrubbed state in
  status + menubar). Prior context: **#253**/**#254** (active account excluded from poll-refresh,
  removing the off-canonical stranded-token variant), **#427** (per-account credential-health vs
  fleet-capacity split — the wrong-remedy lie this avoids re-committing), **#42**
  (credential-dead / quarantine + emergency-swap origin), **#15** (diagnostics stay secret-free —
  the surfaced state carries the account's label handle, never a token or email).
- Code (present, the finding's on-disk footprint): `src/config.rs` — `DEFAULT_MONITOR_401_N`
  (the `monitor_401_n` quarantine threshold, default `3`). `src/use_account.rs`, `src/swap.rs`,
  `src/poke.rs` — the scrubbed / confirmed-absent canonical handling (`CredentialNotFound` = the
  scrubbed item; a locked keychain is a distinct safety abort) and the `--force`-gated
  `adopt_target` recovery the #467 carve-out relaxes for the live-target case.
  `build/version-compat.md` — the 2.1.207 scrub finding + the deferred-live re-check trigger.
- ADRs: [ADR-0007](0007-decided-against-credential-recovery-options.md) (decided-against
  credential recovery — **extended**: decision 4's manual-`/login` remedy is narrowed to the
  genuinely-all-dead case; the scrubbed-with-a-live-target case is newly automated).
  [ADR-0016](0016-dead-active-no-target-surfaced-not-relaxed.md) (surface the stranded state —
  the honest-signal philosophy #469 reuses). [ADR-0015](0015-reactive-refresh-unconditional-proactive-gated.md)
  (reactive refresh unconditional / proactive gated — the arm #468 gates further).
  [ADR-0003](0003-no-torn-swap-invariant.md) (no-torn-swap invariant — the adopt write respects
  it). [ADR-0002](0002-keychain-via-security-cli-zero-ffi.md) (the deferred-live re-check pattern
  the `build/version-compat.md` trigger follows). **None superseded.**
