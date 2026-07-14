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
        try await waitForGlyph(recorder, .attention)   // #524: a warm drop collapses to the attention glyph

        XCTAssertEqual(store.connectionState, .disconnected(reason: "connection closed (EOF)"))
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

        try await waitForGlyph(recorder, .attention)   // #524: stale collapses to the attention glyph
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
        try await waitForGlyph(recorder, .attention)   // #524: stale collapses to the attention glyph
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
        try await waitForGlyph(recorder, .attention)       // watchdog trips (stale → attention, #524)

        continuation.yield(.line(Fixtures.heartbeatBasic)) // a valid beat un-stales AND re-arms
        try await waitForGlyph(recorder, .healthy)
        try await waitForGlyph(recorder, .attention)       // the re-armed watchdog trips again (#524)

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

    // MARK: - AC (#169): the store drives the crash-loop stability debounce end-to-end

    // The store-level proof of the crash-loop debounce: after a RECONNECT (a prior drop armed it), a
    // fresh snapshot is HELD — the store does NOT flash healthy as it does on the first connect — until
    // the injected stability window elapses. The window is the "clock" the test advances (real
    // `Task.sleep`), mirroring the watchdog test; the first connect stays immediate, proving the
    // debounce is reconnect-scoped. Before it, a crash-looping daemon flickered healthy here.
    func testReconnectSnapshotIsDebouncedThenGoesHealthy() async throws {
        let (events, continuation) = AsyncStream<TransportEvent>.makeStream()
        let store = WatchStatusStore(stabilityWindow: .milliseconds(300))
        let recorder = StreamRecorder<PresentationState>()
        recorder.consume(store.presentations)
        store.start(consuming: events)

        // First connect → healthy immediately (no debounce on a cold start).
        continuation.yield(.connected)
        continuation.yield(.line(Fixtures.snapshotBasic))
        try await waitForGlyph(recorder, .healthy)

        // Drop, then reconnect + a fresh snapshot: the debounce holds it (never an immediate flash).
        continuation.yield(.disconnected(reason: "EOF"))
        try await waitForGlyph(recorder, .attention)   // #524: a warm drop collapses to the attention glyph
        let armed = ContinuousClock.now
        continuation.yield(.connected)
        continuation.yield(.line(Fixtures.snapshotBasic))

        // The held snapshot goes healthy only AFTER the stability window — measurably delayed, never
        // the immediate flash the first connect showed.
        try await waitForGlyph(recorder, .healthy)
        let elapsed = ContinuousClock.now - armed
        XCTAssertGreaterThanOrEqual(elapsed, .milliseconds(150),
                                    "the reconnect's healthy was debounced past the window, not flashed")
        XCTAssertEqual(store.connectionState, .connected)

        continuation.finish()
    }

    // MARK: - AC (#499): the store drives the start-grace escalation end-to-end

    // The store-level proof of the not-running/starting split: a COLD connect-refused (a `.disconnected`
    // with no prior `.connected`) is shown as the transient `.starting`, then the injected start grace
    // elapses still refused and the store escalates ITSELF to the durable `.notRunning` — neither ever
    // healthy. The grace is the "clock" the test advances (real `Task.sleep`), mirroring the watchdog /
    // stability tests. Before #499 this stream flipped straight to the socket-dropped `.disconnected`.
    func testColdRefusedGoesStartingThenNotRunningViaTheStartGrace() async throws {
        let (events, continuation) = AsyncStream<TransportEvent>.makeStream()
        let store = WatchStatusStore(startGraceWindow: .milliseconds(200))
        let recorder = StreamRecorder<PresentationState>()
        recorder.consume(store.presentations)
        store.start(consuming: events)

        // Daemon absent at launch: the transport refuses the connect and emits `.disconnected` with no
        // prior `.connected`. The store shows the transient starting glance first…
        continuation.yield(.disconnected(reason: "connect refused"))
        // #524: `.starting` now shares the `.connecting` glyph with the initial state, so a glyph barrier
        // would race the initial `.connecting`; wait on the precise connection-state axis instead.
        try await waitForConnectionState(store, .starting)
        XCTAssertEqual(store.currentPresentation.glyph, .connecting,
                       "#524: starting projects onto the connecting '…' glyph")
        XCTAssertFalse(store.connectionState.isHealthy)

        // …then the start grace elapses still refused → the store escalates itself to not-running.
        try await waitForConnectionState(store, .notRunning)
        XCTAssertEqual(store.currentPresentation.glyph, .attention,
                       "#524: not-running collapses to the attention glyph")
        XCTAssertFalse(store.connectionState.isHealthy, "an absent daemon is never healthy")

        continuation.finish()
    }

    // A daemon that comes up DURING the grace connects straight to healthy — the store's grace timer is
    // superseded by the connect (the shell cancels it), so the genuinely-starting case resolves cleanly.
    func testDaemonConnectingDuringGraceGoesHealthy() async throws {
        let (events, continuation) = AsyncStream<TransportEvent>.makeStream()
        let store = WatchStatusStore(startGraceWindow: .milliseconds(500))
        let recorder = StreamRecorder<PresentationState>()
        recorder.consume(store.presentations)
        store.start(consuming: events)

        continuation.yield(.disconnected(reason: "connect refused"))
        try await waitForConnectionState(store, .starting)   // #524: starting shares connecting's glyph
        // The daemon comes up well within the grace → connect + snapshot → healthy.
        continuation.yield(.connected)
        continuation.yield(.line(Fixtures.snapshotBasic))
        try await waitForGlyph(recorder, .healthy)
        XCTAssertEqual(store.connectionState, .connected)

        continuation.finish()
    }

    // The default start grace is a SHORT, bounded window: positive (starting is a real transient state) and
    // no longer than a few seconds (a truly-absent daemon must reach the actionable not-running promptly).
    // Pins the "short grace" intent so a future edit can't quietly stretch it into a dead-end.
    func testDefaultStartGraceIsAShortBoundedWindow() {
        let grace = WatchStatusStore.defaultStartGraceWindow
        XCTAssertGreaterThan(grace, .zero, "the grace must be positive — starting is a real transient window")
        XCTAssertLessThanOrEqual(grace, .seconds(10),
                                 "a 'short' grace — a truly-absent daemon must reach not-running promptly")
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

    /// Await until the store reaches `state` on the precise CONNECTION axis (issue #524). Needed where the
    /// 4-state glyph projection is lossy — `.starting` shares the `.connecting` "…" glyph with the initial
    /// state, so a glyph barrier would race the initial glance; polling the connection state disambiguates.
    /// The store is `@MainActor` (as is this test class), so the read is main-actor-isolated by inheritance.
    private func waitForConnectionState(_ store: WatchStatusStore, _ state: ConnectionState,
                                        file: StaticString = #filePath, line: UInt = #line) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while ContinuousClock.now < deadline {
            if store.connectionState == state { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("timed out waiting for connection state \(state)", file: file, line: line)
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
