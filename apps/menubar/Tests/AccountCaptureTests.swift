// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Hermetic tests for the in-app capture affordance (issue #360): the redacted `capture`-ack decoder
// (`CaptureAck`), the request serialization (`CaptureCommand`), the affordance model's idle → pending →
// done → failed transitions (`AccountCaptureModel`), and the pure human copy (`StatusPanelFormat`). The
// model is driven by the SAME in-process fake connection the transport suite uses (`CommandFakeConnection`
// / `CommandFakeConnector` in `ControlCommandTransportTests`) — NO real socket, NO live daemon — so every
// phase transition and every redacted ack variant is exercised deterministically. A gated fake proves the
// in-flight (`.pending` / `isBusy`) window that the panel-retain gate depends on.
//
// The `capture` ack wire shape decoded here is DEFINED by #360 (the daemon side, #359, is not yet wired on
// the socket) as the 1:1 mirror of `SwapAck` — these cases are the lockstep guard for that contract.

import XCTest

final class AccountCaptureTests: XCTestCase {

    // MARK: - CaptureAck decoder: the redacted variants (1:1 mirror of the swap ack shape)

    func testDecodeCapturedWithLabelAndCount() throws {
        XCTAssertEqual(
            try CaptureAck.decode(#"{"result":"captured","label":"work","accounts":2}"#),
            .captured(label: "work", accounts: 2))
    }

    // `accounts` is an informational count the UI does not render — TOLERATED when absent, so the label
    // (the load-bearing field) alone decodes cleanly (a narrower exact-match burden on the not-yet-built #359).
    func testDecodeCapturedWithoutCount() throws {
        XCTAssertEqual(
            try CaptureAck.decode(#"{"result":"captured","label":"a1b2c3"}"#),
            .captured(label: "a1b2c3", accounts: nil))
    }

    // Every redacted rejection reason #359 enumerates maps to its kebab-case wire code — the lockstep guard.
    func testDecodeAllRejectionReasons() throws {
        let cases: [(String, CaptureRejection)] = [
            ("no-active-account", .noActiveAccount),
            ("keychain-locked", .keychainLocked),
            ("swap-lock-busy", .swapLockBusy),
            ("failed", .failed),
        ]
        for (code, expected) in cases {
            let line = #"{"result":"rejected","reason":"\#(code)"}"#
            XCTAssertEqual(try CaptureAck.decode(line), .rejected(expected), "reason \(code)")
        }
    }

    // The shared redacted error ack — `unauthorized` is the peer-auth rejection the same-user local client
    // should never see, but it decodes to a case rather than throwing.
    func testDecodeErrorUnauthorized() throws {
        XCTAssertEqual(try CaptureAck.decode(#"{"error":"unauthorized"}"#), .error("unauthorized"))
    }

    // A captured ack carries ONLY the non-secret label (+ optional count) — no credential field exists to
    // leak (redaction by construction, issue #15).
    func testCapturedAckModelsOnlyLabelAndCount() throws {
        guard case .captured(let label, let accounts) =
            try CaptureAck.decode(#"{"result":"captured","label":"work","accounts":3}"#)
        else { return XCTFail("expected .captured") }
        XCTAssertEqual(label, "work")
        XCTAssertEqual(accounts, 3)
    }

    // MARK: - CaptureAck decoder: hard errors (mirror serde's unknown-variant / malformed rejection)

    func testDecodeUnknownResultThrows() {
        XCTAssertThrowsError(try CaptureAck.decode(#"{"result":"future_state"}"#)) { error in
            guard case .unrecognized? = error as? CaptureAck.DecodeError else {
                return XCTFail("expected .unrecognized, got \(error)")
            }
        }
    }

    func testDecodeUnknownRejectionReasonThrows() {
        XCTAssertThrowsError(try CaptureAck.decode(#"{"result":"rejected","reason":"future-reason"}"#)) { error in
            guard case .unrecognized? = error as? CaptureAck.DecodeError else {
                return XCTFail("expected .unrecognized, got \(error)")
            }
        }
    }

    // A `captured` ack that omits the load-bearing `label` is malformed → the decoder's OWN `.unrecognized`,
    // never a raw `DecodingError` (so a caller catches one error type). `rejected` without a reason too.
    func testDecodeMalformedBodyThrowsUnrecognized() {
        for line in [
            #"{"result":"captured","accounts":2}"#,   // missing `label`
            #"{"result":"rejected"}"#,                 // missing `reason`
        ] {
            XCTAssertThrowsError(try CaptureAck.decode(line), "line: \(line)") { error in
                guard case .unrecognized? = error as? CaptureAck.DecodeError else {
                    return XCTFail("expected .unrecognized for \(line), got \(error)")
                }
            }
        }
    }

    func testDecodeNonJSONThrows() {
        XCTAssertThrowsError(try CaptureAck.decode("not json")) { error in
            XCTAssertEqual(error as? CaptureAck.DecodeError, .notJSON)
        }
    }

    func testDecodeNeitherResultNorErrorThrows() {
        XCTAssertThrowsError(try CaptureAck.decode(#"{"unrelated":true}"#)) { error in
            guard case .unrecognized? = error as? CaptureAck.DecodeError else {
                return XCTFail("expected .unrecognized, got \(error)")
            }
        }
    }

    // MARK: - CaptureCommand serialization (the #359 wire: label present, or OMITTED when blank)

    func testCaptureCommandSerializesLabelWhenPresent() throws {
        XCTAssertEqual(try encode(CaptureCommand(label: "work")), #"{"cmd":"capture","label":"work"}"#)
    }

    // A `nil` label OMITS the key (not an empty string) — so the daemon derives the handle from the UUID
    // (never the email — #15/#134).
    func testCaptureCommandOmitsLabelWhenNil() throws {
        XCTAssertEqual(try encode(CaptureCommand(label: nil)), #"{"cmd":"capture"}"#)
    }

    // The command bytes carry no secret (redacted channel by construction, issue #15).
    func testCaptureCommandCarriesNoSecret() throws {
        let line = try encode(CaptureCommand(label: "work"))
        XCTAssertFalse(line.contains("@"), "no email in the command bytes")
        XCTAssertFalse(line.lowercased().contains("token"), "no token in the command bytes")
    }

    // MARK: - StatusPanelFormat: capture copy (pure, tested in isolation)

    func testCaptureDoneTextEchoesTheAssignedLabel() {
        XCTAssertEqual(StatusPanelFormat.captureDoneText(label: "Work"), "Captured \u{2018}Work\u{2019}")
        XCTAssertEqual(StatusPanelFormat.captureDoneText(label: "a1b2c3"), "Captured \u{2018}a1b2c3\u{2019}")
    }

    func testCapturePendingText() {
        XCTAssertEqual(StatusPanelFormat.capturePendingText, "Capturing…")
    }

    func testCaptureErrorTextMapsEveryFailureToHumanCopy() {
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.rejected(.noActiveAccount)),
                       "No active account — run claude /login, then capture.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.rejected(.keychainLocked)),
                       "Keychain is locked — unlock it, then try again.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.rejected(.swapLockBusy)),
                       "The daemon is busy — try again in a moment.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.rejected(.failed)),
                       "Capture failed — try again.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.daemonError("unauthorized")),
                       "Not authorized to capture.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.daemonError("weird")),
                       "Capture failed — try again.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.transport(.connectionRefused(reason: "x"))),
                       "The daemon isn’t running.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.transport(.timedOut)),
                       "The daemon didn’t respond — try again.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.transport(.closedBeforeAck)),
                       "The daemon closed the connection — try again.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.undecodable),
                       "Unexpected reply from the daemon.")
        XCTAssertEqual(StatusPanelFormat.captureErrorText(.unavailable),
                       "The daemon socket is unreachable.")
    }

    // MARK: - AccountCaptureModel: phase transitions (final states, awaited)

    @MainActor
    func testCaptureReachesDoneEchoingDaemonLabel() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"captured","label":"work","accounts":2}"#)
        let model = AccountCaptureModel(client: client(conn))
        await model.capture(rawLabel: "work")
        XCTAssertEqual(model.phase, .done(label: "work"))
    }

    // A blank label → the OMITTED-key command (daemon derives the handle), and the done phase echoes the
    // daemon-ASSIGNED label — never a fabricated one.
    @MainActor
    func testBlankLabelOmitsKeyAndDoneEchoesDaemonHandle() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"captured","label":"a1b2c3"}"#)
        let model = AccountCaptureModel(client: client(conn))
        await model.capture(rawLabel: "   ")
        XCTAssertEqual(conn.sentStrings, [#"{"cmd":"capture"}"# + "\n"])
        XCTAssertEqual(model.phase, .done(label: "a1b2c3"))
    }

    @MainActor
    func testRejectedAckBecomesFailed() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"rejected","reason":"no-active-account"}"#)
        let model = AccountCaptureModel(client: client(conn))
        await model.capture(rawLabel: "work")
        XCTAssertEqual(model.phase, .failed(.rejected(.noActiveAccount)))
    }

    @MainActor
    func testSharedErrorAckBecomesFailed() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"error":"unauthorized"}"#)
        let model = AccountCaptureModel(client: client(conn))
        await model.capture(rawLabel: "work")
        XCTAssertEqual(model.phase, .failed(.daemonError("unauthorized")))
    }

    @MainActor
    func testTransportFailureBecomesFailed() async {
        let model = AccountCaptureModel(
            client: ControlCommandClient(connector: CommandFakeConnector(.fail("ECONNREFUSED")), timeout: .seconds(5)))
        await model.capture(rawLabel: "work")
        XCTAssertEqual(model.phase, .failed(.transport(.connectionRefused(reason: "ECONNREFUSED"))))
    }

    // A well-formed but off-contract ack (a `{"ok":true}` that matches no capture shape) → `.undecodable`,
    // a loud degrade rather than a silent mis-read.
    @MainActor
    func testUndecodableAckBecomesFailed() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"ok":true}"#)
        let model = AccountCaptureModel(client: client(conn))
        await model.capture(rawLabel: "work")
        XCTAssertEqual(model.phase, .failed(.undecodable))
    }

