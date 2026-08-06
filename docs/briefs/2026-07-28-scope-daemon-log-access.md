---
type: scope-brief
date: 2026-07-28
workflow: /scope
status: final
---

# Scope Brief: Operator access to the daemon's logs

## Problem

The daemon writes a durable, structured event log, but an operator running it in the background has no
supported path to it — reaching it means knowing `~/Library/Logs/sessiometer/sessiometer.log` and
typing `tail`. The product itself makes this worse by *directing* users to logs it gives no affordance
for: `LoginItemModel.swift:257` says "Check Console for details", and the panel mock says "check the
daemon log".

Scoping surfaced that this is **two distinct gaps, not one**. Beyond the missing reader, the
diagnostic channel is *structurally unreachable* for a background daemon: `Verbosity` is settable only
via the `run -v` flag (`cli.rs:730`), the installed launchd agent runs `["run", "--managed"]` with no
`-v` (`service.rs:471`), and no config knob exists. A background daemon emits **zero** diagnostics —
so a reader alone would have delivered only the half that was already visible.

## What's In Scope

**Umbrella: issue #772** — carries the requirements (R1–R11), the four design decisions, and three
binding constraints, so they do not have to be re-derived per item.

1. **issue #773 — `sessiometer log`, offline event-log reader.** `--since`, `--json`, `--event`;
   renders with the daemon down, in the shape `stats` and `reliability` already establish. *No deps.*
2. **issue #774 — `--follow` / `-f`.** Poll-with-seek streaming, recovering from truncation or
   external rotation. *Blocked by #773.*
3. **issue #775 — diagnostic reachability.** A config knob so the managed daemon can emit diagnostics
   without a plist hand-edit, plus `--channel event|diag|all` to read them. *Blocked by #773.*
4. **issue #776 — panel `View log` affordance.** Builds what the ratified mock already specified in the
   daemon-starting and crash-looping states; opens Console.app. *Independent.*

## Key Decisions

1. **Diagnostics stay OUT of the durable log** — `log` reads both files and selects with `--channel`.
   Routing them into `sessiometer.log` would break the invariant at `reliability.rs:118` and inject
   per-cycle noise into the file `reliability` parses.
2. **A config knob (not a plist flag) enables managed-daemon diagnostics** — matches the existing
   no-hot-reload tunable semantics already surfaced at `ConfigWire.swift:217`. Exact key placement is
   left as an explicit open sub-decision in #775 (lean: `[tunables] verbose`) because no `[daemon]`
   section exists and `deny_unknown_fields` makes it a deliberate schema change either way.
3. **`View log` opens Console.app** — the mock specifies the button's placement, label, icon and style
   but is **silent on its behavior**. That gap was surfaced rather than silently filled; Console.app
   was chosen for consistency with the app's own existing "Check Console" copy.
4. **`View log` was an orphaned spec** — `StatusPanelView.swift:190` defers it to issues #169/#171,
   both since closed. Nothing tracked it. #776 re-anchors it.
5. **Diagnostics are opt-in because `daemon.err.log` is an ungoverned channel** — unlike the event log,
   whose fields are handles/enums/numbers by construction, raw stderr can carry panic payloads that
   never passed the issue #15 redaction meter.
6. **Deliberately excluded**: log rotation/retention, writer-side levels, any new or changed event, and
   streaming log content over the control socket.

## Adjacent findings (second scoping pass)

Three further findings surfaced during scoping were verified and tracked as **siblings**, deliberately
not folded into the umbrella's four items:

5. **issue #777 — `Restart…`, the other orphan.** The same `StatusPanelView.swift:190` deferral names
   two affordances; #776 re-anchors `View log`, this re-anchors `Restart…`. Filed as an open
   **question, not an impl item**: issue #742 made `KeepAlive` conditional, so launchd already
   respawns a crash-looping daemon — what a *manual* restart adds is unresolved, and "amend the mock
   to drop it" is an explicitly valid verdict.
6. **issue #778 — 9 of 17 mock frames have no dark variant.** Including both frames #776 builds into,
   so #776's stated conformance oracle does not exist for dark mode. #776 was cross-linked and scoped
   to light-only until this lands.
7. **issue #779 — the #745 failed-state copy.** "Check Console for details"
   (`LoginItemModel.swift:257`, shipped in HEAD `2da1cc6`) is this brief's root problem on a *third*
   surface, uncovered by #776 because the mock's panel frames don't govern the Settings window.
   Blocked by #776.

**One candidate investigated and rejected**: `daemon-console.log` / `relaunch.log` in the log
directory are not produced by repo code (grep over `src/`, `apps/`, `scripts/` is empty) — local
dev-shell redirections, not a product surface. Recorded so it is not re-investigated.

## Stats

- **Work Items**: 5 in GitHub (1 umbrella + 4 executable)
- **Ready**: 4/4 executable items
- **Gaps accepted**: 0 — no typed exceptions were needed
- **Deferred**: 0
- **Existence gate**: BUILD, 🟢-supported demand evidence
- **Coverage gate**: PASS-WITH-FINDINGS → 3 findings, all remediated in-place before close
  (R11 help-lockstep AC missing on #774; no accessibility AC on #776; #776's conformance AC presumed
  the outcome of open spike #749 — now made contingent on it)

## Next Steps

- `/do 773` — start the reader; it unblocks both #774 and #775
- `/do 776` — independent of the CLI chain, can run in parallel
- Note: #776's *verification mechanism* depends on spike issue #749 (headless `ImageRenderer` gating),
  which is in flight on the current branch. The affordance can be built either way; only how it is
  gated changes.
