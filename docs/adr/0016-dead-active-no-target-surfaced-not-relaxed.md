---
type: architecture-decision-record
number: 16
title: "A dead active with no viable target is a surfaced capacity signal, not a swap-eligibility bug"
date: 2026-07-11
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0016: A dead active with no viable target is a surfaced capacity signal, not a swap-eligibility bug

## Status

**Accepted** — 2026-07-11. Records the **#405** decision to treat a stranded
*dead active + every live account weekly-exhausted* state as a legitimate terminal
capacity signal to **surface honestly**, and to **reject** relaxing the emergency
swap's weekly filter to engineer around it. Swap eligibility is **unchanged**; this
is purely an observability / operator-signal change.

## Context

sessiometer keeps one live credential active by rotating across a roster. Two
orthogonal axes describe a stranded operator's situation, and the type model
already splits them (this ADR adds no variant to either):

- **Per-account credential health** — `CredentialHealth` (`src/observability.rs`).
  `Dead` means a *proven* refresh-token death → the remedy is `claude /login`
  (**#427**). Independent of fleet capacity.
- **Fleet decision outcome** — `DecisionClass` (`src/observability.rs`).
  `AllExhausted` (active **alive**, every spare exhausted) and `ActiveDeadNoTarget`
  (active **dead**, no live non-exhausted target) already exist as distinct
  variants.

The defect #405 names is a **signal asymmetry** between the two fleet-exhaustion
outcomes — the strictly-*worse* state carried strictly-*less* signal:

- Active **alive** + fleet exhausted → the daemon pushes a rich, durable
  `Event::AllExhausted { hold, cause, resets_at }` under the `signaled_all_exhausted`
  latch, and `status` renders a footer naming the blocker and when it lifts.
- Active **dead** + fleet exhausted (`ActiveDeadNoTarget`) → the emergency
  no-target branch (`src/daemon.rs`) `return`ed **silently**: no durable event, and
  `status` showed only the dead active's `🔴 claude /login` row plus a content-free
  `no viable target` footer. The operator runs `claude /login`; if that account is
  *also* weekly-exhausted, nothing unblocks and nothing names the real blocker or
  its reset — the **#427 wrong-remedy lie, one axis up**.

The relief data the milder sibling surfaces — `(cause, resets_at)` — is already
computed by `all_exhausted_relief()` (`src/daemon.rs`), which excludes the active
index and so works unchanged when the active *is* the dead one. It simply never
reached the operator on this path.

## Decision

**Surface the stranded state honestly across the three channels that already carry
`AllExhausted`; do NOT relax the emergency swap's weekly filter. Swap eligibility
is unchanged.**

`emergency_swap`'s eligibility — including `usage.weekly < weekly_trigger` — is
**untouched**. A weekly-exhausted account 429s immediately, so it is not a useful
swap target; landing on it would trade an honest "out of capacity — wait / add an
account" for a silent swap that fails on first use. Fleet exhaustion is a
**legitimate terminal capacity signal**, not a bug to engineer around.

The fix is three complementary layers over the one reused `all_exhausted_relief()`
data-path:

1. **Wire (`src/daemon/snapshot.rs`).** `NextSwap::NoViableTarget` gains optional
   `{ cause: Option<NoTargetCause>, resets_at: Option<i64> }` (new wire-local enum
   `NoTargetCause { Session, Weekly }`). Additive/optional, mirroring **#393**'s
   `NextSwap::Target { reason }`. Wire schema **1.2 → 1.3** per ADR-0006 (additive
   minor). An absent payload still parses (old daemon ⇄ new menubar, and vice
   versa).

2. **Durable event (`src/observability.rs` + `src/daemon.rs`).** New edge-triggered
   `Event::ActiveDeadNoTarget { hold, cause, resets_at }`, rendered
   `event=active_dead_no_target hold=… cause=… [resets_at=…]` — the same `key=val`
   grammar as `AllExhausted`. Fired once on entry under a
   `signaled_active_dead_no_target` latch (mirroring `signaled_all_exhausted`),
   cleared on exit with a `Diagnostic::ActiveDeadNoTargetCleared` leave-marker. The
   stranded state is now diagnosable from `sessiometer.log` alone (**#399**).

3. **Render (`src/cli.rs` + menubar).** The `next swap:` footer and the menubar
   `nextSwapFooter` show the composite: the dead active's `🔴 claude /login` row
   (fix the credential) **and** the capacity blocker + reset + "add an account"
   (the real blocker + escape). Content-parity between CLI and menubar, not
   byte-identical (footers are medium-idiomatic).

`hold` is the dead active's **label handle** — the secret-free identifier
(**#15**), the same one `AllExhausted.hold` / `CredentialDead.account` carry, never
a token or email. User-visible strings carry **no** issue numbers.

## Alternatives considered

1. **Option 1 — relax the emergency-path weekly filter** (rejected): let the
   emergency swap land on a weekly-exhausted-but-alive account so *some* target is
   always found.
   - **Why rejected**: a weekly-exhausted account returns 429 on its first request,
     so the "successful" swap immediately strands the operator again — with *less*
     signal, because now it looks like a working swap. It converts an honest
     terminal capacity signal into a hidden failure and churns the roster
     (`claude` re-auth thrash) for no live capacity. Fleet exhaustion is a real
     state the operator must act on (wait for reset / add an account), not a
     scheduling defect.

2. **Option 2 — surface the state honestly** (chosen): the three layers above.
   - **Pros**: the operator sees both the credential remedy and the capacity
     blocker + reset; the state is durable in the log; no swap behavior changes and
     no new enum variant is added (the type model already split the axes).
   - **Cons**: three surfaces to keep in content-parity and a wire schema bump —
     both routine and covered by round-trip tests.

3. **Option 3 — document the limitation only** (rejected): a README note that a
   dead active can strand when the fleet is weekly-exhausted.
   - **Why rejected**: #405's own AC required a *clear operator-facing signal*, and
     a prose note in the README does not reach an operator staring at a live
     `status` that says only `🔴 claude /login`. Docs name the symptom; the durable
     event + footer name the blocker at the moment it bites.

## Consequences

### Positive

- **The strictly-worse stranded state now carries at least as much signal as its
  milder sibling.** A dead active with no viable target emits a durable
  `active_dead_no_target` event and a footer that names the capacity blocker and
  its reset — closing the #427 wrong-remedy lie one axis up.
- **Swap behavior is provably unchanged.** No eligibility predicate moved; the
  emergency path still refuses weekly-exhausted targets. The change is confined to
  the projection (`next_swap`), one new event + latch, and the render layer.
- **Reuses one data-path.** `all_exhausted_relief()` feeds both the
  active-alive-exhausted and active-dead-stranded cases; the composite (dead row +
  capacity footer) emerges from the account's `🔴` health showing separately on its
  row while the footer names the fleet blocker.
- **Backward/forward wire compatibility.** The relief payload is optional; a schema
  1.2 consumer ignores it and a 1.3 consumer tolerates its absence (round-trip
  tested).

### Negative / trade-offs

- **Three render surfaces to keep in content-parity** (CLI footer, menubar footer,
  durable event) — mitigated by shared classification (`all_exhausted_relief` /
  `NoTargetCause`) and content-parity (not byte-parity) tests.
- **A wire schema bump (1.3).** Additive and optional per ADR-0006, but every wire
  bump is a compatibility surface; covered by the fixture round-trip tests.
- **The composite signal is split across two surfaces** (the account row's `🔴`
  and the footer's capacity blocker), not one line. Deliberate — the two axes are
  genuinely orthogonal (credential health vs fleet capacity) and collapsing them
  would re-introduce the #427 conflation this fixes.

## Related

- Issues: **#405** (this ADR — surface the stranded out-of-capacity state).
  **#427** (the per-account `CredentialHealth::Dead` vs fleet-capacity split this
  builds on — the wrong-remedy lie one axis down). **#393** (the
  `NextSwap::Target { reason }` optional-payload precedent this mirrors). **#399**
  (durable per-account observability — the log-alone diagnosability this honors).
  **#15** (diagnostics stay secret-free — `hold` is the label handle, never a token
  or email).
- Code: `src/daemon/snapshot.rs` — `NextSwap::NoViableTarget { cause, resets_at }`,
  `NoTargetCause`, `STATUS_SCHEMA_VERSION` 1.3. `src/observability.rs` —
  `Event::ActiveDeadNoTarget`, `Diagnostic::ActiveDeadNoTargetCleared`,
  `DecisionClass::ActiveDeadNoTarget` (pre-existing). `src/daemon.rs` —
  `signaled_active_dead_no_target` latch (edge-triggered set at the emergency
  no-target branch, cleared on exit), `all_exhausted_relief()` (the reused relief
  classification, active-index-excluded), `next_swap()` (projection). `src/cli.rs` +
  `apps/menubar/Sources/StatusPanelFormat.swift` + `apps/menubar/Sources/WireModel.swift`
  — the footer render. `emergency_swap` — **unchanged** (the weekly filter stays).
- ADR-0006 (wire schema evolution — the additive-minor 1.3 bump); ADR-0013 (the
  session-floor / emergency-path reserve, sibling emergency-path behavior);
  ADR-0015 (the reactive-refresh re-scope — the adjacent honest-signal work on the
  credential-health axis).
