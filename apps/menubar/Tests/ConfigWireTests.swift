// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Wire tests for the `config-get` / `config-set` codecs (issue #268) — the config-editing sibling of
// `WireDecoderTests` (`watch`) / `StatsTests` (`stats`). Each test maps to an acceptance criterion of the
// settings UI. Pure `JSONDecoder` / `JSONEncoder`, so they run identically under `xcodebuild test` (CI)
// and any plain verifier. The fixtures live in `Fixtures.swift` (shared, no XCTest dependency).

import XCTest

final class ConfigWireTests: XCTestCase {

    // MARK: - config-get (AC 1: the form populates from the daemon's ConfigView)

    /// AC 1: `config-get` decodes to a `ConfigView` — all 14 tunables map to the right field (distinct
    /// values catch a swapped `CodingKey`), and the roster's non-secret per-account fields decode.
    func testDecodesConfigGetView() throws {
        guard case .ok(let view) = try decodeConfigGetReply(Fixtures.configViewBasic) else {
            return XCTFail("expected a ConfigView, got an error envelope")
        }
        let t = view.tunables
        XCTAssertEqual(t.pollSecs, 300)
        XCTAssertEqual(t.exhaustedPollSecs, 3600)
        XCTAssertEqual(t.nearLimitPollSecs, 120)
        XCTAssertEqual(t.cooldownSecs, 45)
        XCTAssertEqual(t.targetMaxSessionUsage, 85)
        XCTAssertEqual(t.sessionCeiling, 90)
        XCTAssertEqual(t.weeklyCeiling, 95)
        XCTAssertEqual(t.sessionBlindSwapSecs, 900)
        XCTAssertEqual(t.sessionBlindRiskBand, 80)
        XCTAssertEqual(t.sessionVelocityHorizonSecs, 150)
        XCTAssertEqual(t.sessionVelocityMinProjectAbove, 88)
        XCTAssertEqual(t.sessionVelocityEmaAlphaPct, 40)
        XCTAssertEqual(t.monitor401N, 3)
        XCTAssertEqual(t.monitorRecoveryM, 2)

        XCTAssertEqual(view.accounts.count, 2)
        XCTAssertEqual(
            view.accounts[0],
            AccountView(accountUuid: "11111111-1111-1111-1111-111111111111", label: "work", enabled: true))
        XCTAssertEqual(view.accounts[1].accountUuid, "22222222-2222-2222-2222-222222222222")
        XCTAssertEqual(view.accounts[1].label, "personal")
        XCTAssertFalse(view.accounts[1].enabled, "a parked account decodes enabled = false")
    }

    /// AC 7 (read side): `config-get` on a daemon with no `config.toml` → the `{"error":"no config"}`
    /// envelope, surfaced honestly (never a crash, never a blank form).
    func testDecodesConfigGetNoConfig() throws {
        XCTAssertEqual(try decodeConfigGetReply(Fixtures.configGetNoConfig), .error("no config"))
    }

    /// `config-get` when the file exists but does not parse → `{"error":"config unreadable"}`.
    func testDecodesConfigGetUnreadable() throws {
        XCTAssertEqual(try decodeConfigGetReply(Fixtures.configGetUnreadable), .error("config unreadable"))
    }

    /// A non-JSON `config-get` reply is a hard error (a drifted daemon degrades loudly).
    func testConfigGetNonJSONThrows() {
        XCTAssertThrowsError(try decodeConfigGetReply("not json"))
    }

    // MARK: - config-set ack (AC 2/3/4: restart_required / live / invalid effects)

    /// AC 2: a tunable edit is acknowledged `applied` with the `restart_required` effect.
    func testDecodesConfigSetAppliedRestartRequired() throws {
        XCTAssertEqual(
            try decodeConfigSetAck(Fixtures.configSetAppliedRestart), .applied(effect: .restartRequired))
    }

    /// AC 3: a label edit is acknowledged `applied` with the `live` effect (adopted without restart).
    func testDecodesConfigSetAppliedLive() throws {
        XCTAssertEqual(try decodeConfigSetAck(Fixtures.configSetAppliedLive), .applied(effect: .live))
    }

    /// A no-op submit (values equalled current) is acknowledged `applied` with `unchanged`.
    func testDecodesConfigSetAppliedUnchanged() throws {
        XCTAssertEqual(try decodeConfigSetAck(Fixtures.configSetAppliedUnchanged), .applied(effect: .unchanged))
    }