    // No client (socket path unresolved) → `.unavailable`, never a dead button that pretends to work.
    @MainActor
    func testNoClientBecomesUnavailable() async {
        let model = AccountCaptureModel(client: nil)
        await model.capture(rawLabel: "work")
        XCTAssertEqual(model.phase, .failed(.unavailable))
    }

    // MARK: - AccountCaptureModel: the in-flight window (the panel-retain gate depends on `isBusy`)

    @MainActor
    func testCaptureIsBusyWhilePendingThenClearsOnDone() async throws {
        let conn = GatedCaptureConnection(ack: #"{"result":"captured","label":"work"}"#)
        let model = AccountCaptureModel(
            client: ControlCommandClient(connector: OneShotConnector(connection: conn), timeout: .seconds(10)))

        let task = Task { await model.capture(rawLabel: "work") }
        try await waitUntil({ model.phase.isPending }, "pending")
        XCTAssertTrue(model.isBusy, "an in-flight capture keeps the panel retained")

        conn.release()
        await task.value
        XCTAssertEqual(model.phase, .done(label: "work"))
        XCTAssertFalse(model.isBusy, "a done capture (not editing) is dismissible again")
    }

    // MARK: - AccountCaptureModel: editing gating (the other half of `isBusy`)

    @MainActor
    func testEditingMakesBusyAndFocusRequestsPanelKey() {
        let model = AccountCaptureModel(client: nil)
        var keyRequests = 0
        model.panelKeyRequest = { keyRequests += 1 }

        XCTAssertFalse(model.isBusy)
        model.setEditing(true)
        XCTAssertTrue(model.isBusy, "a mid-edit label keeps the panel retained")
        XCTAssertEqual(keyRequests, 1, "focusing re-asserts the panel key so keystrokes land")

        model.setEditing(false)
        XCTAssertFalse(model.isBusy)
    }

    // A fresh edit clears a lingering error so the operator starts from a clean field.
    @MainActor
    func testFocusClearsAPriorError() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"rejected","reason":"keychain-locked"}"#)
        let model = AccountCaptureModel(client: client(conn))
        await model.capture(rawLabel: "work")
        XCTAssertEqual(model.phase, .failed(.rejected(.keychainLocked)))

