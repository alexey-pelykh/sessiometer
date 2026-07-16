// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Hermetic tests for the Settings window's model (issue #268): the `config-get` load → draft population, the
// dirty diff, the batched `config-set` apply (only-edited keys), client-side format validation vs the daemon's
// validation authority, and the app-local notification toggle. Each maps to an acceptance criterion.
//
// The model is driven by the SAME in-process fake connection the swap/stats models use
// (`CommandFakeConnection` / `CommandFakeConnector` from `ControlCommandTransportTests`) — NO real socket, NO
// live daemon — so a test run can NEVER perform a real `config-set` against the operator's `config.toml`.
// `ScriptedCommandConnector` hands a load reply then an apply reply to ONE model over its two one-shot sends,
// and records every command line so the "only dirty keys travel / no credential surface" safety AC is pinned.

import Foundation
import os
import XCTest

@MainActor
final class SettingsModelTests: XCTestCase {

    private let uuidWork = "11111111-1111-1111-1111-111111111111"
    private let uuidPersonal = "22222222-2222-2222-2222-222222222222"

    // MARK: - config-get load (AC 1: the form populates from the daemon's ConfigView)

    /// AC 1: `config-get` populates every tunable draft (all 14, distinct values catch a mis-mapped field) and
    /// the roster's label drafts + read-only `enabled`, and the freshly-loaded form is NOT dirty.
    func testLoadPopulatesEveryDraftFromConfigView() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic])
        await model.load()

        guard case .loaded = model.loadPhase else { return XCTFail("expected .loaded, got \(model.loadPhase)") }
        XCTAssertEqual(model.draft(for: .pollSecs), "300")
        XCTAssertEqual(model.draft(for: .exhaustedPollSecs), "3600")
        XCTAssertEqual(model.draft(for: .nearLimitPollSecs), "120")
        XCTAssertEqual(model.draft(for: .cooldownSecs), "45")
        XCTAssertEqual(model.draft(for: .targetMaxSessionUsage), "85")
        XCTAssertEqual(model.draft(for: .sessionTrigger), "90")
        XCTAssertEqual(model.draft(for: .weeklyTrigger), "95")
        XCTAssertEqual(model.draft(for: .sessionBlindSwapSecs), "900")
        XCTAssertEqual(model.draft(for: .sessionBlindRiskBand), "80")
        XCTAssertEqual(model.draft(for: .sessionVelocityHorizonSecs), "150")
        XCTAssertEqual(model.draft(for: .sessionVelocityMinProjectAbove), "88")
        XCTAssertEqual(model.draft(for: .sessionVelocityEmaAlphaPct), "40")
        XCTAssertEqual(model.draft(for: .monitor401N), "3")
        XCTAssertEqual(model.draft(for: .monitorRecoveryM), "2")

        XCTAssertEqual(model.accounts.count, 2)
        XCTAssertEqual(model.labelDraft(for: uuidWork), "work")
        XCTAssertEqual(model.labelDraft(for: uuidPersonal), "personal")
        XCTAssertEqual(model.accounts.first { $0.accountUuid == uuidPersonal }?.enabled, false)

        XCTAssertFalse(model.isDirty, "a freshly loaded form has nothing to save")
    }

    /// AC 7 (read side): no `config.toml` → the `{"error":"no config"}` envelope surfaces honestly as a
    /// `.daemonError` (never a blank form, never a crash).
    func testLoadNoConfigSurfacesDaemonError() async {
        let (model, _) = makeModel(replies: [Fixtures.configGetNoConfig])
        await model.load()
        XCTAssertEqual(model.loadPhase, .failed(.daemonError("no config")))
    }

    func testLoadUnreadableSurfacesDaemonError() async {
        let (model, _) = makeModel(replies: [Fixtures.configGetUnreadable])
        await model.load()
        XCTAssertEqual(model.loadPhase, .failed(.daemonError("config unreadable")))
    }

    /// A drifted daemon (non-contract reply) degrades LOUDLY, never mis-rendered as a partial form.
    func testLoadUndecodableReplyFailsLoudly() async {
        let (model, _) = makeModel(replies: ["not json"])
        await model.load()
        XCTAssertEqual(model.loadPhase, .failed(.undecodable))
    }

    /// A roster carrying a DUPLICATE `account_uuid` (a drifted daemon) degrades loudly as `.undecodable` — it
    /// must NEVER trap the app on `Dictionary`'s unique-key precondition while building the label map.
    func testLoadDuplicateAccountUUIDDegradesLoudly() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewDuplicateAccount])
        await model.load()
        XCTAssertEqual(model.loadPhase, .failed(.undecodable))
    }

    /// AC 7: daemon down (connection refused) → honest-disconnected transport failure.
    func testLoadTransportFailureIsHonest() async {
        let model = SettingsModel(
            client: ControlCommandClient(connector: CommandFakeConnector(.fail("ECONNREFUSED")),
                                         timeout: .seconds(5)),
            preferences: ephemeralPreferences())
        await model.load()
        guard case .failed(.transport(.connectionRefused)) = model.loadPhase else {
            return XCTFail("expected .failed(.transport(.connectionRefused)), got \(model.loadPhase)")
        }
    }

    /// AC 7: no control client (sandboxed / socket unresolved) → `.unavailable`, never a dead form.
    func testLoadNoClientIsUnavailable() async {
        let model = SettingsModel(client: nil, preferences: ephemeralPreferences())
        await model.load()
        XCTAssertEqual(model.loadPhase, .failed(.unavailable))
    }

    // MARK: - dirty tracking

    func testEditingATunableMakesTheFormDirty() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic])
        await model.load()
        model.setDraft("120", for: .pollSecs)
        XCTAssertTrue(model.isDirty)
        XCTAssertEqual(model.dirtyTunableFields, [.pollSecs])
    }

    /// Re-typing the same value (even with stray whitespace) is NOT dirty — the diff is canonical.
    func testRetypingTheSameValueIsNotDirty() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic])
        await model.load()
        model.setDraft(" 300 ", for: .pollSecs)
        XCTAssertFalse(model.isDirty, "whitespace around an unchanged value is not an edit")
        XCTAssertTrue(model.dirtyTunableFields.isEmpty)
    }

    /// A cosmetically different but numerically identical draft ("0300" == 300) is NOT dirty — the diff is by
    /// value, so a leading zero neither enables Save nor re-dirties a just-saved field.
    func testLeadingZeroDraftIsNotDirty() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic])
        await model.load()
        model.setDraft("0300", for: .pollSecs)
        XCTAssertFalse(model.isDirty, "0300 and 300 are the same value")
        XCTAssertTrue(model.dirtyTunableFields.isEmpty)
    }

    func testEditingALabelMakesTheFormDirty() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic])
        await model.load()
        model.setLabelDraft("renamed", for: uuidWork)
        XCTAssertTrue(model.isDirty)
        XCTAssertEqual(model.dirtyLabels, [uuidWork: "renamed"])
    }

    /// A label draft differing only by surrounding whitespace is NOT dirty (parity with the tunable diff).
    func testLabelWhitespaceIsNotDirty() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic])
        await model.load()
        model.setLabelDraft("work ", for: uuidWork)
        XCTAssertFalse(model.isDirty, "a trailing space on an unchanged label is not an edit")
        XCTAssertTrue(model.dirtyLabels.isEmpty)
    }

    // MARK: - config-set apply (AC 2/3/4)

    /// AC 2: a tunable edit sends ONLY the edited keys and renders `restart_required` (persistent banner).
    func testApplyTunableEditSendsOnlyDirtyKeysAndLatchesRestart() async {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedRestart])
        await model.load()
        model.setDraft("120", for: .pollSecs)
        model.setDraft("88", for: .sessionTrigger)
        await model.apply()

        XCTAssertEqual(model.applyPhase, .applied(effect: .restartRequired))
        XCTAssertTrue(model.restartPending, "a tunable change latches the restart banner")
        XCTAssertFalse(model.isDirty, "an applied edit rebaselines — the form is clean again")

        // ONLY the two edited tunables ride the wire; labels is empty. (Pins the batch-of-dirty-keys contract.)
        XCTAssertEqual(
            connector.sentLines.last,
            #"{"cmd":"config-set","labels":{},"tunables":{"poll_secs":120,"session_trigger":88}}"# + "\n")
    }

    /// AC 3: a label edit renders `live` (no restart) and sends ONLY the labels map.
    func testApplyLabelEditRendersLiveAndSendsOnlyLabels() async {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedLive])
        await model.load()
        model.setLabelDraft("day-job", for: uuidWork)
        await model.apply()

        XCTAssertEqual(model.applyPhase, .applied(effect: .live))
        XCTAssertFalse(model.restartPending, "a label-only change needs no restart")
        XCTAssertEqual(
            connector.sentLines.last,
            #"{"cmd":"config-set","labels":{"\#(uuidWork)":"day-job"},"tunables":{}}"# + "\n")
    }

    /// A genuinely changed label with stray surrounding whitespace rides the wire TRIMMED — internal spaces
    /// preserved ("day job"), ends stripped (parity with the tunable normalization).
    func testLabelEditIsTrimmedOnTheWire() async {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedLive])
        await model.load()
        model.setLabelDraft("  day job  ", for: uuidWork)
        await model.apply()
        XCTAssertEqual(
            connector.sentLines.last,
            #"{"cmd":"config-set","labels":{"\#(uuidWork)":"day job"},"tunables":{}}"# + "\n")
    }

    /// AC 4: an out-of-range / cross-field edit is the DAEMON's to reject — the model renders `invalid` + the
    /// field-naming `detail`, keeps the edit for a retry, and (the daemon wrote nothing) the form stays dirty.
    func testApplyDaemonRejectsInvalidWithDetailAndNoWrite() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetRejectedInvalid])
        await model.load()
        model.setDraft("7200", for: .exhaustedPollSecs)
        await model.apply()

        XCTAssertEqual(
            model.applyPhase,
            .rejected(reason: .invalid, detail: "exhausted_poll_secs (3600) must be >= poll_secs (7200)"))
        XCTAssertTrue(model.isDirty, "a rejected edit is NOT rebaselined — no partial write, edit kept for retry")
    }

    /// A stale label edit (uuid no longer in the roster) → `unknown-account`, no `detail`.
    func testApplyDaemonRejectsUnknownAccount() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetRejectedUnknownAccount])
        await model.load()
        model.setLabelDraft("ghost", for: uuidWork)
        await model.apply()
        XCTAssertEqual(model.applyPhase, .rejected(reason: .unknownAccount, detail: nil))
    }

    /// AC 4 (client side): a non-numeric dirty draft is caught BEFORE any command is sent — inline field
    /// error, `invalidInput`, and crucially NO `config-set` on the wire (no partial write).
    func testClientFormatErrorBlocksTheSendEntirely() async {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedLive])
        await model.load()
        model.setDraft("not-a-number", for: .cooldownSecs)
        await model.apply()

        XCTAssertEqual(model.applyPhase, .invalidInput)
        XCTAssertNotNil(model.fieldErrors[.cooldownSecs], "the offending field is flagged inline")
        XCTAssertEqual(connector.sentLines.count, 1, "only config-get was sent — the bad config-set never left")
        XCTAssertTrue(connector.sentLines.allSatisfy { $0.contains("config-get") })
    }

    /// A negative number is also a format error client-side (the tunables are an unsigned domain) — not sent.
    func testNegativeDraftIsAFormatErrorAndNotSent() async {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedLive])
        await model.load()
        model.setDraft("-5", for: .pollSecs)
        await model.apply()
        XCTAssertEqual(model.applyPhase, .invalidInput)
        XCTAssertEqual(connector.sentLines.count, 1)
    }

    /// Editing a flagged field clears its inline error immediately (fix-as-you-type) AND, once the last error
    /// is gone, drops the `invalidInput` banner so it never lingers pointing at nothing.
    func testEditingAFlaggedFieldClearsItsError() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedLive])
        await model.load()
        model.setDraft("nope", for: .pollSecs)
        await model.apply()
        XCTAssertNotNil(model.fieldErrors[.pollSecs])
        XCTAssertEqual(model.applyPhase, .invalidInput)
        model.setDraft("300", for: .pollSecs)
        XCTAssertNil(model.fieldErrors[.pollSecs], "editing the field clears the stale format error")
        XCTAssertEqual(model.applyPhase, .idle, "clearing the last field error also drops the invalidInput banner")
    }

    /// AC 7: apply with no client → `.unavailable` (honest, never a silent local config write).
    func testApplyNoClientIsUnavailable() async {
        let model = SettingsModel(client: nil, preferences: ephemeralPreferences())
        // No load (no client), but force a dirty-looking apply — it must short-circuit before any wire work.
        await model.apply()
        XCTAssertEqual(model.applyPhase, .failed(.unavailable))
    }

    /// A drifted daemon's non-contract ack degrades loudly rather than being mis-read as success.
    func testApplyUndecodableAckFailsLoudly() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic, #"{"result":"teleported"}"#])
        await model.load()
        model.setDraft("120", for: .pollSecs)
        await model.apply()
        XCTAssertEqual(model.applyPhase, .failed(.undecodable))
    }

    // MARK: - safety boundary (AC 5/6): only tunables + labels can travel

    /// The write surface is exactly `{cmd, tunables, labels}` — a full edit (a tunable AND a label) carries no
    /// `enabled`, no credential, no roster-structure key. The type makes them unrepresentable; this pins that
    /// the model never smuggles one onto the wire.
    func testTheWriteSurfaceCarriesNoCredentialOrRosterStructure() async {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedRestart])
        await model.load()
        model.setDraft("120", for: .pollSecs)
        model.setLabelDraft("renamed", for: uuidWork)
        await model.apply()

        let line = connector.sentLines.last ?? ""
        for forbidden in ["\"enabled\"", "credential", "token", "oauth", "password", "\"accounts\"", "account_uuid"] {
            XCTAssertFalse(line.contains(forbidden), "the config-set surface must never carry \(forbidden): \(line)")
        }
    }

    // MARK: - notification toggle (own section, immediate-apply UserDefaults — NOT the config-set batch)

    func testNotificationToggleWritesThroughToPreferences() {
        let prefs = ephemeralPreferences()
        let model = SettingsModel(client: nil, preferences: prefs)
        model.notificationsEnabled = false
        XCTAssertFalse(prefs.isEnabled, "the toggle writes through immediately")
        model.notificationsEnabled = true
        XCTAssertTrue(prefs.isEnabled)
    }

    func testEnablingFromOffRequestsAuthorizationExactlyOnce() {
        let prefs = ephemeralPreferences()
        prefs.isEnabled = false
        let spy = AuthSpy()
        let model = SettingsModel(client: nil, preferences: prefs, onRequestAuthorization: { spy.count += 1 })

        XCTAssertFalse(model.notificationsEnabled, "initialized from the off preference")
        XCTAssertEqual(spy.count, 0, "init never asks for authorization")

        model.notificationsEnabled = true
        XCTAssertEqual(spy.count, 1, "enabling from off asks the OS for permission")
        model.notificationsEnabled = true
        XCTAssertEqual(spy.count, 1, "re-setting the same on value does not re-ask")
        model.notificationsEnabled = false
        model.notificationsEnabled = true
        XCTAssertEqual(spy.count, 2, "a fresh off→on asks again")
    }

    func testDisablingNeverRequestsAuthorization() {
        let prefs = ephemeralPreferences()
        prefs.isEnabled = true
        let spy = AuthSpy()
        let model = SettingsModel(client: nil, preferences: prefs, onRequestAuthorization: { spy.count += 1 })
        model.notificationsEnabled = false
        XCTAssertEqual(spy.count, 0, "turning notifications off never prompts")
    }

    /// The toggle is NOT part of the config-set batch — flipping it sends nothing on the control socket.
    func testTogglingNotificationsSendsNoControlCommand() {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic])
        model.notificationsEnabled.toggle()
        XCTAssertTrue(connector.sentLines.isEmpty, "the notification toggle never touches the config wire")
        XCTAssertEqual(model.applyPhase, .idle)
    }

    // MARK: - Helpers

    private func makeModel(
        replies: [String?],
        onRequestAuthorization: (@MainActor () -> Void)? = nil
    ) -> (SettingsModel, ScriptedCommandConnector) {
        let connector = ScriptedCommandConnector(replies)
        let model = SettingsModel(
            client: ControlCommandClient(connector: connector, timeout: .seconds(5)),
            preferences: ephemeralPreferences(),
            onRequestAuthorization: onRequestAuthorization)
        return (model, connector)
    }

    /// A per-test volatile `UserDefaults` so a toggle write never touches the real domain or another test.
    private func ephemeralPreferences() -> NotificationPreferences {
        let suite = "org.sessiometer.menubar.settings-tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return NotificationPreferences(defaults: defaults)
    }
}

