---
type: architecture-decision-record
number: 26
title: "Daemon-payload-fault severity is a cross-surface rank, not a per-surface colour register"
date: 2026-07-22
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0026: Daemon-payload-fault severity is a cross-surface rank, not a per-surface colour register

## Status

**Accepted** — 2026-07-22 (**#575**). Records the decision that the three daemon-level
payload faults carry ONE severity **rank**, honoured by both the `status` CLI and the
menubar panel; reconciles the CLI, which ranked them in the OPPOSITE order to the
owner-ratified panel; and supersedes the "register-not-severity" narrative the CLI's
own comments carried. No wire change; no behaviour change; a render + documentation
change confined to `src/cli.rs`.

## Context

The daemon-level payload faults ride ALONGSIDE a perfectly-`connected`, perfectly-healthy
roster — no per-account `AUTH` cell reflects them, because the shared vault is one item
and the refresh mechanism is one process, so neither has a row to live on. Three exist,
plus one calm variant:

| Fault | Blocks the operator? | Issue |
|---|---|---|
| `keychain_locked` — the login keychain is LOCKED, the shared item is UNREADABLE | now | #498 |
| `canonical_scrub = exhausted` — the shared item is readable but EMPTIED, auto-recovery gave up | now | #469 |
| `systemic_refresh_failure` — the refresh MECHANISM is down (N all-error sweeps), every account still alive | pre-death | #378 |
| `canonical_scrub = recovering` — scrubbed, adopt-recovery in flight | no (may self-heal) | #469 |

Both surfaces render these, and **they ranked them in opposite order** (#575):

- The **menubar panel** (owner-ratified 2026-07-17, `../hq/strategy/design-menubar.md`)
  ranks the vault pair `⊘` / `.error` "act now" and `systemic` `!` / `.warning`
  "act at your next break": *"the `⊘`/`!` split is pre-death, not severity-by-feel."*
  The vault pair blocks you NOW; a down refresh mechanism is pre-death by construction
  (that is #378's entire purpose — it fires while every account is still alive), so the
  tool is still keeping you working but cannot self-heal.
- The **`status` CLI** painted `systemic` **red** — its only coloured fault line — and
  left the vault pair **plain**, ranking the LEAST-blocking fault the LOUDEST.

The CLI's rendering was *locally* rationalised, in code comments, as a "register" choice
rather than a severity claim: the vault lines sat in an "action-first footer register"
(remedy-carrying, deliberately uncoloured) borrowed from the `next swap:` footer, while
`systemic` was a "red DATA fault line." Two facts defeat that rationalisation:

1. **The CLI's red IS a severity signal, not a neutral register token.** `red_line`
   emits `Severity::Red.sgr()` — the *same* red the util cells use for
   "the least-available" (`>= 90%`) and the cornered `⊘` state uses for "the loudest,
   distinct state." On this surface, red means *most urgent*. Painting it on the
   least-blocking fault while the two act-now faults sit plain is backwards against the
   CLI's **own** vocabulary — an intra-surface self-contradiction, not merely a
   cross-surface style difference.
2. **[ADR-0016](0016-dead-active-no-target-surfaced-not-relaxed.md) ratifies no colour
   rule.** It governs *content-parity* of the `next swap:` footer for the
   `ActiveDeadNoTarget` capacity state ("not byte-identical… footers are
   medium-idiomatic"); it says nothing about `keychain_locked` / `canonical_scrub` or
   about colour. The "footer register, deliberately uncoloured" story lived ONLY in
   `src/cli.rs` comments — in no ratified record. The inversion was an accident of build
   order (#378 shipped its red line before #469/#498 shipped their plain ones), never a
   decision.

The drift was structural: severity was never located as a property of the *fault*,
derived once. Each surface decided locally, so they were free to disagree — and the panel
already declares the invariant a fourth fault will inherit: *"severity ranks by (fault,
VARIANT), never fault identity"* and *"any FOURTH daemon-level payload fault inherits
this."*

## Decision

**Severity is a property of the fault, ranked ONCE, and rendered by each surface in its
own vocabulary. The colour/glyph *vocabulary* may differ between a terminal line and a
popover banner; the severity *rank* may not — a cross-surface rank divergence on the same
fault is a defect (R-2: "the panel and the `status` CLI are two renders of the ONE
`StatusResponse`… a divergence between them is a bug").**

Concretely:

1. **One home for the rank.** `src/cli.rs` gains `DaemonPayloadFault` with a
   `severity() -> Option<Severity>` mapping — the CLI's single source of the rank,
   mirroring `StatusPanelFormat.daemonFaultBanner`'s `.error` / `.warning` / `.info`:
   - `keychain_locked` → `Some(Severity::Red)` — act now (rank 1)
   - `canonical_scrub = exhausted` → `Some(Severity::Red)` — act now (rank 2)
   - `systemic_refresh_failure` → `Some(Severity::Yellow)` — next break (rank 3)
   - `canonical_scrub = recovering` → `None` (plain, calm) — may self-heal; colouring
     would cry wolf (rank 4)
2. **The CLI conforms in its own vocabulary.** The three fault renderers route through one
   `daemon_fault_line` helper that applies the fault's rank band when the colour gate is
   open. The panel is UNCHANGED — it was already correct.
3. **The remedy-availability distinction moves to the text**, where it already lives:
   "unlock it" (keychain) / "run claude /login" (scrub) vs "check the daemon log
   'reason='" (systemic). Colour now carries *rank*; the text carries *remedy*.
4. **Documentation.** The CLI comments stop citing ADR-0016 as the reason for plainness;
   this ADR is the cross-surface severity-rank record a future editor of either surface
   will meet.

Colour is still purely additive — every fault line carries its whole message in plain
text, so a `--no-color` / piped `status | grep` reader loses nothing (only `recovering`
is intentionally uncoloured even with the gate open).

## Alternatives considered

1. **Option (b) — make `systemic` the loudest on both surfaces** (rejected): raise the
   panel's `systemic` from `.warning` to the vault pair's `.error`.
   - **Why rejected**: it fixes the surface that is already correct and reopens the
     owner-ratified glance taxonomy (`../hq/strategy/design-menubar.md`,
     `../hq/strategy/brand-identity.md`) — "pre-death, not severity-by-feel" is a
     deliberate, ratified call. `systemic` is pre-death by construction; ranking it above
     an active outage is the miscalibration, not the fix.

2. **Option (c) — document the divergence as "register, not severity"** (rejected as
   filed): change no rendering; record that the CLI's SGR is line-register and the two
   media may legitimately differ.
   - **Why rejected**: its premise is ungrounded. The CLI's red is `Severity::Red`
     (used for util cells and the cornered `⊘` state), so it *is* a severity signal, and
     the inversion is intra-surface, not a benign cross-vocabulary difference — you
     cannot "document away" a self-contradiction. ADR-0016 never made the register
     decision the comments attributed to it. (c)-as-filed would enshrine an accident of
     build order. Note: (c)'s *documentation* deliverable is honoured here — this ADR IS
     that record — but its *do-nothing-to-rendering* posture is not.

3. **A shared cross-language severity abstraction** (deferred as over-scoped for now):
   push a computed rank on the wire, or a code-generated rank mirrored Rust↔Swift under a
   parity test.
   - **Why deferred**: for a three-(soon four-)fault taxonomy this is heavier than the
     leak warrants. A single Rust `severity()` home for the CLI plus this ADR as the
     cross-surface record gives the already-declared invariant its first home and lets a
     fourth fault add one rank line. A cross-language parity mechanism can follow if the
     fault set grows.

## Consequences

### Positive

- **Cross-surface rank parity.** An operator reading both surfaces sees the same snapshot
  ranked the same way; the vault pair is act-now on both, `systemic` is next-break on
  both.
- **The CLI is self-consistent.** Its `Severity::Red` now means the same thing on a fault
  line as on a util cell — most urgent — instead of contradicting itself.
- **One home for the rank.** A fourth daemon-payload fault adds one arm to
  `DaemonPayloadFault::severity`, honouring the panel's declared "any FOURTH fault
  inherits this" invariant, instead of re-deriving the rank at a new site.
- **The register/severity confusion is recorded**, so the next editor of either surface
  does not re-file this.

### Negative / trade-offs

- **The CLI now colours the two vault lines** (they were plain). This is the intended
  change; the plain text is unchanged, so `--no-color` / piped output is unaffected.
- **This ADR overrides the CLI comments' "action-first footer register" framing for these
  fault lines.** The `next swap:` footer's own plainness (ADR-0016) is untouched — that is
  a genuinely different line (a capacity signal, not a payload fault).
- **The rank still lives in two languages** (Rust `DaemonPayloadFault::severity` and Swift
  `daemonFaultBanner`), reconciled by this ADR and by tests, not by a shared compiled
  source. Accepted for the current fault count (see Alternative 3).

## Related

- Issues: **#575** (this ADR — the cross-surface inversion). **#378** (systemic refresh
  failure), **#469** (canonical scrub), **#498** (keychain locked) — the three faults.
  **#520 / #523 / PR #574** (where the divergence surfaced, during the menubar half of
  #463).
- Code: `src/cli.rs` — `DaemonPayloadFault` + `severity()` + `daemon_fault_line`,
  `severity_line` (generalised from `red_line`), `render_systemic_refresh_failure`
  (now `Yellow`), `render_keychain_locked` / `render_canonical_scrub` (now `Red` /
  plain, `color`-aware). `apps/menubar/Sources/StatusPanelFormat.swift` —
  `daemonFaultBanner` (the ratified panel rank, **unchanged**).
- [ADR-0016](0016-dead-active-no-target-surfaced-not-relaxed.md) — the `next swap:`
  footer content-parity decision the CLI comments over-cited; this ADR clarifies it does
  NOT govern payload-fault colour.
- Design records: `../hq/strategy/design-menubar.md` (the ratified panel severity model +
  R-2), `../hq/strategy/brand-identity.md` (the glance taxonomy).
