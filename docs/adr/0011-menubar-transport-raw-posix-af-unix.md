---
type: architecture-decision-record
number: 11
title: "menubar↔daemon transport — raw POSIX AF_UNIX from Swift (not Network.framework)"
date: 2026-07-07
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0011: menubar↔daemon transport — raw POSIX AF_UNIX from Swift (not Network.framework)

## Status

**Accepted** — 2026-07-07. Records the outcome of the **#321 spike** (throwaway
knowledge-acquisition, no production code) that de-risks the Swift↔daemon socket
path and settles the transport approach **before** WI-2 (**#323**, `WatchTransport`)
is built on top of it. Like **ADR-0010**, this record **precedes the code it
governs**: `WatchTransport` does not exist yet, and this ADR fixes the approach it
will implement.

## Context

Per **ADR-0010** the macOS menu-bar app (**#168**) is a **pure local-socket client**
of the daemon: it links **no** Rust, shares **no** build graph, and the **AF_UNIX
control socket is the entire boundary** between the two languages. It subscribes to
the daemon's `watch` stream (**#165**), which pushes the **frozen, versioned #164
status snapshot** as newline-delimited JSON frames; **#322** landed the hand-written
Swift decoder (`apps/menubar/Sources/WireModel.swift`) that mirrors that wire
contract.

WI-2 (**#323**) is the `WatchTransport` that connects, subscribes, and feeds decoded
frames to the UI. Three questions had to be settled *empirically* before building it,
and the design **leans toward raw POSIX** going in (pure AF_UNIX, zero network
egress — consistent with **#271**'s no-telemetry posture and the zero-FFI ethos of
**ADR-0002**/**ADR-0004**):

1. **Transport primitive** — raw POSIX `socket(AF_UNIX, SOCK_STREAM)` driven through
   Swift's `Darwin` module, or **`NWConnection`** (Network.framework)?
2. **Socket-path resolution** — does a **non-sandboxed** GUI app resolve the *same*
   path the daemon binds (`src/paths.rs`)?
3. **The hard parts** — `EINTR` / partial-read handling, and bridging a **blocking
   `read()` loop into an `AsyncStream`** for the UI to consume.

### What was validated, and against what

The spike (`apps/menubar/spikes/`) is a standalone Swift client compiled with
`swiftc` **reusing `WireModel.swift` verbatim** (it exercises the real #322 decoder,
not a re-derived contract). It was validated against **two** targets, because the
ideal single target was unavailable:

- **The live production daemon** (read-only) — proved raw POSIX `connect()` reaches
  the daemon's *real* native-local socket at the resolver-derived path, and that an
  unrecognised reply is tolerated (below). `watch` is a **non-secret, non-auth-gated
  read stream** (`src/daemon/socket.rs` — unlike the state-affecting `swap` /
  `manual-swapped`, it takes **no** `getpeereid` handshake), so this touched **no
  credential** — well clear of the **#209** boundary.
- **A throwaway AF_UNIX stub** vending the **byte-exact `snapshotBasic` golden
  fixture** from `apps/menubar/Tests/Fixtures.swift` (itself byte-exact daemon
  encoder output) — proved the full decode path, partial-read accumulation, a
  forced-`EINTR` retry, and the `AsyncStream` bridge, deterministically.

**Why not the ideal target — a fresh empty-config daemon.** The spike could *not*
stand up a clean daemon to serve `watch`, for two independent reasons discovered
during the spike:

- The **control socket path is native-local and un-overridable** — `support_dir()`
  resolves from `getpwuid(getuid())->pw_dir` and, unlike `config_dir()`, ignores
  `$XDG_CONFIG_HOME` by design (issue #7: the lock/socket contention must be
  machine-global). A second `run` therefore binds the **same** real
  `~/Library/Application Support/sessiometer/daemon.sock` and contends on the same
  single-instance lock (`src/cli.rs` — a second `run` exits `3`). A production daemon
  was already running, so a fresh instance could not bind.
- Even standalone, an **empty config does not bind at all**: `run()` calls
  `config.require_roster()?` and *"fail[s] fast with the friendly empty-state,
  **before binding the socket**"* (`src/cli.rs`) when there are zero accounts. So
  "spin up a zero-account daemon and subscribe" is not a thing — a definitive answer
  to the spike's "confirm before assuming."

The byte-exact stub closes the gap: because the golden fixtures **are** the daemon's
own encoder output, validating the decode against the stub is equivalent to
validating it against a #165+ daemon's `watch` stream.

## Decision

1. **Raw POSIX `socket(AF_UNIX, SOCK_STREAM)` / `connect()`, driven through Swift's
   `Darwin` module, is the `WatchTransport` primitive — NOT `NWConnection`.** The
   spike connected, subscribed (`{"cmd":"watch"}\n`), read with an `EINTR`/partial-
   read-safe loop, newline-split, and decoded frames through `WireModel.swift`, on a
   small amount of pure `Darwin`/`Foundation` Swift (the transport core — connect, a
   newline line-reader, and the thread→`AsyncStream` bridge — is ~150 lines; the full
   spike adds a path check and an `EINTR`/logging harness), against both the stub and
   the live daemon. Zero network egress, no networking stack, no entitlement — the
   socket-only boundary ADR-0010 mandates stays literally true.

2. **This is NOT Rust FFI and does NOT conflict with ADR-0010's "no FFI".** ADR-0010
   forbids **linking the Rust library** (a shared build graph / FFI binding). Calling
   `socket(2)` / `connect(2)` / `read(2)` via Swift's `Darwin` module is calling the
   **system libc**, exactly as the Rust crate calls `getpeereid` / `getpwuid` raw
   (**ADR-0004**) and shells `/usr/bin/security` (**ADR-0002**). No Rust symbol is
   linked; the AF_UNIX wire stays the entire boundary. Stated explicitly here to
   pre-empt a "raw POSIX == FFI == ADR-0010 violation" misreading.

3. **Socket-path resolution: the non-sandboxed app computes the daemon's exact path,
   and MUST stay non-sandboxed.** The client resolves `getpwuid(getuid())->pw_dir` +
   `/Library/Application Support/sessiometer/daemon.sock` — the **same** password-
   database home `src/paths.rs::home_dir()` uses (never `$HOME`, never XDG, never a
   sandbox container). The spike confirmed `getpwuid` home **==** `NSHomeDirectory()`
   **==** the daemon's `control_socket()` **==** the live socket. `WatchTransport`
   should resolve via `getpwuid` (matching the daemon) and MAY assert
   `NSHomeDirectory()` agrees as a sandbox tripwire.

4. **The blocking-read → `AsyncStream` bridge WI-2 adopts: a dedicated `Thread`, not
   a cooperative-pool `Task`.** The blocking `read()` loop runs on a dedicated
   `Thread` (a blocking syscall on a `Task` would starve a shared cooperative-pool
   thread); each decoded frame is `continuation.yield`ed; `continuation.onTermination`
   `close()`s the fd, which unblocks the pending `read()` so the thread exits. The UI
   consumes with `for await`. Any non-`EINTR` read error **ends** the subscription
   (the reader returns end-of-stream; reconnect/backoff lives *above* the transport)
   — the same "any I/O error just ends the stream" model the daemon's `serve_watch`
   uses on its side.

## Alternatives considered

1. **`NWConnection` over `NWEndpoint.unix(path:)` (Network.framework).** Network
   .framework *can* address a Unix-domain socket — `NWEndpoint.unix(path:)` is a real
   API (verified: it compiles) — so this is a genuine alternative, not an impossibility.
   - **Pros**: a managed connection state machine, built-in queueing, cancellation,
     and path-change handling; no hand-rolled `read()` loop; Apple's "preferred"
     modern networking API.
   - **Cons**: it interposes a **networking stack and connection state machine** for
     what is a purely local, same-user byte stream — weight this transport does not
     need. It works against the **zero-egress** property that is first-class here
     (the app must be provably socket-only, in the spirit of #271 and the zero-FFI
     ADRs); pulling in Network.framework muddies "this app does no networking." Its
     `receive(minimumIncompleteLength:maximumLength:)` still hands back **arbitrary
     byte runs** that must be reassembled into the daemon's **newline** framing — so
     the framing loop the raw path writes by hand does not disappear, it just moves
     inside `NWConnection` callbacks. And it buys nothing a local UDS needs: no TLS,
     no host resolution, no path migration on a `AF_UNIX` socket.
   - **Why rejected**: heavier and less honest for a local newline-delimited UDS
     stream, with no offsetting benefit. Raw POSIX's transport core is ~150 lines,
     provably egress-free, trivially unit-testable over the same fd abstraction, and
     empirically
     worked first try (connect, `EINTR`, partial reads, `AsyncStream` bridge all
     confirmed). It also matches the crate's own "system primitive, not a framework"
     posture (ADR-0002/0004).

2. **`DispatchIO` / `DispatchSource` read on the fd.** GCD-native async reads, no
   dedicated thread.
   - **Pros**: no blocked thread per subscription; integrates with a dispatch queue.
   - **Cons**: still a raw-fd approach (same `connect()`), so it does not change the
     transport-primitive decision — it is an *implementation detail of the read loop*.
     It trades the (simple, obvious) dedicated-thread bridge for `DispatchData`
     reassembly + newline framing inside GCD callbacks, and a less direct mapping to
     `AsyncStream`. `watch` is a **low-frequency monitoring stream** (a snapshot on
     change, a 15-second heartbeat otherwise — `src/daemon/socket.rs`), so one blocked
     thread per menu-bar subscription is negligible.
   - **Why (deferred, not rejected)**: a valid future optimization for the read loop
     *within* the raw-POSIX decision, not a competing transport. #323 may adopt it if
     the single blocked thread ever matters; the spike shows the dedicated-thread
     bridge is more than adequate.

3. **`FileHandle` / `Stream` (Foundation) over the connected fd.** Wrap the fd in
   `FileHandle(fileDescriptor:)`.
   - **Cons**: `FileHandle`'s readability semantics and exception-on-error behavior
     are a poor fit for a long-lived streaming subscription, and it still needs the
     same newline reassembly. No benefit over the explicit `read()` loop.
   - **Why rejected**: adds a Foundation abstraction without removing the framing
     work, and its error model is awkward for a stream.

## Consequences

### Positive

- **The socket-only, zero-egress boundary stays literally true.** The app links no
  Rust (ADR-0010) *and* pulls in no networking stack — `Darwin` libc + `Foundation` +
  `os` is the whole surface. "This app does no networking" is verifiable by the
  absence of a Network.framework link, aligned with #271's no-telemetry stance.
- **Trivial, testable, hand-auditable.** The transport is a connect + a newline line-
  reader + a thread→`AsyncStream` bridge. The spike unit-exercised `EINTR` (a real
  `SIGALRM` interrupted a real blocking `read()`; the loop retried and still decoded)
  and partial reads (a frame split across two `read()`s was reassembled) — the two
  parts most likely to be got wrong.
- **Path resolution is proven end-to-end.** The resolver derives the exact path the
  daemon binds, confirmed both statically (against `paths.rs`) and live (the running
  daemon answered on the derived path).
- **`watch` needs no peer handshake.** It is not auth-gated, so `WatchTransport` is a
  plain connect-and-read; the `0600`/`0700` same-user filesystem permissions are the
  only gate, which a same-user non-sandboxed app satisfies.

### Negative / trade-offs

- **The app MUST stay non-sandboxed.** Under App Sandbox, `NSHomeDirectory()` returns
  the **container** path (diverging from the passwd-DB home the daemon uses), and the
  sandbox would in any case **deny** a `connect()` to a socket outside the container.
  So the native-local socket is only reachable from a **non-sandboxed** app — which
  this is (no sandbox entitlement; an `LSUIElement` accessory app distributed via
  notarized DMG / Homebrew per ADR-0010/#171, **not** the Mac App Store). **#171 must
  not add the App Sandbox entitlement**, and MAS distribution is incompatible with
  this transport. The spike's path check doubles as a runtime sandbox tripwire
  (`getpwuid` home ≠ `NSHomeDirectory()` ⇒ sandboxed ⇒ degrade loudly).
- **A hand-rolled read loop and one blocked thread per subscription.** The `EINTR` /
  partial-read framing is maintained by hand (the standard retry/accumulate pattern),
  and each live subscription parks one dedicated thread on a blocking `read()`.
  Accepted: the loop is small and now spike-tested, and `watch` is a low-frequency
  monitoring stream, so a single parked thread per menu-bar window is immaterial
  (alternative #2 is the escape hatch if it ever isn't).
- **macOS-specific by construction** — `Darwin`, `getpwuid`, `sockaddr_un`. In line
  with the rest of the product (ADR-0004's closing note).

## Validation caveats & surprises (input for #323)

- **⚠ The currently-running production daemon predates #164/#165.** It replies to
  `{"cmd":"watch"}` with `{"error":"unknown command"}`, and its `status` reply
  carries **no `schema_version`** (the pre-freeze shape) — so the **live snapshot-
  decode path could not be exercised against it**; the byte-exact stub covers decode.
  This is a **dev-environment artifact**, not a design blocker: **#171 version-locks
  the embedded daemon to the app**, so in the shipped artifact `watch` is always
  present and current. But it yields a concrete **#323 requirement**: `WatchTransport`
  must treat *"daemon returns `{"error":…}`"* or *"only `.unknown` frames arrive"* as
  **watch-unavailable → degrade** (surface "can't reach a compatible daemon"), **not**
  hang forever awaiting a snapshot. Pleasingly, the spike already exercised the
  forward-compatible half of this against the real older daemon: the unrecognised
  reply decoded to `.unknown` and was **ignored, not crashed** — the #164 additive
  ethos, confirmed live.
- **The `watch` stream is not authenticated** (only state-affecting commands are).
  Good news for #323 — no `getpeereid` client handshake — but it means the transport
  must not *assume* the first frame is a snapshot (see above).
- **Toolchain**: the spike compiles standalone with `swiftc` (Swift 6.3), which needs
  no full Xcode. `xcodebuild` (the #322 XCTest suite in CI's `swift` job) was **not
  available** in the spike environment (Command Line Tools only) — irrelevant to #323
  beyond noting that the transport's own logic can be unit-tested with `swiftc` if a
  non-Xcode path is ever wanted.

**Green light**: the design's raw-POSIX lean is **confirmed**, the non-sandboxed path
resolver **matches** the daemon, and the `AsyncStream` bridge **works** — #323 may
proceed as scoped, honoring the non-sandbox requirement and the graceful-degrade
requirement above.

## Related

- **Issues**: **#321** (this spike / ADR). Governs **#323** (`WatchTransport`, WI-2 —
  **open**). Consumes **#322** (the Swift `watch` decoder — **closed**), **#165** (the
  daemon `watch` subscription — **closed**), **#164** (the frozen versioned snapshot
  contract — **closed**). Part of **#168** (the menu-bar app). Rationale touchpoints:
  **#271** (no network egress), **#209** (the credential-capture boundary the spike
  stayed clear of), **#7** (the native-local, machine-global lock/socket path),
  **#171** (embed + version-lock the daemon in the `.app`).
- **Code / spike**: `apps/menubar/spikes/watch_spike.swift` + `stub_daemon.py` (the
  throwaway reference `WatchTransport` adapts — **not** in the app build graph;
  `project.yml` lists only `Sources`/`Tests`). `apps/menubar/Sources/WireModel.swift`
  (the reused #322 decoder). Daemon source of truth: `src/paths.rs`
  (`control_socket` / `support_dir` / `home_dir`), `src/daemon/socket.rs`
  (`serve_watch`, the un-auth-gated `watch` branch, the frame encoders),
  `src/daemon/peer_auth.rs` (the `getpeereid` gate on *state-affecting* commands only),
  `src/cli.rs` (`run` — instance lock, `require_roster` empty-state fail-fast, socket
  bind).
- **Prior art**: **ADR-0010** (socket-only boundary, no Rust FFI, non-MAS
  distribution) — this ADR fills in *how* the Swift side speaks that socket.
  **ADR-0002** (keychain via CLI, zero FFI) and **ADR-0004** (incidental `libc` kept
  raw) — the same "system primitive over a framework/binding" ethos, here extended to
  the client transport.