// MARK: - Test doubles

/// A `@MainActor` counter for the authorization hook (the model is `@MainActor`, so the hook is too).
@MainActor
private final class AuthSpy {
    var count = 0
}

/// Hands out a fresh `CommandFakeConnection` per `connect()`, each pre-loaded with the NEXT scripted ack — so
/// ONE `SettingsModel` can `load()` (reply 0) then `apply()` (reply 1) over the production one-shot-per-send
/// shape. Aggregates every sent command line across connections so the safety AC can inspect the wire. A
/// `nil` reply models a connection that never answers (unused by the passing tests).
private final class ScriptedCommandConnector: WatchConnector, @unchecked Sendable {
    private let replies: [String?]
    private let state = OSAllocatedUnfairLock(initialState: State())

    private struct State {
        var index = 0
        var connections: [CommandFakeConnection] = []
    }

    init(_ replies: [String?]) { self.replies = replies }

    func connect() throws -> WatchConnection {
        state.withLock { state in
            let reply = state.index < replies.count ? replies[state.index] : nil
            state.index += 1
            let connection = CommandFakeConnection(ackOnSend: reply)
            state.connections.append(connection)
            return connection
        }
    }

    /// Every command line sent across all connections, in order (each includes its trailing newline).
    var sentLines: [String] { state.withLock { $0.connections.flatMap { $0.sentStrings } } }
}
