---
type: prd
scope: GUI/CLI capability parity — daemon-routed login and the operator capability surface
created: 2026-07-31
workflow: /capture-requirements
source: operator ruling 2026-07-31; audit of menubar/CLI login parity 2026-07-31; 5-lens council
  2026-07-31; empirical spike against Claude Code 2.1.220 2026-07-31 (recorded publicly as ADR-0032).
  Working notes were transient under .tmp/scopes/ and are not part of the repo. This PRD is
  self-contained — nothing downstream needs to dereference that path.
dor_status: passed-with-findings
formulation: {}
features: {}
artifacts: {}
---

# PRD: GUI/CLI Capability Parity

## 1. Problem

### 1.1 Statement

Sessiometer ships two operator surfaces. The CLI parses **18 verbs** (`src/cli.rs:741-765`):
`capture`, `login`, `run`, `service`, `daemon`, `config`, `status`, `list`, `use`, `disable`,
`enable`, `remove`, `poke`, `stats`, `reliability`, `log`, `export`, `import`. The daemon's control
socket — the *only* channel the menu-bar app can speak (ADR-0011) — serves **11 commands**
(`src/daemon/socket.rs:7-46`): `status`, `watch`, `stats`, `manual-swapped`, `roster-reload`,
`restored`, `shutdown`, `swap`, `capture`, `config-get`, `config-set`.

The gap between those two lists is the problem, and **it sits at the daemon socket, not in Swift**.
The menu-bar app already drives every verb the socket serves; it cannot drive what the socket does
not offer. No amount of Swift work closes it.

The operator-visible consequence is that choosing the menu bar silently costs capability. The
sharpest case is `login`: when an account's credential lapses, the panel can *tell* the operator and
cannot *act*, so it renders inert text naming a command to go type somewhere else — a dead end on
the one surface that is always on screen.

### 1.2 Affected users

