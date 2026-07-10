---
type: architecture-decision-record
number: 13
title: "`session_floor` is a default-on swap-target reserve, exempt on the emergency path"
date: 2026-07-10
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0013: `session_floor` is a default-on swap-target reserve, exempt on the emergency path

## Status

**Accepted** — 2026-07-10. Records the **#398** change (`session_floor` restored to a
default-on reserve of 80, dropped on the emergency swap path) and **supersedes issue
#10's resolution** that made the floor opt-in. Like ADR-0008, ADR-0009 and ADR-0012,
this ADR records a **shipped** behavior change, now enforced in `src/daemon.rs` and
`src/config.rs`.

## Context

sessiometer keeps **one** live Claude Code credential active, rotating across a roster
when the active account's usage crosses a **swap-away trigger** (`session_trigger`,
default 95; `weekly_trigger`, default 98). `pick_target` (`src/daemon.rs:3948`) chooses
where to go.

**`session_floor` is a reserve on the TARGET, not a trigger.** Its predicate is
`usage.session < floor` — *only swap **to** an account whose session usage is below this
percent*. It is orthogonal to `session_trigger`, which decides when to swap **away**.
The gap between them (80 → 95) is deliberate **runway**: an account received at ≤80%
has room to work before it trips its own trigger.

**The name reads backwards.** A *higher* `session_floor` is *more permissive* — it is a
**ceiling on the target's usage**, not a minimum. `session_floor == session_trigger` is
inert (the floor then admits exactly what the always-on gate already admits). A rename
(e.g. `target_max_usage`) is tracked separately as **#415**; this ADR does not perform
it, because the key is operator-visible in every rendered `config.toml`.

