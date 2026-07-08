// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// THROWAWAY SPIKE (issue #321) — de-risks the Swift↔daemon AF_UNIX transport before
// WI-2 (#323, `WatchTransport`) is built. NOT part of the app build graph: it lives in
// `apps/menubar/spikes/`, which `project.yml` does NOT list as a source (only `Sources`
// and `Tests` are), so `xcodegen`/`xcodebuild` never compile it. It compiles STANDALONE:
//
//   swiftc -O apps/menubar/spikes/watch_spike.swift apps/menubar/Sources/WireModel.swift \
//     -o .tmp/spike-run/watch_spike
//
// Reusing `Sources/WireModel.swift` (the #322 decoder) verbatim — the whole point is to
// exercise the REAL decoder, not re-derive the contract (see ADR-0011).
//
// What it proves (the spike's questions):
//   1. Raw POSIX `socket(AF_UNIX, SOCK_STREAM)` / `connect()` from Swift's `Darwin` module
//      reaches the daemon's control socket (NOT Network.framework; NOT Rust FFI).
//   2. `{"cmd":"watch"}\n` subscribe → read + newline-split + decode ONE `snapshot` frame
//      via `parseWatchFrame` (WireModel.swift), with EINTR + partial-read handling.
//   3. Bridging a blocking `read()` loop (on a dedicated Thread) into an `AsyncStream`.
//   4. Socket-path resolution: the non-sandboxed app's `getpwuid(getuid())->pw_dir` +
//      `/Library/Application Support/sessiometer/daemon.sock` equals the daemon's
//      `src/paths.rs` `control_socket()`, and `NSHomeDirectory()` agrees.
//
// Usage:
//   watch_spike --self-check                 # path-resolver cross-check only (no socket)
//   watch_spike --socket <path> [--redact]   # connect, decode one frame, AsyncStream demo
//   watch_spike --socket <path> --eintr      # additionally force+observe an EINTR retry
//
// `--redact` prints only structural facts (type / schema / account count), never account
// content — used against the LIVE production daemon so real labels never hit the log.

import Foundation
import os

#if canImport(Darwin)
import Darwin
#endif

let log = Logger(subsystem: "org.sessiometer.menubar.spike", category: "watch")

/// Mirror `os_log` to stdout so a standalone (non-app-bundle) run captures evidence in the
/// terminal — the unified-log sink is invisible to a piped run. #323 uses `Logger` alone.
func note(_ message: String) {
    log.info("\(message, privacy: .public)")
    print(message)
    fflush(stdout)
}

func fail(_ message: String) -> Never {
    log.error("\(message, privacy: .public)")
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

// MARK: - 1/4  Socket-path resolution (the folded-in path cross-check)

/// Resolve the current user's home from the password database via `getpwuid(getuid())` —
/// the SAME source `src/paths.rs::home_dir()` uses (NOT `$HOME`, NOT XDG, NOT a sandbox
/// container). A non-sandboxed process gets the real home here.
func homeFromPasswdDB() -> String {
    let uid = getuid()
    guard let pw = getpwuid(uid), let dirPtr = pw.pointee.pw_dir else {
        fail("getpwuid(\(uid)) returned no home — cannot resolve the socket path")
    }
    return String(cString: dirPtr)
}

/// The daemon's `control_socket()` path, recomputed client-side: `<home>/Library/Application
/// Support/sessiometer/daemon.sock`. `home` is the passwd-DB home (matching the daemon).
func daemonSocketPath(home: String) -> String {
    (home as NSString)
        .appendingPathComponent("Library/Application Support/sessiometer/daemon.sock")
}

/// Cross-check the two home-resolution routes a non-sandboxed app has, and derive the socket
/// path. Prints the verdict; returns the resolved socket path. Under App Sandbox these two
/// DIVERGE (NSHomeDirectory → container), which is the load-bearing caveat for #323/#171.
@discardableResult
func pathSelfCheck() -> String {
    let passwdHome = homeFromPasswdDB()
    let nsHome = NSHomeDirectory()
    let socket = daemonSocketPath(home: passwdHome)

    note("path-check: getpwuid(getuid())->pw_dir = \(passwdHome)")
    note("path-check: NSHomeDirectory()          = \(nsHome)")
    note("path-check: derived control_socket      = \(socket)")

    if passwdHome == nsHome {
        note("path-check: ✅ MATCH — passwd-DB home == NSHomeDirectory() (non-sandboxed); "
            + "client resolves the daemon's native-local socket path")
    } else {
        note("path-check: ⚠️ DIVERGENCE — NSHomeDirectory() != passwd-DB home. This is the "
            + "App-Sandbox signature: the app would target a container, not the daemon's "
            + "native-local socket. #323/#171 MUST keep the app non-sandboxed.")
    }
    return socket
}

// MARK: - 2/4  Raw POSIX AF_UNIX client

/// `socket(AF_UNIX, SOCK_STREAM)` + `connect()` to `path`. Pure `Darwin` POSIX — no
/// Network.framework, no Rust FFI. Returns the connected fd or fails.
func connectUnix(_ path: String) -> Int32 {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    if fd < 0 { fail("socket(AF_UNIX): errno \(errno) (\(String(cString: strerror(errno))))") }

    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    let pathBytes = Array(path.utf8)
    // `sun_path` is a fixed 104-byte C array on Darwin; refuse an over-long path rather than
    // silently truncate to a wrong socket. (`MemoryLayout` gives the tuple's byte size.)
    let cap = MemoryLayout.size(ofValue: addr.sun_path)
    if pathBytes.count >= cap {
        fail("socket path too long (\(pathBytes.count) ≥ \(cap) bytes): \(path)")
    }
    withUnsafeMutablePointer(to: &addr.sun_path) { tuplePtr in
        tuplePtr.withMemoryRebound(to: CChar.self, capacity: cap) { dst in
            for (i, b) in pathBytes.enumerated() { dst[i] = CChar(bitPattern: b) }
            dst[pathBytes.count] = 0
        }
    }

    let len = socklen_t(MemoryLayout<sockaddr_un>.size)
    let rc = withUnsafePointer(to: &addr) { aptr in
        aptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sptr in
            // EINTR-safe connect.
            var r: Int32 = -1
            repeat { r = connect(fd, sptr, len) } while r < 0 && errno == EINTR
            return r
        }
    }
    if rc < 0 {
        let e = errno
        close(fd)
        fail("connect(\(path)): errno \(e) (\(String(cString: strerror(e)))) — "
            + "is the daemon running and is this the right path?")
    }
    return fd
}

