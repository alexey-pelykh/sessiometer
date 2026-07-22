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

    /// AC 1: `config-get` populates every tunable draft (all 15, distinct values catch a mis-mapped field) and
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
        XCTAssertEqual(model.draft(for: .sessionCeiling), "90")
        XCTAssertEqual(model.draft(for: .weeklyCeiling), "95")
        XCTAssertEqual(model.draft(for: .sessionBlindSwapSecs), "900")
        XCTAssertEqual(model.draft(for: .sessionBlindRiskBand), "80")
        XCTAssertEqual(model.draft(for: .sessionVelocityHorizonSecs), "150")
        XCTAssertEqual(model.draft(for: .sessionVelocityMinProjectAbove), "88")
        XCTAssertEqual(model.draft(for: .sessionVelocityEmaAlphaPct), "40")
        XCTAssertEqual(model.draft(for: .monitor401N), "3")
        XCTAssertEqual(model.draft(for: .monitorRecoveryM), "2")
        XCTAssertEqual(model.draft(for: .fleetRunwayWarnSecs), "7200")

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
        model.setDraft("88", for: .sessionCeiling)
        await model.apply()

        XCTAssertEqual(model.applyPhase, .applied(effect: .restartRequired))
        XCTAssertTrue(model.restartPending, "a tunable change latches the restart banner")
        XCTAssertFalse(model.isDirty, "an applied edit rebaselines — the form is clean again")

        // ONLY the two edited tunables ride the wire; labels is empty. (Pins the batch-of-dirty-keys contract.)
        XCTAssertEqual(
            connector.sentLines.last,
            #"{"cmd":"config-set","labels":{},"tunables":{"poll_secs":120,"session_ceiling":88}}"# + "\n")
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

    /// A genuinely-changed label carrying a TRAILING SPACE rebaselines to its trimmed form after apply — the
    /// round-trip leaves the form CLEAN (the stray space is not re-flagged dirty) and canonicalizes the draft
    /// itself to the trimmed value. Pins `rebaselineFromDrafts`' label path directly (previously only indirect).
    func testChangedLabelWithTrailingSpaceIsCleanAfterApply() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedLive])
        await model.load()
        model.setLabelDraft("day-job ", for: uuidWork)  // changed (work → day-job) AND a trailing space
        XCTAssertTrue(model.isDirty, "a genuine label change is dirty before apply")

        await model.apply()

        XCTAssertEqual(model.applyPhase, .applied(effect: .live))
        XCTAssertFalse(model.isDirty, "after apply the trimmed label is the baseline — the stray space isn't re-dirty")
        XCTAssertEqual(model.labelDraft(for: uuidWork), "day-job", "the draft is canonicalized to its trimmed form")
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

    /// AC 1 ("0 = off" affordance) + AC 2: editing `fleet_runway_warn_secs` (issues #650/#692) to 0 — the
    /// operator's way to DISABLE the proactive fleet-runway warning — is an ordinary tunable edit. It rides the
    /// batched `config-set` under its snake_case key (NO local `config.toml` write), and `0` is carried
    /// EXPLICITLY, never dropped as if unset. The daemon owns the band; the app only delivers the number.
    func testApplyFleetRunwayWarnZeroDisablesAndSends() async {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedRestart])
        await model.load()
        model.setDraft("0", for: .fleetRunwayWarnSecs)  // 0 = off; the fixture baseline is 7200, so this is dirty
        XCTAssertTrue(model.isDirty)
        await model.apply()

        XCTAssertEqual(model.applyPhase, .applied(effect: .restartRequired))
        XCTAssertEqual(
            connector.sentLines.last,
            #"{"cmd":"config-set","labels":{},"tunables":{"fleet_runway_warn_secs":0}}"# + "\n")
    }

    /// AC 3: an out-of-band `fleet_runway_warn_secs` (30 — inside the forbidden `0 < n < 60` gap) is the
    /// DAEMON's to reject. The model surfaces the daemon's OWN field-naming `detail` (the `0 | 60..=2_592_000`
    /// message from `Config::validate`), NOT a generic `.undecodable` (the #645 precedent), and keeps the edit
    /// for a retry (the daemon wrote nothing, so the form stays dirty).
    func testApplyFleetRunwayOutOfBandRejectedWithDaemonDetail() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetRejectedFleetRunwayInvalid])
        await model.load()
        model.setDraft("30", for: .fleetRunwayWarnSecs)
        await model.apply()

        XCTAssertEqual(
            model.applyPhase,
            .rejected(
                reason: .invalid,
                detail: "fleet_runway_warn_secs must be 0 (disabled) or in 60..=2592000, got 30"))
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

    /// A draft in `(Int64.max, UInt64.max]` parses as a whole `UInt64` but overflows the `Int64` wire — it is
    /// blocked client-side with its OWN "too large" message (NOT the misleading "0 or greater" format error),
    /// and, like every client-side rejection, never reaches the wire (`UInt64.max` here — 20 digits).
    func testOverlargeDraftIsTooLargeAndNotSent() async {
        let (model, connector) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetAppliedLive])
        await model.load()
        model.setDraft("18446744073709551615", for: .pollSecs)  // UInt64.max — a whole number, but > Int64.max
        await model.apply()

        XCTAssertEqual(model.applyPhase, .invalidInput)
        XCTAssertEqual(model.fieldErrors[.pollSecs], "That number is too large.")
        XCTAssertEqual(connector.sentLines.count, 1, "the over-large config-set never left the client")
        XCTAssertTrue(connector.sentLines.allSatisfy { $0.contains("config-get") })
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

    /// Issue #645 (the #628 client half): a version-skewed apply — the daemon refuses a renamed/stale tunable
    /// with the redacted `{"error":…,"detail":…}` envelope (NOT a `ConfigSetAck`) — surfaces the key-naming
    /// `detail` as a `.daemonError`, NOT the opaque `.undecodable` the missing-`result` decode used to yield.
    /// The daemon wrote nothing, so the edit is kept (form stays dirty) for a fix + retry.
    func testApplyDaemonErrorEnvelopeSurfacesStaleKeyDetail() async {
        let (model, _) = makeModel(replies: [Fixtures.configViewBasic, Fixtures.configSetErrorStaleKey])
        await model.load()
        model.setDraft("120", for: .pollSecs)
        await model.apply()

        guard case .failed(.daemonError(let message)) = model.applyPhase else {
            return XCTFail("expected .failed(.daemonError), got \(model.applyPhase)")
        }
        XCTAssertTrue(
            message.contains("session_trigger"),
            "the daemon's key-naming detail is surfaced, not swallowed: \(message)")
        XCTAssertTrue(model.isDirty, "the daemon wrote nothing — the edit is kept for a retry")
    }

    /// Re-entrancy (mirrors `AccountSwapModel.swap`): two OVERLAPPING `apply()` calls collapse to a SINGLE
    /// `config-set`. `apply` latches `.applying` in its synchronous prefix — before its first `await` — so on
    /// the serial `@MainActor` the second submit observes the in-flight guard and is a no-op. A
    /// `GatedApplyConnector` PINS the first apply in that in-flight window (its `send` blocks until released),
    /// making the overlap deterministic rather than a race against the synchronous fake. Guards a rapid double
    /// Cmd-S from spawning two writes before the view's `saveEnabled` disable (an async re-render) lands.
    func testOverlappingApplyCollapsesToASingleConfigSet() async {
        let connector = GatedApplyConnector(loadReply: Fixtures.configViewBasic,
                                            applyReply: Fixtures.configSetAppliedLive)
        let model = SettingsModel(
            client: ControlCommandClient(connector: connector, timeout: .seconds(5)),
            preferences: ephemeralPreferences())
        await model.load()
        model.setDraft("120", for: .pollSecs)

        // The gated `send` holds the first apply in `.applying` and will NOT advance until released, so
        // yielding until we observe `.applying` converges (no transient-state miss on the serial actor).
        async let firstApply: Void = model.apply()
        while model.applyPhase != .applying { await Task.yield() }

        await model.apply()  // the overlapping submit — observes `.applying`, returns immediately (the guard)
        XCTAssertEqual(model.applyPhase, .applying, "the second apply neither superseded nor cleared the first")

        connector.release()  // let the one in-flight config-set complete
        await firstApply

        XCTAssertEqual(model.applyPhase, .applied(effect: .live))
        XCTAssertFalse(model.isDirty, "the single apply rebaselined")
        XCTAssertEqual(connector.connectCount, 2, "load + exactly ONE config-set connect — the overlap was a no-op")
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

/// A `WatchConnector` for the re-entrancy test: the FIRST connect (the load) answers immediately; the SECOND
/// (the apply) hands back a connection whose `send` BLOCKS until `release()` — pinning that `apply()` in its
/// in-flight `.applying` window so an overlapping `apply()` is provably a no-op, WITHOUT racing the scheduler.
/// `connectCount` lets the test assert exactly one config-set reached the transport (a leaked second send
/// would be a third connect). The gate blocks the client's detached connect task, never the `@MainActor`.
private final class GatedApplyConnector: WatchConnector, @unchecked Sendable {
    private let loadReply: String?
    private let applyReply: String?
    private let gate = DispatchSemaphore(value: 0)
    private let state = OSAllocatedUnfairLock(initialState: 0)  // connect count

    init(loadReply: String?, applyReply: String?) {
        self.loadReply = loadReply
        self.applyReply = applyReply
    }

    func connect() throws -> WatchConnection {
        let index = state.withLock { count -> Int in defer { count += 1 }; return count }
        return index == 0
            ? CommandFakeConnection(ackOnSend: loadReply)          // load: answers immediately
            : GatedCommandConnection(ack: applyReply, gate: gate)  // apply: `send` blocks until release()
    }

    /// Open the gate so the pinned apply's `send` proceeds and the exchange completes.
    func release() { gate.signal() }

    /// How many connections were opened — load + one per apply that actually reached the wire.
    var connectCount: Int { state.withLock { $0 } }
}

/// A one-shot control-command connection whose `send` BLOCKS on a gate until the test releases it — used to
/// pin an `apply()` in its `.applying` window (see `GatedApplyConnector`). After the gate opens it records the
/// bytes and answers with its scripted ack, exactly like `CommandFakeConnection`. `send` runs on the client's
/// DETACHED connect task, so the blocking wait never stalls the `@MainActor` under test.
private final class GatedCommandConnection: WatchConnection, @unchecked Sendable {
    let lines: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private let ack: String?
    private let gate: DispatchSemaphore
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State { var sent: [[UInt8]] = []; var finished = false }

    init(ack: String?, gate: DispatchSemaphore) {
        self.ack = ack
        self.gate = gate
        (lines, continuation) = AsyncStream<String>.makeStream()
    }

    func send(_ bytes: [UInt8]) throws {
        gate.wait()  // hold the exchange open until release() — on the detached connect task, not the @MainActor
        state.withLock { $0.sent.append(bytes) }
        if let ack { continuation.yield(ack) }
    }

    func close() {
        let finish = state.withLock { st -> Bool in
            if st.finished { return false }
            st.finished = true
            return true
        }
        if finish { continuation.finish() }
    }
}
