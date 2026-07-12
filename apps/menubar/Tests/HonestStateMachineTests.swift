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
        XCTAssertEqual(m.nextSwap, .target(to: "personal", reason: .onlyCandidate))
        XCTAssertEqual(m.refreshEnabled, true)

        let work = try XCTUnwrap(m.rows.first { $0.label == "work" })
        let personal = try XCTUnwrap(m.rows.first { $0.label == "personal" })
        XCTAssertTrue(work.isActive)
        XCTAssertEqual(work.auth, .atRisk)
        XCTAssertFalse(work.isNextSwapTarget)
        XCTAssertTrue(personal.isNextSwapTarget, "next_swap target is marked on its row")
        XCTAssertNil(personal.sessionPct)
    }

    // #469 / #516: the daemon-level `canonical_scrub` rollup projects from the snapshot exactly as
    // `nextSwap` does — a fleet-wide fault that rides ALONGSIDE a healthy roster (each row reads healthy
    // while the shared `Claude Code-credentials` item sits emptied). The View surfaces it as an honest
    // banner; here we assert the pure core carries the discriminant so the store can publish it.
    func testProjectsCanonicalScrubAlongsideAHealthyRoster() throws {
        let exhausted = machine([.connected, .line(Fixtures.snapshotCanonicalScrubExhausted)])
        XCTAssertEqual(exhausted.canonicalScrub, .exhausted)
        // The scrub is a SEPARATE daemon-level signal, not a connection degradation: the roster still
        // reads healthy/connected (the crown-jewel case the banner exists to surface honestly).
        XCTAssertEqual(exhausted.connectionState, .connected)
        XCTAssertEqual(exhausted.rows.count, 1)
        XCTAssertEqual(try XCTUnwrap(exhausted.rows.first).auth, .healthy)

        let recovering = machine([.connected, .line(Fixtures.snapshotCanonicalScrubRecovering)])
        XCTAssertEqual(recovering.canonicalScrub, .recovering)

        // A healthy daemon (the wire key omitted) carries no scrub → nil, so no banner ever renders.
        let healthy = machine([.connected, .line(Fixtures.snapshotBasic)])
        XCTAssertNil(healthy.canonicalScrub)

        // Retained across a transition to `.stale` (like `rows`/`nextSwap`) — the View renders the scrub
        // banner in `.stale` too, off the last-known value, so a quiet-then-scrubbed daemon still warns.
        let staleScrub = machine([.connected, .line(Fixtures.snapshotCanonicalScrubExhausted), .stale])
        XCTAssertEqual(staleScrub.connectionState, .stale)
        XCTAssertEqual(staleScrub.canonicalScrub, .exhausted, "scrub retained into stale, like the roster")
    }

    // #498 / #521: the daemon-level `keychain_locked` rollup projects from the snapshot exactly as
    // `canonicalScrub`/`nextSwap` do — a fleet-wide fault that rides ALONGSIDE a healthy roster (each row
    // reads healthy while the LOCKED login keychain makes the shared `Claude Code-credentials` item
    // unreadable). The View surfaces it as an honest banner; here we assert the pure core carries the
    // discriminant so the store can publish it.
    func testProjectsKeychainLockedAlongsideAHealthyRoster() throws {
        let locked = machine([.connected, .line(Fixtures.snapshotKeychainLocked)])
        XCTAssertTrue(locked.keychainLocked)
        // The lock is a SEPARATE daemon-level signal, not a connection degradation: the roster still reads
        // healthy/connected (the crown-jewel case the banner exists to surface honestly).
        XCTAssertEqual(locked.connectionState, .connected)
        XCTAssertEqual(locked.rows.count, 1)
        XCTAssertEqual(try XCTUnwrap(locked.rows.first).auth, .healthy)

        // A healthy daemon (the wire key omitted → false) carries no lock → no banner ever renders.
        let healthy = machine([.connected, .line(Fixtures.snapshotBasic)])
        XCTAssertFalse(healthy.keychainLocked)

        // Retained across a transition to `.stale` (like `rows`/`canonicalScrub`) — the View renders the
        // lock banner in `.stale` too, off the last-known value, so a quiet-then-locked daemon still warns.
        let staleLocked = machine([.connected, .line(Fixtures.snapshotKeychainLocked), .stale])
        XCTAssertEqual(staleLocked.connectionState, .stale)
        XCTAssertTrue(staleLocked.keychainLocked, "lock retained into stale, like the roster")
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
            ("cold connect-refused within grace — starting (#499)", [.disconnected(reason: "connect refused")]),
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

    func testReconnectPassesThroughConnectingThenHealthyAfterStabilityWindow() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connected, "first connect promotes immediately (never debounced)")
        _ = m.apply(.disconnected(reason: "EOF"))
        XCTAssertFalse(m.connectionState.isHealthy)
        // Bare reconnect: the socket is back but no fresh snapshot yet → connecting, NOT resurrected
        // healthy from the pre-drop roster.
        _ = m.apply(.connected)
        XCTAssertEqual(m.connectionState, .connecting, "reconnect must re-confirm before healthy")
        XCTAssertFalse(m.connectionState.isHealthy)
        // A fresh snapshot post-reconnect is HELD by the crash-loop debounce (#169) — the connection
        // must survive the stability window before healthy, so a would-be flash never renders healthy.
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connecting, "post-reconnect snapshot is held (debounced), not healthy")
        XCTAssertTrue(m.isStabilizing)
        XCTAssertFalse(m.connectionState.isHealthy)
        // Surviving the stability window promotes it to healthy.
        m.stabilityElapsed(generation: m.stabilityGeneration)
        XCTAssertEqual(m.connectionState, .connected, "stayed up past the stability window → healthy")
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
        // A fresh snapshot re-confirms, but the crash-loop debounce (#169) HOLDS it until the connection
        // survives the stability window — a post-reconnect snapshot is not immediately healthy.
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connecting, "held by the debounce, not yet healthy")
        m.stabilityElapsed(generation: m.stabilityGeneration)
        XCTAssertEqual(m.connectionState, .connected, "survived the stability window → healthy")
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

    // MARK: - AC (#344): the store-side valid-frame watchdog

    // THE #344 regression, closed. A daemon that holds the connection open and streams ONLY
    // undecodable / unknown / error frames (spaced < the window, so the TRANSPORT's byte timer is
    // perpetually re-armed and never emits `.stale`) after one healthy snapshot must NOT keep the
    // store healthy. The store's OWN watchdog — keyed on VALID decodable frames, not raw bytes — trips
    // `.stale`. This is the exact scenario the 130 prior tests missed; asserted deterministically here
    // via the generation-guarded watchdog seam (no real clock), exactly as `WatchStateMachineTests`
    // drives the transport's liveness timer by feeding a generation-guarded `livenessElapsed`.
    func testContinuousUndecodableFramesAfterHealthyTripWatchdogToStale() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connected, "healthy after the snapshot")
        let armed = m.watchdogGeneration                  // the watchdog armed by the healthy snapshot

        // A continuous stream of NON-decodable frames — garbage, a pre-#164 error line, an unknown
        // future frame, a type-less line — none of which is valid liveness. Each re-arms the
        // TRANSPORT's byte timer in production; NONE must advance the store's valid-frame watchdog.
        for line in ["not json", #"{"error":"unknown command"}"#, "@@@garbage@@@",
                     Fixtures.unknownFutureType, Fixtures.noTypeTag, "still not json"] {
            _ = m.apply(.line(line))
            XCTAssertEqual(m.connectionState, .connected,
                           "still healthy WHILE garbage flows < window — the watchdog has not elapsed yet")
        }
        XCTAssertEqual(m.watchdogGeneration, armed,
                       "garbage / unknown / error lines must NOT re-arm the valid-frame watchdog")

        // The window elapses with no valid frame → the store downgrades ITSELF to stale, independent
        // of the transport (which, byte-live, would never have emitted `.stale`).
        m.watchdogElapsed(generation: armed)
        XCTAssertEqual(m.connectionState, .stale, "no valid frame in the window → the store goes stale")
        XCTAssertFalse(m.connectionState.isHealthy, "MUST NOT render healthy on a garbage-emitting daemon")
        XCTAssertEqual(m.presentation.glyph, .stale)
    }

    // The general never-healthy case: a daemon that connects and then streams ONLY garbage — never a
    // single valid snapshot — must not sit at `.connecting` forever on a byte-live socket. The
    // watchdog (armed by `.connected`, never re-armed absent a valid frame) downgrades it to `.stale`.
    func testConnectThenOnlyGarbageTripsWatchdogFromConnecting() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        XCTAssertEqual(m.connectionState, .connecting)
        let armed = m.watchdogGeneration                   // armed by `.connected`; no valid frame re-arms it
        for line in ["not json", #"{"error":"unknown command"}"#, Fixtures.noTypeTag, "@@@garbage@@@"] {
            _ = m.apply(.line(line))
            XCTAssertEqual(m.connectionState, .connecting, "no valid frame yet → still connecting, never healthy")
        }
        XCTAssertEqual(m.watchdogGeneration, armed, "garbage never re-armed the watchdog")
        m.watchdogElapsed(generation: armed)
        XCTAssertEqual(m.connectionState, .stale, "connect-then-only-garbage goes stale, not stuck connecting")
        XCTAssertFalse(m.connectionState.isHealthy)
    }

    // AC (#344): a HEARTBEAT is a valid frame — it RESETS the watchdog (re-arms under a new token) and
    // keeps a still-open healthy connection healthy, so a daemon beating within the window is NEVER
    // falsely marked stale (no over-correction into flagging healthy daemons). The superseded
    // pre-heartbeat token is ignored; only elapsing the CURRENT token trips stale.
    func testHeartbeatWithinWindowResetsWatchdogAndKeepsHealthy() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        let beforeBeat = m.watchdogGeneration

        XCTAssertEqual(m.apply(.line(Fixtures.heartbeatBasic)), .appliedHeartbeat)
        XCTAssertNotEqual(m.watchdogGeneration, beforeBeat, "a heartbeat re-arms the watchdog")
        XCTAssertEqual(m.connectionState, .connected, "the beat keeps the healthy roster healthy")

        // The SUPERSEDED (pre-heartbeat) timer firing late is ignored — the beat reset the countdown.
        m.watchdogElapsed(generation: beforeBeat)
        XCTAssertEqual(m.connectionState, .connected, "a superseded watchdog token must not trip stale")
        XCTAssertTrue(m.connectionState.isHealthy)

        // Only after the CURRENT window elapses with no further valid frame does it finally go stale.
        m.watchdogElapsed(generation: m.watchdogGeneration)
        XCTAssertEqual(m.connectionState, .stale)
    }

    // The watchdog is a strictly-narrow downgrade: it fires only on the CURRENT token AND only on a
    // LIVE connection, so a stale token, or an elapse after a drop, is a harmless no-op — it can never
    // manufacture `.stale` from `.disconnected` / `.initial`, nor fire twice.
    func testWatchdogElapseIsGenerationGuardedLiveOnlyAndIdempotent() {
        // A stale token after a fresh snapshot re-armed the watchdog → ignored.
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        m.watchdogElapsed(generation: 0)                   // an old token → ignored
        XCTAssertEqual(m.connectionState, .connected)

        // After a disconnect the connection is not live → an elapse cannot resurrect a `.stale`.
        var n = HonestStateMachine()
        _ = n.apply(.connected)
        _ = n.apply(.line(Fixtures.snapshotBasic))
        let tokenBeforeDrop = n.watchdogGeneration
        _ = n.apply(.disconnected(reason: "EOF"))
        n.watchdogElapsed(generation: tokenBeforeDrop)     // token superseded by the drop anyway
        if case .disconnected = n.connectionState {} else {
            XCTFail("an elapse after disconnect must stay disconnected, got \(n.connectionState)")
        }

        // Idempotent: once stale, a repeat elapse of the same token stays stale (no double-fire).
        var p = HonestStateMachine()
        _ = p.apply(.connected)
        _ = p.apply(.line(Fixtures.snapshotBasic))
        let t = p.watchdogGeneration
        p.watchdogElapsed(generation: t)
        XCTAssertEqual(p.connectionState, .stale)
        p.watchdogElapsed(generation: t)
        XCTAssertEqual(p.connectionState, .stale, "a second elapse of the same token is a no-op")
    }

    // The watchdog un-stales exactly like the transport's `.stale`: after it trips, a fresh snapshot
    // (or heartbeat) on the still-open connection re-earns healthy and re-arms the watchdog.
    func testValidFrameAfterWatchdogStaleReturnsHealthyAndReArms() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        m.watchdogElapsed(generation: m.watchdogGeneration)
        XCTAssertEqual(m.connectionState, .stale)

        _ = m.apply(.line(Fixtures.snapshotBasic))         // a fresh valid frame un-stales + re-arms
        XCTAssertEqual(m.connectionState, .connected)
        let rearmed = m.watchdogGeneration
        m.watchdogElapsed(generation: rearmed)             // and the watchdog can trip again
        XCTAssertEqual(m.connectionState, .stale)
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
        // A WARM drop — a live connection was HELD (the snapshot) then lost — is the socket-dropped state
        // (#499: `hasEverConnected` discriminates this from a cold connect-refused).
        XCTAssertEqual(machine([.connected, .line(Fixtures.snapshotBasic), .disconnected(reason: "EOF")]).presentation.accessibilityLabel,
                       "Sessiometer: disconnected — the daemon is not responding")
        XCTAssertEqual(machine([.line(Fixtures.snapshotUnsupportedMajor)]).presentation.accessibilityLabel,
                       "Sessiometer: daemon version unsupported — update required")
        // #499: a COLD connect-refused (fresh machine, never connected) is the transient starting state —
        // NOT the socket-dropped label (the pre-#499 collapse this fixes).
        XCTAssertEqual(machine([.disconnected(reason: "connect refused")]).presentation.accessibilityLabel,
                       "Sessiometer: the daemon is starting…")
        // …and the durable not-running label (built directly; the grace-driven escalation is exercised in
        // the dedicated transition tests below).
        XCTAssertEqual(PresentationState.make(for: .notRunning, accountCount: 0).accessibilityLabel,
                       "Sessiometer: the daemon is not running")
    }

    // MARK: - AC (#169): the crash-loop healthy-flash debounce

    /// Fold a healthy snapshot into a machine that has ALREADY disconnected once (so the debounce is
    /// armed), leaving a held (stabilizing) post-reconnect snapshot awaiting the stability window.
    private func reconnectedWithHeldSnapshot() -> HonestStateMachine {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))          // first connect → healthy immediately
        _ = m.apply(.disconnected(reason: "EOF"))           // arms the debounce (hasEverDisconnected)
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))          // post-reconnect snapshot → HELD
        return m
    }

    // The clean-start happy path is UNCHANGED: the very first connect promotes to healthy the instant a
    // fresh snapshot arrives — the debounce is armed only by a prior drop, so a cold start is immediate.
    func testFirstConnectPromotesToHealthyImmediatelyNoDebounce() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connected)
        XCTAssertFalse(m.isStabilizing, "the first connect is never debounced")
        XCTAssertEqual(m.stabilityGeneration, 0, "no stability timer is armed on a cold start")
    }

    // A post-reconnect snapshot is HELD (not healthy) until the connection survives the stability window
    // — a single clean restart reads as the benign `.connecting`, never the `.crashLooping` fault.
    func testPostReconnectSnapshotIsHeldUntilStabilityWindow() {
        let m = reconnectedWithHeldSnapshot()
        XCTAssertEqual(m.connectionState, .connecting, "a single restart holds as connecting, not crash-looping")
        XCTAssertTrue(m.isStabilizing)
        XCTAssertFalse(m.connectionState.isHealthy, "the healthy-flash is debounced")
        XCTAssertEqual(m.consecutiveUnstableReconnects, 0)
    }

    // Surviving the stability window promotes the held snapshot to healthy and re-earns stability.
    func testStabilityWindowSurvivedPromotesToHealthy() {
        var m = reconnectedWithHeldSnapshot()
        m.stabilityElapsed(generation: m.stabilityGeneration)
        XCTAssertEqual(m.connectionState, .connected)
        XCTAssertFalse(m.isStabilizing, "stabilized → no longer holding")
    }

    // THE crash-loop invariant: a daemon that repeatedly (re)connects, serves a snapshot, then DROPS
    // before the stability window elapses climbs the unstable-reconnect count and, past the threshold,
    // reads as `.crashLooping` — and NEVER healthy — for the whole loop (anti-#137 healthy-flash).
    func testRepeatedUnstableReconnectsSurfaceCrashLoopingNeverHealthy() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))          // first connect → one honest healthy moment
        _ = m.apply(.disconnected(reason: "EOF"))           // arms the debounce; churn still 0

        // Each subsequent cycle: reconnect → snapshot (HELD) → drop before stabilizing → churn += 1.
        var sawHealthyDuringLoop = false
        for _ in 0..<4 {
            _ = m.apply(.connected)
            _ = m.apply(.line(Fixtures.snapshotBasic))
            if m.connectionState.isHealthy { sawHealthyDuringLoop = true }
            _ = m.apply(.disconnected(reason: "EOF"))       // dropped before the stability window
        }
        XCTAssertFalse(sawHealthyDuringLoop, "a crash-looping daemon must NEVER flicker healthy (#137)")
        XCTAssertGreaterThanOrEqual(m.consecutiveUnstableReconnects, HonestStateMachine.crashLoopThreshold)

        // On the next held snapshot, past the threshold, the state is the `.crashLooping` fault.
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .crashLooping)
        XCTAssertEqual(m.presentation.glyph, .crashLooping)
        XCTAssertFalse(m.connectionState.isHealthy)
    }

    // A held snapshot that SURVIVES the window resets the churn — so a daemon that recovers stops
    // reading as crash-looping and returns to healthy.
    func testStabilizationResetsTheUnstableReconnectChurn() {
        var m = HonestStateMachine()
        _ = m.apply(.connected)
        _ = m.apply(.line(Fixtures.snapshotBasic))
        for _ in 0..<3 {                                     // drive several unstable reconnects
            _ = m.apply(.disconnected(reason: "EOF"))
            _ = m.apply(.connected)
            _ = m.apply(.line(Fixtures.snapshotBasic))
        }
        XCTAssertGreaterThanOrEqual(m.consecutiveUnstableReconnects, HonestStateMachine.crashLoopThreshold)
        XCTAssertEqual(m.connectionState, .crashLooping)
        // The daemon finally stays up past the window → churn clears, healthy returns.
        m.stabilityElapsed(generation: m.stabilityGeneration)
        XCTAssertEqual(m.connectionState, .connected)
        XCTAssertEqual(m.consecutiveUnstableReconnects, 0, "stabilizing clears the crash-loop churn")
    }

    // `stabilityElapsed` is generation-guarded, stabilizing-only, and idempotent — a superseded token,
    // an elapse after a drop, or a repeat elapse can never manufacture healthy or fire twice (mirrors
    // the watchdog's guard tests).
    func testStabilityElapseIsGenerationGuardedStabilizingOnlyAndIdempotent() {
        // A superseded token (an old generation) is ignored.
        var m = reconnectedWithHeldSnapshot()
        let held = m.stabilityGeneration
        m.stabilityElapsed(generation: held - 1)
        XCTAssertEqual(m.connectionState, .connecting, "a superseded stability token must not promote")
        XCTAssertTrue(m.isStabilizing)

        // An elapse after a drop (no longer stabilizing) cannot resurrect healthy.
        var n = reconnectedWithHeldSnapshot()
        let tokenBeforeDrop = n.stabilityGeneration
        _ = n.apply(.disconnected(reason: "EOF"))
        n.stabilityElapsed(generation: tokenBeforeDrop)
        XCTAssertFalse(n.connectionState.isHealthy, "an elapse after a drop must not promote to healthy")

        // Idempotent: a second elapse of the (now consumed) token is a no-op.
        var p = reconnectedWithHeldSnapshot()
        p.stabilityElapsed(generation: p.stabilityGeneration)
        XCTAssertEqual(p.connectionState, .connected)
        p.stabilityElapsed(generation: p.stabilityGeneration)
        XCTAssertEqual(p.connectionState, .connected, "a second elapse is a no-op")
    }

    // A transport `.stale` (or the watchdog) while a held snapshot is stabilizing ENDS the hold honestly
    // — it reads `.stale`, never healthy, and never leaves a dangling promote.
    func testStaleWhileStabilizingEndsTheHoldNeverHealthy() {
        var m = reconnectedWithHeldSnapshot()
        XCTAssertTrue(m.isStabilizing)
        _ = m.apply(.stale)
        XCTAssertEqual(m.connectionState, .stale)
        XCTAssertFalse(m.isStabilizing, "going stale ends the stabilization hold")
        // A superseded stability elapse can no longer promote it.
        m.stabilityElapsed(generation: m.stabilityGeneration)
        XCTAssertFalse(m.connectionState.isHealthy)
    }

    // The crash-loop presentation: a distinct fault glyph + a plain spoken label, never the healthy glyph.
    func testCrashLoopingPresentation() {
        let presentation = PresentationState.make(for: .crashLooping, accountCount: 2)
        XCTAssertEqual(presentation.glyph, .crashLooping)
        XCTAssertNotEqual(presentation.glyph, .healthy)
        XCTAssertFalse(ConnectionState.crashLooping.isHealthy)
        XCTAssertEqual(presentation.accessibilityLabel,
                       "Sessiometer: the daemon is restarting repeatedly — holding status until it stays up")
    }

    // MARK: - AC (#499): split daemon-starting (transient) from not-running (durable)

    // A fresh launch with the daemon absent: the transport refuses the connect and emits `.disconnected`
    // BEFORE any `.connected`. Never having held a live connection, that reads as the transient `.starting`
    // glance (a "daemon coming up" state that self-resolves) — NOT the socket-dropped state; then the grace
    // elapses still refused → the durable `.notRunning`. Neither is ever healthy.
    func testColdRefusedGoesStartingThenNotRunningAfterGraceNeverHealthy() {
        var m = HonestStateMachine()
        _ = m.apply(.disconnected(reason: "connect refused"))
        XCTAssertEqual(m.connectionState, .starting)
        XCTAssertEqual(m.presentation.glyph, .starting)
        XCTAssertFalse(m.connectionState.isHealthy)
        XCTAssertTrue(m.isAwaitingStartGrace)
        if case .disconnected = m.connectionState { XCTFail("a cold refusal must not read as socket-dropped") }

        // The grace elapses with the connect still refused → the durable not-running state (the one that
        // WOULD host a Start-daemon affordance, #170).
        m.graceElapsed(generation: m.graceGeneration)
        XCTAssertEqual(m.connectionState, .notRunning)
        XCTAssertEqual(m.presentation.glyph, .notRunning)
        XCTAssertFalse(m.connectionState.isHealthy)
        XCTAssertFalse(m.isAwaitingStartGrace, "not-running is durable — the grace is done")
    }

    // The SAME `.disconnected` transport event means DIFFERENT states by lineage (#499): a WARM drop (a
    // live connection was held, then lost) is the socket-dropped state; a COLD refusal (never connected) is
    // the starting state. They must NOT collapse to one presentation (the pre-#499 bug).
    func testWarmDropIsSocketDroppedNotStartingOrNotRunning() {
        let warm = machine([.connected, .line(Fixtures.snapshotBasic), .disconnected(reason: "EOF")])
        XCTAssertEqual(warm.connectionState, .disconnected(reason: "EOF"))
        XCTAssertEqual(warm.presentation.glyph, .disconnected)

        let cold = machine([.disconnected(reason: "connect refused")])
        XCTAssertEqual(cold.connectionState, .starting)
        XCTAssertEqual(cold.presentation.glyph, .starting)

        XCTAssertNotEqual(warm.connectionState, cold.connectionState,
                          "a warm drop and a cold refusal must NOT collapse to the same state (#499)")
        XCTAssertNotEqual(warm.presentation.glyph, cold.presentation.glyph)
    }

    // The backoff loop retries and is refused again several times: the state stays `.starting` and the grace
    // generation does NOT advance — so the real timer keeps counting toward not-running rather than resetting
    // on every retry (which would starve the escalation and never reach not-running).
    func testRepeatedColdRefusalStaysStartingAndDoesNotResetTheGrace() {
        var m = HonestStateMachine()
        _ = m.apply(.disconnected(reason: "connect refused"))
        XCTAssertEqual(m.connectionState, .starting)
        let armed = m.graceGeneration
        for _ in 0..<4 {
            _ = m.apply(.disconnected(reason: "connect refused"))
            XCTAssertEqual(m.connectionState, .starting)
            XCTAssertEqual(m.graceGeneration, armed, "a repeat refusal must NOT re-arm (reset) the grace")
        }
        m.graceElapsed(generation: armed)
        XCTAssertEqual(m.connectionState, .notRunning)
    }

    // THE load-bearing #499 ↔ #169 interaction: a daemon we were merely WAITING for (starting → not-running)
    // must promote to healthy IMMEDIATELY when it finally connects — a clean cold start, NOT a debounced
    // reconnect. The cold-refused track never arms the crash-loop debounce (`hasEverDisconnected`), so no
    // stability hold is ever imposed.
    func testDaemonConnectingAfterNotRunningPromotesImmediatelyNoDebounce() {
        var m = HonestStateMachine()
        _ = m.apply(.disconnected(reason: "connect refused"))   // cold refused → starting
        m.graceElapsed(generation: m.graceGeneration)           // grace elapses → not running
        XCTAssertEqual(m.connectionState, .notRunning)

        _ = m.apply(.connected)
        XCTAssertEqual(m.connectionState, .connecting, "connected socket, awaiting the first snapshot")
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connected, "promotes immediately — not debounced as a reconnect")
        XCTAssertFalse(m.isStabilizing, "the cold-refused track must not arm the crash-loop debounce")
        XCTAssertEqual(m.stabilityGeneration, 0, "no stability hold was ever armed by the cold-refused track")
    }

    // A daemon that connects DURING the grace (the genuinely-starting case) promotes immediately too — it
    // never reaches not-running, and the pending grace timer is superseded by the connect.
    func testDaemonConnectingDuringGracePromotesAndSupersedesTheGrace() {
        var m = HonestStateMachine()
        _ = m.apply(.disconnected(reason: "connect refused"))
        XCTAssertEqual(m.connectionState, .starting)
        let staleToken = m.graceGeneration
        _ = m.apply(.connected)                                 // the daemon came up within the grace
        XCTAssertFalse(m.isAwaitingStartGrace, "connecting cancels the start grace")
        _ = m.apply(.line(Fixtures.snapshotBasic))
        XCTAssertEqual(m.connectionState, .connected, "a daemon that came up within the grace goes healthy")
        // The superseded grace timer firing late is a harmless no-op — it cannot force not-running onto a
        // now-healthy connection.
        m.graceElapsed(generation: staleToken)
        XCTAssertEqual(m.connectionState, .connected, "a superseded grace elapse must not resurrect not-running")
    }

    // `graceElapsed` is generation-guarded, starting-only, and idempotent — a superseded token, an elapse
    // after the daemon connected, or a repeat elapse can never manufacture not-running or fire twice (mirrors
    // the watchdog / stability guard tests).
    func testGraceElapseIsGenerationGuardedStartingOnlyAndIdempotent() {
        // A superseded token (an old generation) is ignored.
        var m = HonestStateMachine()
        _ = m.apply(.disconnected(reason: "refused"))
        m.graceElapsed(generation: m.graceGeneration - 1)
        XCTAssertEqual(m.connectionState, .starting, "a superseded grace token must not escalate")

        // Starting-only: once escalated to not-running, a repeat elapse of the consumed token is a no-op.
        var p = HonestStateMachine()
        _ = p.apply(.disconnected(reason: "refused"))
        let t = p.graceGeneration
        p.graceElapsed(generation: t)
        XCTAssertEqual(p.connectionState, .notRunning)
        p.graceElapsed(generation: p.graceGeneration)
        XCTAssertEqual(p.connectionState, .notRunning, "a second elapse is a no-op")
    }

    // The presentation surface: starting and not-running are distinct glances from each other and — the
    // load-bearing pair — from the socket-dropped and stale glances, and neither is ever healthy.
    func testStartingAndNotRunningPresentationAreDistinctAndNeverHealthy() {
        let starting = PresentationState.make(for: .starting, accountCount: 0)
        let notRunning = PresentationState.make(for: .notRunning, accountCount: 0)
        XCTAssertEqual(starting.glyph, .starting)
        XCTAssertEqual(notRunning.glyph, .notRunning)
        XCTAssertNotEqual(starting.glyph, notRunning.glyph)
        XCTAssertNotEqual(starting.accessibilityLabel, notRunning.accessibilityLabel)
        for other: ConnectionState in [.disconnected(reason: "EOF"), .stale] {
            let o = PresentationState.make(for: other, accountCount: 0)
            XCTAssertNotEqual(starting.glyph, o.glyph, "starting must be distinct from \(other)")
            XCTAssertNotEqual(notRunning.glyph, o.glyph, "not-running must be distinct from \(other)")
            XCTAssertNotEqual(starting.accessibilityLabel, o.accessibilityLabel, "starting label must be distinct from \(other)")
            XCTAssertNotEqual(notRunning.accessibilityLabel, o.accessibilityLabel, "not-running label must be distinct from \(other)")
        }
        XCTAssertFalse(ConnectionState.starting.isHealthy)
        XCTAssertFalse(ConnectionState.notRunning.isHealthy)
    }
}
