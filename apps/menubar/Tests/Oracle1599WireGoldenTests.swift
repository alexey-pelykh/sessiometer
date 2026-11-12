// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Issue #1599's pre-authored oracle at the cross-language byte pin (ST-4 of the issue's § 8).
// Authored apart from the build and frozen: the build makes it green and edits none of it. The new
// Swift fixture's name is the build's, so this pins what is observable without it: `Fixtures.swift`
// holds the new golden's bytes verbatim, and those bytes decode to the two rows of § 6 step 3.

import XCTest

extension WireGoldenTests {

    /// The repository root, from this file's own location (`<repo>/apps/menubar/Tests/…`).
    private static func oracle1599RepoRoot(file: StaticString = #filePath) -> URL {
        URL(fileURLWithPath: "\(file)")
            .deletingLastPathComponent()   // Tests/
            .deletingLastPathComponent()   // menubar/
            .deletingLastPathComponent()   // apps/
            .deletingLastPathComponent()   // <repo>/
    }

    private func oracle1599Text(_ relativePath: String) throws -> String {
        let bytes = try Data(contentsOf: Self.oracle1599RepoRoot().appendingPathComponent(relativePath))
        return try XCTUnwrap(String(data: bytes, encoding: .utf8), "\(relativePath) is not valid UTF-8")
    }

    // ST-4 (AC-8, AC-9): the sixth golden, `wire-snapshot-no-window.json`, has a byte-identical Swift
    // fixture, and its bytes are the two rows the issue specifies.
    @objc func testTheNoWindowGoldenIsByteIdentical() throws {
        let golden = try oracle1599Text("build/fixtures/wire-snapshot-no-window.json")
        let fixtures = try oracle1599Text("apps/menubar/Tests/Fixtures.swift")
        XCTAssertFalse(golden.isEmpty, "the new golden holds the emitter's bytes")
        XCTAssertTrue(fixtures.contains(golden),
                      "Fixtures.swift must hold a current-daemon fixture byte-identical to "
                        + "wire-snapshot-no-window.json — regenerate the golden by its emitter and paste "
                        + "its bytes verbatim")

        guard case .snapshot(let v) = try parseWatchFrame(golden) else {
            return XCTFail("the new golden is a snapshot frame")
        }
        guard case .snapshot(let basic) = try parseWatchFrame(Fixtures.snapshotBasic) else {
            return XCTFail("the committed basic golden decodes")
        }
        XCTAssertEqual(v.schemaVersion, basic.schemaVersion, "the sixth golden sits at the landed minor")
        XCTAssertEqual(v.accounts.count, 2, "one weekly-only row and one weekly-only blind active row")

        let weeklyOnly = try XCTUnwrap(v.accounts.first { $0.absentWindows == ["session"] },
                                       "a served weekly-only row carrying the key")
        XCTAssertNil(weeklyOnly.sessionPct)
        XCTAssertEqual(weeklyOnly.weeklyPct, 90)
        XCTAssertNil(weeklyOnly.sessionResetsAt)
        XCTAssertNotNil(weeklyOnly.weeklyResetsAt, "the weekly reset rides beside the key")
        XCTAssertNil(weeklyOnly.blindActive)

        let blind = try XCTUnwrap(v.accounts.first { $0.blindActive != nil }, "a blind active row")
        XCTAssertTrue(blind.active)
        XCTAssertNil(blind.sessionPct)
        XCTAssertNil(blind.weeklyPct)
        XCTAssertEqual(blind.absentWindows, [], "a blind row carries no key")
        XCTAssertEqual(blind.blindActive,
                       BlindActive(blindSecs: 480, lastKnownSessionPct: nil, autoProtectionDegraded: true))

        XCTAssertEqual(golden.components(separatedBy: "\"absent_windows\":[\"session\"]").count, 2,
                       "the key appears exactly once, on the weekly-only row")
        XCTAssertFalse(golden.contains("last_known_session_pct"), "no percent key, never null or 0")
    }
}
