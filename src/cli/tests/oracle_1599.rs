// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Issue #1599's pre-authored oracle at the CLI reads of the reshaped percent (RT-4, RT-5, RT-12 of
//! the issue's § 8). Authored apart from the build and frozen: the build makes these green and edits
//! none of them. The DEGRADED reason's words are the build's; only its presence is pinned.

use super::*;

/// A blind active `work` line carrying `blind`, everything else as `status_line` builds it.
fn blind_line(blind: BlindActive) -> AccountStatusLine {
    AccountStatusLine {
        blind_active: Some(blind),
        ..status_line("work", true, None, None)
    }
}

/// A response holding the one line, with no next-swap state (so the blind footer, not the CORNERED
/// alarm, renders).
fn response_with(line: AccountStatusLine) -> StatusResponse {
    StatusResponse {
        accounts: vec![line],
        next_swap: None,
        ..cornered_response(None, None)
    }
}

/// The one rendered line that holds `needle`.
fn line_holding<'a>(out: &'a str, needle: &str) -> &'a str {
    out.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line holds {needle:?} in: {out}"))
}

#[test]
fn render_status_blind_active_without_a_percent_states_the_duration_and_verdict_only() {
    // RT-4 (AC-4, AC-16): a blind block with no percent.
    let dur = humanize_until(480);
    let percent_less = |degraded: bool| BlindActive {
        blind_secs: 480,
        last_known_session_pct: None,
        auto_protection_degraded: degraded,
    };

    // The SESSION% cell falls back to the cleared live reading: `n/a`, uncoloured.
    let row = StatusRow::new(&blind_line(percent_less(true)), NOW);
    assert_eq!(
        row.session, "n/a",
        "an absent percent renders `n/a`, never `~0%` or `~n/a`"
    );
    assert_eq!(
        row.session_severity, None,
        "an absent percent leaves the cell uncoloured"
    );

    // DEGRADED: the duration and the verdict with a parenthetical reason, and nothing else.
    let out = render_status(
        &response_with(blind_line(percent_less(true))),
        NOW,
        None,
        false,
    );
    assert!(
        !out.contains("~0%") && !out.contains("~n/a"),
        "no fabricated or tokenised held percent anywhere: {out}"
    );
    let footer = line_holding(&out, "active work: blind for");
    assert!(
        footer.starts_with(&format!("active work: blind for {dur}")),
        "the footer names the duration: {footer}"
    );
    assert!(
        footer.contains("auto-protection DEGRADED ("),
        "the daemon's DEGRADED verdict, followed by its reason: {footer}"
    );
    for absent in ["%", "last-known", "stale anchor", "auto-protection OK"] {
        assert!(
            !footer.contains(absent),
            "a percent-less DEGRADED footer carries no {absent:?}: {footer}"
        );
    }

    // OK (the daemon's pre-gate verdict): printed as given, still with no percent clause.
    let out = render_status(
        &response_with(blind_line(percent_less(false))),
        NOW,
        None,
        false,
    );
    let footer = line_holding(&out, "active work: blind for");
    assert!(
        footer.contains("auto-protection OK"),
        "the renderer prints the daemon's verdict and never decides it: {footer}"
    );
    for absent in ["%", "last-known", "DEGRADED"] {
        assert!(
            !footer.contains(absent),
            "a percent-less OK footer carries no {absent:?}: {footer}"
        );
    }

    // A present percent renders exactly today's footer and cell, under both verdicts.
    for (degraded, verdict) in [(true, "DEGRADED (acting on a stale anchor)"), (false, "OK")] {
        let line = blind_line(BlindActive {
            blind_secs: 480,
            last_known_session_pct: Some(87),
            auto_protection_degraded: degraded,
        });
        assert_eq!(StatusRow::new(&line, NOW).session, "~87%");
        let out = render_status(&response_with(line), NOW, None, false);
        assert_eq!(
            line_holding(&out, "active work: blind for"),
            format!(
                "active work: blind for {dur} — last-known session 87% — auto-protection {verdict}"
            ),
            "a present percent keeps today's footer byte for byte",
        );
    }
}

#[test]
fn render_cornered_without_a_percent_omits_the_last_known_clause() {
    // RT-5 (AC-4): the CORNERED alarm drops its percent clause when the block has none; everything
    // else stays.
    let dur = humanize_until(480);
    let cornered = |last_known_session_pct: Option<u8>| {
        let mut response = cornered_response(None, None);
        response.accounts = vec![blind_line(BlindActive {
            blind_secs: 480,
            last_known_session_pct,
            auto_protection_degraded: true,
        })];
        response
    };

    let out = render_status(&cornered(None), NOW, None, false);
    let alarm = line_holding(&out, "CORNERED");
    assert!(
        alarm.starts_with(&format!("CORNERED: active work blind for {dur}")),
        "the alarm names the account and the duration: {alarm}"
    );
    assert!(
        alarm.ends_with("auto-protection cannot act — no viable target; add or free an account"),
        "the alarm keeps its verdict, blocker and remedy: {alarm}"
    );
    for absent in ["%", "last-known"] {
        assert!(
            !alarm.contains(absent),
            "a percent-less CORNERED alarm carries no {absent:?}: {alarm}"
        );
    }

    let out = render_status(&cornered(Some(87)), NOW, None, false);
    assert_eq!(
        line_holding(&out, "CORNERED"),
        format!(
            "CORNERED: active work blind for {dur} at last-known session 87% and auto-protection \
             cannot act — no viable target; add or free an account"
        ),
        "a present percent keeps today's alarm byte for byte",
    );
}

#[test]
fn render_blind_preempt_swap_without_a_percent_drops_the_last_known_clause() {
    // RT-12 (AC-17): the swap narration off a session-less anchor names both labels and the undo,
    // and carries no percent; a present percent keeps today's line.
    let with = |last_known_session_pct: Option<u8>| StatusResponse {
        recent_blind_preempt_swap: Some(BlindPreemptSwap {
            from_label: "spare".to_owned(),
            to_label: "work".to_owned(),
            last_known_session_pct,
        }),
        ..response_with(status_line("work", true, Some(20), Some(15)))
    };

    let line = render_blind_preempt_swap(&with(None));
    for present in ["swapped off spare", "work", "undo with 'use spare'"] {
        assert!(
            line.contains(present),
            "the narration keeps {present:?}: {line}"
        );
    }
    for absent in ["%", "last-known"] {
        assert!(
            !line.contains(absent),
            "a percent-less narration carries no {absent:?}: {line}"
        );
    }

    assert_eq!(
        render_blind_preempt_swap(&with(Some(68))),
        "swapped off spare (blind @ last-known 68%) → work; undo with 'use spare' if it recovered\n",
        "a present percent keeps today's line byte for byte",
    );
}
