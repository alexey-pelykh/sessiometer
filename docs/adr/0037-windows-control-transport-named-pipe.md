---
type: architecture-decision-record
number: 37
title: "The Windows daemon control transport is a named pipe; peer identity is the caller's user SID"
date: 2026-09-08
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0037: The Windows daemon control transport is a named pipe — peer identity is the caller's user SID

## Status

**Accepted** — 2026-09-08. Records the outcome of the **#972 spike** (throwaway
knowledge-acquisition, no production code) under umbrella **#970**, settling the Windows control
transport **before** the dependent peer-identity/lock item (**#976**, open and declared blocked by
#972) and the run-loop work are built on it.
Like **ADR-0011**, this record **precedes the code it governs**: no Windows control transport
exists, and this ADR fixes the approach one will implement.

**It is a decision in force, not a landed port** — the same distinction **ADR-0029** draws for
Linux. Nothing in the crate builds for Windows today, and no CI job compiles for it — **#978** is
the enforcing job, exactly as **#964** is on the Linux side. The port this decision governs is
**#1511**; the dependent identity and single-instance-lock work is **#976**. What is landed
is a *proof*, in `spikes/windows-control-transport/`, which is deliberately outside the root build
graph and is run by the non-required `spike-972-windows-transport` workflow.

**Amended** — 2026-09-09, on **#1511**. The decision is unchanged; the record gained the output of
a SECOND proof. #1511 added an accept-loop / `watch` mode to the same spike package, and its
measured block is quoted in § The second proof: the accept loop and `watch` (#1511) — a NEW section,
sibling to the #972 one under § Context, rather than an addition inside it. That is § Lifecycle's
own requirement rather than a courtesy: it disposes of the spike directory and says this ADR is
what survives it, *"which is why the measured output above is quoted here in full rather than
linked to a CI run that expires"*. The second proof's output had been quoted only in an issue
comment linking a run — precisely the shape that sentence rejects.

## Context

### What the control channel is today

The daemon binds a Unix-domain socket and gates state-affecting commands on the peer's identity.
Four pieces, all cited by symbol rather than by line, since line numbers rot silently:

- **Bind** — `bind_control_socket` in `src/cli.rs` unlinks any stale socket (the single-instance
  lock guarantees no live daemon owns it), calls `UnixListener::bind`, then `chmod`s the path to
  `0600`. The enclosing support dir (`control_socket` in `src/paths.rs`, `support_dir()/daemon.sock`)
  is `0700`, so the socket is owner-only-reachable even during the bind→chmod window.
- **Accept + authenticate** — `UnixControl::serve` in `src/daemon/socket.rs` computes
  `peer_authenticated` **before** it serves anything, and hands `serve_control` a plain `bool`. The
  ordering is not incidental; see § Decision part 4.
- **Peer credential** — `peer_is_same_user` in `src/daemon/peer_auth.rs` narrows `getpeereid(3)`
  (macOS) or `SO_PEERCRED` (Linux) to an `Option<uid_t>` and compares it to our own `getuid()`.
  FAIL CLOSED: an unreadable credential is `None`, which is never a uid, so it can never match ours.
- **Framing** — `serve_control` in `src/daemon/socket.rs`: `BufReader` +
  `.take(MAX_CONTROL_LINE_BYTES)` + `read_line`, then a `serde_json` parse of the trimmed line. The
  one-shot ack is written **inline and best-effort** — payload, `b"\n"`, flush, result discarded
  with `let _ = ack;` — and the discard is deliberate rather than sloppy: propagating a write error
  would drop the `ControlSignal` at `UnixControl::serve`'s error arm and silently cancel an action
  the operator had already authenticated. `write_line` is a *different* writer in the same file,
  carrying the `watch` stream and the inline rejection replies. The distinction matters to a port,
  because the two arms want opposite error handling on a transport whose hang-up error has a
  different name.

### Why this cluster is a spike and not a substitution

Every other Windows cluster is a different symbol for the same idea. This one is not: **tokio
exposes no AF_UNIX surface on Windows at all**, even though Win10 1803+ supports `AF_UNIX` at the OS
level. There is no in-place swap, and the candidates differ in their async story, their
peer-identity story, and their filesystem semantics.

Measured on this branch, `cargo check --target x86_64-pc-windows-msvc --all-targets` fails on both
the binary and its test target. The error counts are deliberately **not** quoted: they move with
every `src/**` change, so a number written here is stale on landing and carries nothing the
enumeration below does not. Two things in that output are worth more than any total:

- **Both `compile_error!` arms fire first, and by name** — the peer-credential one from
  `src/daemon/peer_auth.rs` and the accumulated-suspend one from `src/contract.rs`, each naming the
  unported syscall rather than leaving a bare "cannot find function" at a call site. That is the
  #963 design working exactly as `CONTRIBUTING.md` and the project `CLAUDE.md` say it should.
- **The transport cluster is distinct, and it is not the biggest thing in that output.** `UnixStream`
  and `UnixListener` raise a cluster of their own, joined by `std::os::fd` and `tokio::signal::unix`;
  it sits alongside others — `libc` symbols, Unix file modes and `PermissionsExt`, `OsStrExt`
  byte conversion — that belong to other items in umbrella #970 and are not this decision's to
  answer. No share or per-symbol count is quoted, and **no ranking among those other clusters is
  asserted**; the omission is the point rather than a
  hedge: one source line can raise several diagnostics and one diagnostic can name several symbols,
  so any such number measures the compiler's grouping rather than the porting surface, and it is
  stale the moment `src/**` moves. What makes this cluster a spike is not its size — it is that
  every other cluster has an in-place substitution and this one has none.

### What was measured, and where

The spike is a standalone package under `spikes/windows-control-transport/` — the root manifest
declares no `[workspace]` table, so `cargo metadata --no-deps` at the root lists `sessiometer`
alone and the root build / test / clippy / doc / deny / `check-no-security-framework.sh` gates never
see it. Same posture as `apps/menubar/spikes/**` under ADR-0011. That posture has a flip side the upside
hides: being out of the graph also puts the spike outside `cargo deny`,
`check-no-security-framework.sh` and the three `src/usage.rs` egress lints (two walk
`CARGO_MANIFEST_DIR/src`; the third scans the root `Cargo.lock`, which the spike's own lockfile is
no part of — so none of the three reaches it), while its own workflow runs only `fmt` / `clippy`
/ `build` / `run`. What substitutes for **the graph-level ones** — `cargo deny`,
`check-no-security-framework.sh`, and the lockfile-scanning lint — is a subset property: its
committed `Cargo.lock` pins every transitive crate to the version the root lockfile already holds,
and the only package it adds is the spike itself — so it exercises a dependency set those gates
have already cleared, and it exercises the one a real port inherits. **The two source-walking
lints have no substitute here**: they scan file text, and a lockfile says nothing about the
spike's own sources, so nothing stops a raw socket or a network binary literal appearing under
`spikes/**`. The same gap qualifies § Alternatives considered → 3, which calls TCP-on-loopback
*already red on the existing suite* — true of `src/`, not of a prototype written here. **Nothing
enforces either property.** The subset one was verified by hand
against the root lockfile and it is one `cargo update` away from silently ceasing to hold; the
mitigation is `--locked` throughout the workflow, plus the spike's own deletion under § Lifecycle.

It had to run on a **real Windows host**. The repo has none (ARM macOS, Linux-only Docker), and the
installed `x86_64-pc-windows-msvc` target gives `cargo check` only, which **type-checks without
linking or running** — precisely the blindness class ADR-0029 records for the Mach `extern` block,
and not sufficient for the issue's AC2. The proof therefore runs on a GitHub-hosted
`windows-latest` runner, on the toolchain `RUST_STABLE` pins in the spike's own workflow. The
`windows-latest` label resolved to Windows Server 2025 on the run quoted below; GitHub re-points
that label over time, so treat it as a fact about that run rather than as a requirement — nothing
here depends on the image.

The proof binary's complete stdout, from workflow run `34252922668` — the
`spike-972-windows-transport.yml` run at commit `da006bc` on the spike's branch. Both referents are
**provenance for how this output was obtained, not a currency check a later reader can run**, and
saying so is the point: this repo squash-merges, so `da006bc` never reaches `main`; the workflow
triggers on `pull_request` and `workflow_dispatch` only, so the squash commit has no run of its
own; and § Lifecycle deletes the spike outright. Earlier revisions of this paragraph shipped a
`git log` rule for re-deriving the pin — first against the branch tip, then against the files
behind the binary. Both are **withdrawn**: each of the three facts above makes such a rule report a
byte-identical quote as stale, which is the failure direction that discredits a correct record.
Quoting the output in full is what carries it instead — nothing below depends on the commit, the
run, or the spike directory outliving this ADR. It is the whole of what the program printed; the
workflow step around it also emits cargo's own `Compiling` / `Finished` / `Running` lines, which
are not reproduced.

**The pids are per-run**, so a later run is not byte-identical to this one and that is not
staleness: the host pid, the client child pid, the pipe name that embeds the host pid, and CHECK 4's
restatement of both all move every run. Everything else — every `CHECK` verdict, every
`MEASUREMENT`, and the equality CHECK 5 asserts — is invariant, and that invariant part is the
evidence. Measured, not assumed: re-running the proof at this branch's head reproduced the block
with exactly those substitutions and no other difference. The user SID is the runner account's, a
property of the image rather than of the code; it happened to reproduce across those two runs, which
is a fact about the image and not one to rely on.

```text
[spike-972] host pid           : 7992
[spike-972] host user SID      : S-1-5-21-1456194669-2875347699-3862154473-500
[spike-972] pipe name          : \\.\pipe\sessiometer-spike-972-7992
[spike-972] CHECK 1a create    : PASS — owner-only server instance created (D:P(A;;GA;;;S-1-5-21-1456194669-2875347699-3862154473-500))
[spike-972] CHECK 2  squat     : PASS — second first_pipe_instance create denied (ERROR_ACCESS_DENIED = 5)
[spike-972] client child pid   : 4412
[spike-972] CHECK 1b accept    : PASS — connect() returned; a client is attached
[spike-972] CHECK 1c handshake : PASS — the client has opened the pipe and written NOTHING (it is blocked awaiting our release)
[spike-972] CHECK 0a canary   : PASS — this thread carries NO impersonation token yet (OpenThreadToken -> ERROR_NO_TOKEN = 1008)
[spike-972] MEASUREMENT pre-read impersonation : S-1-5-21-1456194669-2875347699-3862154473-500
[spike-972] CHECK 6  pre-read   : PASS — the peer's SID resolved with NO read having occurred, so the documented wording imposes no read-first ordering constraint
[spike-972] CHECK 0b canary   : PASS — the impersonation token is gone again after the pre-read window
[spike-972] CHECK 4  peer pid  : PASS — 4412 == the spawned child, != our own 7992 (DIAGNOSTIC: a pid is reusable and TOCTOU-prone, never the authentication primitive)
[spike-972] CHECK 3a request   : PASS — read one framed line, one serde_json parse, cmd="status" (17 bytes on the wire)
[spike-972] MEASUREMENT post-read impersonation: S-1-5-21-1456194669-2875347699-3862154473-500
[spike-972] CHECK 5  peer SID  : PASS — S-1-5-21-1456194669-2875347699-3862154473-500 == our own SID (the `getpeereid` analogue: a per-USER identity, not a per-process one)
[spike-972] CHECK 0c canary   : PASS — the impersonation token is gone again after the post-read window
[spike-972] CHECK 3b reply     : PASS — one reply line attempted inline and best-effort, exactly as the daemon writes its ack (delivery is proven by CHECK 3c, not by this write)
[spike-972] CHECK 3c client    : PASS — the child parsed the reply frame and exited 0
[spike-972] VERDICT: PASS — every gated check succeeded.
```

Two properties of that run carry more weight than the passes themselves.

**The client is a child process of the server**, so the pid CHECK 4 resolves is provably not our
own — which is what makes the pid-vs-SID distinction observable rather than asserted.

**The CHECK 0 canaries are a negative control, and without them CHECK 5 is not evidence.** The
child runs as the same user as its parent — which is what lets the run assert "the peer is us", and
is also what makes a SID string alone unable to say where it came from.

The failure the canaries close is specific, and it is worth stating exactly, because the obvious
one is already covered without them: an impersonation that silently no-ops leaves the thread with no
token at all, so `OpenThreadToken` fails `ERROR_NO_TOKEN` and CHECK 5 reddens on its own `Failed`
arm. What CHECK 5 cannot see is a **`RevertToSelf` that returns TRUE and leaves the token in place**.
The next window's impersonation would then succeed-and-do-nothing, `OpenThreadToken` would read the
*residual* token from the previous window — whose user SID is the client's — and the check would
print PASS off a stale reading. `OpenThreadToken` fails `ERROR_NO_TOKEN` on a thread carrying no
impersonation token, so the run proves that failure before impersonating and again after each
`RevertToSelf`. That is what `RevertToSelf`'s own return value cannot tell you: the token is *gone*
afterwards, not merely that the call reported success. **One run exercises both states of the
instrument**, on each side of *both* windows rather than only the last, so a window that failed to
close is caught wherever it sat — and no mutation pass is needed to show the canary discriminates,
since it rejects a success and equally rejects a failure carrying any code but `ERROR_NO_TOKEN`.

**CHECK 1c is what makes CHECK 6 a measurement rather than a race.** The client announces on its
stdout the moment `ClientOptions::open` returns and then blocks until the server releases it, so
every impersonation above the release happens against a connected peer with **zero bytes in the
pipe** — "no read had occurred" joined by the stronger "nothing was there to read". Without that
handshake the client would write its request immediately and whether bytes were already available
would differ per run and go unobserved, which for a question phrased around *the last message read
from the pipe* answers only one branch. The synchronisation deliberately rides stdio and never the
pipe, or it would be the very traffic it exists to exclude.

### The second proof: the accept loop and `watch` (#1511)

Added by **#1511**, in the same throwaway package, as `cargo run -- watch` — a second mode rather
than more checks in the first, so the #972 block above keeps being *"the whole of what the program
printed"*. It answers that item's AC3: how many pipe instances the accept loop keeps outstanding,
and what happens when they are exhausted. Quoted here for the reason § Lifecycle gives — this
record outlives the directory that produced it.

What varies per run is the `host pid`, the pipe name it appears in, and the elapsed times CHECK 7
and CHECK 9 print. Everything else — every `CHECK` verdict, the `MEASUREMENT` line, and the ANSWER
— is invariant, and that was checked rather than assumed: a second green run of byte-identical
spike source differs from this block on those lines and no others. Read an elapsed time as an
ORDER OF MAGNITUDE and never as a value to reproduce. Each is paced by the retry cadence of the
check that measured it as much as by the kernel, and the two checks do not share one — CHECK 7's
own line states its cadence, and CHECK 9 retries on the instance interval alone.

```text
[spike-1511] host pid           : 7512
[spike-1511] pipe name          : \\.\pipe\sessiometer-spike-1511-7512
[spike-1511] max_instances      : 4 (production sets none — tokio's PIPE_UNLIMITED_INSTANCES, under which Windows bounds instances by system resources)
[spike-1511] CHECK 1  bind       : PASS — first instance created with first_pipe_instance (1 listening, 0 connected)
[spike-1511] CHECK 2  squat      : PASS — a second first_pipe_instance create is denied (ERROR_ACCESS_DENIED = 5)
[spike-1511] CHECK 3  subscribe  : PASS — client 1 connected and is HELD OPEN (the `watch` shape); the loop refilled (1 listening, 1 connected)
[spike-1511] CHECK 4  stream     : PASS — 3 newline-delimited JSON frames were all pushed to the held subscriber BEFORE any was read, so the subscriber had them BUFFERED, and each arrived intact over the one connection (order holds by byte-mode construction, not by this measurement)
[spike-1511] CHECK 5  concurrent : PASS — 4 subscribers are connected AT ONCE, each holding its own instance; a one-instance server would have refused every one after the first
[spike-1511] MEASUREMENT outstanding instances: 4 connected + 0 listening = 4 of max_instances=4
[spike-1511] CHECK 6a exhaustion : PASS — with all 4 instances connected, creating the replacement is denied ERROR_PIPE_BUSY and NOTHING is listening
[spike-1511] CHECK 6b client busy: PASS — an arriving client gets ERROR_PIPE_BUSY (= 231), the documented RETRY signal — NOT ERROR_FILE_NOT_FOUND (= 2). True of THIS state — every instance connected, all of them still open. CHECK 9 measures the state it does not cover
[spike-1511] CHECK 7  recovery   : PASS — one subscriber left and a new client connected 0.180s later; no intervention, no restart. Reported on the PASS and not only on the failure, because the FINDING is that the figure is non-zero at all: reclaim is not synchronous with the client's disconnect, so a single refill attempt can still be refused. The figure itself resolves no finer than one poll pass (25ms + 50ms), so read its order of magnitude and not its digits — it measures this proof's cadence as much as the kernel's
[spike-1511] CHECK 8  cancel-safe: PASS — an accept dropped mid-connect left its listening instance alive and the name held; the next client connected on the SAME instance
[spike-1511] CHECK 9  name gone  : PASS — the last instance dropped, and a client then got ERROR_PIPE_BUSY first and then ERROR_FILE_NOT_FOUND (= 2) within 0.063s. So the BUSY-not-NOT-FOUND guarantee holds only while an instance EXISTS, and the name is then free for another process to take. At zero instances a running daemon reads first as SATURATED and then as ABSENT.
[spike-1511] ANSWER (ADR-0037, #1511 AC3): the accept loop keeps exactly ONE listening instance outstanding — a STRUCTURAL fact, carried by the `Option<NamedPipeServer>` this loop and production both hold, corroborated by every check here and measurable by none of them (see `listening()`) — plus one per live connection, so a `watch` subscriber occupies one for its whole lifetime. At the ceiling the refill is denied ERROR_PIPE_BUSY, nothing listens, and an arriving client is told BUSY rather than NOT-FOUND — but only while an instance still exists (CHECK 9): the refill is a single attempt, so a refused refill plus the end of the exchange it served reaches zero instances, and there a live daemon reads first as saturated and then, once teardown completes, as ABSENT. When any connection ends the loop refills and service resumes unattended — but NOT necessarily on the very next accept, since the instance is not reclaimed synchronously with the client's disconnect. That is what production's `wait_for_instance` retry cadence is for, and CHECK 7 prints how long it actually took on this run.
[spike-1511] VERDICT: PASS — every gated check succeeded.
```

Two bounds on that block, both stated in the proof's own source and repeated here because this
record is what survives it. **`max_instances` is pinned to make a ceiling reachable in a CI run**;
production sets none and takes tokio's `PIPE_UNLIMITED_INSTANCES`, a sentinel under which Windows
bounds instances by system resources — so what generalizes is the BEHAVIOUR at a refused create,
not the number, and not the assumption that production's configuration refuses with the same error
code. Nothing measures that. **The loop it exercises is a TRANSCRIPTION.** The spike sits outside
the root build graph, so it cannot `use` `src/control_transport.rs`; `AcceptLoop` mirrors it, and
nothing compares the two. If they drift, the block above measures a loop the daemon does not run.

## Decision

**1. The Windows control transport is a named pipe** (`\\.\pipe\...`), driven through
`tokio::net::windows::named_pipe`. It is the idiomatic Windows control channel and the only
candidate with a first-class async surface in the runtime the daemon already uses (ADR-0001).

**2. The pipe is created owner-only, single-instance, and local-only.**
`ServerOptions::create_with_security_attributes_raw` with a `SECURITY_ATTRIBUTES` whose descriptor
is built from the SDDL string `D:P(A;;GA;;;<our user SID>)` — a *protected* DACL (`P`, so nothing is
inherited from the pipe namespace's defaults) granting `GENERIC_ALL` to exactly one SID and nothing
to anyone else. Plus `first_pipe_instance(true)` and `reject_remote_clients(true)`. The last of
those is already tokio's default and is set explicitly anyway, because on the raw Win32 API it is
opt-in and a future port that stops going through tokio must not silently lose it.

This is the analogue of `bind_control_socket`'s `0600` chmod, and like it, it is the first line of
defence: a foreign user cannot open an instance we created. **Reasoned, not measured** — the runner
is a single account, so no foreign-account open was ever attempted against that DACL; the claim
rests on the descriptor being *protected* (`P`, nothing inherited) with exactly one ACE, and on the
SDDL the run printed being the one it was built from. **`first_pipe_instance` additionally buys a
kernel-enforced name reservation** the Unix side has to get from its lockfile — and that half *is*
measured, at the width the check actually ran: CHECK 2 saw a second create **that also sets
`first_pipe_instance`** fail `ERROR_ACCESS_DENIED` against a held name. That is the documented rule's
own scope, and it is narrower than it sounds — what stops a same-user create that OMITS the flag from
adding an instance is `FILE_CREATE_PIPE_INSTANCE` in the DACL, which is a different mechanism and was
not measured here.

**The `0700` DIRECTORY has no analogue, and that is a real asymmetry rather than a detail.** A DACL
governs who may OPEN an instance we created; it says nothing about who may CREATE the name. On Unix
the support dir is `DIR_MODE` (`0700`, `src/paths.rs`), so a foreign user cannot create our socket
path at all. The pipe namespace has no directory to protect, so `first_pipe_instance`'s
first-creator-wins semantics — the very thing CHECK 2 measured — **cut both ways**: whoever creates
`\\.\pipe\sessiometer-...` first holds it, and the loser is denied. The spike did not measure
whether a foreign local user can win that race, so this ADR does not claim they cannot; see
§ What this spike did NOT establish. What follows for **#976** is that **the CLIENT
must verify the SERVER's identity**, not only the reverse — the reverse is all `getpeereid` ever
had to do, because the directory mode made the forward direction unnecessary.

**3. Peer identity is the caller's USER SID, resolved by impersonation.**
`ImpersonateNamedPipeClient` → `OpenThreadToken` → `GetTokenInformation(TokenUser)` →
`ConvertSidToStringSidW` → `RevertToSelf`, compared against our own process token's user SID.
FAIL CLOSED, mirroring `peer_euid`'s `None`-on-error contract: every failure arm yields a value no
caller can mistake for an identity.

**`GetNamedPipeClientProcessId` is a DIAGNOSTIC only and never the authentication primitive.** A pid
is reusable, and the peer can exit and be replaced between the read and the decision — a TOCTOU
window `getpeereid` does not have, because its uid is captured by the kernel at connect time and is
a property of the *connection*, not of a live process. It is worth reading for logs and for
`sessiometer` diagnostics; it is not worth trusting.

**4. The resolution happens BEFORE the first read, and the existing call ordering is preserved.**
This was the spike's open question: `ImpersonateNamedPipeClient` is documented to give "the security
context of the last message read from the pipe", and whether that wording *implies* a read-first
ordering constraint on a byte-mode pipe is not something an ADR should assert from a doc sentence.
Measured: **the pre-read attempt resolved the peer's SID with a client attached, no read having
occurred, and nothing in the pipe to read** — the last of those established by the out-of-band
handshake (CHECK 1c) rather than by hoping the client had not written yet. That matters because `UnixControl::serve` computes `peer_authenticated` *before* calling `serve_control`,
so a read-first constraint would have forced the authenticate-then-serve split apart. It does not.

The proof's pre-read attempt was **un-gated on its first run** — asserting an answer would have
assumed the finding — and is **gated from this ADR onward** (CHECK 6). The reason is this section:
once a measurement becomes a decision in force, and the proof stays re-runnable until the dependent
items land, an un-gated measurement is a recorded decision a later run can regress in silence.

**5. The message framing survives UNCHANGED.** Measured, not argued: the same `BufReader` +
`.take(MAX_CONTROL_LINE_BYTES)` + `read_line`, one `serde_json` parse, one reply line terminated
`b"\n"` and flushed, worked verbatim over a `NamedPipeServer` in place of a `UnixStream`. One
constraint rides along: **the pipe must stay in BYTE mode** (`PipeMode::Byte`, tokio's default).
Message mode would impose datagram boundaries the newline framing does not need and would change
read semantics under the same code.

## Alternatives considered

### 1. Named pipe — CHOSEN

First-class async via `tokio::net::windows::named_pipe`, a peer-identity story at least as strong as
`getpeereid`'s on the identity axis (§ Consequences), an ACL model that reproduces the `0600`
posture, and a kernel-enforced single-instance flag. Its costs are structural rather than
security-relevant and are enumerated under § Consequences.

### 2. Raw AF_UNIX outside tokio — REJECTED

Win10 1803+ supports `AF_UNIX` on a `SOCKET`, which would keep the path shape and the framing.
Rejected for two independent reasons, either sufficient:

- **No tokio integration.** The daemon is a `current_thread` runtime whose idle select drives the
  control seam between polls (ADR-0001). A raw `SOCKET` has no `AsyncRead`/`AsyncWrite`, so this
  option means hand-writing a readiness/async bridge — new, security-relevant, hand-rolled I/O
  machinery on the one path that authenticates its peer. The whole point of choosing a transport is
  to *avoid* writing that.
- **Peer identity is notably weaker, and that is the load-bearing property.** There is no
  `SO_PEERCRED` equivalent on Windows AF_UNIX. This option would have to fall back to something like
  the pid, which decision 3 above rejects as an authentication primitive on its own terms. Among the
  local-IPC candidates on this platform the pipe's impersonated SID is the strongest identity on
  offer, and trading it for path-shape familiarity inverts the priority the issue sets. No claim is
  made about identity mechanisms outside that set.

### 3. TCP on loopback — REJECTED, and the rejection recorded deliberately

It would put the daemon's control channel **on the host network**, which is what this project's
zero-egress posture exists to prevent — and the guard that bites here is sharper than a posture.
`no_raw_tcp_or_udp_socket_primitive_is_used` in `src/usage.rs` fails if `TcpStream` or
`TcpListener` appears in the crate at all, which are the very types a loopback listener needs, so
this option is **already red on the existing suite** rather than merely disfavoured. Two siblings
hold the rest of the line — `no_in_process_http_tls_or_telemetry_client_is_linked` and
`curl_is_the_only_network_capable_binary_referenced` — all three run in the `test` job, and `test`
is in `ci-ok.needs`, the one required check on `main`. `CONTRIBUTING.md`
§ *System CLIs, not client crates (the transport rule)* states the rule they enforce;
`scripts/check-menubar-zero-egress.sh` holds the Swift side by failing the build if the app so much
as *imports* a networking module; and ADR-0011 records the menu-bar app as a pure local-socket
client for the same reason.

`scripts/check-no-security-framework.sh` is **not** one of these guards, though its place in the
`deny` job invites the assumption. Its own header scopes it to a Security.framework SDK binding and
to keychain access going through `/usr/bin/security` (issue #2); it says nothing about networking.

Three further reasons, each independent of the posture:

- **A loopback port has no owner.** Any local process of any user can `connect()` to it. The
  owner-only DACL and the `0600` socket both make unauthorized reachability impossible in the first
  place; loopback would demote that to an authentication-only defence and delete the defence in
  depth the code comment in `UnixControl::serve` explicitly relies on.
- **Peer identity over loopback TCP is worse still than option 2's.** Recovering the peer requires
  a connection-table lookup (`GetExtendedTcpTable`) keyed on the 4-tuple — a pid, arrived at
  indirectly, with a wider TOCTOU window than the pipe's direct call.
- **It is externally visible.** A listening port shows up in `netstat` and in any host-level
  connection inventory — surface a local control socket simply does not have.

This rejection is recorded rather than assumed precisely because loopback is the *easy* answer when
a Unix socket is unavailable, and the reason not to take it is a project posture rather than a
technical impossibility.

## Consequences

### Positive

- **The wire format is untouched**, so the client side and any future non-macOS client stay shared.
  `StatusResponse`, `ControlRequest` and the `serde` decode above the frame are pure Rust with no OS
  surface; only the byte transport under them changes.
- **Peer identity is finer-grained than a uid.** A SID identifies an account uniquely within its
  domain; a uid is a machine-local integer. On the identity axis this is at least as strong as
  `getpeereid`.
- **The single-instance guarantee gains a kernel-enforced half.** `first_pipe_instance` denies a
  second `first_pipe_instance` create against a held name, measured — see § Decision 2 for why that
  qualifier is load-bearing rather than pedantic.
- **No stale socket to unlink.** The pipe namespace is not the filesystem, so the
  remove-then-bind dance in `bind_control_socket` and the best-effort `remove_file` on shutdown both
  disappear on this target.

### Negative / trade-offs

- **A named pipe serves one client per INSTANCE.** There is no listening socket that accepts
  repeatedly: the server must create a *new* instance for each accept, and only the first may carry
  `first_pipe_instance`. That is a real structural change to `UnixControl`, and it interacts with
  the reservation above — the first instance must stay alive for the name to remain held. This is
  the largest single piece of work the decision implies, and it is **#1511**'s — the transport
  port itself. Deliberately **not** #976's: that issue's Boundaries say *"Do not implement the
  transport here — that is the spike plus its follow-on"*, and until this record was written that
  follow-on did not exist. #970 had left the cluster undecomposed on purpose, because *"the
  transport choice determines the shape"* — this ADR is that choice, so the item is now filable,
  and filed.
- **Impersonation mutates the calling thread's token.** The resolution must therefore be fully
  synchronous: **no `.await` between `ImpersonateNamedPipeClient` and `RevertToSelf`**. What an
  `.await` there costs is **not** primarily a multi-thread hazard, and reading it that way is the
  trap: on the daemon's own `current_thread` runtime (ADR-0001) an `.await` in that window lets
  tokio poll **other tasks on the same thread while that thread carries the client's token** —
  and `UnixControl::serve` (`src/daemon/socket.rs`) spawns exactly such tasks, its own comment
  noting they run *"cooperatively on the one thread"*. On a multi-thread runtime it additionally
  strands the impersonation on a thread that resumes elsewhere. So single-threadedness does not
  make the rule sufficient — the **absence of a suspension point** does, on either flavour, and
  single-threadedness is what turns a violation into a leak to co-scheduled tasks rather than one
  stranded thread. Nor does the type system catch it here: the spike's `!Send` guard makes holding
  the token across an `.await` a compile error only where a `Send` bound is demanded, and
  `trait Control::serve` (`src/daemon/socket.rs`) declares none. It is a rule the port must keep,
  not a property it inherits. `getpeereid` is a pure read and has no equivalent hazard.
- **The client controls the impersonation level.** A client may open the pipe with
  `SECURITY_ANONYMOUS`, in which case the server's impersonation yields an anonymous token. Under
  the fail-closed comparison this can only make the client **deny itself** — an anonymous token's
  user SID is not ours — so it is not an escalation path. It is still an asymmetry `getpeereid` does
  not have, where the peer has no say in what the kernel reports. **Reasoned, not measured**: the
  proof's client opens the pipe with tokio's defaults and never requests a different impersonation
  level, so this paragraph is an argument from the fail-closed comparison and from the API contract,
  not a reading taken off a run.
- **The name is squattable in a way the socket path is not.** The `0700` support dir means a
  foreign user cannot create our socket path at all, so `getpeereid` only ever had to answer the
  forward direction. The pipe namespace has no directory to protect, so first-creator-wins — the
  property CHECK 2 measured in our favour — cuts the other way too: a foreign local process that
  creates `\\.\pipe\sessiometer-...` first either denies the daemon its own name or stands a server
  in front of the CLI. **#976** therefore owes a **client-side check of the server's
  owner SID**, and **#1511** **must open** with
  `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` so a rogue
  server cannot impersonate the CLI even if it wins the race — the flag pair rides on the client's
  own open call, so it is transport code even though identity is what it protects. Neither is
  optional, and neither is
  work the Unix side ever had to do. § What this spike did NOT establish restates this pair in the
  same words on purpose — that is the section an implementer skims for what is owed, so the two
  must not drift apart in strength.
- **`paths::control_socket()` gains a per-target shape**, and with it every caller that reasons
  about the socket as a *file*. The CLI's friendly `Error::DaemonNotRunning` currently keys on a
  failed connect to a filesystem path; on Windows the not-running case is `ERROR_FILE_NOT_FOUND`
  from `CreateFile`, and the busy case is `ERROR_PIPE_BUSY`, which is **not** an error at all but
  the documented signal to retry.
- **`tokio::signal::unix` has no Windows analogue.** `SignalKind::interrupt` / `terminate` in
  `src/daemon/seams.rs` map onto `tokio::signal::windows::{ctrl_c, ctrl_break, ctrl_close}`, which
  is a different shape (console control events, not POSIX signals). Out of scope here; named
  because this ADR is what a reader will consult when the daemon run loop is ported.

### What this spike did NOT establish

Recorded as residuals rather than left to be rediscovered, and **each names its own owner** — they
do not all belong to one item, which an earlier revision of this section assumed. Identity-shaped
residuals are **#976**'s (*build: peer identity and the single-instance lock on Windows*, open and
declared `Blocked by #972`). Transport-shaped ones — the accept loop, and `watch` — are
**#1511**'s, because #976's Boundaries exclude implementing the transport. Enforcement is
**#978**'s, since
#976's own AC2 *presupposes* the Windows CI job by requiring verification *"on the Windows CI
job"*, so it cannot also be the item that builds it. Several residuals below sit
outside #976's acceptance criteria as that issue is currently written: it was authored before this
spike and says outright that it *"deliberately does not prescribe the mechanism"*. Carrying them
onto their owners is tracker work this record does not perform, and merging this PR auto-closes
#972, so the carry is owed at that moment and not later:

- **No cross-user test.** The proof's client is a child of its server and so runs as the same user
  by construction. The negative control proves the instrument distinguishes *impersonating* from
  *not impersonating*; it does not prove that a **different** user's SID would be reported as
  different. Nothing in the API suggests otherwise, but nothing here measured it. **#976** owes it.
- **No foreign-account open against the DACL.** Every connection in the run was opened by the same
  account that created the pipe, so the descriptor's *denying* half was never exercised — only its
  granting half, implicitly, by the client's own successful open. **#976** owes one cross-account open attempt against a live
  instance; it is a two-account test, not a design question.
- **No standard-user run.** The runner's account SID ends in `-500`, the built-in Administrator RID,
  so every measurement was taken in a privileged context. Impersonating a client at Identification
  level is documented not to require `SeImpersonatePrivilege`, and a same-user token is a further
  exemption — but Administrators hold that privilege by default and a standard interactive user does
  not, so the run cannot distinguish "no privilege was needed" from "the privilege was present". The
  daemon's gate only ever asks about a same-user peer, which is the exempt case; confirming that on
  a non-privileged account is **#976**'s work, not spike work.
- **Nothing about pipe-name pre-creation.** CHECK 2 measured only the case where *we* create the
  name first. Whether a foreign local user can create `\\.\pipe\sessiometer-...` before the daemon
  does — and so either deny the daemon its own name or stand a server in front of the CLI — was not
  measured, and cannot be on a runner where everything is one account. It is the direction the
  `0700` directory closes for free on Unix, so it is the one place the port is structurally exposed
  where the socket was not. **#976** owes a client-side check of the server's owner
  SID, and **#1511** **must open** with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` so a
  rogue server cannot impersonate the CLI even if it wins the race — the same split and the same
  strength § Consequences → Negative states them at, and § Decision 2 with them. The flag pair is
  a binary the port either sets
  or does not (`SECURITY_IDENTIFICATION` without `SECURITY_SQOS_PRESENT` is not requested at all),
  and nothing here measured it, so this prose is the only carrier it has.
- **Nothing about performance, reconnection, or the `watch` stream.** The proof round-trips exactly
  one message on one connection. The long-lived `watch` subscription (#165), which hands the
  connection to a spawned task and streams frames indefinitely, is untested here — and it is where
  one-client-per-instance (§ Negative) bites hardest, since a subscriber occupies an instance for
  its whole lifetime. **#1511 owes a `watch`-shaped proof before that subscription is
  ported**, answering how many instances the accept loop keeps outstanding — the accept loop is
  the transport port's work, not #976's, for the same Boundaries reason as § Negative above. This
  record does not settle it and no measurement here bears on it.
- **Nothing is enforced, and this one is #978's, not #976's.** No CI job compiles the crate for
  Windows; **#978** — *ci: add a Windows job that builds and tests (not just checks)* — is the
  enforcing job, the Windows half of the guard **#964** provides on Linux. The
  `spike-972-windows-transport` workflow builds and runs the *spike*, is not in `ci.yml`, and is not
  in `ci-ok.needs` — deliberately, so a throwaway proof never becomes a required check. Its
  existence is evidence about the transport, never about the crate.

### Lifecycle

`spikes/windows-control-transport/` and `.github/workflows/spike-972-windows-transport.yml` are
throwaway. Delete both once the dependent Windows items no longer need the proof re-runnable; this
ADR is what survives them, which is why the measured output above is quoted here in full rather than
linked to a CI run that expires.
