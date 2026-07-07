// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Real-fd tests for the PRODUCTION `PosixSocketConnection` (issue #323): the fake-based
// `WatchTransportTests` prove the actor SHELL's wiring, but the dedicated-thread `read()` loop, the
// newline framing, partial-read accumulation, EOF handling, and idempotent teardown are raw POSIX I/O
// that a fake never exercises. `socketpair(AF_UNIX, SOCK_STREAM)` gives a kernel-backed connected fd
// PAIR with no filesystem socket and no daemon — so the real reader runs against a real socket while
// the test drives the peer end directly. This is the minimal real-socket coverage #323 needs; the
// full mock-socket server (#328) is a separate item.

import XCTest
import os
#if canImport(Darwin)
import Darwin
#endif

final class PosixSocketConnectionTests: XCTestCase {

    // MARK: - Framing (real read() loop)

    // The reader splits the byte stream on `\n` (trailing newline stripped) AND accumulates PARTIAL
    // reads to a boundary: "gamma" arrives as two writes yet surfaces as one line.
    func testReadsNewlineDelimitedLinesIncludingAcrossPartialWrites() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        writeBytes(pair.peer, "alpha\nbeta\n")            // two complete lines in one write
        try await XCTAssertNextLine(collector, "alpha")
        try await XCTAssertNextLine(collector, "beta")

        writeBytes(pair.peer, "gam")                      // a line split across two writes …
        writeBytes(pair.peer, "ma\n")                     // … proves partial-read accumulation
        try await XCTAssertNextLine(collector, "gamma")

