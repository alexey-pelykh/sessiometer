# Solution Design: GUI/CLI Capability Parity

**Source PRD**: `docs/requirements/gui-cli-capability-parity.md` (`dor_status: passed-with-findings`)
**Decision records**: `../hq/strategy/gui-capability-parity.md` (the ruling) · ADR-0032 (the
empirical result that unblocked it) · `../hq/strategy/design-menubar.md` (REQ-MBR-C-005 + its
routing-as-compliance rider) · `../hq/strategy/design-login.md` (C1 as-built correction — read
before touching `src/paths.rs`)
**Status**: `draft` — one load-bearing open question remains (§ 14, OQ-1). Locks when OQ-1 is decided.
**Date**: 2026-07-31

## 1. Goals and Drivers

Make every `routable` CLI capability reachable from the menu-bar app, and — more durably — make an
**unclassified verb detectable**, so no future verb can silently join the gap the way `login` did.

The design driver is that the gap is at the **daemon socket**, not in Swift. The app already drives
every command the socket serves. Each closure is therefore a socket command plus the affordance that
issues it — not an app rewrite.

## 2. Constraints

| # | Constraint | Source |
|---|---|---|
| C-1 | The client originates no credential/keychain/API seam. Routing satisfies this; a GUI-side credential surface does not. | REQ-MBR-C-005; `src/daemon/socket.rs:32-33` |
| C-2 | The daemon runs a single-threaded `current_thread` Tokio runtime. Blocking work must be spawned off the run loop. | ADR-0001; the shipped `stats` / `config-get` precedent |
| C-3 | `swap.lock` must never be held for a human-paced flow. | PRD R-10 |
| C-4 | The shared `Claude Code-credentials` item stays byte-for-byte unchanged. | Login engine safety invariant; PRD AC-5 |
| C-5 | The isolated login dir's **fixed** path must not change — its hash names the suffixed isolated keychain item the #133 reaper targets. | `src/paths.rs:225-234`; `src/refresh.rs:952` |
| C-6 | Login spawn `argv` stays `&'static [&'static str]`. | `SpawnPlan::argv` in `src/isolated_spawn.rs` |
| C-7 | CLI verbs keep their daemon-*independent* standalone writes. Parity is GUI-client-scoped only. | PRD § 1b |
| C-8 | The frozen `watch` stream contract is not to be disturbed casually. | `src/daemon/socket.rs:8` |

## 3. Context and Scope

```
  menu-bar app  ──AF_UNIX 0600, peer-cred──▶  daemon  ──▶  keychain / config.toml / event log
   (no seam)         newline JSON                          (all credential work happens here)
```

