// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Shell-wiring tests for `WatchStatusStore` (issue #324): they drive the store with a SYNTHETIC
// `AsyncStream<TransportEvent>` — no real socket, no `WatchTransport`, independent of #328's
// mock-socket harness — and assert the observable `@Published` projection plus the `presentations`
// glance stream. The exhaustive honest-state transition LOGIC is proven synchronously in
// `HonestStateMachineTests`; here we prove the shell actually pumps the machine from the injected
// stream and mirrors its derived state onto both surfaces (including that a snapshot-less / error-only
// stream never drives the store healthy).

import XCTest
import os

@MainActor
final class WatchStatusStoreTests: XCTestCase {

    /// Build a store plus a hand-driven event feed. The caller yields `TransportEvent`s into
    /// `continuation` and (for ordered assertions) consumes the returned recorder.
    private func makeStoreUnderTest()
        -> (store: WatchStatusStore, continuation: AsyncStream<TransportEvent>.Continuation,
            recorder: StreamRecorder<PresentationState>) {
        let (events, continuation) = AsyncStream<TransportEvent>.makeStream()
        let store = WatchStatusStore()
        let recorder = StreamRecorder<PresentationState>()
        recorder.consume(store.presentations)   // attach BEFORE start so the seed is captured
        store.start(consuming: events)
        return (store, continuation, recorder)
    }

    // MARK: - Initial glance

    func testInitialPresentationIsConnecting() async throws {
        let store = WatchStatusStore()
        let recorder = StreamRecorder<PresentationState>()
        recorder.consume(store.presentations)
        let first = try await next(recorder)
        XCTAssertEqual(first.glyph, .connecting)
        XCTAssertEqual(store.currentPresentation.glyph, .connecting)
    }

    // MARK: - AC: snapshot → connected + rows, mirrored to both surfaces

    func testSnapshotDrivesConnectedAndPublishesRows() async throws {
        let (store, continuation, recorder) = makeStoreUnderTest()
        continuation.yield(.connected)
        continuation.yield(.line(Fixtures.snapshotBasic))

        try await waitForGlyph(recorder, .healthy)
        XCTAssertEqual(store.connectionState, .connected)
        XCTAssertEqual(store.rows.count, 1)
        XCTAssertEqual(store.rows.first?.label, "work")
        XCTAssertEqual(store.generatedAt, 42)
        XCTAssertEqual(store.refreshEnabled, false)
        XCTAssertEqual(store.currentPresentation.glyph, .healthy)

        continuation.finish()
    }

    // MARK: - AC: `.disconnected` → last-good stale, never live

    func testDisconnectFlipsToDisconnectedNeverHealthy() async throws {
        let (store, continuation, recorder) = makeStoreUnderTest()
        continuation.yield(.connected)
        continuation.yield(.line(Fixtures.snapshotBasic))
        try await waitForGlyph(recorder, .healthy)

        continuation.yield(.disconnected(reason: "connection closed (EOF)"))
        try await waitForGlyph(recorder, .disconnected)

        XCTAssertFalse(store.connectionState.isHealthy)
        XCTAssertEqual(store.rows.count, 1, "last-good rows retained for a dimmed render")

        continuation.finish()
    }

    // MARK: - AC: an error-only / snapshot-less stream never drives the store healthy

