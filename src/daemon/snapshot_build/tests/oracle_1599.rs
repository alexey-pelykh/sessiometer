// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Issue #1599's pre-authored oracle at the `status` / `watch` wire seams (RT-1, RT-2, RT-7, RT-8,
//! RT-11 of the issue's § 8). Authored apart from the build and frozen: the build makes these green
//! and edits none of them. Every byte asserted here is one the issue pins; the schema minor is never
//! pinned (it is read from `STATUS_SCHEMA_VERSION`, because the minor is taken at landing).

use super::*;

use std::time::{Duration, Instant};

/// The account lines of a `status` reply, each serialised on its own, in roster order — the per-line
/// bytes a client reads, so an assertion about one line cannot be satisfied by another.
fn line_bytes(response: &StatusResponse) -> Vec<String> {
    response
        .accounts
        .iter()
        .map(|line| serde_json::to_string(line).expect("an account line serialises"))
        .collect()
}

/// The anchor bundle `blind_active_view` takes, for a retained pre-blind reading observed at `at`.
fn anchor(session: Option<f64>, weekly: Option<f64>, at: Instant) -> AnchorArmInputs {
    AnchorArmInputs {
        last_good: Some(LastGood {
            session,
            weekly,
            at,
        }),
        high_water: None,
    }
}

