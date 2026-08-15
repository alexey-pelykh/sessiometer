---
type: architecture-decision-record
number: 32
title: "`login` is daemon-routable; the TTY gate is sessiometer's own conservatism, not a Claude Code requirement"
date: 2026-07-31
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0032: `login` is daemon-routable; the TTY gate is sessiometer's own conservatism, not a Claude Code requirement

## Status

**Accepted** — 2026-07-31. Records an empirical result that reverses a standing architectural belief,
and the decision that follows from it. No code change lands with this ADR; it is the record the
subsequent build items are gated on.

## Context

The daemon's control socket serves 11 commands (`src/daemon/socket.rs:7-46`); the CLI parses 18 verbs
(`parse_subcommand`, `src/cli.rs:772-804`). `login` is in the second list and not the first, and
the reason held throughout the project was that **`login` cannot be served over a socket because
it needs an inherited TTY**.

That belief had real textual backing. `src/login.rs:331` calls
`require_tty(std::io::stdout().is_terminal())`, and the module's own doc comment at `:27-29` states
the design intent plainly:

> *Inherit-terminal, never a mediated pty. […] the engine ABORTS if stdout is not a TTY rather than
> allocate a pty the operator could not drive.*

Read as a statement about the world, that closes the question: a daemon has no TTY, a GUI has no TTY,
therefore no routing. It was cited that way — in an audit, and by four of five panelists on a design
council — as the architectural barrier that made GUI login impossible without either shelling out to
Terminal.app or reimplementing OAuth inside the app.

**The belief was never executed against the actual Claude Code binary.**

This repeats the pattern ADR-0031 records at the test layer, where `BarGlyphParityTests`' header
asserted `ImageRenderer` needs a windowserver, that assertion routed an entire scope, and executing
it falsified it. Same shape here: a doc comment describing *sessiometer's own choice* was read as
describing *an external constraint*, and the misreading propagated into architecture.

