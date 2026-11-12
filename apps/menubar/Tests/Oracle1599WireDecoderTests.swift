// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Issue #1599's pre-authored oracle at the Swift decoder (ST-1, ST-2, ST-3, ST-6 of the issue's § 8).
// Authored apart from the build and frozen: the build makes these green and edits none of them. The
// frames are built from `Fixtures.snapshotBasic`, so their schema minor follows the committed golden
// and is never pinned here — except ST-3's frame, which is the outgoing 1.14 shape on purpose.

import XCTest

/// The fixture these frames are built from no longer holds the row they replace.
private struct Oracle1599FixtureDrift: Error {}

extension WireDecoderTests {

    /// The one account row of `Fixtures.snapshotBasic`, which the frames below replace.
    private static let oracle1599BasicRow = #"""
    {"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}
    """#

    /// A served weekly-only row (issue #1599 § 6 step 3): no session window, the key saying so.
    private static let oracle1599WeeklyOnlyRow = #"""
    {"label":"spare","active":false,"enabled":true,"quarantined":false,"recovering":false,"session_pct":null,"weekly_pct":90,"session_resets_at":null,"weekly_resets_at":1794005999,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy","absent_windows":["session"]}
    """#

    /// A weekly-only blind active row (issue #1599 § 6 step 3): the block with no percent.
    private static let oracle1599PercentLessBlindRow = #"""
    {"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy","blind_active":{"blind_secs":480,"auto_protection_degraded":true}}
    """#

    /// A frame frozen at the OUTGOING minor 1.14 with the three-field `blind_active` block — the
    /// backward tolerance 1.14 proved, kept under test after the current-minor fixtures move.
    private static let oracle1599Pinned114BlindFrame = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":14},"generated_at":1893456000,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy","blind_active":{"blind_secs":1380,"last_known_session_pct":87,"auto_protection_degraded":true}},{"label":"spare","active":false,"enabled":true,"quarantined":false,"recovering":false,"session_pct":20,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":{"state":"target","to":"spare","reason":{"kind":"soonest_reset","resets_at":1893800000}},"refresh_enabled":true,"systemic_refresh_failure":null}
    """#

    /// `Fixtures.snapshotBasic` with its account row replaced by `rows`, at the golden's own minor.
    private func oracle1599Frame(_ rows: [String]) throws -> String {
        let accounts = "\"accounts\":[\(Self.oracle1599BasicRow)]"
        guard Fixtures.snapshotBasic.contains(accounts) else {
            XCTFail("Fixtures.snapshotBasic no longer holds the row these frames replace")
            throw Oracle1599FixtureDrift()
        }
        return Fixtures.snapshotBasic.replacingOccurrences(
            of: accounts, with: "\"accounts\":[\(rows.joined(separator: ","))]")
    }

    // ST-1 (AC-3): the key decodes to its token list, and to `[]` when absent or null.
    @objc func testDecodesTheAbsentWindowsKeyAndDefaultsItToEmpty() throws {
        let frame = try oracle1599Frame([Self.oracle1599WeeklyOnlyRow, Self.oracle1599PercentLessBlindRow])
        guard case .snapshot(let v) = try parseWatchFrame(frame) else {
            return XCTFail("expected a snapshot frame")
        }
        let weeklyOnly = v.accounts[0]
        XCTAssertEqual(weeklyOnly.absentWindows, ["session"])
        XCTAssertNil(weeklyOnly.sessionPct, "no session window: the percent is nil")
        XCTAssertEqual(weeklyOnly.weeklyPct, 90)
        XCTAssertEqual(weeklyOnly.weeklyResetsAt, 1794005999)
        XCTAssertEqual(v.accounts[1].absentWindows, [], "a blind row carries no key → []")

        // No key (a two-window row, or a pre-this-minor daemon) → [].
        guard case .snapshot(let basic) = try parseWatchFrame(Fixtures.snapshotBasic) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(basic.accounts[0].absentWindows, [])

        // An explicit null → [].
        let nullRow = Self.oracle1599BasicRow.replacingOccurrences(
            of: "\"auth\":\"healthy\"}", with: "\"auth\":\"healthy\",\"absent_windows\":null}")
        guard case .snapshot(let nulled) = try parseWatchFrame(try oracle1599Frame([nullRow])) else {
            return XCTFail("a null key must not cost the frame")
        }
        XCTAssertEqual(nulled.accounts[0].absentWindows, [])

        // A string list, not an enum: an unknown future token never costs the frame.
        let futureRow = Self.oracle1599BasicRow.replacingOccurrences(
            of: "\"auth\":\"healthy\"}", with: "\"auth\":\"healthy\",\"absent_windows\":[\"fortnight\"]}")
        guard case .snapshot(let future) = try parseWatchFrame(try oracle1599Frame([futureRow])) else {
            return XCTFail("an unknown window token must not cost the frame")
        }
        XCTAssertEqual(future.accounts[0].absentWindows, ["fortnight"])
    }

    // ST-2 (AC-2, AC-3): a present block without its percent decodes; a block missing either required
    // field still throws.
    @objc func testDecodesABlindActiveBlockWithoutALastKnownPercent() throws {
        let frame = try oracle1599Frame([Self.oracle1599WeeklyOnlyRow, Self.oracle1599PercentLessBlindRow])
        guard case .snapshot(let v) = try parseWatchFrame(frame) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertNotNil(v.accounts[1].blindActive, "the percent-less block is kept, not dropped")
        XCTAssertEqual(v.accounts[1].blindActive,
                       BlindActive(blindSecs: 480, lastKnownSessionPct: nil, autoProtectionDegraded: true))

        let noSecs = Self.oracle1599PercentLessBlindRow.replacingOccurrences(
            of: "\"blind_secs\":480,", with: "")
        XCTAssertNotEqual(noSecs, Self.oracle1599PercentLessBlindRow, "fixture: blind_secs removed")
        XCTAssertThrowsError(try parseWatchFrame(try oracle1599Frame([noSecs])),
                             "a block missing blind_secs still throws")

        let noVerdict = Self.oracle1599PercentLessBlindRow.replacingOccurrences(
            of: ",\"auto_protection_degraded\":true", with: "")
        XCTAssertNotEqual(noVerdict, Self.oracle1599PercentLessBlindRow, "fixture: the verdict removed")
        XCTAssertThrowsError(try parseWatchFrame(try oracle1599Frame([noVerdict])),
                             "a block missing auto_protection_degraded still throws")
    }

    // ST-3 (AC-10): the outgoing 1.14 three-field block still decodes with its percent.
    @objc func testA114ThreeFieldBlindBlockStillDecodes() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Self.oracle1599Pinned114BlindFrame) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 14))
        XCTAssertTrue(v.isSchemaSupported, "a 1.14 daemon stays a supported contract")
        XCTAssertEqual(v.accounts[0].blindActive?.lastKnownSessionPct, 87)
        XCTAssertEqual(v.accounts[0].blindActive,
                       BlindActive(blindSecs: 1380, lastKnownSessionPct: 87, autoProtectionDegraded: true))
    }

    // ST-6 (AC-10): the future-daemon frame stays exactly one minor ahead of the committed golden.
    @objc func testTheFutureFrameSitsOneMinorAheadOfTheCommittedGolden() throws {
        guard case .snapshot(let future) = try parseWatchFrame(Self.futureSnapshotFrame) else {
            return XCTFail("the future frame decodes")
        }
        guard case .snapshot(let golden) = try parseWatchFrame(Fixtures.snapshotBasic) else {
            return XCTFail("the committed golden decodes")
        }
        XCTAssertEqual(future.schemaVersion.major, golden.schemaVersion.major)
        XCTAssertEqual(future.schemaVersion.minor, golden.schemaVersion.minor + 1,
                       "the future frame must sit one minor ahead of the current contract")
    }
}