#[test]
fn status_response_says_no_session_window_with_the_absent_windows_key_and_a_failed_poll_without_it()
{
    // RT-1 (AC-1): a served weekly-only reading, a failed poll and a two-window reading. The first
    // two both carry `"session_pct":null`; only the key tells them apart, and only the first carries
    // it. A two-window row carries no key at all (omitted when empty).
    let snapshot = StatusSnapshot {
        generated_at: 42,
        accounts: vec![
            AccountReading {
                label: "work".to_owned(),
                active: true,
                enabled: true,
                usage: Some(Usage {
                    session: None,
                    weekly: Some(0.90),
                    weekly_resets_at: Some(1_794_005_999),
                    session_resets_at: None,
                }),
                ..Default::default()
            },
            AccountReading {
                label: "spare".to_owned(),
                enabled: true,
                usage: None,
                ..Default::default()
            },
            AccountReading {
                label: "backup".to_owned(),
                enabled: true,
                usage: Some(Usage {
                    session: Some(0.60),
                    weekly: Some(0.10),
                    weekly_resets_at: None,
                    session_resets_at: None,
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let response = status_response(&snapshot);
    let lines = line_bytes(&response);
    let (no_window, failed_poll, two_window) = (&lines[0], &lines[1], &lines[2]);

    assert!(
        no_window.contains(r#""session_pct":null"#),
        "the weekly-only row keeps `session_pct: null`: {no_window}"
    );
    assert!(
        no_window.contains(r#""absent_windows":["session"]"#),
        "the weekly-only row says which window is absent: {no_window}"
    );
    assert!(
        no_window.contains(r#""weekly_pct":90"#),
        "the weekly-only row carries its weekly percent: {no_window}"
    );
    assert!(
        no_window.contains(r#""weekly_resets_at":1794005999"#),
        "the weekly-only row carries its weekly reset: {no_window}"
    );

    assert!(
        failed_poll.contains(r#""session_pct":null"#),
        "a failed poll stays `session_pct: null`: {failed_poll}"
    );
    assert!(
        !failed_poll.contains("absent_windows"),
        "a failed poll has no reading, so it carries no key: {failed_poll}"
    );

    assert!(
        !two_window.contains("absent_windows"),
        "a row whose windows both apply omits the key: {two_window}"
    );
    assert!(two_window.contains(r#""session_pct":60"#), "{two_window}");

    // The two `null` lines differ by bytes even with their labels made equal.
    let failed_poll_as_work = failed_poll.replace(r#""label":"spare""#, r#""label":"work""#);
    assert_ne!(
        no_window, &failed_poll_as_work,
        "\"no session window\" and \"no reading\" must differ by bytes"
    );

    // The same key rides the `watch` snapshot frame, exactly once (the weekly-only row only).
    let json = serde_json::to_string(&response).unwrap();
    assert_eq!(
        json.matches("absent_windows").count(),
        1,
        "only the weekly-only row carries the key on the status reply: {json}"
    );
    let frame = encode_snapshot_frame(&versioned_status_response(&snapshot));
    assert_eq!(
        frame.matches(r#""absent_windows":["session"]"#).count(),
        1,
        "the watch snapshot frame carries the key exactly once: {frame}"
    );
    assert_eq!(
        frame.matches("absent_windows").count(),
        1,
        "and nowhere else: {frame}"
    );
}

#[test]
fn a_weekly_only_blind_active_serializes_its_block_without_a_percent_and_never_a_zero() {
    // RT-2 (AC-2, AC-16): `status_response` over `blind_active_view`. A blind active whose retained
    // anchor has no session window, 480 s blind with no `Retry-After` hold and no velocity, carries
    // the block with its percent omitted and `auto_protection_degraded: true` (the no-session-window
    // arm alone). The blind row carries no `absent_windows` key: during blindness there is no served
    // reading, and the key is never computed from the anchor.
    let base = Instant::now();
    let at = base + Duration::from_secs(480);
    let weekly_only = blind_active_view(
        anchor(None, Some(0.90), base),
        true,
        false,
        false,
        None,
        0.95,
        at,
    );
    let two_window = blind_active_view(
        anchor(Some(0.87), Some(0.90), base),
        true,
        false,
        false,
        None,
        0.95,
        at,
    );
    let snapshot = StatusSnapshot {
        generated_at: 42,
        accounts: vec![
            AccountReading {
                label: "work".to_owned(),
                active: true,
                enabled: true,
                blind_active: weekly_only,
                ..Default::default()
            },
            AccountReading {
                label: "spare".to_owned(),
                enabled: true,
                blind_active: two_window,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let response = status_response(&snapshot);
    let lines = line_bytes(&response);
    let blind_row = &lines[0];

    assert!(
        blind_row.contains(r#""blind_active":{"blind_secs":480,"auto_protection_degraded":true}"#),
        "a weekly-only blind active keeps its block, percent omitted, DEGRADED past the gate: \
         {blind_row}"
    );
    assert!(
        !blind_row.contains("last_known_session_pct"),
        "no percent key at all for an anchor without a session window: {blind_row}"
    );
    assert!(
        !blind_row.contains(r#""auto_protection_degraded":false"#),
        "never auto-protection OK past the gate on a session gate that cannot arm: {blind_row}"
    );
    assert!(
        !blind_row.contains("absent_windows"),
        "the key is a fact about a served reading; a blind row has none: {blind_row}"
    );
    assert!(
        blind_row.contains(r#""session_pct":null"#),
        "the cleared live reading still reads null, never a fabricated 0: {blind_row}"
    );

    // A present percent keeps today's three-field bytes.
    assert!(
        lines[1].contains(
            r#""blind_active":{"blind_secs":480,"last_known_session_pct":87,"auto_protection_degraded":true}"#
        ),
        "a two-window anchor keeps the three-field block: {}",
        lines[1]
    );

    // Round-trip: the percent-less block decodes back with its percent absent, never `Some(0)`.
    let json = serde_json::to_string(&response).unwrap();
    let parsed: StatusResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed.accounts[0].blind_active,
        Some(BlindActive {
            blind_secs: 480,
            last_known_session_pct: None,
            auto_protection_degraded: true,
        }),
        "the percent-less block round-trips with `last_known_session_pct: None`",
    );
    assert_eq!(
        parsed.accounts[1].blind_active,
        Some(BlindActive {
            blind_secs: 480,
            last_known_session_pct: Some(87),
            auto_protection_degraded: true,
        }),
    );
}

#[test]
fn windows_absent_from_lists_each_absent_window_in_fixed_order() {
    // RT-7 (AC-1, D11): the emission helper over the four combinations, both resets `None`, as
    // `serde_json` writes the list — `session` before `weekly`, never the reverse.
    let usage = |session: Option<f64>, weekly: Option<f64>| Usage {
        session,
        weekly,
        weekly_resets_at: None,
        session_resets_at: None,
    };
    for (reading, expected) in [
        (usage(Some(0.60), Some(0.10)), "[]"),
        (usage(None, Some(0.90)), r#"["session"]"#),
        (usage(Some(0.42), None), r#"["weekly"]"#),
        (usage(None, None), r#"["session","weekly"]"#),
    ] {
        let absent = crate::daemon::snapshot::windows_absent_from(&reading);
        assert_eq!(
            serde_json::to_string(&absent).unwrap(),
            expected,
            "windows_absent_from(session: {:?}, weekly: {:?})",
            reading.session,
            reading.weekly,
        );
    }
}

/// A committed baseline golden row: a two-window reading at `"session_pct":60`, with neither the key
/// nor a blind block. The five goldens may move only in `schema_version.minor`.
fn is_baseline_row(row: &serde_json::Value) -> bool {
    row.get("session_pct") == Some(&serde_json::json!(60))
        && row.get("absent_windows").is_none()
        && row.get("blind_active").is_none()
}

#[test]
fn the_five_baseline_goldens_carry_no_absent_windows_and_no_blind_block() {
    // RT-8 (AC-8): after the regeneration both sides of the byte pin are the executor's, so this
    // fails an accidental re-baseline that folds a weekly-only row or a blind block into any of the
    // five. Row counts measured at the base of this item.
    let current = serde_json::to_value(STATUS_SCHEMA_VERSION).unwrap();
    for (name, golden, rows) in [
        ("wire-snapshot-basic.json", WIRE_SNAPSHOT_GOLDEN, 1),
        (
            "wire-snapshot-canonical-scrub.json",
            WIRE_SNAPSHOT_CANONICAL_SCRUB_GOLDEN,
            1,
        ),
        ("wire-snapshot-expiry.json", WIRE_SNAPSHOT_EXPIRY_GOLDEN, 5),
        (
            "wire-snapshot-next-swap.json",
            WIRE_SNAPSHOT_NEXT_SWAP_GOLDEN,
            1,
        ),
    ] {
        let frame: serde_json::Value =
            serde_json::from_str(golden).unwrap_or_else(|err| panic!("{name} is not JSON: {err}"));
        assert_eq!(frame["type"], "snapshot", "{name}");
        assert_eq!(
            frame["schema_version"], current,
            "{name} carries the current schema version"
        );
        let accounts = frame["accounts"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} has an accounts array"));
        assert_eq!(accounts.len(), rows, "{name} keeps its account rows");
        for row in accounts {
            assert!(
                is_baseline_row(row),
                "{name}: every row is a two-window `session_pct: 60` row with no `absent_windows` \
                 and no `blind_active`: {row}"
            );
        }
    }

    let heartbeat: serde_json::Value =
        serde_json::from_str(WIRE_HEARTBEAT_GOLDEN).expect("the heartbeat golden is JSON");
    let mut keys: Vec<&str> = heartbeat
        .as_object()
        .expect("the heartbeat golden is one object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["generated_at", "schema_version", "type"],
        "the heartbeat carries no account rows, exactly three keys: {heartbeat}"
    );
    assert_eq!(heartbeat["type"], "heartbeat");
    assert_eq!(heartbeat["generated_at"], 42);
    assert_eq!(heartbeat["schema_version"], current);

    // The same predicate rejects both rows of the new golden (issue #1599 § 6 step 3) — the proof
    // that it can fail.
    let weekly_only_row = serde_json::json!({
        "label": "spare", "active": false, "enabled": true, "quarantined": false,
        "recovering": false, "session_pct": null, "weekly_pct": 90, "session_resets_at": null,
        "weekly_resets_at": 1_794_005_999, "weekly_exhausted": false, "access_expires_at": null,
        "refresh_health": null, "auth": "healthy", "absent_windows": ["session"]
    });
    let blind_row = serde_json::json!({
        "label": "work", "active": true, "enabled": true, "quarantined": false,
        "recovering": false, "session_pct": null, "weekly_pct": null, "session_resets_at": null,
        "weekly_resets_at": null, "weekly_exhausted": false, "access_expires_at": null,
        "refresh_health": null, "auth": "healthy",
        "blind_active": {"blind_secs": 480, "auto_protection_degraded": true}
    });
    assert!(
        !is_baseline_row(&weekly_only_row),
        "a weekly-only row is no baseline row"
    );
    assert!(
        !is_baseline_row(&blind_row),
        "a weekly-only blind row is no baseline row"
    );
}

#[test]
fn snapshot_frame_omits_the_blind_preempt_swap_percent_when_the_anchor_had_no_session_window() {
    // RT-11 (AC-17): the swap notice off a session-less anchor encodes with its two labels and no
    // percent key anywhere in the frame, and round-trips; a present percent keeps today's bytes.
    let mut snapshot = watch_snapshot("work", 42, 0.60);
    snapshot.recent_blind_preempt_swap = Some(BlindPreemptSwap {
        from_label: "work".to_owned(),
        to_label: "spare".to_owned(),
        last_known_session_pct: None,
    });
    let frame = encode_snapshot_frame(&versioned_status_response(&snapshot));
    assert!(
        frame.contains(r#""recent_blind_preempt_swap":{"from_label":"work","to_label":"spare"}"#),
        "the notice keeps its labels and drops the percent: {frame}"
    );
    assert!(
        !frame.contains("last_known_session_pct"),
        "no percent key anywhere in the frame, never `null` or `0`: {frame}"
    );
    let decoded: BlindPreemptSwap =
        serde_json::from_str(r#"{"from_label":"work","to_label":"spare"}"#)
            .expect("the percent-less notice decodes");
    assert_eq!(
        decoded,
        BlindPreemptSwap {
            from_label: "work".to_owned(),
            to_label: "spare".to_owned(),
            last_known_session_pct: None,
        },
    );

    let mut with_pct = watch_snapshot("work", 42, 0.60);
    with_pct.recent_blind_preempt_swap = Some(BlindPreemptSwap {
        from_label: "work".to_owned(),
        to_label: "spare".to_owned(),
        last_known_session_pct: Some(68),
    });
    let frame = encode_snapshot_frame(&versioned_status_response(&with_pct));
    assert!(
        frame.contains(
            r#""recent_blind_preempt_swap":{"from_label":"work","to_label":"spare","last_known_session_pct":68}"#
        ),
        "a present percent keeps today's bytes: {frame}"
    );
}
