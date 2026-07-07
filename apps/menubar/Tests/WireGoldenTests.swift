// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Cross-language wire byte-drift guard (issue #340) — the Swift half.
//
// #322 hand-mirrored the daemon's frozen #164 wire contract into Swift `Codable` types
// (`Sources/WireModel.swift`) + byte-exact fixtures (`Tests/Fixtures.swift`). ADR-0010 keeps Rust
// out of the Swift build, so nothing caught a FUTURE daemon wire change silently diverging from
// that hand-written mirror: the Swift-only suite would keep validating against its own now-stale
// fixtures and stay green.
//
// The Rust crate closes that gap by serializing its OWN wire encoders into committed goldens
// (`build/fixtures/wire-*.json`, emitted + byte-equality-pinned in `src/daemon.rs`
// `emit_wire_golden_fixtures` / `the_committed_wire_goldens_still_match_the_daemon_encoders`). This
// test is the cross-language assertion: the daemon-output fixtures Swift decodes MUST be
// byte-identical to those goldens. If a daemon wire type changes, the Rust pin test forces the
// golden to be regenerated; the regenerated golden then no longer matches the Swift literal here,
// so THIS fails until the Swift mirror + fixtures are updated in lockstep. That is the whole point
// of the guard — a divergence between the Rust wire types and the Swift mirror can no longer land
// green.
//
// The goldens are read from the repo (not bundled as resources): the fixtures deliberately stay
// inline literals in `Fixtures.swift` (its documented "one source of truth for XCTest + any plain
// verifier" design, no resource-bundling surface), and this test asserts those literals equal the
// committed bytes. The golden path is resolved relative to THIS source file (`#filePath`), so it
// works identically under `xcodebuild test` (CI) and locally without a working-directory
// assumption. `build/fixtures/**` is in the `swift` CI path filter, so a golden regeneration
// re-runs this check.

import XCTest

final class WireGoldenTests: XCTestCase {

    /// Repo-root `build/fixtures/` resolved from this test file's own location. Strips the filename
    /// then three directory levels (`Tests` → `menubar` → `apps`) off
    /// `<repo>/apps/menubar/Tests/WireGoldenTests.swift` to reach `<repo>/`, then appends
    /// `build/fixtures`.
    private static func goldenDir(file: StaticString = #filePath) -> URL {
        URL(fileURLWithPath: "\(file)")
            .deletingLastPathComponent()   // Tests/
            .deletingLastPathComponent()   // menubar/
            .deletingLastPathComponent()   // apps/
            .deletingLastPathComponent()   // <repo>/
            .appendingPathComponent("build/fixtures", isDirectory: true)
    }

    private func golden(_ name: String) throws -> String {
        let url = Self.goldenDir().appendingPathComponent(name)
        let bytes = try Data(contentsOf: url)
        return try XCTUnwrap(String(data: bytes, encoding: .utf8), "\(name) is not valid UTF-8")
    }

    /// The representative healthy `snapshot` frame: the Swift `snapshotBasic` fixture must be
    /// byte-identical to the Rust-emitted `encode_snapshot_frame` golden.
    func testSnapshotFixtureMatchesRustGolden() throws {
        XCTAssertEqual(
            Fixtures.snapshotBasic,
            try golden("wire-snapshot-basic.json"),
            "snapshotBasic drifted from the Rust wire golden — the daemon's snapshot wire type "
                + "changed; regenerate the golden (cargo test -- --ignored emit_wire_golden_fixtures) "
                + "and update the Swift mirror (Sources/WireModel.swift) + this fixture in lockstep"
        )
    }

    /// The `heartbeat` frame: the Swift `heartbeatBasic` fixture must be byte-identical to the
    /// Rust-emitted `encode_heartbeat_frame` golden.
    func testHeartbeatFixtureMatchesRustGolden() throws {
        XCTAssertEqual(
            Fixtures.heartbeatBasic,
            try golden("wire-heartbeat-basic.json"),
            "heartbeatBasic drifted from the Rust wire golden — the daemon's heartbeat wire type "
                + "changed; regenerate the golden (cargo test -- --ignored emit_wire_golden_fixtures) "
                + "and update the Swift mirror (Sources/WireModel.swift) + this fixture in lockstep"
        )
    }
}