There was also a falsifier sitting in the tree the whole time. `capture` also touches TTY state —
`src/capture.rs:151` checks `!std::io::stdout().is_terminal()` — and `capture` **was** daemon-routed
(#359/#360/#394). "Touches a TTY" was therefore already known not to imply "not routable."

### The empirical walk

Run 2026-07-31 against Claude Code **2.1.220** (the installed build; the repo's prior isolation walk
was at 2.1.197, per `build/version-compat.md` / #130, and `auth login` had never been walked at all).

Method: `claude auth login --claudeai` under an isolated `CLAUDE_CONFIG_DIR`, stdin `/dev/null`,
stdout and stderr **piped** (`[ -t 1 ]` verified false), the production child environment replicated
(`DISABLE_AUTOUPDATER` / `DISABLE_TELEMETRY` / `DISABLE_ERROR_REPORTING` / `DISABLE_BUG_COMMAND=1`,
`SPAWN_ENV_REMOVE` entries unset), a PATH-shimmed `open` capturing the browser URL instead of
launching it, and a hard `timeout 20` bound.

Safety: the shared `Claude Code-credentials` keychain item was hashed at baseline, after the probe,
and after cleanup — **identical at all three points** (`9107fee6…a541a5`). No isolated keychain item
was created. Isolation held.

Findings:

1. **No TTY is required.** The command ran to the 20 s timeout (exit 124). It did not abort, did not
   warn, did not degrade. The TTY requirement is sessiometer's, scoped to the *slash-command* shape.
2. **The loopback listener is live.** The shimmed `open` captured
   `redirect_uri=http%3A%2F%2Flocalhost%3A62185%2Fcallback` — the child binds its own loopback port
   and expects the callback there.
3. **The printed URL is a different, secondary path.** stdout printed a `redirect_uri` pointing at
   `platform.claude.com/oauth/code/callback` plus `Paste code here if prompted > `. Both URLs carry
   the same `code_challenge` and `state`: one OAuth flow, two completion paths — loopback (primary,
   no user round-trip) and paste-code (manual fallback). A routed design should let the child open
   the loopback URL and never relay the printed one.
4. **The onboarding seed becomes unnecessary on this path.** `auth login` against a started-empty
   config wrote a `.claude.json` with **no `hasCompletedOnboarding` key** and ran no onboarding
   prompts. The seed in `src/login.rs` exists because interactive `/login` triggers first-start
   onboarding, whose own auto-login makes the operator log in twice (#130). That concern dissolves.
5. **`claude auth status` is a clean headless JSON probe** —
   `{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}`, exit 0, no TTY.

## Decision

**`login` is daemon-routable. The project will route it, and will move the isolated login spawn from
the slash-command shape to the `auth login` subcommand shape.**

Concretely: `SpawnPlan::login`'s `argv` moves from `&["/login"]` to `&["auth", "login", "--claudeai"]`
(`SpawnPlan::login` in `src/isolated_spawn.rs`). That single change simultaneously removes the TTY requirement, removes
the need for the onboarding seed, removes the operator's manual `/exit` at the end of a capture, and
unlocks routing. `argv` **stays** `&'static [&'static str]`, preserving the compile-time no-injection
guarantee (the `argv` field of `SpawnPlan`, `src/isolated_spawn.rs`) — the operator's label is
not argv.

**Three prerequisites are binding and must land before or with the argv change:**

1. **Scrub the refresh-token triple.** `SPAWN_ENV_REMOVE` (`src/isolated_spawn.rs`) removes
   `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and `CLAUDE_SECURESTORAGE_CONFIG_DIR`. It does
   **not** remove `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`, `CLAUDE_CODE_OAUTH_SCOPES`, or
   `CLAUDE_CODE_OAUTH_CLIENT_ID`. `auth login` short-circuits on an inherited refresh token, writing
   a credential and exiting 0 **with no browser at all**. Under `/login` this is inert. Under
   `auth login` it silently harvests the **wrong account** while every existing safety check passes.
   This is the sharpest consequence of the argv change and the reason it cannot land alone.
2. **A fail-closed `login.lock`.** `paths::isolated_login_dir()` is a single fixed leaf
   (`src/paths.rs:235-237`) and `create_isolated_dir` `remove_dir_all`s a pre-existing one (`:264`).
   The design assumed sequential, human-paced logins. Routing makes concurrency ordinary, and two
   concurrent logins delete each other's isolation directory mid-OAuth, orphaning a credential-bearing
   keychain item. The lock must **not** be `swap.lock`, which must never be held for a ~180 s
   interactive flow, and `reap_login_orphan` (`src/refresh.rs:962`, `src/login.rs:338`) must honour
   it or become a saboteur against a live login.
3. **The zero-egress gate must reject `AuthenticationServices` and `WebKit`.**
   `scripts/check-menubar-zero-egress.sh` lists neither, and `ASWebAuthenticationSession` does not
   contain the substring `URLSession`. The two APIs that would most directly defeat the gate's own
   rationale currently pass it green — which matters more once a login feature makes reaching for
   them tempting.

**The fixed isolated-login path is correct and must not be changed.** Its hash names the suffixed
isolated keychain item, which is how the #133 orphan reaper targets precisely rather than by scanning
(`src/refresh.rs:952`: it "can NEVER touch a sibling `CLAUDE_CONFIG_DIR`"). The concurrency defect is
remedied by the lock, not by keying the path.

### What remains unproven

The walk deliberately did not complete a real OAuth. It is therefore **not proven** that under
`auth login` Claude Code writes the harvested credential to the **suffixed isolated** keychain item —
that is proven for `/login` at 2.1.197 (#130), not for this path. This is an acceptance criterion on
the routed-login build item, discharged by one real completed login on the existing #130 manual gate.
Also untested: loopback bind under a launchd LaunchAgent session.

## Alternatives considered

**Keep `login` CLI-only; give the panel a one-click handoff.** Cheapest, honours every invariant, and
was the recommendation before the walk. Rejected on the operator's capability-parity ruling: a handoff
still ends with the operator typing in a terminal, which is the thing being removed. Retained as the
*interim* behaviour until routing ships — it is strictly better than inert text either way.

**Allocate a pty in the daemon and proxy the interactive flow.** What `src/login.rs:27-29` refuses,
and rightly: it produces a terminal no operator can drive. The walk makes it moot — with no TTY
required, there is nothing to allocate.

**Implement OAuth natively in the menu-bar app.** Rejected on *subject*, not cost: there is no
sessiometer OAuth to host. `login` **harvests** the credential Claude Code writes to its own isolated
keychain item; the app reimplementing OAuth would be authenticating to something that does not exist.
It is also a direct breach of the panel-originates-no-seam invariant.

**Shell out to Terminal.app from the panel.** Running the CLI with extra steps, and it inherits the
GUI session's environment — the exact vector prerequisite 1 exists to close.

**Change the isolated login dir to be ephemeral-id-keyed** (as an early design note specified).
Rejected — see the fixed-path note above; it would weaken the orphan reaper on a credential-bearing
item to fix a problem a lock fixes properly.

## Consequences

**Accepted:**

- The daemon gains its first **long-running** command. Every existing socket command completes in
  milliseconds; a ~180 s login needs a start/observe shape rather than request/reply, and a second
  read so the panel can watch a login without re-issuing it.
- The panel becomes a full operator surface for the account lifecycle. The glance property must now
  be protected by design rather than by absence of capability.
- The login engine's production path **simplifies**: the onboarding seed and the manual `/exit` both
  become unnecessary.

**Rejected as consequences (explicitly not accepted):**

- No relaxation of the panel-originates-no-seam invariant. It constrains *origination*; routing
  satisfies it, exactly as `src/daemon/socket.rs:32-33` already records for routed `capture`.
- No credential crosses the socket to the client, at any phase.
- No change to the shared `Claude Code-credentials` item's byte-for-byte-unchanged invariant.

**Costs:**

- A version-coupled dependency on Claude Code's `auth login` surface. The project already carries
  this class of coupling and has a mechanism for it (#130's manual gate, the version-compat record).
  `auth login` is a stable, documented subcommand — a weaker coupling than the slash-command shape it
  replaces, which depended on interactive first-start behaviour.

## Related

- **ADR-0011** — menubar↔daemon transport (raw POSIX AF_UNIX); the channel this routes over.
- **ADR-0008** — `login` decouples un-quarantine from activation; the verb's existing semantics.
- **ADR-0031** — the same failure mode one layer down: a documented-but-unexecuted belief routing a
  scope until someone ran it.
- **#130** — the manual version-compat gate that discharges the one remaining unproven claim.
- **#132 / #133 / #134** — the isolated login engine, its orphan reaper, and its production wiring.
- **#359 / #360 / #394** — daemon-routed `capture`, the 1:1 template for a routed credential verb.
- **`docs/requirements/gui-cli-capability-parity.md`** — the requirement set this decision serves.