    /// AC 4: an out-of-range / cross-field edit is `rejected` with `invalid` and the field-naming `detail`.
    func testDecodesConfigSetRejectedInvalidWithDetail() throws {
        guard case .rejected(let reason, let detail) = try decodeConfigSetAck(Fixtures.configSetRejectedInvalid)
        else {
            return XCTFail("expected a rejection")
        }
        XCTAssertEqual(reason, .invalid)
        XCTAssertEqual(detail, "exhausted_poll_secs (3600) must be >= poll_secs (7200)")
    }

    /// A stale label edit is `rejected` with `unknown-account` and NO `detail` (absent for all but `invalid`).
    func testDecodesConfigSetRejectedUnknownAccountNoDetail() throws {
        XCTAssertEqual(
            try decodeConfigSetAck(Fixtures.configSetRejectedUnknownAccount),
            .rejected(reason: .unknownAccount, detail: nil))
    }

    /// Every `ConfigSetRejection` kebab-case wire value maps to its case (the daemon can send any of them).
    func testDecodesEveryRejectionReason() throws {
        let cases: [(String, ConfigSetRejection)] = [
            ("invalid", .invalid),
            ("unknown-account", .unknownAccount),
            ("no-config", .noConfig),
            ("config-unreadable", .configUnreadable),
            ("save-failed", .saveFailed),
            ("unavailable", .unavailable),
        ]
        for (wire, expected) in cases {
            let line = #"{"result":"rejected","reason":"\#(wire)"}"#
            XCTAssertEqual(try decodeConfigSetAck(line), .rejected(reason: expected, detail: nil), wire)
        }
    }

    /// A drifted daemon: an unknown `result` / `effect` / `reason` is a hard decode error (degrade loudly,
    /// never silently mis-render an outcome).
    func testConfigSetUnknownTagsThrow() {
        XCTAssertThrowsError(try decodeConfigSetAck(#"{"result":"teleported"}"#), "unknown result")
        XCTAssertThrowsError(
            try decodeConfigSetAck(#"{"result":"applied","effect":"telepathy"}"#), "unknown effect")
        XCTAssertThrowsError(
            try decodeConfigSetAck(#"{"result":"rejected","reason":"vibes"}"#), "unknown reason")
    }

    // MARK: - config-set request encode (AC 5/6: only the allow-listed surface can travel)

    /// The `config-set` request encodes ONLY the edited tunables (a `nil` field is OMITTED via Swift's
    /// synthesized `encodeIfPresent`) plus the labels map — the wire the daemon's `deny_unknown_fields`
    /// allow-list requires. SAFETY (AC 5/6): the request type cannot express a credential or a roster
    /// structure key, so this pins that the write surface is exactly `{tunables, labels}`.
    func testConfigSetRequestEncodesOnlyEditedKeys() throws {
        var tunables = SetTunables()
        tunables.pollSecs = 120
        tunables.sessionCeiling = 88
        let command = ConfigSetCommand(
            tunables: tunables, labels: ["11111111-1111-1111-1111-111111111111": "renamed"])

        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys  // deterministic — the same framing `ControlCommandClient` uses
        let json = String(decoding: try encoder.encode(command), as: UTF8.self)

        XCTAssertEqual(
            json,
            #"{"cmd":"config-set","labels":{"11111111-1111-1111-1111-111111111111":"renamed"},"tunables":{"poll_secs":120,"session_ceiling":88}}"#)
    }

    /// An all-empty submit encodes to empty `tunables` / `labels` objects — the no-op path the daemon
    /// answers with `unchanged`. Confirms the `nil`-omission leaves NO stray keys.
    func testConfigSetRequestEncodesEmptyWhenNoEdits() throws {
        let command = ConfigSetCommand(tunables: SetTunables(), labels: [:])
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let json = String(decoding: try encoder.encode(command), as: UTF8.self)
        XCTAssertEqual(json, #"{"cmd":"config-set","labels":{},"tunables":{}}"#)
    }

    /// `SetTunables.isEmpty` is true only when every field is unedited — the "labels-only / no-op" gate.
    func testSetTunablesIsEmpty() {
        XCTAssertTrue(SetTunables().isEmpty)
        var edited = SetTunables()
        edited.monitorRecoveryM = 5
        XCTAssertFalse(edited.isEmpty)
    }

    /// The `config-get` request is the fixed `{"cmd":"config-get"}` line.
    func testConfigGetCommandEncoding() throws {
        let json = String(decoding: try JSONEncoder().encode(ConfigGetCommand()), as: UTF8.self)
        XCTAssertEqual(json, #"{"cmd":"config-get"}"#)
    }
}