        model.setEditing(true)
        XCTAssertEqual(model.phase, .idle, "focusing the field clears the prior error")
    }

    // Esc (cancelEditing) drops focus + clears back to idle so an outside click can dismiss again.
    @MainActor
    func testCancelEditingResetsToIdle() {
        let model = AccountCaptureModel(client: nil)
        model.setEditing(true)
        XCTAssertTrue(model.isBusy)
        model.cancelEditing()
        XCTAssertFalse(model.isBusy)
        XCTAssertEqual(model.phase, .idle)
    }

    // MARK: - AccountCaptureModel: the #394 capture-surface presentation flag

    // "Add account…" (the status-item menu path) requests the capture surface; the panel observes the flag
    // and presents the capture card over the populated roster.
    @MainActor
    func testRequestCaptureSurfaceSetsFlag() {
        let model = AccountCaptureModel(client: nil)
        XCTAssertFalse(model.captureSurfaceRequested)
        model.requestCaptureSurface()
        XCTAssertTrue(model.captureSurfaceRequested)
    }

    // Every panel close dismisses the surface, so a menu-summoned capture mode never outlives the panel —
    // the next open shows the normal roster.
    @MainActor
    func testDismissCaptureSurfaceClearsFlag() {
        let model = AccountCaptureModel(client: nil)
        model.requestCaptureSurface()
        model.dismissCaptureSurface()
        XCTAssertFalse(model.captureSurfaceRequested)
    }

    // Dismissing the surface also releases the outside-click retain predicate: a focused-but-idle field
    // left `isEditing == true` would otherwise stick `isBusy` on the reopened roster panel.
    @MainActor
    func testDismissCaptureSurfaceReleasesEditingRetain() {
        let model = AccountCaptureModel(client: nil)
        model.requestCaptureSurface()
        model.setEditing(true)
        XCTAssertTrue(model.isBusy)
        model.dismissCaptureSurface()
        XCTAssertFalse(model.isBusy, "the reopened roster panel is dismissible again")
        XCTAssertEqual(model.phase, .idle)
    }

    // A dismiss while a capture is in flight clears the surface flag but leaves the pending capture to run
    // to completion (its row still arrives via the watch snapshot) — never a torn-off in-flight write.
    @MainActor
    func testDismissCaptureSurfaceLeavesInFlightCaptureRunning() async throws {
        let conn = GatedCaptureConnection(ack: #"{"result":"captured","label":"work"}"#)
        let model = AccountCaptureModel(
            client: ControlCommandClient(connector: OneShotConnector(connection: conn), timeout: .seconds(10)))
        model.requestCaptureSurface()

        let task = Task { await model.capture(rawLabel: "work") }
        try await waitUntil({ model.phase.isPending }, "pending")

        model.dismissCaptureSurface()
        XCTAssertFalse(model.captureSurfaceRequested, "the surface flag clears immediately")
        XCTAssertTrue(model.phase.isPending, "but the in-flight capture is NOT cancelled")

        conn.release()
        await task.value
        XCTAssertEqual(model.phase, .done(label: "work"), "the capture ran to completion")
    }

    // MARK: - Helpers

    private func encode(_ command: CaptureCommand) throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        return String(decoding: try encoder.encode(command), as: UTF8.self)
    }

    @MainActor
    private func client(_ connection: CommandFakeConnection) -> ControlCommandClient {
        ControlCommandClient(connector: CommandFakeConnector(.succeed(connection)), timeout: .seconds(5))
    }

    /// Spin the cooperative executor until `predicate` holds (bounded), so a wiring bug fails the test
    /// instead of hanging. `@MainActor`, so it reads the model on its own actor without a hop.
    @MainActor
    private func waitUntil(_ predicate: () -> Bool, _ label: String) async throws {
        for _ in 0..<10_000 {
            if predicate() { return }
            await Task.yield()
        }
        XCTFail("timed out waiting for \(label)")
    }
}

// MARK: - Test doubles (a gated one-shot connection for the in-flight window)

/// Returns one pre-built `WatchConnection` — the any-`WatchConnection` analogue of the transport suite's
/// `CommandFakeConnector` (which is typed to `CommandFakeConnection`).
private struct OneShotConnector: WatchConnector {
    let connection: WatchConnection
    func connect() throws -> WatchConnection { connection }
}

/// A one-shot control-command connection that HOLDS its ack until `release()` — so a test can observe the
/// model's `.pending` / `isBusy` window before the ack resolves it. `send` records nothing extra; the
/// stream stays open until `release()` yields the canned ack (then finishes) or `close()` finishes it.
private final class GatedCaptureConnection: WatchConnection, @unchecked Sendable {
    let lines: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private let ack: String

    init(ack: String) {
        self.ack = ack
        (lines, continuation) = AsyncStream<String>.makeStream()
    }

    func send(_ bytes: [UInt8]) throws {}   // hold — do not ack until release()
    func release() { continuation.yield(ack); continuation.finish() }
    func close() { continuation.finish() }
}
