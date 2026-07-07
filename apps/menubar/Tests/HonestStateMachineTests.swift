// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Pure, synchronous transition tests for the honest-state machine (issue #324) — the D2 decision
// core. Because the machine is a value type with no I/O, no clock, and no concurrency, every
// transition is exercised deterministically here (no socket, no async, no #328 harness); the thin
// `WatchStatusStore` shell is covered separately in `WatchStatusStoreTests`. Each test maps to an
// acceptance criterion; the load-bearing one is `testNeverHealthyOnAnyDegradedOrAbsentPath`.

import XCTest

final class HonestStateMachineTests: XCTestCase {

    /// Fold a sequence of events into a fresh machine and return it.
    private func machine(_ events: [TransportEvent]) -> HonestStateMachine {
        var m = HonestStateMachine()
        for event in events { _ = m.apply(event) }
        return m
    }

    // MARK: - AC: snapshot → connected + rows

    func testInitialStateIsConnectingNotHealthy() {
        let m = HonestStateMachine()
        XCTAssertEqual(m.connectionState, .connecting)
        XCTAssertEqual(m.presentation.glyph, .connecting)
        XCTAssertFalse(m.connectionState.isHealthy)
        XCTAssertTrue(m.rows.isEmpty)
        XCTAssertNil(m.generatedAt)
    }

    func testSnapshotWithAccountsGoesConnectedHealthy() {
        let m = machine([.connected, .line(Fixtures.snapshotBasic)])
        XCTAssertEqual(m.connectionState, .connected)
        XCTAssertTrue(m.connectionState.isHealthy)
        XCTAssertEqual(m.presentation.glyph, .healthy)
        XCTAssertEqual(m.rows.count, 1)
        XCTAssertEqual(m.rows.first?.label, "work")
        XCTAssertEqual(m.generatedAt, 42)
        XCTAssertEqual(m.refreshEnabled, false)
        XCTAssertNil(m.nextSwap)
    }

    // AC: a snapshot arriving WITHOUT a prior transport `.connected` still applies (the transport
    // buffers early lines; the store must not require `.connected` first).
    func testSnapshotBeforeConnectedStillApplies() {
        let m = machine([.line(Fixtures.snapshotBasic)])
        XCTAssertEqual(m.connectionState, .connected)
        XCTAssertEqual(m.rows.count, 1)
    }

    func testRowProjectionResolvesNextSwapTarget() throws {
        let m = machine([.connected, .line(Fixtures.snapshotRichTarget)])
        XCTAssertEqual(m.connectionState, .connected)
        XCTAssertEqual(m.rows.count, 2)
        XCTAssertEqual(m.nextSwap, .target(to: "personal"))
        XCTAssertEqual(m.refreshEnabled, true)

        let work = try XCTUnwrap(m.rows.first { $0.label == "work" })
        let personal = try XCTUnwrap(m.rows.first { $0.label == "personal" })
        XCTAssertTrue(work.isActive)
        XCTAssertEqual(work.auth, .atRisk)
        XCTAssertFalse(work.isNextSwapTarget)
        XCTAssertTrue(personal.isNextSwapTarget, "next_swap target is marked on its row")
        XCTAssertNil(personal.sessionPct)
    }

    // MARK: - AC: empty accounts → empty-roster (DISTINCT from daemon-down)

    func testEmptyAccountsGoesEmptyRosterNotDisconnectedNotHealthy() {
        let m = machine([.connected, .line(Fixtures.snapshotEmptyRoster)])
        XCTAssertEqual(m.connectionState, .emptyRoster)
        XCTAssertEqual(m.presentation.glyph, .empty)
        XCTAssertFalse(m.connectionState.isHealthy, "zero accounts is NOT the healthy state")
        XCTAssertNotEqual(m.connectionState, .connected)
        // Distinct from a daemon-down state: the daemon is present and answering.
        if case .disconnected = m.connectionState { XCTFail("empty-roster must not be a disconnect") }
        XCTAssertTrue(m.rows.isEmpty)
    }

    // MARK: - AC: `.disconnected` → last-good marked stale, NEVER shown as live

