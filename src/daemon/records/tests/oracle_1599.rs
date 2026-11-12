// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Issue #1599's pre-authored oracle at `blind_active_view` (RT-3 of the issue's § 8). Authored
//! apart from the build and frozen: the build makes it green and edits none of it. It replaces
//! #1602's interim test
//! `blind_active_view_emits_no_block_for_a_weekly_only_blind_active_until_the_wire_can_say_so`,
//! deleted from the host in the same change.

use super::*;

#[test]
fn blind_active_view_emits_the_block_for_a_session_less_anchor_with_the_percent_absent() {
    // RT-3 (AC-2, AC-16): a blind active whose anchor has no session window projects the block with
    // its percent absent and `blind_secs` measured from the anchor; the verdict follows issue
    // #1599 § 2a's four cases. No velocity is retained in any case.
    let base = Instant::now();
    let anchor = |session: Option<f64>| AnchorArmInputs {
        last_good: Some(LastGood {
            session,
            weekly: Some(0.90),
            at: base,
        }),
        high_water: None,
    };
    let at = |secs: u64| base + Duration::from_secs(secs);
    assert_eq!(
        BLIND_GATE_SECS, 300,
        "fixture: the cases straddle the 300 s gate"
    );

    // 480 s, no hold: the no-session-window arm alone reads DEGRADED.
    assert_eq!(
        blind_active_view(anchor(None), true, false, false, None, 0.95, at(480)),
        Some(BlindActive {
            blind_secs: 480,
            last_known_session_pct: None,
            auto_protection_degraded: true,
        }),
        "a session-less anchor blind past the gate is DEGRADED with no hold, percent absent",
    );

    // 480 s with a `Retry-After` hold: still DEGRADED.
    assert_eq!(
        blind_active_view(anchor(None), true, false, true, None, 0.95, at(480)),
        Some(BlindActive {
            blind_secs: 480,
            last_known_session_pct: None,
            auto_protection_degraded: true,
        }),
        "a session-less anchor blind past the gate behind a hold is DEGRADED",
    );

    // 240 s, before the gate, no hold: OK, the pre-gate verdict every blind account gets.
    assert_eq!(
        blind_active_view(anchor(None), true, false, false, None, 0.95, at(240)),
        Some(BlindActive {
            blind_secs: 240,
            last_known_session_pct: None,
            auto_protection_degraded: false,
        }),
        "before the gate a session-less anchor reads OK, still with its block",
    );

    // A two-window anchor below the risk band, 480 s, no hold, no velocity: OK — the new arm keys on
    // the absent session only.
    assert_eq!(
        blind_active_view(anchor(Some(0.30)), true, false, false, None, 0.95, at(480)),
        Some(BlindActive {
            blind_secs: 480,
            last_known_session_pct: Some(30),
            auto_protection_degraded: false,
        }),
        "a present below-band anchor is never DEGRADED by the no-session-window arm",
    );
}
