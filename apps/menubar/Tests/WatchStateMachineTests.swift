// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Unit tests for the pure decision core of `WatchTransport` (issue #323). Because `WatchStateMachine`
// has no I/O, no clock, and no async, every reconnect / liveness / backoff transition is asserted
// synchronously and DETERMINISTICALLY here — no socket, no real time (the #328 mock-socket harness is
// a separate item). The actor-shell wiring is covered in `WatchTransportTests`.

import XCTest

final class WatchStateMachineTests: XCTestCase {

    // A machine with tiny, distinctive timings so effects are easy to assert.
    private let window = Duration.milliseconds(500)
    private func makeMachine() -> WatchStateMachine {
        WatchStateMachine(
            backoff: ExponentialBackoff(base: .milliseconds(10), multiplier: 2, cap: .milliseconds(80)),
            livenessWindow: window)
    }

    // MARK: - Connect

    // AC: connect + push-only. `start` asks the shell to connect; a success emits `.connected`
    // (BEFORE any line) and arms the liveness timer — never a poll/write effect (push-only).
    func testStartConnectsAndSuccessEmitsConnectedThenArmsLiveness() {
        var m = makeMachine()
        XCTAssertEqual(m.advance(.start), [.connect])
        XCTAssertEqual(m.state, .connecting)

        XCTAssertEqual(
            m.advance(.connectSucceeded),
            [.emit(.connected), .armLiveness(after: window, generation: 1)])
        XCTAssertEqual(m.state, .connected(stale: false))
    }