/// Write `bytes` in full, looping over partial writes and retrying `EINTR`.
func sendAll(_ fd: Int32, _ bytes: [UInt8]) {
    var off = 0
    bytes.withUnsafeBytes { raw in
        let base = raw.baseAddress!
        while off < bytes.count {
            let n = write(fd, base + off, bytes.count - off)
            if n < 0 {
                if errno == EINTR { continue }
                fail("write: errno \(errno) (\(String(cString: strerror(errno))))")
            }
            off += n
        }
    }
}

/// Newline-delimited line reader over a blocking fd. The reusable core WI-2 adapts: one
/// `read()` loop that (a) retries `EINTR`, (b) accumulates PARTIAL reads until a `\n`
/// boundary, (c) hands back exactly one line at a time. `eintrRetries` counts observed
/// EINTR retries so the spike can PROVE the branch was taken.
final class LineReader {
    private let fd: Int32
    private var buffer = [UInt8]()
    private var chunk = [UInt8](repeating: 0, count: 4096)
    private(set) var eintrRetries = 0
    private(set) var reads = 0

    init(_ fd: Int32) { self.fd = fd }

    /// Next complete line (without the trailing `\n`), or nil at EOF. Blocks until a full
    /// line is available. EINTR is retried transparently; a short read is accumulated.
    func nextLine() -> String? {
        while true {
            if let nl = buffer.firstIndex(of: 0x0A) {
                let lineBytes = Array(buffer[0..<nl])
                buffer.removeSubrange(0...nl)
                return String(decoding: lineBytes, as: UTF8.self)
            }
            let n = chunk.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
            if n < 0 {
                if errno == EINTR { eintrRetries += 1; continue }   // (a) EINTR-safe
                // Any other read error ends the subscription (return nil), the model a real
                // watch client uses — reconnect logic lives ABOVE the reader. A peer reset is
                // logged; EBADF is our own teardown close() racing an in-flight read, so quiet.
                if errno != EBADF {
                    let e = String(cString: strerror(errno))
                    log.error("read: errno \(errno) (\(e, privacy: .public)) — ending stream")
                }
                return nil
            }
            if n == 0 {                                             // EOF
                if buffer.isEmpty { return nil }
                let rest = String(decoding: buffer, as: UTF8.self)
                buffer.removeAll()
                return rest
            }
            reads += 1
            buffer.append(contentsOf: chunk[0..<n])                 // (b) accumulate partials
        }
    }
}

// MARK: - 3/4  Blocking read → AsyncStream bridge (the pattern WI-2 adopts)

/// Bridge the blocking `LineReader` into an `AsyncStream<WatchFrame>`. A DEDICATED Thread
/// runs the blocking `read()` loop (NOT a `Task` on the cooperative pool — a blocking syscall
/// would starve a shared pool thread); each decoded frame is `yield`ed to the async consumer.
/// `onTermination` closes the fd, which unblocks the `read()` so the Thread exits. This is the
/// recommended shape for `WatchTransport` (#323).
func watchFrames(fd: Int32) -> AsyncStream<WatchFrame> {
    AsyncStream { continuation in
        let reader = LineReader(fd)
        let thread = Thread {
            while let line = reader.nextLine() {
                if line.isEmpty { continue }
                do {
                    continuation.yield(try parseWatchFrame(line))
                } catch {
                    // A malformed line is a hard error per the contract; end the stream.
                    log.error("watch: decode failed: \(String(describing: error), privacy: .public)")
                    break
                }
            }
            continuation.finish()
        }
        thread.name = "sessiometer.watch.spike"
        thread.stackSize = 512 * 1024
        continuation.onTermination = { _ in close(fd) }   // unblocks read() → Thread exits
        thread.start()
    }
}

