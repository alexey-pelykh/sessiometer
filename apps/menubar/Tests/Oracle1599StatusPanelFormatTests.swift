// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Issue #1599's pre-authored oracle at the panel interim (ST-5 of the issue's § 8). Authored apart from
// the build and frozen: the build makes it green and edits none of it. The § 5 gate function's name is
// the build's, so this pins what is observable without it: the decoded model keeps a percent-less
// block, the glance reads the raw block and escalates, the glance's spoken label stays today's, and a
// present percent is still spoken as a number, never as an optional.

import XCTest

extension StatusPanelFormatTests {

    /// `Fixtures.snapshotBasic`'s one account row, replaced below by a weekly-only blind active row.
    private static let oracle1599BasicRow = #"""
    {"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}
    """#

    private static let oracle1599PercentLessBlindRow = #"""
    {"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy","blind_active":{"blind_secs":480,"auto_protection_degraded":true}}
    """#

    // ST-5 (AC-5): a percent-less block is kept by the model and read raw by the glance (§ 5's
    // sub-default, settled by council, 2026-11-04); a present percent is spoken exactly as today.
    @objc func testAPercentLessBlindBlockRendersAsTheTwoMeterRowUntilClassified() throws {
        XCTAssertTrue(Fixtures.snapshotBasic.contains(Self.oracle1599BasicRow),
                      "fixture: snapshotBasic still holds the row this frame replaces")
        let frame = Fixtures.snapshotBasic.replacingOccurrences(
            of: Self.oracle1599BasicRow, with: Self.oracle1599PercentLessBlindRow)

        var machine = HonestStateMachine()
        _ = machine.apply(.connected)
        _ = machine.apply(.line(frame))
        XCTAssertEqual(machine.connectionState, .connected, "the percent-less frame is applied, not dropped")
        let active = try XCTUnwrap(machine.rows.first { $0.isActive })
        XCTAssertEqual(active.blindActive,
                       BlindActive(blindSecs: 480, lastKnownSessionPct: nil, autoProtectionDegraded: true),
                       "the decoded model keeps the block, its percent nil, never 0")
        XCTAssertEqual(machine.presentation.glyph, .attention,
                       "the glance reads the raw block: a DEGRADED percent-less block escalates")
        XCTAssertEqual(machine.presentation.accessibilityLabel,
                       "Sessiometer: auto-protection degraded — acting on a stale anchor",
                       "the glance's spoken label stays today's until #1594")

        let spoken = StatusPanelFormat.rowAccessibilityLabel(
            label: "work", isActive: true, auth: .healthy, recovering: false, enabled: true,
            quarantined: false, sessionPct: nil, weeklyPct: nil, sessionReset: "n/a", weeklyReset: "n/a",
            blind: BlindActive(blindSecs: 480, lastKnownSessionPct: 87, autoProtectionDegraded: true))
        XCTAssertEqual(spoken,
            "work, active, auth healthy, blind for 8m, last-known session 87 percent, auto-protection degraded, acting on a stale anchor",
            "a present percent is spoken byte for byte as today")
        XCTAssertFalse(spoken.contains("Optional"), "never an optional read aloud")
    }
}
