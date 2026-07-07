// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The socket-I/O seam for `WatchTransport` (issue #323) plus its production raw-POSIX AF_UNIX
// implementation, adapting the proven #321 spike (`apps/menubar/spikes/watch_spike.swift`).
//
// Putting connect / read / write behind the `WatchConnector` / `WatchConnection` protocols is what
// lets `WatchTransport` be driven by an in-process fake in tests — no real socket, and no need for
// #328's full mock-socket harness (a separate item). Production is raw POSIX via Swift's `Darwin`
// module — NOT Network.framework, NOT Rust FFI (ADR-0011) — so the app pulls in no networking stack
// and has ZERO network egress by construction: a `PosixSocketConnector` can only reach a same-user
// local Unix-domain socket.

import Foundation
import os
#if canImport(Darwin)
import Darwin
#endif

private let transportLog = Logger(subsystem: "com.sessiometer.menubar", category: "watch-transport")

// MARK: - The seam

/// A live connection to the daemon's `watch` stream. Sendable so the actor can hold it and hand it
/// across task boundaries; implementations synchronize their own teardown.
protocol WatchConnection: Sendable {
    /// Newline-delimited lines from the stream (trailing `\n` stripped), consumed with `for await`.
    /// The stream FINISHES on EOF or a read error — which the shell reads as "connection closed".
    var lines: AsyncStream<String> { get }
    /// Write raw bytes (the `{"cmd":"watch"}\n` subscribe). Throws on a write failure, which the
    /// shell treats as a failed connect (the only write is the subscribe — `watch` is push-only
    /// thereafter, so the transport never writes again).
    func send(_ bytes: [UInt8]) throws
    /// Idempotently tear down: close the fd, which unblocks the blocked reader so `lines` finishes.
    func close()
}

/// Establishes `WatchConnection`s. Sendable so the actor can retain it across reconnects.
protocol WatchConnector: Sendable {
    /// Open a connection (may block briefly). Throws on failure (daemon absent, path too long).
    func connect() throws -> WatchConnection
}

/// A transport-layer failure, carrying a redaction-free reason for `os_log` / the UI (a socket error
/// string is not a secret — `watch` is unauthenticated and carries only redacted status).
enum TransportError: Error, Equatable {
    case socket(String)
    case connect(String)
    case write(String)
    case pathTooLong(bytes: Int, cap: Int)

    /// A human-readable one-liner for `.disconnected(reason:)`.
    var reason: String {
        switch self {
        case .socket(let e): return "socket(AF_UNIX) failed: \(e)"
        case .connect(let e): return "connect failed: \(e) — is the daemon running?"
        case .write(let e): return "subscribe write failed: \(e)"
        case .pathTooLong(let bytes, let cap): return "socket path too long (\(bytes) ≥ \(cap) bytes)"
        }
    }
}

// MARK: - Production: raw POSIX AF_UNIX

/// The production connector: `socket(AF_UNIX, SOCK_STREAM)` + `connect()` via `Darwin` (ADR-0011).
struct PosixSocketConnector: WatchConnector {
    /// The daemon control-socket path, resolved by `SocketPathResolver`.
    let path: String

    func connect() throws -> WatchConnection {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        if fd < 0 { throw TransportError.socket(errnoString()) }

        // Deliver a broken-pipe write as EPIPE (caught → `.disconnected` + backoff) rather than a
        // process-terminating SIGPIPE: if the daemon closes between connect() and the subscribe
        // write, a plain write() to a peer with no read end would raise SIGPIPE, whose default
        // disposition terminates the process. Darwin has no MSG_NOSIGNAL, so guard the fd itself.
        var noSigPipe: Int32 = 1
        _ = setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size))

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(path.utf8)
        // `sun_path` is a fixed 104-byte C array on Darwin; refuse an over-long path rather than
        // silently truncate to a WRONG socket. `MemoryLayout` gives the tuple's byte size.
        let cap = MemoryLayout.size(ofValue: addr.sun_path)
        if pathBytes.count >= cap {
            Darwin.close(fd)
            throw TransportError.pathTooLong(bytes: pathBytes.count, cap: cap)
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
                var r: Int32 = -1
                repeat { r = Darwin.connect(fd, sptr, len) } while r < 0 && errno == EINTR  // EINTR-safe
                return r
            }
        }
        if rc < 0 {
            let e = errnoString()   // capture errno BEFORE close() can overwrite it
            Darwin.close(fd)
            throw TransportError.connect(e)
        }
        return PosixSocketConnection(fd: fd)
    }
}

/// A live POSIX UDS connection. `lines` bridges a blocking `read()` loop on a DEDICATED `Thread`
/// (per ADR-0011 §4 — a blocking syscall on the cooperative pool would starve a shared thread) into
/// an `AsyncStream<String>`; `close()` shuts the fd (exactly once), which unblocks the reader so its
/// stream finishes. `@unchecked Sendable` is justified: every stored property is immutable or the
/// `os` lock; the fd is closed at most once under that lock; the reader thread only yields to the
/// Sendable continuation.
final class PosixSocketConnection: WatchConnection, @unchecked Sendable {
    private let fd: Int32
    private let hasClosed = OSAllocatedUnfairLock(initialState: false)
    let lines: AsyncStream<String>
    private let linesContinuation: AsyncStream<String>.Continuation

