// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Golden-fixture tests for the `watch` wire decoder (issue #322). The fixtures live in
// `Fixtures.swift` (shared, no XCTest dependency); each test below maps to an acceptance
// criterion. The decoder is pure `JSONDecoder`, so these run identically under `xcodebuild test`
// (CI) and any plain verifier.

import XCTest

final class WireDecoderTests: XCTestCase {

    // AC: "Decodes real `snapshot` … frames."
    func testDecodesRealSnapshotFrame() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotBasic) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 0))
        XCTAssertEqual(v.generatedAt, 42)
        XCTAssertTrue(v.isSchemaSupported)
        XCTAssertNil(v.nextSwap, "next_swap null decodes to nil")
        XCTAssertEqual(v.refreshEnabled, false)
        XCTAssertEqual(v.accounts.count, 1)

        let a = v.accounts[0]
        XCTAssertEqual(a.label, "work")
        XCTAssertTrue(a.active)
        XCTAssertTrue(a.enabled)
        XCTAssertFalse(a.quarantined)
        XCTAssertFalse(a.recovering)
        XCTAssertEqual(a.sessionPct, 60)
        XCTAssertEqual(a.weeklyPct, 10)
        XCTAssertNil(a.sessionResetsAt)
        XCTAssertNil(a.weeklyResetsAt)
        XCTAssertFalse(a.weeklyExhausted)
        XCTAssertNil(a.accessExpiresAt)
        XCTAssertNil(a.refreshHealth)
        XCTAssertEqual(a.auth, .healthy)
    }

    // AC: "Decodes real … `heartbeat` frames." + heartbeat carries the freshness envelope.
    func testDecodesRealHeartbeatFrame() throws {
        let frame = try parseWatchFrame(Fixtures.heartbeatBasic)
        XCTAssertEqual(frame, .heartbeat(generatedAt: 42, schemaVersion: SchemaVersion(major: 1, minor: 0)))
        XCTAssertEqual(frame.schemaVersion, SchemaVersion(major: 1, minor: 0))
        XCTAssertTrue(WireContract.isSupported(try XCTUnwrap(frame.schemaVersion)))
    }

    // AC: "All three `next_swap` states … decode" — `target` here (+ null in the basic test).
    // AC: "`auth` → CredentialHealth …; `refresh_health` … tolerated" — present + null both here.
    func testDecodesRichSnapshotWithTargetAndMixedHealth() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotRichTarget) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.generatedAt, 1_893_456_000)
        XCTAssertEqual(v.nextSwap, .target(to: "personal"))
        XCTAssertEqual(v.refreshEnabled, true)
        XCTAssertEqual(v.accounts.count, 2)

        let work = v.accounts[0]
        XCTAssertEqual(work.sessionPct, 30)
        XCTAssertEqual(work.weeklyPct, 20)
        XCTAssertEqual(work.sessionResetsAt, 1_893_460_000)
        XCTAssertEqual(work.weeklyResetsAt, 1_893_800_000)
        XCTAssertEqual(work.accessExpiresAt, 1_893_470_000)
        XCTAssertEqual(work.refreshHealth, RefreshHealth(lastOk: true, rotated: true, consecutiveFailures: 0))
        XCTAssertEqual(work.auth, .atRisk)

        let personal = v.accounts[1]
        XCTAssertFalse(personal.active)
        XCTAssertNil(personal.sessionPct)
        XCTAssertNil(personal.refreshHealth, "refresh_health null is tolerated → nil")
        XCTAssertEqual(personal.auth, .unknown)
    }

    // AC: "All three `next_swap` states …" — `no_viable_target`. + `auth` stale, failure streak.
    func testDecodesNoViableTargetAndStale() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotNoViable) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.nextSwap, .noViableTarget)
        let a = v.accounts[0]
        XCTAssertTrue(a.weeklyExhausted)
        XCTAssertEqual(a.auth, .stale)
        XCTAssertEqual(a.refreshHealth, RefreshHealth(lastOk: false, rotated: false, consecutiveFailures: 2))
    }

    // AC: "All three `next_swap` states …" — `awaiting_data`. + `auth` dead, quarantined.
    func testDecodesAwaitingDataAndDead() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotAwaitingDead) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.nextSwap, .awaitingData)
        let a = v.accounts[0]
        XCTAssertTrue(a.quarantined)
        XCTAssertNil(a.sessionPct)
        XCTAssertEqual(a.auth, .dead)
    }

    // AC: "`auth` → CredentialHealth including `null`".
    func testAuthNullIsTolerated() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotAuthNull) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertNil(v.accounts[0].auth)
    }

    // AC: additive-default path — a pre-#109/#119 account with only required fields decodes.
    func testLegacyMinimalAccountDecodesWithDefaults() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotLegacyMinimal) else {
            return XCTFail("expected a snapshot frame")
        }
        let a = v.accounts[0]
        XCTAssertEqual(a.label, "work")
        XCTAssertFalse(a.recovering)
        XCTAssertFalse(a.weeklyExhausted)
        XCTAssertNil(a.sessionPct)
        XCTAssertNil(a.weeklyPct)
        XCTAssertNil(a.sessionResetsAt)
        XCTAssertNil(a.weeklyResetsAt)
        XCTAssertNil(a.accessExpiresAt)
        XCTAssertNil(a.refreshHealth)
        XCTAssertNil(a.auth)
    }

    // AC: forward-compat MINOR — unknown additive keys ignored, still supported.
    func testUnknownAdditiveFieldsAreIgnored() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotUnknownAdditiveFields) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 5))
        XCTAssertTrue(v.isSchemaSupported, "a minor bump stays supported")
        XCTAssertEqual(v.accounts[0].label, "work")
    }

    // AC: "Unknown `type` → ignored (returns an 'unknown' frame, NOT an error)".
    func testUnknownFrameTypesAreIgnoredNotErrors() throws {
        XCTAssertEqual(try parseWatchFrame(Fixtures.unknownFutureType), .unknown)
        XCTAssertEqual(try parseWatchFrame(Fixtures.noTypeTag), .unknown)
        XCTAssertNil(try parseWatchFrame(Fixtures.unknownFutureType).schemaVersion)
    }

    // AC: "malformed line → error".
    func testMalformedLineThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.notJSON))
        XCTAssertThrowsError(try parseWatchFrame(""))
    }

    // AC: "`schema_version.major != 1` → flagged unsupported … never mis-rendered".
    func testUnsupportedMajorDecodesButIsFlagged() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotUnsupportedMajor) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion.major, 2)
        XCTAssertFalse(v.isSchemaSupported)
    }

    // AC: "a pre-freeze/absent version decodes to major `0` → unsupported".
    func testPreFreezeVersionDecodesToMajorZeroUnsupported() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotPreFreeze) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 0, minor: 0))
        XCTAssertEqual(v.generatedAt, 0, "absent generated_at defaults to 0")
        XCTAssertFalse(v.isSchemaSupported)

        let beat = try parseWatchFrame(Fixtures.heartbeatPreFreeze)
        XCTAssertEqual(beat, .heartbeat(generatedAt: 7, schemaVersion: SchemaVersion(major: 0, minor: 0)))
        XCTAssertFalse(WireContract.isSupported(try XCTUnwrap(beat.schemaVersion)))
    }

    // Faithful mirror: an unknown internally-tagged `next_swap` state is a hard error.
    func testUnknownNextSwapStateThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotUnknownNextSwap))
    }

    // Faithful mirror: an unknown `auth` value is a hard error.
    func testUnknownAuthValueThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotUnknownAuth))
    }

    // Faithful mirror: a snapshot missing a required field is a hard error. Covers every
    // required (non-Option, no-`serde(default)`) field across the type graph: account `label`,
    // envelope `accounts`, `next_swap` target's `to`, `schema_version.minor`, heartbeat
    // `generated_at`. Each mirrors serde's "missing field" error (verified against the daemon).
    func testMissingRequiredFieldThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotMissingLabel))
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotMissingAccounts))
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotTargetMissingTo))
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotSchemaMissingMinor))
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.heartbeatMissingGeneratedAt))
    }

    // The supported-major constant tracks the daemon's STATUS_SCHEMA_VERSION.major (= 1).
    func testSupportedSchemaMajorMatchesFrozenContract() {
        XCTAssertEqual(WireContract.supportedSchemaMajor, 1)
        XCTAssertTrue(WireContract.isSupported(SchemaVersion(major: 1, minor: 0)))
        XCTAssertTrue(WireContract.isSupported(SchemaVersion(major: 1, minor: 99)))
        XCTAssertFalse(WireContract.isSupported(SchemaVersion(major: 0, minor: 0)))
        XCTAssertFalse(WireContract.isSupported(SchemaVersion(major: 2, minor: 0)))
    }
}
