// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Hermetic tests for the control-command transport (issue #358): the short-lived client→daemon write
// path (`ControlCommandClient`) and its redacted-ack decoder (`SwapAck`). Driven by an in-process
// one-shot fake connection that replies to a `send` with a canned ack line — NO real socket, NO live
// daemon — so the request→ack→close exchange, the bounded timeout, connection-refused, and
// closed-before-ack paths are all exercised deterministically, and every redacted ack variant the
// daemon can return (`src/daemon/socket.rs`) is decoded. The production raw-POSIX read/write path it
// reuses (`PosixSocketConnection`) already has real-fd coverage in `PosixSocketConnectionTests`.

import XCTest
import os

final class ControlCommandTransportTests: XCTestCase {

    // MARK: - Transport: request → redacted ack → close

    // AC: a short-lived transport sends a `{"cmd":…}` line and returns the daemon's redacted ack, then
    // closes. The exact command bytes are written verbatim (verb + payload the caller supplied), one
    // newline-terminated line, and the connection is closed after the exchange.
    func testSendsCommandLineAndReturnsRedactedAckThenCloses() async throws {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"accepted","from":"work","to":"personal"}"#)
        let client = ControlCommandClient(connector: CommandFakeConnector(.succeed(conn)), timeout: .seconds(5))

        let result = try await awaitSend(client, SwapCommandRequest(target: "personal", force: false))