**Why it was turned off (#10).** The original code hardcoded `DEFAULT_SESSION_FLOOR =
80`. Issue #10 ("Swap cooldown / anti-oscillation") removed it, making the floor opt-in
(`Option<u8>`, absent ⇒ off), on the reasoning that a **mandatory** floor could freeze
the daemon: with every candidate above 80%, `pick_target` returns `None` and the loop
enters the `NoViableTarget` / all-exhausted state. #10's remedy was to let the
**cooldown alone** bound oscillation.

**Why that reasoning no longer holds.** #10's fear was *thrash*, not *freeze* — freeze
was the price it refused to pay for anti-thrash. Since then an **always-on session
gate** landed in `pick_target` (`usage.session < session_trigger`,
`src/daemon.rs:3967`): a session-saturated account is **never** a swap target, mirroring
the pre-existing weekly-exhaustion filter. That gate prevents #10's oscillation **at its
source**, unconditionally, with no floor at all. The floor is therefore no longer
load-bearing for anti-thrash — it is free to be what it was always meant to be: a
**runway reserve**, layered strictly on top (effective ceiling
`min(session_trigger, floor)`).

**Why the default value matters more than it looks.** Config sections default on
absence — every `RawConfig` field carries `#[serde(default)]` (`src/config.rs:1351`), so
an absent `[tunables]` block silently yields every tunable's default. With the floor
opt-in, "absent" meant **off**, and the daemon would happily hand the active session to
a 94%-full account (below the 95 trigger, so the gate admits it), which then trips its
own trigger almost immediately. The reserve existed but nobody had it.

**The trap the default-on flip introduces.** `pick_target` has **four** callers. Three
are advisory or proactive. The fourth is `emergency_swap` (`src/daemon.rs:3257`), taken
when the **active credential is dead or quarantined**. That call site already drops the
session gate (it passes `f64::INFINITY`), and its comment relied on *"the default floor
OFF"*. Flip the default on, and a dead active + every live account ≥80% returns
`ActiveDeadNoTarget` — the daemon **strands itself on a dead credential** rather than
escape to a live-but-busy one. A self-DoS, introduced by an otherwise-benign default.

## Decision

**Make `session_floor` a default-on, always-valued reserve (80) that is a HARD filter on
the proactive path and is DROPPED entirely on the emergency path — shipped atomically.**

1. **Default-on, always-valued.** `Tunables.session_floor: u8` (the `Option` is gone),
   `DEFAULT_SESSION_FLOOR = 80` (`src/config.rs:74`). An absent key maps to 80 in
   `Config::validate` (`src/config.rs:734`) — like any other tunable. The raw layer keeps
   `Option<i64>` (`src/config.rs:1404`) solely to *detect* absence. `render()`
   (`src/config.rs:993`) emits a **live** `session_floor = 80` line, so the value is
   visible in every config the tool writes. The cross-field invariant
   (`session_floor <= session_trigger`) keeps its distinct `ConfigFloorAboveTrigger`
   error.

2. **It stays a HARD filter on the PROACTIVE path.** When no candidate sits below the
   floor, `pick_target` returns `None` and the daemon **HOLDS** — the existing
   `NoViableTarget` + edge-triggered `all_exhausted` signal. Holding on genuine
   exhaustion is **correct**: swapping among near-exhausted accounts buys minutes and
   costs a thrash cycle. This is the behavior #10 called a freeze; here it is named as
   the intended answer, and the `all_exhausted` event is what makes it legible.

3. **The EMERGENCY path drops the floor.** `emergency_swap` passes `floor: None`
   alongside the `f64::INFINITY` session gate it already dropped. Liveness beats the
   reserve when the active is dead: **any live account beats a corpse.** Only the floor
   argument changes — the #42 dead-vs-exhausted model is untouched, and #369's caution
   against overloading `emergency_swap` still stands.

4. **`all_exhausted` says WHY, and reports the reset that ends it.** The event gains
   `cause=session|weekly`, carried by the **closed `SwapReason` enum** — never a
   free-form re-derived string, so diagnostics stay secret-free by construction (#15).
   When the block is session-wide it now reports the soonest **session** `resets_at`
   (the acknowledged follow-up at `src/daemon.rs:2669-2672`) instead of a weekly reset
   that is not what the operator is waiting for. `diag=start` prints the effective value;
   the `session_floor=off` sentinel disappears from the log grammar.

5. **Atomicity is mandatory.** (3) MUST ship with (1), test-gated, or the default stays
   OFF. Shipping the default-on flip without the emergency floor-drop converts a benign
   proactive hold into a self-DoS strand on a dead credential. The gate is the
   regression test `emergency_swap_escapes_a_dead_active_ignoring_the_floor`
   (`src/daemon.rs`), which was **mutation-verified**: reverting the emergency argument
   to `Some(self.session_floor)` makes it fail with `ActiveDeadNoTarget` vs
   `EmergencySwapped { from: 0, to: 1 }`. The test is a real guard, not a vacuous pass.

## Alternatives considered

1. **Keep the floor opt-in (status quo, #10)** — leave `Option<u8>`, absent ⇒ off.
   - **Pros**: zero change; no risk of the emergency-strand trap; #10's freeze concern
     stays structurally impossible.
   - **Cons**: the reserve exists but is unreachable in practice — a minimal
     `config.toml` (the shape `capture` bootstraps and the shape operators hand-edit
     down to) has no `[tunables]` block at all, so the floor is off for everyone who did
     not opt in. Swaps land on 94%-full targets that re-trip their own trigger.
     Empirically, the maintainer's own config had drifted into exactly this state.
   - **Why rejected**: the opt-in's *stated* justification (freeze avoidance) was
     superseded by the always-on session gate. Retaining it preserves the cost with the
     benefit gone.

2. **Fail-to-off: keep the value optional, treat absence as "off," seed 80 only into
   freshly-created configs.** — new configs get a floor; old ones keep today's behavior.
   - **Pros**: no behavior change for any existing deployment; a hostile-or-truncated
     config can never freeze proactive swapping.
   - **Cons**: it makes "absent" mean something **different from every other key in the
     file**, where absent ⇒ documented default. That is precisely the surprise that bit
     the maintainer, and it is unobservable without a `config show --origin` (#401).
     Two configs that render identically would behave differently depending on their
     birth date.
   - **Why rejected**: consistency of the absent-key rule is worth more than the
     narrow safety it buys, **and** the danger it guards against (freeze) is covered by
     Decision 3 on the only path where freezing is fatal. The maintainer ratified
     "absent → 80, like any config file."

3. **Make the floor a soft PREFERENCE tier rather than a hard filter** — prefer
   below-floor candidates; if none exist, fall through to the always-on
   `session_trigger` gate and swap anyway.
   - **Pros**: never holds when *some* account could receive the session; intuitively
     "graceful degradation" rather than a stop.
   - **Cons**: the fall-through case is exactly where the tier does harm. With no
     candidate below 80, the fallback hands the session to (say) a 94% account, which
     trips `session_trigger` on its next observation and swaps again once the cooldown
     elapses — the thrash the always-on gate exists to prevent, re-entered through the
     back door. And in the non-marginal case (candidates below the floor exist), the tier
     is indistinguishable from the hard filter, since `pick_target` already ranks
     survivors by soonest weekly reset (#37).
   - **Why rejected**: it adds a code path that is inert when it is safe and harmful
     when it fires. **Holding on genuine exhaustion is the correct answer**, and it is
     already observable — Decision 4 makes it say *why* and *when it ends*.

4. **Drop `session_floor` entirely and rely on the always-on session gate.** — one
   ceiling (`session_trigger`) instead of two.
   - **Pros**: deletes a tunable whose name reads backwards; one fewer cross-field
     invariant; no emergency-path exemption needed.
   - **Cons**: the gate's ceiling *is* the swap-away trigger, so a target may be
     received at 94.9% and swap away immediately. There is no runway. The gate prevents
     thrash from a *saturated* target; it does not reserve *headroom* on a viable one.
     They are different jobs.
   - **Why rejected**: runway is the floor's whole purpose, and the 80→95 gap is what
     buys it. Deleting it would trade a naming wart for a behavioral regression.

## Consequences

### Positive

- **The reserve is on for everyone, including minimal configs.** An absent `[tunables]`
  block now yields a floor of 80, matching the rule every other key already follows. The
  value is a live line in every rendered `config.toml` and in `diag=start`.
- **A dead active can always escape to a live account.** The emergency path is
  floor-exempt and gate-exempt; the only remaining filter is weekly exhaustion. Guarded
  by a mutation-verified regression test.
- **The hold is legible.** `all_exhausted` now names the cause (`session` vs `weekly`)
  and the reset that ends the block, so "the daemon stopped swapping" is answerable from
  the event log without attaching a debugger. The `cause` rides the closed `SwapReason`
  enum, so no free-form string can ever carry a label or UUID into the durable log (#15).
- **#10's anti-oscillation guarantee is unchanged.** The always-on session gate — not
  the floor — is what prevents thrash, and it is untouched. The regression test
  `two_session_saturated_accounts_hold_the_gate_prevents_oscillation` stays green.

### Negative / trade-offs

- **The proactive path can now HOLD where it previously swapped.** With every candidate
  ≥80%, the daemon refuses to swap and signals `all_exhausted`, where an
  off-floor daemon would have swapped to a 94% account. This is intended (Decision 2 /
  Alternative 3) but it *is* a behavior change for any deployment that never set the
  key. The `all_exhausted` cause + reset make it diagnosable rather than mysterious.
- **`session_floor = 0` disables proactive swapping outright.** The predicate is
  `usage.session < floor`, so a floor of 0 admits nothing. The validated range is
  `0..=session_trigger`, which permits it. This footgun pre-dates #398 (an opt-in
  `Some(0)` behaved identically), but the default-on flip makes the key a live,
  hand-editable line, so it is now *reachable* rather than theoretical. Accepted, not
  fixed: rejecting 0 would be a separate config-validation change, and 0 is a legitimate
  (if blunt) way to say "never proactively swap."
- **The floor is coupled to `poll_secs` through reading staleness.** The floor filters on
  the **last-known** reading, which can be up to ~`2·poll_secs` old for a peer (ADR-0012,
  Decision 4). At `poll_secs = 300` a spare's reading may lag ~10 min; a fast climber can
  cross the floor in that window and be selected on stale data. The 80→95 gap absorbs
  this. **Raising the floor toward `session_trigger` narrows the runway *and* the
  staleness margin simultaneously** — a `session_floor = 90` with `poll_secs = 300` is
  the trap: 5 points of runway against a ~10-minute-stale reading. Loosening the floor is
  safe *because* the gap widens; tightening it is not.
- **The name still reads backwards.** `session_floor` is a ceiling on the target's usage;
  higher is more permissive. Documented in the rendered comment, the README, and here.
  Not renamed: the key is operator-visible in every existing `config.toml`, so a rename
  is a migration (ADR-0006), tracked separately as **#415**.
- **ADR-0005's `session_floor` illustration is now stale.** ADR-0005 (§ Context) cites
  the floor as an example of a *commented-out opt-in line* — `# session_floor =
  <session_trigger>` — embedding another field's value. `render()` now emits a live
  `session_floor = 80`. ADR-0005's **decision** (parse with the `toml` crate, emit by
  hand) is unaffected and remains `Accepted`; per the immutability convention that ADR is
  not rewritten, and this note is the correction.

## Related

- **Issues**: **#398** (this ADR — the atomic default-on flip + emergency exemption).
  **#10** (the opt-in resolution this supersedes — closed). **#405** (the residual
  emergency strand: even with the floor dropped, the *weekly* filter still applies, so a
  dead active + all-live-weekly-exhausted still returns `ActiveDeadNoTarget` — **open**,
  a follow-up decision, not blocking). **#401** (`config show --origin` — the
  effective-vs-on-disk observability this change makes more valuable). **#402**
  (absent-section defaults are silent). Follow-ups this ADR spawned: **#414** (the
  `session_floor = 0` footgun — the strict predicate disables proactive swapping, and
  `validate` permits it), **#415** (rename the backwards-reading key). Prior art: **#11** (the all-exhausted terminal
  state this event describes), **#15** (diagnostics secret-free by construction — the
  `SwapReason` enum reuse), **#37** (soonest-weekly-reset target ranking), **#42**
  (dead-vs-exhausted model), **#36** (disabled-account exclusion), **#88**
  (`next_swap` candidate), **#369** (caution against overloading `emergency_swap`).
- **ADRs**: **ADR-0012** (sibling cadence decision — `poll_secs` stays 300; the source of
  the staleness coupling above). **ADR-0009** (per-account rate-limit back-off).
  **ADR-0005** (config parsed by crate, emitted by hand — its `session_floor` example is
  superseded here; its decision is not).