Operators who drive Sessiometer primarily through the menu-bar app. The app is not a demo surface —
it ships signed and notarized (#171), launches at login (#170), and carries a settings window (#268).
An operator can reasonably run it as their only surface, and today that choice quietly reduces what
they can do.

### 1.3 Why now

Three things converged:

1. **The exclusion was never actually decided.** In-UI `capture` and `login` were excluded together
   as a single clause. `capture` graduated out of it and shipped daemon-routed (#359/#360/#394);
   `login` did not, and no record says why. The split was an absence of a decision, not a decision.
2. **The stated blocker was empirically false.** `login` was believed non-routable because it needs
   an inherited TTY — `src/login.rs:331` calls `require_tty(std::io::stdout().is_terminal())`, and
   the module doc at `:27-29` states the engine "ABORTS if stdout is not a TTY rather than allocate a
   pty the operator could not drive." A spike against the shipping Claude Code binary disproved the
   premise: `claude auth login --claudeai` runs to completion with no TTY at all. The gate is
   sessiometer's own conservatism for the *slash-command* shape, not a Claude Code requirement.
   Recorded as **ADR-0032**.
3. **The operator ruled.** 2026-07-31: an operator on the GUI must reach the same capabilities as an
   operator on the CLI. Choosing a surface is not accepting reduced capability.

### 1.4 Framing provenance

The original framing — *"should the panel mirror the CLI's verb surface?"* — was correctly answered
**no**. Mirroring 18 verbs, several of which are structurally GUI-hostile, dissolves the glance
property that justifies a menu bar. This PRD answers a **different** question: *may a GUI operator be
a second-class citizen?* — answered **no**. Capability parity is not surface parity, and §1b bounds
the difference. Ruling operator-ratified 2026-07-31; the reframe is theirs, not derived.

## 1b. Boundaries

### Appetite

Bounded by capability reachability, not by verb count. The work is: (a) a small number of new socket
commands whose shape already exists (`swap`/`capture`/`config-set` are the template), (b) the panel
affordances that drive them, and (c) two safety prerequisites that must land **before** `login`
routes. Explicitly *not* a rebuild of the panel into a control console.

### Out of scope

- **Structurally-unroutable verbs**, and the exclusion is principled rather than residual: `run`,
  `service install`/`uninstall`, and `daemon restart` for an unmanaged daemon cannot be served over a
  socket **by the process they control**. A verb that manages the daemon's own lifecycle has no
  meaning as a request *to* that daemon. A managed agent is stopped with `launchctl bootout`, not
  over the socket (`src/daemon/socket.rs:19-23`).
- **Presentation parity.** `log --json` and `stats --json` exist to be piped. The GUI must reach the
  same *information*; it need not reproduce the *serialization*.
- **Changing any CLI verb's shape.** Every requirement here is GUI-client-scoped. CLI mutations keep
  their standalone, daemon-absent writes — the CLI's daemon-*independence* and the GUI's
  daemon-*dependence* are opposite constraints on purpose, and drafting parity the other way would
  silently re-open a twice-held architectural verdict.
- **`export` / `import` GUI exposure** — deferred behind #999, not excluded. See A-4 and R-14.
- **A GUI-side credential surface** — permanently out. See §9.1.

## 2. Object Model (OOUX)

| Object | Definition | Key attributes | Actions |
|---|---|---|---|
| **Capability** | An operator-meaningful outcome, independent of which surface delivers it ("onboard an account", "re-authenticate a lapsed account") | `name`, `cliVerb`, `socketCommand?`, `routability` | reach, deny |
| **Routability** | Whether a capability can be served over the control socket | one of `routed` · `routable` · `structurally-unroutable` | classify |
| **SocketCommand** | A newline-delimited JSON request the daemon answers | `cmd`, `authGated`, `stateAffecting`, `ackRedacted` | issue, authorize, answer |
| **OperatorState** | A condition the panel can display that the operator may need to act on | `label`, `isActionable`, `forwardPath` | surface, resolve |
| **ForwardPath** | How an actionable state is resolved from the panel | one of `act` · `launch` · `copy` · `none` | offer |
| **LoginSession** | A ~180 s isolated interactive OAuth run owned by the daemon | `label?`, `phase`, `startedAt`, `outcome` | start, observe, cancel |

**Relationships** — a `Capability` has exactly one `Routability`; a `routed` Capability is delivered
by ≥1 `SocketCommand`; an `OperatorState` that `isActionable` must have a `ForwardPath` other than
`none`; a `LoginSession` is created by the `login` SocketCommand and observed by a separate read.

## 3. Requirements (EARS)

### CapabilitySurface

**R-1** *(ubiquitous)* — Every CLI capability the project classifies as `routable` **shall** be
reachable by an operator driving only the menu-bar app.

**R-2** *(ubiquitous)* — Each CLI verb **shall** carry exactly one recorded `Routability`
classification, and a `structurally-unroutable` classification **shall** state the structural reason
(not "not yet built").

**R-3** *(unwanted)* — **If** a capability is classified `routable` and no socket command serves it,
**then** the project **shall** treat that as a tracked parity gap rather than a design choice.

> Rationale — R-2 exists so the parity claim is falsifiable. Without a recorded classification per
> verb, "parity" has no acceptance criterion and the scope is unbounded, which is the failure mode
> that made the original exclusion defensible.

### ForwardPathCompleteness

**R-4** *(ubiquitous)* — Every `OperatorState` the panel surfaces that `isActionable` **shall** offer
a `ForwardPath` of `act` or `launch`.

**R-5** *(unwanted)* — **If** a state's only offered resolution is inert text naming a command,
**then** that state **shall** be treated as a defect against R-4.

**R-6** *(state-driven)* — **While** the app itself can perform the resolution, the `ForwardPath`
**shall** be `act`, not `copy`.

> Rationale — the panel already ships `launch` twice (`DaemonLog.swift:120-125` opens Console.app;
> `SettingsView.swift:90` opens Login Items). `copy` is correct for a command the app genuinely
> cannot run (`brew upgrade`); it is not a substitute for a capability the app *can* reach.

### LoginRouting

**R-7** *(event-driven)* — **When** an authenticated same-user peer sends `{"cmd":"login"}` with an
optional non-secret `label`, the daemon **shall** run the isolated login capture itself and return a
**redacted** ack.

**R-8** *(ubiquitous)* — The client **shall not** receive any credential material, at any phase of
the login. The request carries a verb and a non-secret label; the ack carries a label and an outcome.

**R-9** *(ubiquitous)* — `login` **shall** be authorization-gated to an authenticated same-user peer,
matching `swap` / `capture` / `config-set` and unlike the un-gated reads.

**R-10** *(state-driven)* — **While** a `LoginSession` is in flight, the daemon **shall not** hold
`swap.lock`.

> Rationale — a login is a ~180 s interactive OAuth. `swap.lock` guards the torn-keychain-write race
> and must never be held for a human-paced flow; holding it would stall the daemon's autonomous swap
> for the duration.

**R-11** *(unwanted)* — **If** a second `login` is requested while one is in flight, **then** the
daemon **shall** refuse the second, fail-closed, without disturbing the first.

> Rationale — `paths::isolated_login_dir()` is a single fixed leaf (`src/paths.rs:235-237`) and
> `create_isolated_dir` `remove_dir_all`s a pre-existing one (`:264`). Concurrent logins delete each
> other's isolation directory mid-OAuth and orphan a credential-bearing keychain item. The fixed path
> is **correct and must not change** — its hash names the suffixed isolated item, which is how the
> #133 orphan reaper targets precisely (`src/refresh.rs:952`). The remedy is a lock, not a path
> change. `reap_login_orphan` (`src/refresh.rs:962`, `src/login.rs:338`) must honour the same lock or
> it becomes a saboteur against a live login.

**R-12** *(event-driven)* — **When** a login is in flight, the panel **shall** be able to observe its
phase without issuing a second `login`.

> Rationale — the request/reply verbs the socket serves today all complete in milliseconds. A ~180 s
> command is a new lifecycle shape; without a separate observation read, the only way to learn the
> outcome is to re-issue the command, which R-11 refuses.

### SpawnSafety

**R-13** *(ubiquitous)* — Before the login spawn's argv changes to the `auth login` form, the child
environment scrub **shall** additionally remove `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`,
`CLAUDE_CODE_OAUTH_SCOPES`, and `CLAUDE_CODE_OAUTH_CLIENT_ID`.

> Rationale — **this is a security prerequisite, not a cleanup.** `SPAWN_ENV_REMOVE`
> (`src/isolated_spawn.rs:67-71`) scrubs `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and
> `CLAUDE_SECURESTORAGE_CONFIG_DIR` — not the refresh-token triple. Claude Code's `auth login`
> short-circuits on an inherited refresh token, writing a credential and exiting 0 **with no browser
> at all**. Under the current argv (`/login`) this is inert; the moment argv becomes `auth login`, an
> inherited refresh token silently harvests the **wrong account** while every existing safety check
> passes. R-13 must land **before** the argv change, not alongside it.

**R-14** *(ubiquitous)* — The login spawn's `argv` **shall** remain `&'static [&'static str]`.

> Rationale — the compile-time no-injection guarantee (`src/isolated_spawn.rs:111`). The operator's
> label is not argv and does not weaken this.

### DeferredCapabilities

**R-15** *(state-driven)* — **While** `import` stages credential bytes without adopting them
(umbrella #999), the project **shall not** expose `export` or `import` in the GUI.

> Rationale — #999 established that `import` writes each per-account stash but never the canonical
> item Claude Code reads (`src/cli.rs:4549-4611`), so imported bytes are staged, not adopted; and the
> artifact is a point-in-time snapshot of a *rotating* secret whose staleness is unrepresentable.
> Exposing that behind a friendlier button ships a known-broken operation. This is a **dependency**,
> not an exclusion — see A-4.

**R-16** *(ubiquitous)* — When `export` is eventually exposed, the `--plaintext` mode **shall not**
be reachable from the GUI.

> Rationale — `export --full` is a *different risk class* from `login`: `login` **adds** a credential
> to the keychain's protection domain, while `export --full` **removes every credential from it**
> into a file with no ACL (`src/migration.rs:67-69`). `--plaintext` (`src/cli.rs:4370-4374`) is a
> one-click total compromise whose stderr warning has no GUI analogue, and the passphrase prompt
> opens `/dev/tty` directly (`src/migration.rs:809-819`), which is structurally unreachable from a
> GUI regardless.

### EgressGate

**R-17** *(ubiquitous)* — The menu-bar zero-egress gate **shall** reject `AuthenticationServices` and
`WebKit`.

> Rationale — `scripts/check-menubar-zero-egress.sh` lists neither as a forbidden import or symbol,
> and `ASWebAuthenticationSession` does not contain the substring `URLSession`. The two APIs that
> would most directly violate the gate's own rationale — by hosting OAuth inside the app — currently
> pass it green. R-17 closes the hole **before** login work makes reaching for them tempting.

## 4. Acceptance Criteria (GWT + BUT NOT)

**AC-1** — *Given* a lapsed account shown in the panel, *when* the operator acts on it from the
panel, *then* the login completes and the account returns to healthy **without the operator typing a
command anywhere**. **BUT NOT** by the panel hosting the OAuth: no credential, code, or token is
handled by app code.

**AC-2** — *Given* a login in flight, *when* a second login is requested, *then* it is refused with a
machine-readable reason and the first is unaffected. **BUT NOT** by queueing the second.

**AC-3** — *Given* a login in flight, *when* the daemon's autonomous swap timer fires, *then* the
swap proceeds. **BUT NOT** by the swap and the login sharing a lock.

**AC-4** — *Given* an environment carrying `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`, *when* a routed login
runs, *then* a browser flow is required and no credential is written from the inherited token.

**AC-5** — *Given* the login capture completes, *then* the shared `Claude Code-credentials` keychain
item is **byte-for-byte unchanged**, verified by hash before and after.

> AC-5 is the invariant the whole isolated-login design exists to protect. It is carried from the
> existing login engine's safety invariants and is not weakened by routing.

**AC-6** — *Given* the panel's full set of actionable states, *when* each is enumerated, *then* every
one offers `act` or `launch`. **BUT NOT** satisfied by a state whose only affordance is selectable
text.

**AC-7** — *Given* a source tree importing `AuthenticationServices` or `WebKit` in the menu-bar
target, *when* the zero-egress gate runs, *then* it fails.

**AC-8** — *Given* the `Routability` classification table, *when* a CLI verb is added, *then* the
table is incomplete until that verb is classified. (Mechanized — see §5b.)

## 5. State Matrix — LoginSession

| Phase | Panel shows | Second `login` | `swap.lock` held | Terminal? |
|---|---|---|---|---|
| `idle` | the account's normal row | accepted | no | — |
| `starting` | in-progress affordance | **refused** (R-11) | no | no |
| `awaiting-browser` | in-progress + cancel | **refused** | no | no |
| `harvesting` | in-progress | **refused** | **briefly, at reconcile only** | no |
| `completed` | account healthy | accepted | no | yes |
| `cancelled` | prior state restored | accepted | no | yes |
| `timed-out` | prior state + reason | accepted | no | yes |
| `failed` | prior state + reason | accepted | no | yes |

The four terminal phases map onto outcomes the existing login engine already produces; routing adds
observation, not new outcomes. `harvesting` is the only phase that touches the swap lock, and only
for the reconcile write — never for the interactive span (R-10).

## 5b. Feature Completeness

The `Routability` table (R-2) is the completeness oracle. It is complete when every verb parsed at
`src/cli.rs:741-765` carries a classification. **A test shall assert the table's verb set equals the
parser's verb set** — otherwise a verb added later is silently unclassified and R-3 never fires,
which is exactly how the `capture`/`login` split went unrecorded for the life of the project.

## 6. Success Criteria

- **M1** — An operator can complete the full account lifecycle (onboard, swap, disable, remove,
  re-authenticate) without leaving the menu bar.
- **M2** — Zero actionable panel states resolve to inert text (AC-6).
- **M3** — The glance property survives: the at-a-glance view remains readable at a glance, with
  capability one interaction deeper. **This is the ruling's own falsifier** — if capability growth
  measurably degrades glanceability, the design is wrong even though the capability is right.
- **M4** — No credential ever crosses the socket to the client, at any phase (R-8, verified by the
  existing redaction discipline).

## 7. Assumption Registry

| # | Assumption | Grade | If false |
|---|---|---|---|
| **A-1** | Under `auth login`, Claude Code writes the harvested credential to the **suffixed isolated** keychain item, as it does under `/login` | 🔴 **unverified** | The harvest step finds nothing; the routed login yields no account. **Blocking for the login item only.** |
| **A-2** | The child's loopback callback listener binds successfully when the daemon runs as a launchd LaunchAgent | 🟡 partly | Login fails in the managed configuration — the only configuration that matters — while passing in foreground testing |
| **A-3** | The panel's actionable-state set can be enumerated exhaustively from source | 🟡 | AC-6 becomes unfalsifiable; needs an enumeration mechanism first |
| **A-4** | #999 resolves `import`'s stage-vs-adopt semantics before GUI exposure is wanted | 🟢 | R-15 simply holds longer; no rework |
| **A-5** | `enable`/`disable` can extend `config-set`'s schema rather than needing a new verb | 🟡 | One extra socket command; no design change |

**A-1 is the one residual empirical claim from the spike and must be discharged by a real completed
login.** The spike proved the *TTY premise* false and confirmed the loopback listener binds, but it
deliberately did not complete an OAuth, so the write-destination under `auth login` is proven only
for `/login` on an older Claude Code build. It is an acceptance criterion on the login work item, not
a blocker on the rest of this PRD — every other requirement is independent of it.

**A-5 detail**: `config-get` already projects each account's `enabled` (`src/daemon/socket.rs:35-38`),
but `config-set`'s editable surface is "tunables + existing-account LABELS only", enforced by
`deny_unknown_fields` — so `enabled` is readable and not writable today. That asymmetry is the design
question, not a defect.

## 8. Source Traceability

| Requirement | Source | Reliability |
|---|---|---|
| R-1, R-2, R-3 | Operator ruling 2026-07-31; verb inventories at `src/cli.rs:741-765` and `src/daemon/socket.rs:7-46` | **A** (direct observation) + **B** (ratified) |
| R-4, R-5, R-6 | Audit finding F-3 (2026-07-31); `DaemonLog.swift:120-125`, `SettingsView.swift:90` as the shipped `launch` precedent | A |
| R-7, R-8, R-9 | `src/daemon/socket.rs:28-33` — routed `capture`, the 1:1 template, incl. its explicit REQ-MBR-C-005 satisfaction note | A |
| R-10, R-11 | `src/paths.rs:235-237`, `:264`; `src/refresh.rs:952`, `:962`; `src/login.rs:338` | A |
| R-12 | Inferred from the socket's request/reply shape vs a ~180 s command | **C** (inference) |
| R-13 | `src/isolated_spawn.rs:67-71` (the scrub list, read directly) + a static read of the shipping Claude Code binary's `authLogin` short-circuit | A / **C** |
| R-14 | `src/isolated_spawn.rs:111` | A |
| R-15, R-16 | #999 + `src/cli.rs:4549-4611`; `src/migration.rs:67-69`, `:809-819`; `src/cli.rs:4370-4374` | A |
| R-17 | `scripts/check-menubar-zero-egress.sh` — absence of both names from either list | A |
| Login is routable at all | Empirical spike, Claude Code 2.1.220, 2026-07-31 → **ADR-0032** | A |
| "16 actionable states, 8 dead-ended" | Council panelist assertion; **not independently re-counted** | **D** (unverified) — which is why R-4 is written as a property per state, and AC-6 as an enumeration, rather than pinning a number |

### A note on one corrected input

The council that informed this PRD reported a doc/impl drift on the isolated login directory and
judged the *design* side (ephemeral-id-keyed) safer than the *as-built* side (fixed path). **That is
backwards, and R-11's rationale records the correction**: the fixed path's hash is what names the
suffixed keychain item, which is how the #133 orphan reaper targets precisely rather than by
scanning. Acting on the council's reading would have weakened a credential-bearing safety mechanism.
The concurrency defect it identified is real; only the remedy direction was wrong.

## 9. Cross-Cutting & Non-Functional

**9.1 Security** — The governing invariant (REQ-MBR-C-005) is that the GUI client originates **no
new credential/keychain/API seam**. It is a constraint on *origination*, not on capability, and
**daemon-routing satisfies it** — `src/daemon/socket.rs:32-33` already names routed `capture` as
satisfying it. What stays permanently forbidden, and is **not** made reachable by this PRD: an
embedded browser, a native OAuth implementation, a token entry field, or any app code path that
reads or writes a credential. The test is *who originates*, never *what the operator can accomplish*.
R-17 hardens the mechanical gate against the specific APIs that would breach this.

**9.2 Concurrency** — Routing moves concurrency; it does not remove it. Every newly-routed mutation
owes an explicit answer against the daemon's autonomous timer-swap (R-10, R-11). `login` is the only
one with a human-paced duration, and is the only one needing a second lock.

**9.3 Compatibility** — New socket commands are additive. Existing clients are unaffected; the frozen
`watch` stream contract is untouched. No schema version bumps are implied by this PRD — `login`'s ack
is a new command's reply, not a change to `status`.

**9.4 Operability** — A routed login that fails must surface a machine reason, not a generic failure:
the operator's next action differs by cause (browser cancelled vs timed out vs wrong account
harvested).

## 10. Requirement Provenance (DoR check 6)

| Class | Requirements | Provenance |
|---|---|---|
| **Traces to an explicit operator request** | R-1, R-7, R-15 (the `login`, `import/export`, and parity asks, verbatim) | Operator, 2026-07-31 |
| **Derived from a ratified ruling** | R-2, R-3, R-4, R-5, R-6, R-12 | Ruling applied to the observed surface |
| **Safety-derived, not requested** | R-10, R-11, R-13, R-14, R-16, R-17 | Found by council + audit; **these constrain rather than expand scope**, and are the class most worth challenging |
| **Dependency-derived** | R-15 | #999 |

R-13 and R-17 deserve explicit note: neither was asked for, and both **block** work rather than
enabling it. They are included because the ruling's direction makes their absence dangerous — R-13
turns an inert scrub gap into a wrong-account credential write the moment argv changes, and R-17
leaves the gate blind to exactly the APIs a login feature invites. Both are cheap.

## Change Log

| Date | Change |
|---|---|
| 2026-07-31 | Initial PRD. Supersedes the in-UI-`login` exclusion; re-scopes REQ-MBR-C-005 as origination-only per its own text and the shipped `capture` precedent. |