    init(fd: Int32) {
        self.fd = fd
        (self.lines, self.linesContinuation) = AsyncStream<String>.makeStream()
        startReader()
    }

    // Backstop: a connection dropped WITHOUT an explicit `close()` (e.g. a subscribe-write failure
    // that discards it before attach) still closes its fd, which unblocks the reader thread so it
    // exits. `close()` is idempotent, so the normal explicit-teardown path is unaffected.
    deinit { close() }

    private func startReader() {
        let fd = self.fd
        let continuation = self.linesContinuation
        let thread = Thread {
            let reader = LineReader(fd)
            while let line = reader.nextLine() {
                continuation.yield(line)   // blank-line filtering is the transport's contract (WatchTransport)
            }
            continuation.finish()   // EOF or read error → the stream ends
        }
        thread.name = "com.sessiometer.menubar.watch-reader"
        thread.stackSize = 512 * 1024
        // When the stream terminates (finished, OR the consumer stops / cancels), close the fd —
        // which unblocks a pending read() so the reader Thread exits.
        continuation.onTermination = { [weak self] _ in self?.close() }
        thread.start()
    }

    func send(_ bytes: [UInt8]) throws {
        var off = 0
        try bytes.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            while off < bytes.count {
                let n = Darwin.write(fd, base + off, bytes.count - off)
                if n < 0 {
                    if errno == EINTR { continue }              // EINTR-safe
                    throw TransportError.write(errnoString())
                }
                off += n                                        // loop over partial writes
            }
        }
    }

    func close() {
        let shouldClose: Bool = hasClosed.withLock { closed in
            if closed { return false }
            closed = true
            return true
        }
        if shouldClose { Darwin.close(fd) }
    }
}

/// Newline-delimited line reader over a blocking fd (adapted from the #321 spike): it (a) retries
/// `EINTR`, (b) accumulates PARTIAL reads to a `\n` boundary, (c) hands back one line at a time
/// (trailing `\n` stripped), and returns `nil` at EOF or on any non-EINTR read error — the "any I/O
/// error just ends the stream" model the daemon's `serve_watch` uses on its side; reconnect lives
/// ABOVE, in `WatchStateMachine`.
private final class LineReader {
    private let fd: Int32
    private var buffer = [UInt8]()
    private var chunk = [UInt8](repeating: 0, count: 4096)

    /// Cap on a single unterminated line, far above any real snapshot (even a many-account frame is a
    /// few KB) — a guard so a buggy / hostile daemon streaming bytes with NO newline cannot grow
    /// client memory without bound. The daemon caps its own control line (MAX_CONTROL_LINE_BYTES);
    /// this is the client-side counterpart for the `watch` read path.
    private static let maxLineBytes = 1 << 20   // 1 MiB

    init(_ fd: Int32) { self.fd = fd }

    func nextLine() -> String? {
        while true {
            if let nl = buffer.firstIndex(of: 0x0A) {
                let lineBytes = Array(buffer[0..<nl])
                buffer.removeSubrange(0...nl)
                return String(decoding: lineBytes, as: UTF8.self)
            }
            if buffer.count > Self.maxLineBytes {               // runaway no-newline stream → end it
                transportLog.error(
                    "watch read: line exceeded \(Self.maxLineBytes) bytes with no newline — ending stream")
                return nil
            }
            let n = chunk.withUnsafeMutableBytes { Darwin.read(fd, $0.baseAddress, $0.count) }
            if n < 0 {
                if errno == EINTR { continue }                  // (a) EINTR-safe
                // EBADF is our own teardown close() racing an in-flight read — quiet. Any other read
                // error is logged, then ends the stream.
                let err = errno
                if err != EBADF {
                    transportLog.error(
                        "watch read: errno \(err) (\(errnoDescription(err), privacy: .public)) — ending stream")
                }
                return nil
            }
            if n == 0 {                                         // EOF
                if buffer.isEmpty { return nil }
                let rest = String(decoding: buffer, as: UTF8.self)
                buffer.removeAll()
                return rest
            }
            buffer.append(contentsOf: chunk[0..<n])             // (b) accumulate partials
        }
    }
}

/// The current `errno` as an "errno N (message)" string. Call IMMEDIATELY after the failing syscall,
/// before any other libc call can overwrite `errno`.
private func errnoString() -> String {
    let err = errno
    return "errno \(err) (\(errnoDescription(err)))"
}

/// A thread-safe description of `err`. Uses `strerror_r` (into a local buffer) rather than plain
/// `strerror`, whose shared static buffer could race between the reader thread and the connect task.
private func errnoDescription(_ err: Int32) -> String {
    var buffer = [CChar](repeating: 0, count: 256)
    guard strerror_r(err, &buffer, buffer.count) == 0 else { return "unknown error" }
    return String(cString: buffer)
}