In scope: new socket commands + the panel affordances that issue them + two safety prerequisites.
Out of scope: CLI verb shapes, `export`/`import` exposure (blocked on #999), presentation parity.

## 4. Solution Strategy

Four strategic calls:

1. **Reuse the routed-mutation template rather than invent one.** `swap` (#167), `capture` (#359) and
   `config-set` (#268) are the same shape: auth-gated, state-affecting, daemon performs it itself,
   redacted ack. Every new mutation here follows it. The only command that *cannot* is `login`, for
   one reason — duration.
2. **Treat duration as the sole novel problem.** Every existing socket command completes in
   milliseconds. `login` is ~180 s. That is the one genuinely new lifecycle, and § 5.2 is about it.
3. **Sequence safety prerequisites strictly first.** Two of them (§ 5.1) block the argv change rather
   than accompany it. They are cheap, invisible to the operator, and dangerous to defer.
4. **Make completeness mechanical.** A classification table that a test holds to the parser's verb
   set (§ 5.5) is what stops this recurring. Without it, "parity" is a claim; with it, it is a gate.

## 5. Building Blocks

### 5.1 Safety prerequisites — must land before the argv change

**SP-1 — scrub the refresh-token triple.** Add `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`,
`CLAUDE_CODE_OAUTH_SCOPES`, `CLAUDE_CODE_OAUTH_CLIENT_ID` to `SPAWN_ENV_REMOVE`
(`src/isolated_spawn.rs`). Inert under today's `/login` argv for
`CLAUDE_CODE_OAUTH_REFRESH_TOKEN` only. `CLAUDE_CODE_OAUTH_CLIENT_ID` is read by CC's general
OAuth config resolver, so scrubbing it changes both children's behaviour — a wanted change, not a
no-op. `CLAUDE_CODE_OAUTH_SCOPES` has a second read site outside the login handler whose
reachability is not established, so it is not claimed inert either (issue #1009). Under
`auth login` an inherited refresh token short-circuits the browser entirely and harvests the
**wrong account** while every existing check passes. Ordering is the whole point: this landing
*after* the argv change is a live vulnerability window.

**SP-2 — close the zero-egress gate hole.** `scripts/check-menubar-zero-egress.sh` lists neither
`AuthenticationServices` nor `WebKit`, and `ASWebAuthenticationSession` does not contain the
substring `URLSession`, so the two APIs that would most directly defeat the gate's rationale pass it
green. Add both to the forbidden lists. Independently valuable; urgent here because a login feature
is exactly the context that invites them.

**SP-3 — `login.lock`.** A fail-closed single-flight lock over the isolated login directory.
Explicitly **not** `swap.lock` (C-3). `reap_login_orphan` (`src/refresh.rs:962`, `src/login.rs:338`)
must acquire it too — a reaper that runs against a live login is a saboteur, and it currently has no
way to know one is in flight.

> **Do not "fix" the fixed path.** An early design note specified an ephemeral-id-keyed login dir.
> The as-built fixed leaf is correct (C-5) and the lock is the proper remedy. Changing the path to
> fix concurrency would trade a fixable race for a weakened credential reaper.

### 5.2 `login` — the long-running command shape

**Selected: start-and-observe, two commands, reusing the shipped signal path.**

| Command | Auth | Shape | Returns |
|---|---|---|---|
| `{"cmd":"login","label":"<label>"}` | same-user peer | acquires `login.lock`, spawns off-loop, returns immediately | `{"accepted":true}` or `{"error":"login-in-progress"}` |
| `{"cmd":"login-status"}` | same-user peer | read | `{"phase":"…","label":"…","reason":"…"}` |

Flow: `login` acquires `login.lock` fail-closed → spawns the isolated login **off the run loop** (C-2,
the `stats`/`config-get` precedent) → returns a redacted ack immediately. The panel polls
`login-status` for phase. On terminal success the task writes the stash and raises the **existing**
`roster-reload` signal (#139) — so the reconcile path is shipped machinery, not new code. `swap.lock`
is taken only for the reconcile write, never for the interactive span (C-3).

Phases per PRD § 5: `idle` · `starting` · `awaiting-browser` · `harvesting` · `completed` ·
`cancelled` · `timed-out` · `failed`.

**Why not the alternatives:**

- **Stream phases on the `login` connection** (hold it open, frame-per-phase). Keeps observation
  auth-gated with no polling — but introduces a second long-lived connection lifecycle in both the
  daemon and Swift, where exactly one (`watch`) exists today. Cost is disproportionate to a poll of a
  ~180 s operation.
- **Carry login phase in the `watch` snapshot.** Tempting: the panel already subscribes, so Swift
  needs no new observation machinery. Rejected on two counts — it bumps `STATUS_SCHEMA_VERSION`,
  dragging the whole Swift fixture lockstep along for a field unrelated to status; and `watch` is
  **un-auth-gated** (`src/daemon/socket.rs:8-9`), so login state would be readable by a peer that
  cannot start a login. Widening an un-gated surface to carry auth-flow state is the wrong direction
  even at ≈0 single-user risk.
- **Block the run loop for the duration.** Violates C-2 outright; the daemon would stop polling and
  stop swapping for three minutes.

### 5.3 argv change

`SpawnPlan::login`'s `argv`: `&["/login"]` → `&["auth", "login", "--claudeai"]`
(`SpawnPlan::login` in `src/isolated_spawn.rs`). Stays `&'static` (C-6). Removes the TTY requirement, the onboarding
seed, and the operator's manual `/exit` in one edit (ADR-0032). `--console` / `--sso` remain available
if account-type selection is ever needed.

The onboarding seed in `src/login.rs` and its `hasCompletedOnboarding` handling become dead on this
path and should be removed with the change, not left as a vestige — its presence would imply a
first-start-onboarding concern that no longer exists.

### 5.4 The cheap parity verbs

| Capability | Mechanism | Notes |
|---|---|---|
| `reliability` | new un-gated read, `stats` template | `ReliabilityWire` already exists (`src/reliability.rs`) — mirror `stats`' byte-parity property |
| `log` | new un-gated read, bounded | one-shot tail; follow-mode is a separate question, not required for capability parity |
| `poke` | new auth-gated signal | fire-and-forget, `manual-swapped` template |
| `enable` / `disable` | **extend `config-set`** | `config-get` already projects `enabled` (`src/daemon/socket.rs:35-38`); `config-set`'s allow-list is tunables + labels, enforced by `deny_unknown_fields`. Adding an `enabled` map is a schema addition, not a new command. Read/write asymmetry closed. |
| `remove` | **new auth-gated mutation**, `capture` template | A keychain operation. Mirrors routed `capture` 1:1, under the swap lock. |

`remove` is the only one of these with real risk: it is destructive and irreversible from the panel.
It needs a confirmation affordance and must refuse to remove the account currently active without an
explicit force, matching CLI semantics.

### 5.5 The completeness oracle

A `Routability` classification table — one row per CLI verb, each `routed` · `routable` ·
`structurally-unroutable`, the last requiring a stated structural reason — **plus a test asserting
the table's verb set equals the set parsed by `parse_subcommand` (`src/cli.rs:772-804`).**

This is the durable half of the whole design. The `capture`/`login` split went unrecorded for the
project's life because nothing forced the question. A test that fails on an unclassified verb forces
it exactly once per new verb, at the cheapest possible moment.

### 5.6 Component inventory

| Component | New/changed | Where |
|---|---|---|
| `SPAWN_ENV_REMOVE` | changed (SP-1) | `src/isolated_spawn.rs` |
| zero-egress gate lists | changed (SP-2) | `scripts/check-menubar-zero-egress.sh` |
| `login.lock` acquire/release | new (SP-3) | `src/paths.rs`, `src/login.rs`, `src/refresh.rs:962` |
| `SpawnPlan::login.argv` | changed | `src/isolated_spawn.rs` |
| onboarding seed | removed | `src/login.rs` |
| `login` / `login-status` commands | new | `src/daemon/socket.rs` |
| `reliability` / `log` / `poke` / `remove` commands | new | `src/daemon/socket.rs` |
| `config-set` `enabled` map | changed | `src/daemon/socket.rs`, `src/config.rs` (`apply_settings`) |
| Routability table + parser-parity test | new | test module |
| Panel affordances | new | `apps/menubar/Sources/` |

## 6. Runtime View — routed login

```
panel ──{"cmd":"login","label":"x"}──▶ daemon
                                       ├─ acquire login.lock ──fail──▶ {"error":"login-in-progress"}
                                       ├─ spawn OFF run loop ────────▶ {"accepted":true}
                                       │    └─ isolated CLAUDE_CONFIG_DIR
                                       │       └─ claude auth login --claudeai
                                       │          └─ binds localhost:<port>, opens browser
                                       │             └─ operator completes in browser
                                       │                └─ CC writes suffixed isolated item
                                       │                   └─ harvest → stash write
                                       │                      └─ [swap.lock] reconcile
                                       │                         └─ raise roster-reload (#139)
                                       └─ run loop keeps polling and swapping throughout
panel ──{"cmd":"login-status"}──▶ {"phase":"awaiting-browser", …}   (polled)
```

The run loop is never blocked. The autonomous swap timer fires normally during a login (PRD AC-3).

## 7. Interface Contracts

All new commands are **additive**. No existing reply changes shape; the frozen `watch` contract is
untouched (C-8). **No schema version bump is implied** — `login-status` is a new command's reply, not
a change to `status`. This is a deliberate consequence of the § 5.2 selection.

Auth posture follows the shipped split: reads that expose no secret are un-gated (`reliability`,
`log`); anything state-affecting or auth-flow-related is same-user peer gated (`login`,
`login-status`, `poke`, `remove`, `config-set`).

## 8. UX Architecture

Capability lives **one interaction deeper than the glance** — the glance view stays glanceable
(PRD M3, which is the ruling's own falsifier). The recommended host for lifecycle actions is the
status-item menu rather than the panel body, which keeps the panel a display surface and matches the
app's existing menu usage. **OQ-1** — this is a recommendation, not a decision.

For dead-end states the app's own shipped pattern is **launch**, not copy: `DaemonLog.swift:120-125`
opens Console.app, `SettingsView.swift:90` opens Login Items. `copy` stays correct only where the app
genuinely cannot act (`brew upgrade`).

## 9. Crosscutting Concepts

**Security.** Every new mutation is daemon-side. The client sends a verb plus a non-secret label and
receives a redacted ack. `login`'s ack must carry no credential material at any phase — including
failure reasons, which must be machine-readable causes rather than passthrough error text that could
carry a token fragment.

**Concurrency.** Each new mutation owes a locking answer. `login` → `login.lock` (never `swap.lock`
for the interactive span). `remove` → swap lock, `capture` template. `config-set` `enabled` → existing
`config-set` path unchanged.

**Testing.** Three tiers: (a) the parser-parity test (§ 5.5) as the completeness gate; (b) a
concurrency test asserting a second `login` is refused fail-closed and the first is undisturbed;
(c) the AC-5 hash assertion — shared credential unchanged — on every login path, which the existing
login tests already model.

## 10. Architecture Decisions

| ID | Decision | Where recorded |
|---|---|---|
| ADR-0032 | `login` is daemon-routable; TTY gate is ours | `docs/adr/0032-…md` |
| D-1 | Start-and-observe over stream-or-snapshot for `login` | § 5.2 |
| D-2 | `enable`/`disable` extends `config-set` rather than a new command | § 5.4 |
| D-3 | Fixed isolated-login path retained; lock is the concurrency remedy | § 5.1, C-5 |
| D-4 | No schema bump — new commands, not changed replies | § 7 |

## 11. Risks and Open Questions

### Feasibility

| Component | Verdict | Why |
|---|---|---|
| SP-1 / SP-2 | **FEASIBLE** | list additions |
| SP-3 `login.lock` | **FEASIBLE** | the daemon already uses `flock` (`daemon.lock`, `swap.lock`) |
| `login` / `login-status` | **FEASIBLE** | off-loop spawn is the shipped `stats` pattern; reconcile is the shipped `roster-reload` signal |
| argv change | **FEASIBLE-WITH-GATE** | mechanism trivial; gated on A-1 (§ below) |
| cheap verbs | **FEASIBLE** | 1:1 with shipped templates |
| `remove` routing | **FEASIBLE** | `capture` template; needs a confirmation UX |

### Risk register

| # | Risk | Sev | Mitigation |
|---|---|---|---|
| R-1 | Refresh-token short-circuit harvests the wrong account | **HIGH** | SP-1 lands first — strict ordering, not co-delivery |
| R-2 | Concurrent logins orphan a credential-bearing keychain item | **HIGH** | SP-3 fail-closed; reaper honours the lock |
| R-3 | Loopback bind fails under a launchd LaunchAgent | **MED** | PRD A-2; verify in the managed configuration, which is the only one that matters |
| R-4 | Panel grows into a console, losing glanceability | **MED** | PRD M3 as a per-item design-review criterion |
| R-5 | Routed `remove` destroys an account by misclick | **MED** | confirmation + refuse-active-without-force |
| R-6 | Claude Code changes `auth login`'s surface | **LOW** | #130 manual gate; weaker coupling than the slash-command shape it replaces |

### Open questions

**OQ-1 (load-bearing — holds `draft`)** — Which panel surface hosts lifecycle actions: status-item
menu, panel row affordances, or the settings window? § 8 recommends the menu. This is a UX-architecture
decision and it shapes the affordance items, not the socket items — so the socket work can proceed
while it is open.

**OQ-2 (not load-bearing)** — Does `log` need follow-mode in the GUI, or is a bounded tail enough for
capability parity? Default: bounded tail; follow is presentation, not capability.

### Carried empirical gate

**A-1** — that `auth login` writes to the **suffixed isolated** keychain item is proven for `/login`
at Claude Code 2.1.197 (#130), **not** for this path. One real completed login on the existing #130
manual gate discharges it. It gates the argv-change item and the routed-login item; it gates nothing
else in this design.

## 12. Requirement-to-Track Coverage Matrix

| Requirement | Covered by |
|---|---|
| R-1, R-2, R-3 | § 5.5 Routability table + parser-parity test |
| R-4, R-5, R-6 | § 8 forward-path rule; enumeration item |
| R-7, R-8, R-9 | § 5.2 `login` command |
| R-10, R-11 | § 5.1 SP-3 `login.lock` |
| R-12 | § 5.2 `login-status` |
| R-13 | § 5.1 SP-1 |
| R-14 | § 5.3 (`&'static` retained) |
| R-15, R-16 | deferred behind #999; § 3 out of scope |
| R-17 | § 5.1 SP-2 |

Backward check: every building block in § 5.6 traces to at least one requirement above. No orphans.

## Design Lock Gate

Lifts `draft` when **OQ-1** is decided. OQ-2 and A-1 do not hold the lock — OQ-2 is presentation, and
A-1 is an acceptance criterion carried on two specific items rather than a design unknown.
