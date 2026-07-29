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

private let transportLog = Logger(subsystem: "org.sessiometer.menubar", category: "watch-transport")

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
    ///
    /// Sending AFTER `close()` is not a supported call; how it fails is the implementation's business
    /// (for the POSIX one, see `PosixSocketConnection.send` — issue #859 changed it).
    func send(_ bytes: [UInt8]) throws
    /// Idempotently tear down: disconnect the socket, which unblocks the blocked reader so `lines`
    /// finishes. The descriptor itself is retired by the reader once it has stopped (issue #859).
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

/// Sole owner of a connection's file descriptor (issue #859).
///
/// The descriptor NUMBER may only be released once nothing can still `read()` it. `close()` on a
/// live fd does not give that guarantee: it frees the number immediately, and a reader thread that
/// has not yet entered its `read()` then issues that syscall against whatever the process allocated
/// the number to next — silently consuming another socket's bytes. That is not hypothetical here:
/// `WatchTransport` tears a connection down and reconnects (`WatchTransport.swift` — close, then
/// `connector.connect()`), and `socket()` hands back the lowest free number, which is the one just
/// released. Nor is `close()` a reliable wake: on Darwin a `read()` already in flight holds its own
/// reference to the file, so closing the descriptor beneath it need not return at all.
///
/// So teardown is split in two. `shutdown()` disconnects the socket — waking a blocked `read()`
/// with EOF and making every later one return EOF too — while KEEPING the number reserved, so a
/// late reader can only ever re-read this dead socket. `Darwin.close()` runs exactly once, from the
/// reader itself, after it has stopped reading. Both syscalls run under the lock so they cannot be
/// reordered against each other: a `shutdown()` can never land on a number already retired (and
/// therefore possibly reissued).
///
/// The tradeoff this accepts is that release is LATER and has a single owner. Later: measured across
/// 500 reconnects, the peak descriptor count was 13 against a baseline of 3, settling back to 3 with
/// none leaked — the reader only has to wake and unwind. Single owner: nothing but the reader ever
/// releases the number, so a reader thread that never ran at all would never release it. That needs
/// `Thread.start()` itself to have failed, i.e. process-level resource exhaustion, which the
/// measurement above does not and cannot speak to.
private final class FileDescriptorOwner: @unchecked Sendable {
    /// Valid until `retire()`; only the reader thread may act on it after teardown begins.
    let raw: Int32
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State {
        var disconnected = false
        var retired = false
    }

    init(_ raw: Int32) { self.raw = raw }

    /// Wake and permanently silence the socket without releasing its number. Idempotent, and a
    /// no-op once retired.
    func disconnect() {
        state.withLock { st in
            guard !st.retired, !st.disconnected else { return }
            st.disconnected = true
            _ = Darwin.shutdown(raw, SHUT_RDWR)
        }
    }

    /// Release the number. Called ONLY by the reader thread, and only after its loop has ended.
    func retire() {
        state.withLock { st in
            guard !st.retired else { return }
            st.retired = true
            Darwin.close(raw)
        }
    }
}

/// A live POSIX UDS connection. `lines` bridges a blocking `read()` loop on a DEDICATED `Thread`
/// (per ADR-0011 §4 — a blocking syscall on the cooperative pool would starve a shared thread) into
/// an `AsyncStream<String>`; `close()` disconnects the socket, which unblocks the reader so its
/// stream finishes and it then retires the descriptor. `@unchecked Sendable` is justified: every
/// stored property is immutable or the `os` lock; descriptor lifetime is serialized by
/// `FileDescriptorOwner`; the reader thread only yields to the Sendable continuation.
final class PosixSocketConnection: WatchConnection, @unchecked Sendable {
    private let descriptor: FileDescriptorOwner
    let lines: AsyncStream<String>
    private let linesContinuation: AsyncStream<String>.Continuation

    init(fd: Int32) {
        self.descriptor = FileDescriptorOwner(fd)
        (self.lines, self.linesContinuation) = AsyncStream<String>.makeStream()
        startReader()
    }

    // Backstop: a connection dropped WITHOUT an explicit `close()` (e.g. a subscribe-write failure
    // that discards it before attach) still disconnects its socket, which unblocks the reader thread
    // so it exits and retires the descriptor — the reader holds the owner directly, so this stays
    // leak-free even though `self` is already gone. `close()` is idempotent, so the normal
    // explicit-teardown path is unaffected.
    deinit { close() }

    private func startReader() {
        // Held STRONGLY by the thread (not via `self`, which the deinit backstop above may outlive),
        // so the descriptor is always retired by whoever actually stops reading it.
        let descriptor = self.descriptor
        let continuation = self.linesContinuation
        let thread = Thread {
            let reader = LineReader(descriptor.raw)
            while let line = reader.nextLine() {
                continuation.yield(line)   // blank-line filtering is the transport's contract (WatchTransport)
            }
            continuation.finish()   // EOF or read error → the stream ends
            descriptor.retire()     // nothing reads this fd again → the number is safe to release
        }
        thread.name = "org.sessiometer.menubar.watch-reader"
        thread.stackSize = 512 * 1024
        // When the stream terminates (finished, OR the consumer stops / cancels), disconnect the
        // socket — which unblocks a pending read() so the reader Thread exits.
        continuation.onTermination = { [weak self] _ in self?.close() }
        thread.start()
    }

    /// Since issue #859 a send AFTER `close()` fails differently: the descriptor is shut down rather
    /// than closed, so the write reports EPIPE where it used to report EBADF — and on a socket
    /// WITHOUT `SO_NOSIGPIPE` it raises SIGPIPE, terminating the process instead of throwing. Every
    /// production socket comes from `PosixSocketConnector.connect()`, which sets the option; anything
    /// else handing an fd to this type (a test's `socketpair`) must set it too.
    func send(_ bytes: [UInt8]) throws {
        var off = 0
        try bytes.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            while off < bytes.count {
                let n = Darwin.write(descriptor.raw, base + off, bytes.count - off)
                if n < 0 {
                    if errno == EINTR { continue }              // EINTR-safe
                    throw TransportError.write(errnoString())
                }
                off += n                                        // loop over partial writes
            }
        }
    }

    func close() { descriptor.disconnect() }
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
                // EBADF stays quiet, but should now be unreachable: since issue #859 teardown
                // `shutdown()`s the socket (read → EOF) and only THIS thread closes the descriptor,
                // after it has stopped reading — so no close can land under an in-flight read. Kept
                // as a defensive guard. Any other read error is logged, then ends the stream.
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
