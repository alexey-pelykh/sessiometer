---
type: scope-brief
scope: GUI/CLI capability parity — daemon-routed login and the operator capability surface
date: 2026-08-04
umbrella: "#1008"
---

# Scope Brief: GUI/CLI Capability Parity

## The ask

Four parts, given 2026-07-31: the audit findings; make `login` daemon-routable and expose it in the
UI; parity between CLI and UI; and explore issuing `claude '/exit'` so the operator need not type
`exit` manually.

Mid-scope the operator ruled, in their own words: *"users who use UI must have access to same
features as CLI, login, import/export etc."*

## What the ruling changed

The scope's own Existence gate had returned **RESHAPE** on the parity and login asks, on the argument
that surface-constraint parity is a category error — the CLI and GUI carry deliberately *opposite*
constraints (the CLI verb must work daemon-absent; the GUI client must not originate a seam).

That critique was answering *"should the panel mirror the CLI's verb surface?"* — correctly, **no**.
The operator's reframe was sharper and asked a different question: *may a GUI operator be a
second-class citizen?* — **no**. Capability equity, not surface symmetry. That is theirs to set, and
it moved both sources RESHAPE → BUILD.

## The finding that unblocked it

`login` was believed non-routable because it needs an inherited TTY. The belief had real backing:
`src/login.rs:331` calls `require_tty(...)`, and `:27-29` states the engine "ABORTS if stdout is not
a TTY rather than allocate a pty the operator could not drive." It was cited that way in the audit
and by **four of five** design-council panelists — a FALSIFIER-CONVERGENT verdict.

**It had never been executed.** A spike against Claude Code 2.1.220 found `claude auth login
--claudeai` runs to completion with stdout piped and **no TTY at all**. The gate is sessiometer's
own, scoped to the *slash-command* shape.

A falsifier had been in the tree the whole time: `capture` also touches TTY state
(`src/capture.rs:151`) and *was* daemon-routed (#359). "Touches a TTY" never implied "not routable."

The fourth ask (`/exit`) turned out **moot in the best way** — `auth login` is a one-shot subcommand
that exits on its own, so there is no interactive session to leave. The operator's instinct was
right; the mechanism is cleaner than the one proposed.

## Three corrections made during the run

1. **A council defect was inverted.** It read the fixed isolated-login path as drift against an
   early design note specifying ephemeral-id keying, and judged the design side safer. Backwards —
   the path's hash *names* the suffixed isolated keychain item, which is how the #133 orphan reaper
   targets precisely rather than by scanning. Acting on it would have weakened a credential-bearing
   safety mechanism. The concurrency defect is real; the remedy is a lock, not a path change.
2. **Council sequencing reversed.** It advised export/import *before* login. Umbrella #999 has since
   established that `import` **stages credential bytes without adopting them**, so GUI exposure would
   ship a known-broken operation. Export/import now sequences **last**, behind #999.
3. **A panelist's "16 states, 8 dead-ended" count was not verified** and is graded accordingly. The
   requirement is written as a property per state and the AC as an enumeration, so it does not
   depend on the number.

## Delivered

| Artifact | Where |
|---|---|
| PRD — 17 EARS requirements, 8 ACs | `docs/requirements/gui-cli-capability-parity.md` |
| Solution design (`draft`, one open question) | `docs/design/gui-cli-capability-parity-solution-design.md` |
| ADR-0032 — the empirical result | `docs/adr/0032-login-is-daemon-routable-tty-gate-is-ours.md` |
| Four feature specs | `docs/specs/{spawn-env-scrub,login-single-flight,login-routing,verb-routability-oracle}.feature.md` |
| Ruling record + four consuming-doc amendments | private strategy repo |
| 16 tracked items | #1008 umbrella + #1009–#1023 |

Merged as #1024 and #1025.

## Safety posture — unchanged

REQ-MBR-C-005 is **re-scoped, not relaxed**. It constrains *origination*; routing satisfies it, and
`src/daemon/socket.rs:32-33` already recorded that for routed `capture`. Permanently out: an embedded
browser, a native OAuth implementation, a token field, or any app path that reads or writes a
credential.

Two safety prerequisites **block** rather than accompany the argv change — #1009 (the refresh-token
scrub, an inert gap that becomes a wrong-account credential write) and #1011 (the login single-flight
lock).

## The durable outcome

#1012 — a routability classification per CLI verb, with a test asserting the table covers the
parser's verb set. The `capture`/`login` split went unrecorded for the project's life because nothing
forced the question. That test forces it once per new verb.

## Carried open

- **A-1** — that `auth login` writes to the *suffixed isolated* keychain item is proven for `/login`
  at Claude Code 2.1.197 (#130), **not** for this path. One real completed login on the #130 manual
  gate discharges it. AC on #1020; gates nothing else.
- **OQ-1** — which panel surface hosts lifecycle actions. Holds the design at `draft`; carried on
  #1022.