        guard case .success(let line) = result else { return XCTFail("expected success, got \(result)") }
        XCTAssertEqual(line, #"{"result":"accepted","from":"work","to":"personal"}"#)
        // The exact command line the caller's request serialized to — verb + payload, one `\n` line,
        // keys in deterministic sorted order (the client's `.sortedKeys` encoding).
        XCTAssertEqual(conn.sentStrings, [#"{"cmd":"swap","force":false,"target":"personal"}"# + "\n"])
        XCTAssertGreaterThanOrEqual(conn.closeCount, 1, "the transport closes the connection after the exchange")
    }

    // AC: the returned redacted ack decodes into the typed swap verdict — end-to-end transport → decode.
    func testReturnedAckDecodesIntoTypedVerdict() async throws {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"accepted","from":"work","to":"personal"}"#)
        let client = ControlCommandClient(connector: CommandFakeConnector(.succeed(conn)), timeout: .seconds(5))

        let line = try await awaitSend(client, SwapCommandRequest(target: "personal", force: false)).get()
        XCTAssertEqual(try SwapAck.decode(line), .accepted(from: "work", to: "personal"))
    }

    // AC: the client performs NO credential handling — it serializes only the caller's non-secret
    // command (verb + labels/flags) and adds nothing; the wire bytes carry no secret (redacted channel,
    // issue #15), and there is no keychain access anywhere on the path.
    func testCommandBytesCarryNoSecret() async throws {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"accepted","from":"work","to":"personal"}"#)
        let client = ControlCommandClient(connector: CommandFakeConnector(.succeed(conn)), timeout: .seconds(5))

        _ = try await awaitSend(client, SwapCommandRequest(target: "personal", force: true))

        let sent = conn.sentStrings.joined()
        XCTAssertEqual(sent, #"{"cmd":"swap","force":true,"target":"personal"}"# + "\n")
        XCTAssertFalse(sent.contains("@"), "no email in the command bytes")
        XCTAssertFalse(sent.lowercased().contains("token"), "no token in the command bytes")
    }

    // AC (reusability): a swap-shaped request and a capture-shaped request both flow through the SAME
    // transport type — the verb + payload are parameters, the transport is not duplicated per verb.
    func testSameTransportCarriesDifferentVerbs() async throws {
        let swapConn = CommandFakeConnection(ackOnSend: #"{"result":"already_active","to":"work"}"#)
        let swapClient = ControlCommandClient(connector: CommandFakeConnector(.succeed(swapConn)), timeout: .seconds(5))
        _ = try await awaitSend(swapClient, SwapCommandRequest(target: "work", force: false))
        XCTAssertEqual(swapConn.sentStrings, [#"{"cmd":"swap","force":false,"target":"work"}"# + "\n"])

        // A different verb + payload — modelling the #360 capture call site — through the identical API.
        let captureConn = CommandFakeConnection(ackOnSend: #"{"ok":true}"#)
        let captureClient = ControlCommandClient(connector: CommandFakeConnector(.succeed(captureConn)), timeout: .seconds(5))
        let captureLine = try await awaitSend(captureClient, CaptureCommandRequest(label: "work")).get()
        XCTAssertEqual(captureConn.sentStrings, [#"{"cmd":"capture","label":"work"}"# + "\n"])
        XCTAssertEqual(captureLine, #"{"ok":true}"#)  // the transport hands back any verb's raw ack line
    }

    // MARK: - Transport: bounded error paths

    // AC: a daemon that accepts but never replies is bounded — the exchange times out rather than hangs.
    //
    // The close is AWAITED, not sampled (issue #912). `send` has two paths to `.timedOut` and they tear
    // down on different schedules: a READ-phase timeout closes in `readAck`'s `defer`, before `send`
    // returns, whereas a CONNECT-phase timeout returns at once and hands the close to a detached sweep.
    // This test means to drive the read path — but the SAME `timeout` bounds both phases, and the fake's
    // instant connect+write still has to be SCHEDULED onto a detached task, so under load that scheduling
    // can miss the 150 ms window and the exchange silently takes the connect path instead. `.timedOut`
    // asserts either way; `closeCount` read on the next line does not, and that is the whole of #912.
    // Which path a run took is decidable after the fact, and STRUCTURALLY rather than statistically:
    // `readAck`'s `defer` closes before `send` returns, and `CommandFakeConnection.close()` increments
    // `closeCount` before it finishes `lines`, so a read-phase timeout can never return with
    // `closeCount == 0`. That reading is the connect path necessarily, not probably — which is why no
    // measurement is quoted here. Awaiting the close covers both paths and keeps the assertion at `>= 1`
    // — relaxing it would delete the coverage rather than repair it. The connect path's own sweep is
    // pinned deterministically by `testSlowConnectIsBoundedByTimeout` below, so misrouting costs fidelity
    // here but not coverage.
    func testSilentDaemonTimesOut() async throws {
        let conn = CommandFakeConnection(ackOnSend: nil)  // accepts the command, never answers
        let client = ControlCommandClient(connector: CommandFakeConnector(.succeed(conn)), timeout: .milliseconds(150))

        let result = try await awaitSend(client, SwapCommandRequest(target: "x", force: false))

        XCTAssertEqual(result, .failure(.timedOut))
        try await awaitClose(conn)
        XCTAssertGreaterThanOrEqual(conn.closeCount, 1, "a timed-out exchange still closes the connection")
    }

    // AC: connection refused (daemon absent / socket gone) surfaces as a distinct connection-refused
    // error — the "no daemon" signal, distinct from a mid-exchange failure.
    func testConnectionRefusedSurfacesDistinctly() async throws {
        let client = ControlCommandClient(connector: CommandFakeConnector(.fail("ECONNREFUSED")), timeout: .seconds(5))

        let result = try await awaitSend(client, SwapCommandRequest(target: "x", force: false))

        XCTAssertEqual(result, .failure(.connectionRefused(reason: "ECONNREFUSED")))
    }

    // The daemon went away mid-exchange (EOF before any ack line) → a closed-before-ack error, distinct
    // from a timeout (no waiting for the whole window).
    func testClosedBeforeAck() async throws {
        let conn = CommandFakeConnection(eofOnSend: true)  // accepts, then EOFs without replying
        let client = ControlCommandClient(connector: CommandFakeConnector(.succeed(conn)), timeout: .seconds(5))

        let result = try await awaitSend(client, SwapCommandRequest(target: "x", force: false))

        XCTAssertEqual(result, .failure(.closedBeforeAck))
    }

    // AC (bounded): even a blocking connect() (a saturated / wedged accept) is bounded — `send` returns
    // `.timedOut` on the CONNECT phase without waiting for the connect to complete. Proves the bound
    // covers connect+write, not only the read.
    //
    // The sweep assertion pins the OTHER half of that bargain, and pins it deterministically because a
    // 1 s blocking connect against a 150 ms budget cannot resolve any other way: abandoning the connect
    // is only safe if the connection it eventually yields is still closed, so `send` hands that close to
    // a detached sweep (`readAck`'s `defer` never runs on this path — there is no `readAck`). Untested
    // until issue #912, whose root cause turns on exactly this: the deferred close is what can leave
    // `closeCount` at 0 when `send` returns, and the sweep is what keeps that a synchronisation gap
    // rather than a leaked connection per timed-out command. Asserted as eventually-closed rather than
    // still-open-then-closed — a `closeCount == 0` probe here would be a negative timing assertion, i.e.
    // a fresh flake of precisely the kind this issue exists to remove.
    func testSlowConnectIsBoundedByTimeout() async throws {
        let conn = CommandFakeConnection(ackOnSend: nil)
        let client = ControlCommandClient(
            connector: SlowConnectConnector(delaySeconds: 1.0, connection: conn),
            timeout: .milliseconds(150))

        let clock = ContinuousClock()
        let start = clock.now
        let result = try await awaitSend(
            client, SwapCommandRequest(target: "x", force: false), testTimeout: .seconds(2))
        let elapsed = clock.now - start

        XCTAssertEqual(result, .failure(.timedOut))
        XCTAssertLessThan(elapsed, .milliseconds(700), "the connect phase must be bounded, not wait the full connect")
        // Budget generous against the 1 s the connect deliberately blocks for — this waits out that
        // sleep, not a product latency, and only a genuinely missing sweep should ever exhaust it.
        try await awaitClose(conn, within: .seconds(10))
        XCTAssertGreaterThanOrEqual(conn.closeCount, 1, "an abandoned connect is swept closed, not leaked")
    }

    // MARK: - SwapAck decoder: the redacted variants (mirror of src/daemon/socket.rs)

    func testDecodeAccepted() throws {
        XCTAssertEqual(
            try SwapAck.decode(#"{"result":"accepted","from":"work","to":"personal"}"#),
            .accepted(from: "work", to: "personal"))
    }

    func testDecodeAlreadyActive() throws {
        XCTAssertEqual(try SwapAck.decode(#"{"result":"already_active","to":"work"}"#), .alreadyActive(to: "work"))
    }

    // Every redacted rejection reason maps to its kebab-case wire code — the lockstep guard with the
    // daemon's `SwapRejection` enum.
    func testDecodeAllRejectionReasons() throws {
        let cases: [(String, SwapRejection)] = [
            ("unknown-target", .unknownTarget),
            ("ambiguous-target", .ambiguousTarget),
            ("quarantined", .quarantined),
            ("weekly-exhausted", .weeklyExhausted),
            ("cooldown", .cooldown),
            ("no-active-account", .noActiveAccount),
            ("keychain-locked", .keychainLocked),
            ("swap-lock-busy", .swapLockBusy),
            ("failed", .failed),
        ]
        for (code, expected) in cases {
            let line = #"{"result":"rejected","reason":"\#(code)"}"#
            XCTAssertEqual(try SwapAck.decode(line), .rejected(expected), "reason \(code)")
        }
    }

    // The shared redacted error ack — `unauthorized` is the peer-auth rejection the same-user local
    // client should never actually see, but decodes to a case rather than throwing.
    func testDecodeErrorUnauthorized() throws {
        XCTAssertEqual(try SwapAck.decode(#"{"error":"unauthorized"}"#), .error("unauthorized"))
    }

    // An accepted ack carries ONLY the two non-secret labels — no credential field exists to leak
    // (redaction by construction, issue #15).
    func testAcceptedAckModelsOnlyLabels() throws {
        guard case .accepted(let from, let to) = try SwapAck.decode(#"{"result":"accepted","from":"a","to":"b"}"#)
        else { return XCTFail("expected .accepted") }
        XCTAssertEqual(from, "a")
        XCTAssertEqual(to, "b")
    }

    // MARK: - SwapAck decoder: hard errors (mirror serde's unknown-variant / malformed rejection)

    func testDecodeUnknownResultThrows() {
        XCTAssertThrowsError(try SwapAck.decode(#"{"result":"future_state"}"#)) { error in
            guard case .unrecognized? = error as? SwapAck.DecodeError else {
                return XCTFail("expected .unrecognized, got \(error)")
            }
        }
    }

    func testDecodeUnknownRejectionReasonThrows() {
        XCTAssertThrowsError(try SwapAck.decode(#"{"result":"rejected","reason":"future-reason"}"#)) { error in
            guard case .unrecognized? = error as? SwapAck.DecodeError else {
                return XCTFail("expected .unrecognized, got \(error)")
            }
        }
    }

    // A `{"ok":true}` ack (a payload-less command's reply) is unrecognized for the SWAP decoder — a
    // hard error, not a silent mis-read; the capture call site (#360) decodes its own ack shape.
    func testDecodeOkAckIsUnrecognizedForSwap() {
        XCTAssertThrowsError(try SwapAck.decode(#"{"ok":true}"#)) { error in
            guard case .unrecognized? = error as? SwapAck.DecodeError else {
                return XCTFail("expected .unrecognized, got \(error)")
            }
        }
    }

    func testDecodeNonJSONThrows() {
        XCTAssertThrowsError(try SwapAck.decode("not json")) { error in
            XCTAssertEqual(error as? SwapAck.DecodeError, .notJSON)
        }
    }

    // A recognized-but-incomplete body maps to the decoder's own `.unrecognized`, NEVER a raw
    // `DecodingError` — so a caller catches one error type for every malformed ack.
    func testDecodeMalformedBodyThrowsUnrecognized() {
        for line in [
            #"{"result":"accepted","from":"work"}"#,  // missing `to`
            #"{"result":"already_active"}"#,           // missing `to`
            #"{"result":"rejected"}"#,                 // missing `reason`
        ] {
            XCTAssertThrowsError(try SwapAck.decode(line), "line: \(line)") { error in
                guard case .unrecognized? = error as? SwapAck.DecodeError else {
                    return XCTFail("expected .unrecognized for \(line), got \(error)")
                }
            }
        }
    }

    // MARK: - Awaiting helpers (timeout-guarded so a wiring bug fails instead of hanging)

    private enum WaitError: Error { case timeout }

    /// Await `client.send(request)`, failing the test (via a thrown timeout) rather than hanging if the
    /// client's own bound is broken. Mirrors the timeout-guarded await helpers in the sibling suites.
    private func awaitSend(
        _ client: ControlCommandClient, _ request: some Encodable & Sendable,
        testTimeout: Duration = .seconds(5)
    ) async throws -> Result<String, ControlCommandError> {
        try await withThrowingTaskGroup(of: Result<String, ControlCommandError>.self) { group in
            group.addTask { await client.send(request) }
            group.addTask { try await Task.sleep(for: testTimeout); throw WaitError.timeout }
            let result = try await group.next()!
            group.cancelAll()
            return result
        }
    }

    /// Await the transport's `close()` on `connection` instead of racing it, failing the test (via a
    /// thrown timeout) if it never lands. ONE helper serves both `.timedOut` paths because `closes`
    /// buffers (see its doc on `CommandFakeConnection`): a close that already landed and one still in
    /// flight are awaited identically (issue #912).
    ///
    /// Deliberately not a `waitUntil`-shaped poll. The suite's other copies of that helper are a
    /// separate concern (issue #1078), and a signal beats a poll here anyway: nothing to sample, and
    /// `budget` bounds only how long a MISSING close is waited for — it can never miss one that landed.
    private func awaitClose(
        _ connection: CommandFakeConnection, within budget: Duration = .seconds(5)
    ) async throws {
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { for await _ in connection.closes { return } }
            group.addTask { try await Task.sleep(for: budget); throw WaitError.timeout }
            try await group.next()!
            group.cancelAll()
        }
    }
}

// MARK: - Test requests (caller-supplied Encodables — the verb + payload the transport carries)

/// A swap-on-click (#169) shaped request. Field order is the serialized key order the daemon parses.
private struct SwapCommandRequest: Encodable, Sendable {
    let cmd = "swap"
    let target: String
    let force: Bool
}

/// A distinct verb + payload (modelling the #360 capture call site) that reuses the SAME transport.
private struct CaptureCommandRequest: Encodable, Sendable {
    let cmd = "capture"
    let label: String
}

// MARK: - In-process one-shot fake (the socket seam, faked for a request→reply exchange)

/// Hands out a single pre-built one-shot connection (or a connect failure) — the request→reply analogue
/// of `WatchTransportTests`' streaming `FakeConnector`, typed to `CommandFakeConnection`.
struct CommandFakeConnector: WatchConnector {
    enum Outcome: Sendable { case succeed(CommandFakeConnection); case fail(String) }
    let outcome: Outcome

    init(_ outcome: Outcome) { self.outcome = outcome }

    func connect() throws -> WatchConnection {
        switch outcome {
        case .succeed(let connection): return connection
        case .fail(let reason): throw TransportError.connect(reason)
        }
    }
}

/// A connector whose `connect()` BLOCKS for `delaySeconds` before returning `connection` — models a
/// saturated / wedged accept, so a test can prove the transport bounds the CONNECT phase, not just the
/// read. The blocking sleep runs on the detached connect task's thread, never the caller's.
struct SlowConnectConnector: WatchConnector {
    let delaySeconds: TimeInterval
    let connection: CommandFakeConnection

    func connect() throws -> WatchConnection {
        Thread.sleep(forTimeInterval: delaySeconds)
        return connection
    }
}

/// A one-shot control-command `WatchConnection` the test drives: on `send` it records the command bytes
/// and either replies with one canned ack line (the daemon answering), EOFs without a line (the daemon
/// going away mid-exchange), or stays silent (a wedged daemon → the transport's timeout). `closeCount`
/// lets a test assert the transport closes the connection after the exchange.
final class CommandFakeConnection: WatchConnection, @unchecked Sendable {
    let lines: AsyncStream<String>

    /// One element per `close()`, so a test can AWAIT the teardown rather than sample `closeCount` and
    /// hope it has landed (issue #912). Buffered and never finished, which is the point: a close that
    /// already happened is still delivered to a consumer that only starts awaiting afterwards, so the
    /// same await serves the read-phase teardown (before `send` returns) and the connect-phase sweep
    /// (after it). Yielded outside the lock but after the count is incremented, so any consumer this
    /// resumes already reads the new `closeCount`.
    let closes: AsyncStream<Void>

    private let continuation: AsyncStream<String>.Continuation
    private let closesContinuation: AsyncStream<Void>.Continuation
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State { var sent: [[UInt8]] = []; var closeCount = 0; var finished = false }

    private let ackOnSend: String?
    private let eofOnSend: Bool

    /// - Parameters:
    ///   - ackOnSend: the ack line to reply with when the command is written, or `nil` to stay silent.
    ///   - eofOnSend: when true, finish the stream on `send` WITHOUT a line — an EOF before any ack.
    init(ackOnSend: String? = nil, eofOnSend: Bool = false) {
        self.ackOnSend = ackOnSend
        self.eofOnSend = eofOnSend
        (lines, continuation) = AsyncStream<String>.makeStream()
        (closes, closesContinuation) = AsyncStream<Void>.makeStream()
    }

    func send(_ bytes: [UInt8]) throws {
        state.withLock { $0.sent.append(bytes) }
        if let ackOnSend { continuation.yield(ackOnSend) }  // the daemon answers with one redacted ack line
        if eofOnSend { finishOnce() }                        // the daemon EOF'd before acking
    }

    func close() {
        let shouldFinish = state.withLock { st -> Bool in
            st.closeCount += 1
            if st.finished { return false }
            st.finished = true
            return true
        }
        closesContinuation.yield(())  // count already incremented — an awaiter reads the new value
        if shouldFinish { continuation.finish() }
    }

    private func finishOnce() {
        let shouldFinish = state.withLock { st -> Bool in
            if st.finished { return false }
            st.finished = true
            return true
        }
        if shouldFinish { continuation.finish() }
    }

    var sentStrings: [String] { state.withLock { $0.sent.map { String(decoding: $0, as: UTF8.self) } } }
    var closeCount: Int { state.withLock { $0.closeCount } }
}