        connection.close()
        Darwin.close(pair.peer)
    }

    // Blank lines are surfaced VERBATIM at this (framing) layer — skipping them is the `WatchTransport`
    // shell's contract, not the reader's. Pinning it here keeps the empty-line skip from drifting back
    // down into `PosixSocketConnection` (where it lived before, and where it would starve the shell of
    // the information it needs to decide).
    func testEmptyLinesArePassedThroughAtTheFramingLayer() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        writeBytes(pair.peer, "\nafter\n")                // leading bare "\n" is an empty line
        try await XCTAssertNextLine(collector, "", "empty line surfaced, not swallowed")
        try await XCTAssertNextLine(collector, "after")

        connection.close()
        Darwin.close(pair.peer)
    }

    // MARK: - EOF

    // Peer close → `read()` returns 0 → the stream FINISHES (the shell reads that as "connection
    // closed" and reconnects).
    func testPeerCloseEndsTheStream() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        writeBytes(pair.peer, "only\n")
        try await XCTAssertNextLine(collector, "only")

        Darwin.close(pair.peer)                           // EOF
        let end = try await nextLine(collector)
        XCTAssertNil(end, "stream finishes on EOF")

        connection.close()
    }

    // A trailing line with NO newline is not lost: EOF flushes the accumulated buffer as a final line,
    // then the stream finishes.
    func testTrailingUnterminatedBytesAreDeliveredAtEof() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        writeBytes(pair.peer, "no-trailing-newline")      // no "\n"
        Darwin.close(pair.peer)                           // EOF flushes the partial line

        try await XCTAssertNextLine(collector, "no-trailing-newline")
        let end = try await nextLine(collector)
        XCTAssertNil(end, "then finishes")

        connection.close()
    }

    // MARK: - Write path

    // `send()` writes the exact bytes to the peer (the single `{"cmd":"watch"}\n` subscribe). Proven
    // against a real socket, not a fake's array.
    func testSendWritesTheSubscribeBytesToThePeer() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        let subscribe = Array(#"{"cmd":"watch"}"#.utf8) + [0x0A]
        try connection.send(subscribe)
        XCTAssertEqual(readAvailable(pair.peer), #"{"cmd":"watch"}"# + "\n")

        connection.close()
        Darwin.close(pair.peer)
    }

    // MARK: - Teardown

    // `close()` shuts the fd, which unblocks the blocked `read()` so the reader thread exits and the
    // stream finishes (ADR-0011 §4 teardown mechanism). It is idempotent: a second call is a safe
    // no-op. The 5 s timeout in `nextLine` converts a teardown HANG into a clear failure rather than a
    // stuck suite.
    func testCloseFinishesStreamAndIsIdempotent() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        connection.close()
        connection.close()                                // idempotent — must not crash or double-close

        let end = try await nextLine(collector)
        XCTAssertNil(end, "close() finishes the stream")

        Darwin.close(pair.peer)
    }

    // MARK: - Fixtures

    private struct SocketPairError: Error { let errnoValue: Int32 }

    /// A connected AF_UNIX stream fd pair (no filesystem socket, no daemon). `conn` is wrapped by the
    /// production `PosixSocketConnection`; `peer` is the test's end to write / close directly.
    private func makeSocketPair() throws -> (conn: Int32, peer: Int32) {
        var fds: [Int32] = [-1, -1]
        guard socketpair(AF_UNIX, SOCK_STREAM, 0, &fds) == 0 else { throw SocketPairError(errnoValue: errno) }
        return (fds[0], fds[1])
    }

    @discardableResult
    private func writeBytes(_ fd: Int32, _ string: String) -> Int {
        let bytes = Array(string.utf8)
        return bytes.withUnsafeBytes { Darwin.write(fd, $0.baseAddress, $0.count) }
    }

    /// One blocking read of whatever is currently buffered on `fd`, decoded as UTF-8. Safe here because
    /// every call site reads AFTER a synchronous `send()` has already put the bytes in the socket
    /// buffer, so the read returns immediately.
    private func readAvailable(_ fd: Int32, max: Int = 4096) -> String {
        var buffer = [UInt8](repeating: 0, count: max)
        let n = buffer.withUnsafeMutableBytes { Darwin.read(fd, $0.baseAddress, $0.count) }
        guard n > 0 else { return "" }
        return String(decoding: buffer[0..<n], as: UTF8.self)
    }

    // MARK: - Stream awaiting helpers (timeout-guarded so a wiring bug fails instead of hanging)

    private enum WaitError: Error { case timeout }

    private func nextLine(_ collector: LineCollector, timeout: Duration = .seconds(5)) async throws -> String? {
        try await withThrowingTaskGroup(of: String?.self) { group in
            group.addTask { await collector.next() }
            group.addTask { try await Task.sleep(for: timeout); throw WaitError.timeout }
            let result = try await group.next()!
            group.cancelAll()
            return result
        }
    }

    private func XCTAssertNextLine(
        _ collector: LineCollector, _ expected: String, _ message: String = "",
        timeout: Duration = .seconds(5), file: StaticString = #filePath, line: UInt = #line
    ) async throws {
        let value = try await nextLine(collector, timeout: timeout)
        XCTAssertEqual(value, expected, message, file: file, line: line)
    }
}

// MARK: - Line collector

/// Consumes an `AsyncStream<String>` into a queue and hands lines out one at a time via `next()`
/// (`nil` = the stream finished) — so a test can assert an ordered sequence without arbitrary sleeps.
/// Mirrors `WatchTransportTests`' `EventRecorder`, specialized to `String`.
final class LineCollector: @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State {
        var buffer: [String] = []
        var waiter: CheckedContinuation<String?, Never>?
        var finished = false
    }

    func consume(_ stream: AsyncStream<String>) {
        Task { [weak self] in
            for await line in stream { self?.push(line) }
            self?.finish()
        }
    }

    private func push(_ line: String) {
        let waiter: CheckedContinuation<String?, Never>? = state.withLock { st in
            if let w = st.waiter { st.waiter = nil; return w }
            st.buffer.append(line)
            return nil
        }
        waiter?.resume(returning: line)
    }

    private func finish() {
        let waiter: CheckedContinuation<String?, Never>? = state.withLock { st in
            st.finished = true
            let w = st.waiter; st.waiter = nil; return w
        }
        waiter?.resume(returning: nil)
    }

    func next() async -> String? {
        await withCheckedContinuation { continuation in
            let immediate: String?? = state.withLock { st -> String?? in
                if !st.buffer.isEmpty { return .some(st.buffer.removeFirst()) }
                if st.finished { return .some(nil) }
                st.waiter = continuation
                return nil
            }
            if let value = immediate { continuation.resume(returning: value) }
        }
    }
}