    func testDisconnectMarksLastGoodStaleNeverLive() {
        let m = machine([.connected, .line(Fixtures.snapshotBasic),
                         .disconnected(reason: "connection closed (EOF)")])
        XCTAssertEqual(m.connectionState, .disconnected(reason: "connection closed (EOF)"))
        XCTAssertEqual(m.presentation.glyph, .disconnected)
        XCTAssertFalse(m.connectionState.isHealthy, "a dropped daemon is never healthy")
        // Last-good roster is RETAINED (not blanked) — the panel dims it; the STATE says not-live.
        XCTAssertEqual(m.rows.count, 1, "last-good rows retained for a dimmed render")
        XCTAssertEqual(m.generatedAt, 42, "last-known freshness retained so the panel can show age")
    }

    // MARK: - AC: `.stale` → stale

    func testStaleAfterSnapshotGoesStaleNeverLive() {
        let m = machine([.connected, .line(Fixtures.snapshotBasic), .stale])
        XCTAssertEqual(m.connectionState, .stale)
        XCTAssertEqual(m.presentation.glyph, .stale)
        XCTAssertFalse(m.connectionState.isHealthy)
        XCTAssertEqual(m.rows.count, 1, "last-good rows retained, marked stale")
    }

    // MARK: - AC: schema major mismatch → unsupported (minimal), numbers refused

    func testUnsupportedMajorSnapshotRefusesNumbers() {
        let m = machine([.connected, .line(Fixtures.snapshotUnsupportedMajor)])
        XCTAssertEqual(m.connectionState, .unsupported)
        XCTAssertEqual(m.presentation.glyph, .unsupported)
        XCTAssertFalse(m.connectionState.isHealthy)
        XCTAssertTrue(m.rows.isEmpty, "numbers refused — no roster from an unreadable contract")
        XCTAssertNil(m.nextSwap)
        XCTAssertNil(m.refreshEnabled)
    }

    func testPreFreezeSnapshotIsUnsupported() {
        // Absent schema_version → major 0 → unsupported (fail-safe), never mis-rendered as healthy.
        let m = machine([.connected, .line(Fixtures.snapshotPreFreeze)])
        XCTAssertEqual(m.connectionState, .unsupported)
        XCTAssertFalse(m.connectionState.isHealthy)
    }

    func testUnsupportedHeartbeatIsUnsupported() {
        let m = machine([.connected, .line(Fixtures.heartbeatPreFreeze)])
        XCTAssertEqual(m.connectionState, .unsupported)
        XCTAssertEqual(m.presentation.glyph, .unsupported)
    }

    // MARK: - THE load-bearing invariant: never healthy on a degraded or absent daemon