    func testErrorOnlyStreamNeverGoesHealthy() async throws {
        let (store, continuation, recorder) = makeStoreUnderTest()
        continuation.yield(.connected)
        continuation.yield(.line(#"{"error":"unknown command"}"#))  // pre-#164 daemon, no snapshot
        continuation.yield(.stale)

        try await waitForGlyph(recorder, .stale)
        XCTAssertEqual(store.connectionState, .stale)
        XCTAssertFalse(store.connectionState.isHealthy)
        XCTAssertTrue(store.rows.isEmpty)

        continuation.finish()
    }

    // MARK: - Decode-defensiveness: an undecodable line does not crash or corrupt the store

    func testUndecodableLineIsNonFatal() async throws {
        let (store, continuation, recorder) = makeStoreUnderTest()
        continuation.yield(.connected)
        continuation.yield(.line("not json at all"))
        // The store keeps running; a later valid snapshot still applies (proves the loop survived).
        continuation.yield(.line(Fixtures.snapshotBasic))

        try await waitForGlyph(recorder, .healthy)
        XCTAssertEqual(store.connectionState, .connected)
        XCTAssertEqual(store.rows.count, 1)

        continuation.finish()
    }

    // MARK: - AC (#344): the store's own valid-frame watchdog trips stale on a byte-live daemon

    // The store-level end-to-end proof of the #344 fix: a daemon holding the connection open and
    // streaming ONLY undecodable frames (spaced well under the window) after a healthy snapshot drives
    // the STORE to `.stale` on its own. The injected window is the "clock" the test advances (real
    // `Task.sleep`), mirroring how `WatchTransportTests` drives the transport's liveness timer with a
    // small injected `livenessWindow`. Before the watchdog, this stream held the store healthy forever.
    func testContinuousUndecodableStreamTripsTheStoreWatchdogToStale() async throws {
        let (events, continuation) = AsyncStream<TransportEvent>.makeStream()
        let store = WatchStatusStore(validFrameWindow: .milliseconds(200))
        let recorder = StreamRecorder<PresentationState>()
        recorder.consume(store.presentations)
        store.start(consuming: events)

        continuation.yield(.connected)
        continuation.yield(.line(Fixtures.snapshotBasic))
        try await waitForGlyph(recorder, .healthy)

        // Keep the byte stream "live" with continuous garbage spaced well under the window — exactly
        // what perpetually re-arms the transport and starves the store of a transport `.stale`.
        let emitter = Task {
            for i in 0..<40 {
                continuation.yield(.line("garbage line \(i) — not a frame"))
                try? await Task.sleep(for: .milliseconds(40))
            }
        }

        // The store's valid-frame watchdog trips ~one window after the snapshot, DESPITE the garbage.
        try await waitForGlyph(recorder, .stale)
        XCTAssertEqual(store.connectionState, .stale)
        XCTAssertFalse(store.connectionState.isHealthy, "never healthy on a garbage-emitting daemon")

        emitter.cancel()
        continuation.finish()
    }

    // AC (#344): a heartbeat RE-ARMS the store watchdog end-to-end. After the watchdog trips stale on
    // a silent connection, a heartbeat un-stales to healthy AND re-arms — proven by the watchdog
    // tripping stale a SECOND time. (That a beat *keeps* a fresh connection healthy WITHIN the window
    // is proven deterministically in `HonestStateMachineTests`.)
    func testHeartbeatReArmsTheStoreWatchdog() async throws {
        let (events, continuation) = AsyncStream<TransportEvent>.makeStream()
        let store = WatchStatusStore(validFrameWindow: .milliseconds(200))
        let recorder = StreamRecorder<PresentationState>()
        recorder.consume(store.presentations)
        store.start(consuming: events)

        continuation.yield(.connected)
        continuation.yield(.line(Fixtures.snapshotBasic))
        try await waitForGlyph(recorder, .healthy)
        try await waitForGlyph(recorder, .stale)           // watchdog trips on the now-silent connection

        continuation.yield(.line(Fixtures.heartbeatBasic)) // a valid beat un-stales AND re-arms
        try await waitForGlyph(recorder, .healthy)
        try await waitForGlyph(recorder, .stale)           // the re-armed watchdog trips again

        continuation.finish()
    }

    // AC (#344) window rationale: the store's valid-frame window must exceed 2× the daemon's 15 s
    // heartbeat (a healthy daemon beating every ≤ 15 s is never falsely marked stale) — the same
    // contract the transport's liveness window is pinned to. Pinning it here means a future edit can't
    // quietly shrink it below the threshold without turning this test red.
    func testDefaultValidFrameWindowExceedsTwiceTheDaemonHeartbeat() {
        let daemonHeartbeat = Duration.seconds(15)         // src/daemon/socket.rs WATCH_HEARTBEAT
        let window = WatchStatusStore.defaultValidFrameWindow
        XCTAssertGreaterThan(window, daemonHeartbeat * 2, "must tolerate one missed heartbeat (>2×15s)")
        XCTAssertGreaterThan(window, .seconds(30), "in the same ballpark as the transport's 32 s window")
    }

    // MARK: - Event-stream awaiting helpers (mirror of the transport suite's)

    private enum WaitError: Error { case timeout }

    private func next(_ recorder: StreamRecorder<PresentationState>,
                      timeout: Duration = .seconds(5)) async throws -> PresentationState {
        try await withThrowingTaskGroup(of: PresentationState?.self) { group in
            group.addTask { await recorder.next() }
            group.addTask { try await Task.sleep(for: timeout); throw WaitError.timeout }
            let result = try await group.next()!
            group.cancelAll()
            return try XCTUnwrap(result, "presentation stream finished before the expected value")
        }
    }

    /// Await until a presentation with `glyph` is observed (robust to latest-wins buffer collapsing
    /// intermediate transitions — the current glance always eventually arrives).
    private func waitForGlyph(_ recorder: StreamRecorder<PresentationState>, _ glyph: StatusGlyph,
                              file: StaticString = #filePath, line: UInt = #line) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while ContinuousClock.now < deadline {
            let presentation = try await next(recorder)
            if presentation.glyph == glyph { return }
        }
        XCTFail("timed out waiting for glyph \(glyph)", file: file, line: line)
    }
}

// MARK: - A tiny async recorder (generic sibling of the transport suite's EventRecorder)

/// Consumes an `AsyncStream` once and hands elements out one at a time via `next()`, so tests assert
/// an ordered sequence without arbitrary sleeps.
final class StreamRecorder<Element: Sendable>: @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State {
        var buffer: [Element] = []
        var waiter: CheckedContinuation<Element?, Never>?
        var finished = false
    }

    func consume(_ stream: AsyncStream<Element>) {
        Task { [weak self] in
            for await element in stream { self?.push(element) }
            self?.finish()
        }
    }

    private func push(_ element: Element) {
        let waiter: CheckedContinuation<Element?, Never>? = state.withLock { st in
            if let w = st.waiter { st.waiter = nil; return w }
            st.buffer.append(element)
            return nil
        }
        waiter?.resume(returning: element)
    }

    private func finish() {
        let waiter: CheckedContinuation<Element?, Never>? = state.withLock { st in
            st.finished = true
            let w = st.waiter; st.waiter = nil; return w
        }
        waiter?.resume(returning: nil)
    }

    func next() async -> Element? {
        await withCheckedContinuation { continuation in
            let immediate: Element?? = state.withLock { st -> Element?? in
                if !st.buffer.isEmpty { return .some(st.buffer.removeFirst()) }
                if st.finished { return .some(nil) }
                st.waiter = continuation
                return nil
            }
            if let value = immediate { continuation.resume(returning: value) }
        }
    }
}
