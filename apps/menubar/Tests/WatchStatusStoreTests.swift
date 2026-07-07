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
