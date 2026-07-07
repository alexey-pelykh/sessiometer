# menubar spikes — throwaway reference code

Code here is **throwaway spike output** (knowledge acquisition), **not** part of the
app. `project.yml` lists only `Sources` and `Tests` as build sources, so `xcodegen`
/ `xcodebuild` never compile anything under `spikes/` — it will not enter the app or
the CI `swift` job's build graph.

## `watch_spike.swift` + `stub_daemon.py` — issue #321

De-risks the Swift↔daemon **AF_UNIX transport** before WI-2 (**#323**,
`WatchTransport`). The decision it produced is durable in
[`docs/adr/0011-menubar-transport-raw-posix-af-unix.md`](../../../docs/adr/0011-menubar-transport-raw-posix-af-unix.md)
— **read the ADR first**; this code is the reference `WatchTransport` adapts.

It proves, on pure `Darwin`/`Foundation`/`os` Swift (no Network.framework, no Rust
FFI):

1. raw POSIX `socket(AF_UNIX, SOCK_STREAM)` / `connect()` to the daemon's control
   socket, `{"cmd":"watch"}\n` subscribe;
2. an `EINTR`- and partial-read-safe newline line-reader → decode via the **real**
   `Sources/WireModel.swift` (#322 decoder, reused verbatim);
3. bridging the blocking `read()` loop into an `AsyncStream` on a dedicated `Thread`;
4. socket-path resolution — `getpwuid(getuid())->pw_dir` == `NSHomeDirectory()` ==
   the daemon's `src/paths.rs::control_socket()` (the non-sandboxed invariant).

### Build & run (standalone `swiftc` — no Xcode needed)

```sh
# from the repo root
swiftc -O apps/menubar/spikes/watch_spike.swift apps/menubar/Sources/WireModel.swift \
  -o .tmp/spike-run/watch_spike

# 1) path-resolver cross-check only (no socket)
.tmp/spike-run/watch_spike --self-check

# 2) against the golden-fixture stub, forcing a partial read across two read()s
python3 apps/menubar/spikes/stub_daemon.py --socket "$PWD/.tmp/k.sock" --serve 2 --chunked &
.tmp/spike-run/watch_spike --socket "$PWD/.tmp/k.sock"

# 3) force + observe an EINTR retry (stub delays; a SIGALRM interrupts the read)
python3 apps/menubar/spikes/stub_daemon.py --socket "$PWD/.tmp/k.sock" --serve 2 --delay 0.2 &
.tmp/spike-run/watch_spike --socket "$PWD/.tmp/k.sock" --eintr

# 4) against the LIVE daemon (path derived by the resolver; redact real labels)
.tmp/spike-run/watch_spike --redact
```

`stub_daemon.py` stands in for `src/daemon/socket.rs::serve_watch`, vending the
**byte-exact** `snapshotBasic` / `heartbeatBasic` frames from
[`../Tests/Fixtures.swift`](../Tests/Fixtures.swift). It also documents how #323 can
test `WatchTransport` **without** a live daemon (and without touching any credential —
the #209 boundary). Use a scratch socket under the repo `.tmp/` — **never** `/tmp`.

### Findings (full rationale in ADR-0011)

- **Transport**: raw POSIX **confirmed** over `NWConnection` — ~180 lines, zero
  network egress, egress-provably socket-only.
- **Path**: the non-sandboxed app resolves the **same** native-local path the daemon
  binds; App Sandbox would diverge (`NSHomeDirectory()` → container) — the app **must
  stay non-sandboxed**.
- **⚠ Surprise**: the daemon build running at spike time **predated #164/#165** (no
  `watch`, no `schema_version`) — so #323 must treat `{"error":…}` / unknown-only
  streams as **watch-unavailable → degrade**, not hang. `watch` is **not** auth-gated.