// MARK: - Frame logging (redaction-aware)

func describe(_ frame: WatchFrame, redact: Bool) -> String {
    switch frame {
    case .snapshot(let s):
        let head = "snapshot: schema \(s.schemaVersion.major).\(s.schemaVersion.minor) "
            + "(supported=\(s.isSchemaSupported)) generated_at=\(s.generatedAt) "
            + "accounts=\(s.accounts.count) next_swap=\(s.nextSwap.map { "\($0)" } ?? "nil")"
        if redact { return head + " [content redacted]" }
        let labels = s.accounts.map { $0.label }.joined(separator: ",")
        return head + " labels=[\(labels)]"
    case .heartbeat(let generatedAt, let v):
        return "heartbeat: schema \(v.major).\(v.minor) generated_at=\(generatedAt)"
    case .unknown:
        return "unknown frame (ignored)"
    }
}

// MARK: - Entry

// `@main` (not top-level code) because the spike is compiled ALONGSIDE `WireModel.swift`;
// Swift forbids top-level statements outside a lone `main.swift`.
@main
enum Spike {
    static func main() async {
        let args = CommandLine.arguments
        func arg(_ name: String) -> String? {
            guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
            return args[i + 1]
        }
        let selfCheckOnly = args.contains("--self-check")
        let redact = args.contains("--redact")
        let eintrDemo = args.contains("--eintr")

        note("=== watch_spike (#321) — macOS "
            + "\(ProcessInfo.processInfo.operatingSystemVersionString) ===")

        // (4/4) Path cross-check — always runs.
        let derivedSocket = pathSelfCheck()

        if selfCheckOnly {
            note("self-check only — done.")
            exit(0)
        }

        let socketPath = arg("--socket") ?? derivedSocket
        note("connecting: \(socketPath)")

        // (1) raw POSIX connect + (2) subscribe.
        let fd = connectUnix(socketPath)
        note("connected: fd \(fd) — sending {\"cmd\":\"watch\"}")
        sendAll(fd, Array("{\"cmd\":\"watch\"}\n".utf8))

        if eintrDemo {
            // Force an EINTR during a blocking read: a `sigaction` handler WITHOUT SA_RESTART
            // means the pending `read()` returns -1/EINTR rather than auto-restarting;
            // `LineReader` retries.
            var sa = sigaction()
            sa.__sigaction_u.__sa_handler = { _ in }   // no-op; flags omit SA_RESTART
            sigemptyset(&sa.sa_mask)
            sa.sa_flags = 0
            sigaction(SIGALRM, &sa, nil)
            var it = itimerval(it_interval: timeval(tv_sec: 0, tv_usec: 0),
                               it_value: timeval(tv_sec: 0, tv_usec: 60_000))   // 60ms
            setitimer(ITIMER_REAL, &it, nil)
            note("eintr-demo: armed SIGALRM (60ms, no SA_RESTART) before the blocking read")
        }

        // (2) synchronous first-frame decode via the EINTR/partial-read-safe reader.
        let syncReader = LineReader(fd)
        guard let firstLine = syncReader.nextLine() else { fail("EOF before any frame") }
        let firstFrame: WatchFrame
        do {
            firstFrame = try parseWatchFrame(firstLine)
        } catch {
            fail("decode failed on frame#1: \(error)")
        }
        note("frame#1 (sync): \(describe(firstFrame, redact: redact))")
        note("reader stats: reads=\(syncReader.reads) eintrRetries=\(syncReader.eintrRetries)")
        if eintrDemo {
            if syncReader.eintrRetries > 0 {
                note("eintr-demo: ✅ observed \(syncReader.eintrRetries) EINTR retr(y/ies) — "
                    + "the read loop survived a signal and still decoded the frame")
            } else {
                note("eintr-demo: (no EINTR observed — the peer answered before the 60ms "
                    + "alarm; the retry branch is still correct-by-construction)")
            }
        }
        close(fd)   // done with the synchronous connection

        // (3) AsyncStream bridge demo on a fresh connection.
        note("--- AsyncStream bridge: fresh connection, consume 1 frame via `for await` ---")
        let fd2 = connectUnix(socketPath)
        sendAll(fd2, Array("{\"cmd\":\"watch\"}\n".utf8))

        // A detached watchdog force-exits if a wedged peer never sends — the async demo
        // itself is `await`ed directly (async `main()` drives the concurrency runtime).
        let watchdog = Task.detached {
            do {
                try await Task.sleep(nanoseconds: 5_000_000_000)
            } catch {
                return   // cancelled — the demo finished in time, so do NOT fail
            }
            fail("AsyncStream demo timed out after 5s")
        }
        var count = 0
        for await frame in watchFrames(fd: fd2) {
            count += 1
            note("frame#\(count) (async): \(describe(frame, redact: redact))")
            if count >= 1 { break }   // stream `onTermination` closes fd2 → Thread exits
        }
        watchdog.cancel()
        note("AsyncStream bridge: ✅ consumed \(count) frame(s) via for-await; torn down")
        note("=== spike complete ===")
    }
}