    // Exhaustively assert that NO degraded / absent path yields the healthy state or glyph. Each row
    // is a sequence of events that must leave the machine non-healthy.
    func testNeverHealthyOnAnyDegradedOrAbsentPath() {
        let degradedPaths: [(name: String, events: [TransportEvent])] = [
            ("fresh, never connected", []),
            ("connected, no snapshot yet", [.connected]),
            ("error-only line (unknown frame)", [.connected, .line(#"{"error":"unknown command"}"#)]),
            ("undecodable garbage line", [.connected, .line("not json")]),
            ("heartbeat only, no snapshot", [.connected, .line(Fixtures.heartbeatBasic)]),
            ("empty roster", [.connected, .line(Fixtures.snapshotEmptyRoster)]),
            ("unsupported major snapshot", [.connected, .line(Fixtures.snapshotUnsupportedMajor)]),
            ("stale after a healthy snapshot",
             [.connected, .line(Fixtures.snapshotBasic), .stale]),
            ("disconnected after a healthy snapshot",
             [.connected, .line(Fixtures.snapshotBasic), .disconnected(reason: "EOF")]),
            ("bare reconnect after a healthy snapshot (no fresh snapshot yet)",
             [.connected, .line(Fixtures.snapshotBasic),
              .disconnected(reason: "EOF"), .connected]),
            ("garbage line while stale (must not un-stale into healthy)",
             [.connected, .line(Fixtures.snapshotBasic), .stale, .line("not json")]),
            ("unknown frame while stale (must not un-stale into healthy)",
             [.connected, .line(Fixtures.snapshotBasic), .stale, .line(#"{"error":"x"}"#)]),
            ("heartbeat after disconnect, no reconnect (must not resurrect the pre-drop roster)",
             [.connected, .line(Fixtures.snapshotBasic),
              .disconnected(reason: "EOF"), .line(Fixtures.heartbeatBasic)]),
        ]
        for path in degradedPaths {
            let m = machine(path.events)
            XCTAssertFalse(m.connectionState.isHealthy,
                           "MUST NOT be healthy: \(path.name) → \(m.connectionState)")
            XCTAssertNotEqual(m.presentation.glyph, .healthy,
                              "MUST NOT show the healthy glyph: \(path.name)")
        }
    }

    // MARK: - AC: an error-only / snapshot-less stream leaves the store non-healthy

    func testErrorOnlyStreamNeverHealthyThenDegradesToStale() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        // A pre-#164 daemon: `{"error":…}` is valid JSON with no `type` → an ignored unknown frame.
        let outcome = m.apply(.line(#"{"error":"unknown command"}"#))
        XCTAssertEqual(outcome, .ignoredUnknownFrame)
        XCTAssertEqual(m.connectionState, .connecting, "an error line yields no roster → still connecting")
        XCTAssertFalse(m.connectionState.isHealthy)
        // Then the daemon goes silent → transport `.stale`.
        _ = m.apply(.stale)
        XCTAssertEqual(m.connectionState, .stale)
        XCTAssertFalse(m.connectionState.isHealthy)
    }

    // MARK: - Decode-defensiveness: undecodable / unknown lines are non-fatal, never corrupt state

    func testUndecodableLineIsNonFatalAndDoesNotAdvanceLiveness() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        let outcome = m.apply(.line("definitely not json"))
        guard case .ignoredUndecodable = outcome else {
            return XCTFail("expected .ignoredUndecodable, got \(String(describing: outcome))")
        }
        XCTAssertEqual(m.connectionState, .connecting, "a garbage line leaves state untouched, never healthy")
        XCTAssertTrue(m.rows.isEmpty)
    }

    func testGarbageWhileStaleDoesNotClearStale() {
        // The crux of decode-defensiveness: a decode-fail must NOT resurrect a healthy view from a
        // stale one. The transport re-arms its own liveness on any byte, but the STORE stays stale
        // until VALID data arrives — its honesty tracks valid frames, not raw bytes.
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connected)
        _ = m.apply(.stale)
        XCTAssertEqual(m.connectionState, .stale)
        _ = m.apply(.line("not json"))
        XCTAssertEqual(m.connectionState, .stale, "garbage must not un-stale")
        _ = m.apply(.line(#"{"error":"still broken"}"#))
        XCTAssertEqual(m.connectionState, .stale, "an unknown frame must not un-stale")
    }

    // MARK: - Heartbeats are liveness-only, NOT snapshots

    func testHeartbeatAloneIsLivenessOnlyNeverHealthy() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        let outcome = m.apply(.line(Fixtures.heartbeatBasic))
        XCTAssertEqual(outcome, .appliedHeartbeat)
        XCTAssertEqual(m.generatedAt, 42, "a heartbeat refreshes the freshness stamp")
        XCTAssertTrue(m.rows.isEmpty, "a heartbeat carries no roster — never populates rows")
        XCTAssertEqual(m.connectionState, .connecting, "no snapshot yet → still connecting, not healthy")
        XCTAssertFalse(m.connectionState.isHealthy)
    }

    func testHeartbeatUnStalesToHealthyOnTheSameConnection() {
        // A heartbeat on a STILL-OPEN connection proves the last snapshot is current → it may return
        // a stale-but-valid roster to healthy. This is honest (the daemon demonstrably beat), and it
        // is NOT "treating a heartbeat as a snapshot" — the roster comes from the prior snapshot.
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        _ = m.apply(.stale)
        XCTAssertEqual(m.connectionState, .stale)
        _ = m.apply(.line(Fixtures.heartbeatBasic))
        XCTAssertEqual(m.connectionState, .connected, "beat on the same connection restores the valid roster")
        XCTAssertEqual(m.rows.count, 1, "the roster is the prior snapshot's, unchanged")
    }

    // MARK: - Reconnect must re-confirm with a fresh snapshot (no resurrection from stale rows)

    func testReconnectPassesThroughConnectingThenHealthyOnFreshSnapshot() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        _ = m.apply(.disconnected(reason: "EOF"))
        XCTAssertFalse(m.connectionState.isHealthy)
        // Bare reconnect: the socket is back but no fresh snapshot yet → connecting, NOT resurrected
        // healthy from the pre-drop roster.
        _ = m.apply(.connected)
        XCTAssertEqual(m.connectionState, .connecting, "reconnect must re-confirm before healthy")
        XCTAssertFalse(m.connectionState.isHealthy)
        // A fresh snapshot promotes to healthy.
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connected)
    }

    // Defense-in-depth: a disconnect drops the snapshot classification, so even a heartbeat arriving
    // BEFORE the reconnect `.connected` (out of the transport's normal ordering) cannot resurrect the
    // pre-drop roster as healthy — a fresh snapshot is required. (Contrast with the same heartbeat on
    // a still-open connection after `.stale`, which DOES restore healthy — see the un-stale test.)
    func testHeartbeatAfterDisconnectDoesNotResurrectHealthy() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        _ = m.apply(.disconnected(reason: "EOF"))
        _ = m.apply(.line(Fixtures.heartbeatBasic))
        XCTAssertEqual(m.connectionState, .connecting)
        XCTAssertFalse(m.connectionState.isHealthy)
        _ = m.apply(.line(Fixtures.snapshotBasic))    // a fresh snapshot re-earns healthy
        XCTAssertEqual(m.connectionState, .connected)
    }

    // A snapshot arriving AFTER stale (same connection) refreshes the roster and returns to healthy.
    func testFreshSnapshotAfterStaleReturnsHealthy() {
        let m = machine([.connected, .line(Fixtures.snapshotBasic), .stale,
                         .line(Fixtures.snapshotRichTarget)])
        XCTAssertEqual(m.connectionState, .connected)
        XCTAssertEqual(m.rows.count, 2, "the fresh snapshot's roster replaced the old one")
    }

    // A snapshot that goes from accounts → empty is honestly reflected (roster emptied, not stuck).
    func testHealthyThenEmptySnapshotBecomesEmptyRoster() {
        let m = machine([.connected, .line(Fixtures.snapshotBasic), .line(Fixtures.snapshotEmptyRoster)])
        XCTAssertEqual(m.connectionState, .emptyRoster)
        XCTAssertTrue(m.rows.isEmpty)
    }

    // MARK: - Presentation labels (spot-check the a11y surface per state)

    func testPresentationAccessibilityLabelsPerState() {
        XCTAssertEqual(HonestStateMachine().presentation.accessibilityLabel,
                       "Sessiometer: connecting to the daemon…")
        XCTAssertEqual(machine([.line(Fixtures.snapshotBasic)]).presentation.accessibilityLabel,
                       "Sessiometer: live — 1 account")
        XCTAssertEqual(machine([.line(Fixtures.snapshotRichTarget)]).presentation.accessibilityLabel,
                       "Sessiometer: live — 2 accounts", "plural agreement")
        XCTAssertEqual(machine([.line(Fixtures.snapshotEmptyRoster)]).presentation.accessibilityLabel,
                       "Sessiometer: connected — no accounts configured")
        XCTAssertEqual(machine([.line(Fixtures.snapshotBasic), .stale]).presentation.accessibilityLabel,
                       "Sessiometer: data may be stale — the daemon has gone quiet")
        XCTAssertEqual(machine([.disconnected(reason: "EOF")]).presentation.accessibilityLabel,
                       "Sessiometer: disconnected — the daemon is not responding")
        XCTAssertEqual(machine([.line(Fixtures.snapshotUnsupportedMajor)]).presentation.accessibilityLabel,
                       "Sessiometer: daemon version unsupported — update required")
    }
}
