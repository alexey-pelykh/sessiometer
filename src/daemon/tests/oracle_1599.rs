// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Issue #1599's pre-authored oracle at the swap-notice seams (RT-9, RT-10 of the issue's § 8).
//! Authored apart from the build and frozen: the build makes these green and edits none of them.
//! RT-9 inverts #1602's interim test
//! `a_retry_after_blind_preempt_from_a_weekly_only_anchor_retains_no_status_notice`, deleted from
//! the host in the same change.

use super::*;

#[tokio::test]
async fn a_retry_after_blind_swap_off_a_session_less_anchor_narrates_the_swap_with_no_percent_on_the_wire(
) {
    // RT-9 (AC-17): a weekly-only active held by a 3600 s `Retry-After` past
    // `session_blind_swap_secs`, with two viable spares (a session-less `Retry-After` swap is
    // speculative, and the #582 reserve breaker holds it on a sole candidate). The swap fires, its
    // notice is retained, and `status` narrates it with both labels and no percent: never a `0`,
    // never a dropped record. The percent is read through the serialised bytes only.
    let mut daemon =
        blind_retry_after_daemon_without_a_session_window(Duration::from_secs(3600)).await;
    let swapped = daemon.tick().await;
    assert!(
        matches!(
            swapped.action,
            TickAction::PreemptivelySwapped { from: 0, .. }
        ),
        "the server directive swaps the blind weekly-only active away: {:?}",
        swapped.action,
    );
    assert!(
        daemon.state.last_blind_preempt_swap.is_some(),
        "the swap off a session-less anchor retains its notice record",
    );

    let target = swapped
        .snapshot
        .accounts
        .iter()
        .find(|account| account.active)
        .map(|account| account.label.clone())
        .expect("an account is active after the swap");
    assert_ne!(target, "work", "the active moved off the throttled account");
    let expected = format!(r#"{{"from_label":"work","to_label":"{target}"}}"#);

    let notice = serde_json::to_string(&swapped.snapshot.recent_blind_preempt_swap).unwrap();
    assert_eq!(
        notice, expected,
        "the same tick's snapshot narrates the swap with both labels and no percent key",
    );
    assert!(!notice.contains("last_known_session_pct"), "{notice}");

    let readings = daemon.decision_readings(daemon.state.active);
    let later = serde_json::to_string(
        &daemon
            .snapshot(daemon.state.active, &readings, 0)
            .recent_blind_preempt_swap,
    )
    .unwrap();
    assert_eq!(
        later, expected,
        "a later snapshot narrates the same notice, still with no percent",
    );
}

#[test]
fn recent_blind_preempt_swap_view_carries_an_absent_percent_through() {
    // RT-10 (AC-17): the projection copies the record's percent as it stands — absent stays absent,
    // never `Some(0)`; a present percent stays that percent.
    let base = Instant::now();
    let record = |last_known_session_pct: Option<u8>| BlindPreemptSwapRecord {
        from: "spare".to_owned(),
        to: "work".to_owned(),
        last_known_session_pct,
        at: base,
    };

    let absent = recent_blind_preempt_swap_view(Some(&record(None)), Some("work"), base)
        .expect("a current, recent swap is narrated");
    assert_eq!(absent.from_label, "spare");
    assert_eq!(absent.to_label, "work");
    assert_eq!(
        absent.last_known_session_pct, None,
        "an anchor without a session window projects no percent",
    );

    let present = recent_blind_preempt_swap_view(Some(&record(Some(68))), Some("work"), base)
        .expect("a current, recent swap is narrated");
    assert_eq!(present.last_known_session_pct, Some(68));
}