    // Push-only invariant: across a full connect→line→line→close cycle the machine NEVER emits an
    // effect that writes to the socket. The only effects are connect / closeConnection / emit /
    // arm-timer — there is no "send"/"poll" effect in the vocabulary at all.
    func testMachineNeverEmitsAWriteEffectAfterSubscribe() {
        var m = makeMachine()
        var all: [WatchStateMachine.Effect] = []
        all += m.advance(.start)
        all += m.advance(.connectSucceeded)
        all += m.advance(.lineReceived(#"{"type":"heartbeat"}"#))
        all += m.advance(.lineReceived(#"{"type":"snapshot"}"#))
        all += m.advance(.connectionClosed(reason: "eof"))
        for effect in all {
            switch effect {
            case .connect, .closeConnection, .emit, .armLiveness, .armBackoff:
                break   // the whole (push-only) effect vocabulary
            }
        }
        // And exactly one outbound action is ever implied: the single `.connect`.
        XCTAssertEqual(all.filter { $0 == .connect }.count, 1)
    }

    // MARK: - Lines + liveness → stale

    // AC: a received line surfaces as `.line` and RE-ARMS liveness under a new generation.
    func testLineReceivedEmitsLineAndReArmsLiveness() {
        var m = makeMachine()
        _ = m.advance(.start); _ = m.advance(.connectSucceeded)   // liveness generation 1
        XCTAssertEqual(
            m.advance(.lineReceived("hello")),
            [.emit(.line("hello")), .armLiveness(after: window, generation: 2)])
    }

    // AC: silence past the liveness window → `.stale`; the connection stays OPEN (no disconnect,
    // no reconnect). Emitted once per silence gap.
    func testLivenessElapsedEmitsStaleOnceAndStaysConnected() {
        var m = makeMachine()
        _ = m.advance(.start); _ = m.advance(.connectSucceeded)   // arms liveness gen 1
        XCTAssertEqual(m.advance(.livenessElapsed(generation: 1)), [.emit(.stale)])
        XCTAssertEqual(m.state, .connected(stale: true))
        // A second (spurious) fire of the same generation does NOT re-emit `.stale`.
        XCTAssertEqual(m.advance(.livenessElapsed(generation: 1)), [])
    }

    // AC: "the timer resets on every received line" — a superseded liveness timer that still fires
    // is IGNORED (generation guard), and a fresh line re-arms it.
    func testSupersededLivenessTimerIsIgnored() {
        var m = makeMachine()
        _ = m.advance(.start); _ = m.advance(.connectSucceeded)   // liveness gen 1
        _ = m.advance(.lineReceived("x"))                          // re-arms → liveness gen 2
        // The OLD gen-1 timer firing late is ignored (the line already reset liveness).
        XCTAssertEqual(m.advance(.livenessElapsed(generation: 1)), [])
        XCTAssertEqual(m.state, .connected(stale: false))
        // The CURRENT gen-2 timer firing → stale.
        XCTAssertEqual(m.advance(.livenessElapsed(generation: 2)), [.emit(.stale)])
    }

    // A line after going stale clears the stale flag; a later silence can go stale AGAIN.
    func testLineAfterStaleClearsItAndStaleCanRecur() {
        var m = makeMachine()
        _ = m.advance(.start); _ = m.advance(.connectSucceeded)   // gen 1
        _ = m.advance(.livenessElapsed(generation: 1))            // → stale
        XCTAssertEqual(m.state, .connected(stale: true))

        let effects = m.advance(.lineReceived("recovered"))       // clears stale, re-arms gen 2
        XCTAssertEqual(effects, [.emit(.line("recovered")), .armLiveness(after: window, generation: 2)])
        XCTAssertEqual(m.state, .connected(stale: false))

        XCTAssertEqual(m.advance(.livenessElapsed(generation: 2)), [.emit(.stale)])   // stale recurs
    }

    // MARK: - Liveness window default (vs the daemon's heartbeat)

    // AC #3: stale must trip only after "no line for >30s (>2× the daemon's 15s WATCH_HEARTBEAT)".
    // The DEFAULT window therefore has to exceed TWO full heartbeat intervals, so a single dropped
    // beat plus scheduling jitter never false-trips `.stale` — only a genuinely silent daemon does.
    // Pinning it here to the daemon contract (src/daemon/socket.rs WATCH_HEARTBEAT) means a future
    // edit can't quietly shrink the window below the AC threshold without turning this test red.
    func testDefaultLivenessWindowExceedsTwiceTheDaemonHeartbeat() {
        let daemonHeartbeat = Duration.seconds(15)   // src/daemon/socket.rs WATCH_HEARTBEAT
        let window = WatchStateMachine().livenessWindow
        XCTAssertGreaterThan(window, daemonHeartbeat * 2, "must tolerate one missed heartbeat (>2×15s)")
        XCTAssertGreaterThan(window, .seconds(30), "AC #3: stale only after >30s of silence")
    }

    // MARK: - Disconnect + bounded backoff

    // AC: daemon absent (connect fails) → `.disconnected` + a bounded-backoff retry (never a tight
    // loop). The first retry waits the base delay; no `.closeConnection` (there is no connection yet).
    func testConnectFailedEmitsDisconnectedAndArmsBackoff() {
        var m = makeMachine()
        _ = m.advance(.start)
        XCTAssertEqual(
            m.advance(.connectFailed(reason: "ENOENT")),
            [.emit(.disconnected(reason: "ENOENT")), .armBackoff(after: .milliseconds(10), generation: 1)])
        XCTAssertEqual(m.state, .backingOff)
        XCTAssertEqual(m.advance(.backoffElapsed(generation: 1)), [.connect])   // retry
    }

    // AC: EOF → `.disconnected` + backoff; the dropped connection is closed first, and the pending
    // liveness timer is invalidated (a late fire is ignored).
    func testConnectionClosedClosesEmitsDisconnectedAndArmsBackoff() {
        var m = makeMachine()
        _ = m.advance(.start); _ = m.advance(.connectSucceeded)   // liveness gen 1
        XCTAssertEqual(
            m.advance(.connectionClosed(reason: "eof")),
            [.closeConnection,
             .emit(.disconnected(reason: "eof")),
             .armBackoff(after: .milliseconds(10), generation: 1)])
        XCTAssertEqual(m.state, .backingOff)
        // The liveness timer armed while connected is now stale → ignored.
        XCTAssertEqual(m.advance(.livenessElapsed(generation: 1)), [])
    }

    // AC: bounded EXPONENTIAL backoff — repeated failures grow the delay (10 → 20 → 40 → 80 cap),
    // never a tight loop.
    func testRepeatedFailuresGrowBackoffExponentiallyToCap() {
        var m = makeMachine()
        _ = m.advance(.start)
        let expected: [Duration] = [.milliseconds(10), .milliseconds(20), .milliseconds(40),
                                    .milliseconds(80), .milliseconds(80)]
        for (i, delay) in expected.enumerated() {
            let effects = m.advance(.connectFailed(reason: "fail#\(i)"))
            XCTAssertEqual(effects.last, .armBackoff(after: delay, generation: i + 1),
                           "attempt \(i) should back off \(delay)")
            _ = m.advance(.backoffElapsed(generation: i + 1))     // → connecting, ready to fail again
        }
    }

    // A successful connect RESETS the backoff growth: after a failure the next drop starts from base.
    func testSuccessResetsBackoffGrowth() {
        var m = makeMachine()
        _ = m.advance(.start)
        _ = m.advance(.connectFailed(reason: "1"))                // attempt 0 → delay 10, counter→1
        _ = m.advance(.backoffElapsed(generation: 1))             // retry
        _ = m.advance(.connectSucceeded)                          // RESET counter to 0
        // The next drop backs off from base again, not from the grown value.
        let effects = m.advance(.connectionClosed(reason: "eof"))
        XCTAssertEqual(effects.last, .armBackoff(after: .milliseconds(10), generation: 2))
    }

    // A superseded backoff timer that still fires is ignored (generation guard).
    func testSupersededBackoffTimerIsIgnored() {
        var m = makeMachine()
        _ = m.advance(.start)
        _ = m.advance(.connectFailed(reason: "1"))                // arms backoff gen 1
        // Imagine a stop/rescheduling bumped the generation; a stale gen-0 fire is ignored.
        XCTAssertEqual(m.advance(.backoffElapsed(generation: 0)), [])
        XCTAssertEqual(m.state, .backingOff)                      // still waiting
    }

    // MARK: - Stop

    func testStopFromConnectedClosesAndTerminates() {
        var m = makeMachine()
        _ = m.advance(.start); _ = m.advance(.connectSucceeded)
        XCTAssertEqual(m.advance(.stop), [.closeConnection])
        XCTAssertEqual(m.state, .stopped)
        // Terminal: every subsequent event is ignored, including a late timer.
        XCTAssertEqual(m.advance(.lineReceived("late")), [])
        XCTAssertEqual(m.advance(.livenessElapsed(generation: 1)), [])
        XCTAssertEqual(m.advance(.connectionClosed(reason: "eof")), [])
    }

    func testStopWhileBackingOffTerminatesWithoutClose() {
        var m = makeMachine()
        _ = m.advance(.start)
        _ = m.advance(.connectFailed(reason: "1"))                // backingOff, no live connection
        XCTAssertEqual(m.advance(.stop), [])                      // nothing to close
        XCTAssertEqual(m.state, .stopped)
    }

    // MARK: - Spurious events

    func testSpuriousEventsInWrongStateAreIgnored() {
        var m = makeMachine()
        // Before start: everything but `.start` is a no-op.
        XCTAssertEqual(m.advance(.connectSucceeded), [])
        XCTAssertEqual(m.advance(.lineReceived("x")), [])
        XCTAssertEqual(m.state, .idle)
        // A duplicate `.start` after connecting is idempotent.
        _ = m.advance(.start)
        XCTAssertEqual(m.advance(.start), [])
    }
}

// MARK: - Backoff schedule

final class ExponentialBackoffTests: XCTestCase {

    // The production schedule: 1 → 2 → 4 → 8 → 16 → 30 (cap) seconds.
    func testDefaultScheduleDoublesToCap() {
        let b = ExponentialBackoff.default
        XCTAssertEqual(b.delay(forAttempt: 0), .seconds(1))
        XCTAssertEqual(b.delay(forAttempt: 1), .seconds(2))
        XCTAssertEqual(b.delay(forAttempt: 2), .seconds(4))
        XCTAssertEqual(b.delay(forAttempt: 3), .seconds(8))
        XCTAssertEqual(b.delay(forAttempt: 4), .seconds(16))
        XCTAssertEqual(b.delay(forAttempt: 5), .seconds(30), "capped")
        XCTAssertEqual(b.delay(forAttempt: 6), .seconds(30), "stays capped")
    }

    // Bounded: no delay ever exceeds the cap, and the sequence is monotonic non-decreasing — the
    // "never a tight loop" AC (base ≥ the first delay, growth capped).
    func testScheduleIsMonotonicAndBounded() {
        let b = ExponentialBackoff.default
        var previous = Duration.zero
        for attempt in 0...50 {
            let d = b.delay(forAttempt: attempt)
            XCTAssertGreaterThanOrEqual(d, previous, "monotonic at \(attempt)")
            XCTAssertLessThanOrEqual(d, .seconds(30), "never above cap at \(attempt)")
            XCTAssertGreaterThanOrEqual(d, .seconds(1), "never below base at \(attempt) — no tight loop")
            previous = d
        }
    }

    // A negative attempt index is clamped to the base delay (defensive).
    func testNegativeAttemptClampsToBase() {
        XCTAssertEqual(ExponentialBackoff.default.delay(forAttempt: -5), .seconds(1))
    }
}
