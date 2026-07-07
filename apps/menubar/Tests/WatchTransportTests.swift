// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Actor-shell wiring tests for `WatchTransport` (issue #323): they drive the actor with an in-process
// fake `WatchConnector` / `WatchConnection` — NO real socket, independent of #328's mock-socket
// harness — and assert the observable `AsyncStream<TransportEvent>` output for the connect / line /
// reconnect / liveness paths. The exact transition + backoff-schedule LOGIC is proven deterministically
// in `WatchStateMachineTests`; here we prove the effects are actually performed and marshalled to the
// event stream, including the two ADR-0011 design inputs (graceful-degrade, and never hanging on a
// first-snapshot precondition).

import XCTest
import os

final class WatchTransportTests: XCTestCase {

    // Tiny timings so real-timer paths (liveness, backoff) run in well under a second.
    private let fastBackoff = ExponentialBackoff(base: .milliseconds(20), multiplier: 2, cap: .milliseconds(80))
    private let fastWindow = Duration.milliseconds(150)

    // MARK: - Connect + push-only

    func testConnectEmitsConnectedAndSendsSubscribeExactlyOnce() async throws {
        let conn = FakeConnection()
        let connector = FakeConnector([.succeed(conn)])
        let transport = WatchTransport(connector: connector, backoff: fastBackoff, livenessWindow: fastWindow)
        let recorder = EventRecorder(); recorder.consume(transport.events)

        await transport.start()

        try await expect(recorder, .connected)
        // Push-only: the ONLY bytes ever written are the single subscribe command.
        XCTAssertEqual(conn.sentStrings, [#"{"cmd":"watch"}"# + "\n"])

        await transport.stop()
    }

    func testReceivedLinesAreSurfacedVerbatimAndPushOnly() async throws {
        let conn = FakeConnection()
        let transport = WatchTransport(connector: FakeConnector([.succeed(conn)]),
                                       backoff: fastBackoff, livenessWindow: .seconds(30))
        let recorder = EventRecorder(); recorder.consume(transport.events)
        await transport.start()
        try await expect(recorder, .connected)

        conn.emit(#"{"type":"snapshot"}"#)
        conn.emit("")                                  // empty lines are skipped, never surfaced
        conn.emit(#"{"type":"heartbeat"}"#)

        try await expect(recorder, .line(#"{"type":"snapshot"}"#))
        try await expect(recorder, .line(#"{"type":"heartbeat"}"#))
        // Still push-only after receiving frames — no client polling write occurred.
        XCTAssertEqual(conn.sentStrings.count, 1)

        await transport.stop()
    }

    // MARK: - Disconnect + reconnect

    // AC: EOF → `.disconnected` + an automatic reconnect (bounded backoff). The second connection
    // succeeds and re-emits `.connected`; the connector was asked to connect twice.
    func testEofDisconnectsThenReconnects() async throws {
        let first = FakeConnection(), second = FakeConnection()
        let connector = FakeConnector([.succeed(first), .succeed(second)])
        let transport = WatchTransport(connector: connector, backoff: fastBackoff, livenessWindow: .seconds(30))
        let recorder = EventRecorder(); recorder.consume(transport.events)

        await transport.start()
        try await expect(recorder, .connected)

        first.eof()                                    // peer closed → stream ends
        try await expectDisconnected(recorder)
        try await expect(recorder, .connected, "should reconnect after backoff")
        XCTAssertEqual(connector.connectCount, 2)

        await transport.stop()
    }

    // AC: daemon absent (connect fails) → `.disconnected` + bounded-backoff retry, NEVER a tight
    // loop. Two failures each surface `.disconnected`; the third attempt succeeds.
    func testDaemonAbsentBacksOffAndEventuallyConnects() async throws {
        let conn = FakeConnection()
        let connector = FakeConnector([.fail("ENOENT"), .fail("ECONNREFUSED"), .succeed(conn)])
        let transport = WatchTransport(connector: connector, backoff: fastBackoff, livenessWindow: .seconds(30))
        let recorder = EventRecorder(); recorder.consume(transport.events)

        await transport.start()
        try await expectDisconnected(recorder)
        try await expectDisconnected(recorder)
        try await expect(recorder, .connected)
        XCTAssertEqual(connector.connectCount, 3)

        await transport.stop()
    }

    // MARK: - Liveness → stale (ADR-0011 design inputs)

    // AC: no line for longer than the liveness window → `.stale`, with the connection still open
    // (no disconnect / reconnect). Here a line arrives first (so `.connected` → `.line`), then
    // silence trips stale — proving the timer is armed off received data.
    func testSilenceAfterALineGoesStaleWithoutDisconnecting() async throws {
        let conn = FakeConnection()
        let connector = FakeConnector([.succeed(conn)])
        let transport = WatchTransport(connector: connector, backoff: fastBackoff, livenessWindow: fastWindow)
        let recorder = EventRecorder(); recorder.consume(transport.events)

        await transport.start()
        try await expect(recorder, .connected)
        conn.emit(#"{"type":"snapshot"}"#)
        try await expect(recorder, .line(#"{"type":"snapshot"}"#))

        // No more lines → after the window, `.stale`. Crucially NOT `.disconnected` (still connected)
        // and the connector was never asked to reconnect.
        try await expect(recorder, .stale)
        XCTAssertEqual(connector.connectCount, 1, "stale must NOT trigger a reconnect")

        await transport.stop()
    }

    // ADR-0011 design input #1 (graceful-degrade): a pre-#164 daemon answers `watch` with an
    // `{"error":…}` line and then streams nothing. The transport must NOT hang awaiting a snapshot:
    // it emits `.connected`, surfaces the error line verbatim as `.line` (frame INTERPRETATION is
    // #324's job), and lets the liveness timer drive `.stale`.
    func testErrorOnlyStreamDegradesToStaleAndNeverHangs() async throws {
        let conn = FakeConnection()
        let connector = FakeConnector([.succeed(conn)])
        let transport = WatchTransport(connector: connector, backoff: fastBackoff, livenessWindow: fastWindow)
        let recorder = EventRecorder(); recorder.consume(transport.events)

        await transport.start()
        try await expect(recorder, .connected)
        conn.emit(#"{"error":"unknown command"}"#)     // never a snapshot
        try await expect(recorder, .line(#"{"error":"unknown command"}"#))
        try await expect(recorder, .stale, "degrades to stale instead of hanging")

        await transport.stop()
    }

    // MARK: - Event stream awaiting helpers

    private enum WaitError: Error { case timeout }

    /// Await the next transport event, failing the test (via a thrown timeout) rather than hanging if
    /// none arrives — so a wiring regression surfaces as a clear failure, not a stuck suite.
    private func next(_ recorder: EventRecorder, timeout: Duration = .seconds(5)) async throws -> TransportEvent {
        try await withThrowingTaskGroup(of: TransportEvent?.self) { group in
            group.addTask { await recorder.next() }
            group.addTask { try await Task.sleep(for: timeout); throw WaitError.timeout }
            let result = try await group.next()!
            group.cancelAll()
            return try XCTUnwrap(result, "event stream finished before the expected event")
        }
    }

    /// Assert the next event equals `expected`. (Awaiting inside `XCTAssertEqual`'s autoclosure is not
    /// allowed, so the value is bound first.)
    private func expect(
        _ recorder: EventRecorder, _ expected: TransportEvent, _ message: String = "",
        timeout: Duration = .seconds(5), file: StaticString = #filePath, line: UInt = #line
    ) async throws {
        let event = try await next(recorder, timeout: timeout)
        XCTAssertEqual(event, expected, message, file: file, line: line)
    }

    /// Assert the next event is a `.disconnected` (reason text is not pinned).
    private func expectDisconnected(
        _ recorder: EventRecorder, file: StaticString = #filePath, line: UInt = #line
    ) async throws {
        let event = try await next(recorder)
        guard case .disconnected = event else {
            return XCTFail("expected .disconnected, got \(event)", file: file, line: line)
        }
    }
}

// MARK: - In-process test doubles (the socket seam, faked)

/// A scriptable `WatchConnector`: hands out pre-built outcomes (success with a specific
/// `FakeConnection`, or a failure) one per `connect()`, so reconnect sequences are deterministic.
final class FakeConnector: WatchConnector, @unchecked Sendable {
    enum Outcome { case succeed(FakeConnection); case fail(String) }
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State { var outcomes: [Outcome] = []; var index = 0 }

    init(_ outcomes: [Outcome]) { state.withLock { $0.outcomes = outcomes } }

    /// How many times `connect()` has been called — the reconnect count.
    var connectCount: Int { state.withLock { $0.index } }

    func connect() throws -> WatchConnection {
        let outcome: Outcome = try state.withLock { st in
            guard st.index < st.outcomes.count else { throw TransportError.connect("no more scripted outcomes") }
            defer { st.index += 1 }
            return st.outcomes[st.index]
        }
        switch outcome {
        case .succeed(let connection): return connection
        case .fail(let reason): throw TransportError.connect(reason)
        }
    }
}

/// An in-process `WatchConnection` the test drives directly: `emit`/`eof` push lines / EOF onto the
/// `lines` stream, and `sentStrings` records what the transport wrote (to assert push-only).
final class FakeConnection: WatchConnection, @unchecked Sendable {
    let lines: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State { var sent: [[UInt8]] = []; var closed = false }

    init() { (lines, continuation) = AsyncStream<String>.makeStream() }

    func send(_ bytes: [UInt8]) throws { state.withLock { $0.sent.append(bytes) } }

    func close() {
        let shouldFinish = state.withLock { st -> Bool in
            if st.closed { return false }
            st.closed = true
            return true
        }
        if shouldFinish { continuation.finish() }
    }

    // Test drivers:
    func emit(_ line: String) { continuation.yield(line) }
    func eof() { continuation.finish() }

    var sentStrings: [String] { state.withLock { $0.sent.map { String(decoding: $0, as: UTF8.self) } } }
}

/// A tiny async buffer that consumes the transport's event stream and hands events out one at a time
/// via `next()` — so tests can assert an ordered sequence without arbitrary sleeps.
final class EventRecorder: @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State {
        var buffer: [TransportEvent] = []
        var waiter: CheckedContinuation<TransportEvent?, Never>?
        var finished = false
    }

    func consume(_ stream: AsyncStream<TransportEvent>) {
        Task { [weak self] in
            for await event in stream { self?.push(event) }
            self?.finish()
        }
    }

    private func push(_ event: TransportEvent) {
        let waiter: CheckedContinuation<TransportEvent?, Never>? = state.withLock { st in
            if let w = st.waiter { st.waiter = nil; return w }
            st.buffer.append(event)
            return nil
        }
        waiter?.resume(returning: event)
    }

    private func finish() {
        let waiter: CheckedContinuation<TransportEvent?, Never>? = state.withLock { st in
            st.finished = true
            let w = st.waiter; st.waiter = nil; return w
        }
        waiter?.resume(returning: nil)
    }

    func next() async -> TransportEvent? {
        await withCheckedContinuation { continuation in
            let immediate: TransportEvent?? = state.withLock { st -> TransportEvent?? in
                if !st.buffer.isEmpty { return .some(st.buffer.removeFirst()) }
                if st.finished { return .some(nil) }
                st.waiter = continuation
                return nil
            }
            if let value = immediate { continuation.resume(returning: value) }
        }
    }
}
