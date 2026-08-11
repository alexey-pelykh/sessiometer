// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Real-fd tests for the PRODUCTION `PosixSocketConnection` (issue #323): the fake-based
// `WatchTransportTests` prove the actor SHELL's wiring, but the dedicated-thread `read()` loop, the
// newline framing, partial-read accumulation, EOF handling, and idempotent teardown are raw POSIX I/O
// that a fake never exercises. `socketpair(AF_UNIX, SOCK_STREAM)` gives a kernel-backed connected fd
// PAIR with no filesystem socket and no daemon — so the real reader runs against a real socket while
// the test drives the peer end directly. This is the minimal real-socket coverage #323 needs; the
// full mock-socket server (#328) is a separate item.

import XCTest
import os
#if canImport(Darwin)
import Darwin
#endif

final class PosixSocketConnectionTests: XCTestCase {

    // MARK: - Framing (real read() loop)

    // The reader splits the byte stream on `\n` (trailing newline stripped) AND accumulates PARTIAL
    // reads to a boundary: "gamma" arrives as two writes yet surfaces as one line.
    func testReadsNewlineDelimitedLinesIncludingAcrossPartialWrites() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        writeBytes(pair.peer, "alpha\nbeta\n")            // two complete lines in one write
        try await XCTAssertNextLine(collector, "alpha")
        try await XCTAssertNextLine(collector, "beta")

        writeBytes(pair.peer, "gam")                      // a line split across two writes …
        writeBytes(pair.peer, "ma\n")                     // … proves partial-read accumulation
        try await XCTAssertNextLine(collector, "gamma")

        connection.close()
        Darwin.close(pair.peer)
    }

    // Blank lines are surfaced VERBATIM at this (framing) layer — skipping them is the `WatchTransport`
    // shell's contract, not the reader's. Pinning it here keeps the empty-line skip from drifting back
    // down into `PosixSocketConnection` (where it lived before, and where it would starve the shell of
    // the information it needs to decide).
    func testEmptyLinesArePassedThroughAtTheFramingLayer() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        writeBytes(pair.peer, "\nafter\n")                // leading bare "\n" is an empty line
        try await XCTAssertNextLine(collector, "", "empty line surfaced, not swallowed")
        try await XCTAssertNextLine(collector, "after")

        connection.close()
        Darwin.close(pair.peer)
    }

    // MARK: - EOF

    // Peer close → `read()` returns 0 → the stream FINISHES (the shell reads that as "connection
    // closed" and reconnects).
    func testPeerCloseEndsTheStream() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        writeBytes(pair.peer, "only\n")
        try await XCTAssertNextLine(collector, "only")

        Darwin.close(pair.peer)                           // EOF
        let end = try await nextLine(collector)
        XCTAssertNil(end, "stream finishes on EOF")

        connection.close()
    }

    // A trailing line with NO newline is not lost: EOF flushes the accumulated buffer as a final line,
    // then the stream finishes.
    func testTrailingUnterminatedBytesAreDeliveredAtEof() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        writeBytes(pair.peer, "no-trailing-newline")      // no "\n"
        Darwin.close(pair.peer)                           // EOF flushes the partial line

        try await XCTAssertNextLine(collector, "no-trailing-newline")
        let end = try await nextLine(collector)
        XCTAssertNil(end, "then finishes")

        connection.close()
    }

    // MARK: - Write path

    // `send()` writes the exact bytes to the peer (the single `{"cmd":"watch"}\n` subscribe). Proven
    // against a real socket, not a fake's array.
    func testSendWritesTheSubscribeBytesToThePeer() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        let subscribe = Array(#"{"cmd":"watch"}"#.utf8) + [0x0A]
        try connection.send(subscribe)
        XCTAssertEqual(readAvailable(pair.peer), #"{"cmd":"watch"}"# + "\n")

        connection.close()
        Darwin.close(pair.peer)
    }

    // MARK: - Teardown

    // `close()` disconnects the socket, which unblocks the blocked `read()` so the reader thread
    // exits and the stream finishes (ADR-0011 §4 teardown mechanism; since issue #859 the wake is a
    // `shutdown()` and the descriptor is retired by the reader — see
    // `testTeardownDoesNotStrandAReaderOnAReusedDescriptor`). It is idempotent: a second call is a
    // safe no-op. The 5 s timeout in `nextLine` converts a teardown HANG into a clear failure rather
    // than a stuck suite.
    func testCloseFinishesStreamAndIsIdempotent() async throws {
        let pair = try makeSocketPair()
        let connection = PosixSocketConnection(fd: pair.conn)
        let collector = LineCollector(); collector.consume(connection.lines)

        connection.close()
        connection.close()                                // idempotent — must not crash or double-close

        let end = try await nextLine(collector)
        XCTAssertNil(end, "close() finishes the stream")

        Darwin.close(pair.peer)
    }

    // MARK: - Descriptor ownership (issue #859)

    // Teardown must not release the descriptor NUMBER while the reader thread can still `read()` it.
    // When it does, a connection torn down without awaiting its stream leaves behind a reader that
    // issues its first `read()` against whatever the process allocated that number to NEXT —
    // consuming a LATER connection's bytes, and leaving that connection to observe EOF on an empty
    // buffer. `socket()` returns the lowest free descriptor, so the number handed back is precisely
    // the one just released.
    //
    // This is the production reconnect shape (`WatchTransport` closes the connection, then calls
    // `connector.connect()`), and it is what made `testTrailingUnterminatedBytesAreDeliveredAtEof`
    // flake under full-suite load: the test preceding it leaves exactly such a reader behind, and a
    // loaded machine is what delays that reader's first `read()` past the next test's write. The
    // victim test was CORRECT — it was reporting this defect, not suffering a bad assertion.
    //
    // Measured against the pre-fix reader, out of process: the payload was lost in 342–391 of 400
    // iterations of this shape (the rate is load-dependent; two independent runs bracket it), and 0
    // of 400 WITHOUT the abandoned connection — which is also what refutes the originally suspected
    // mechanism, a `close()` overtaking its own unread bytes, something AF_UNIX stream semantics do
    // not permit.
    //
    // The loop is what turns that race into a gate. Per-iteration the pre-fix failure rate measures
    // ~70–100%, so a single iteration would be a coin-flip in the other direction, while 20 makes a
    // spurious pass vanishingly unlikely; observed pre-fix failures were 12–20 of 20 across runs.
    func testTeardownDoesNotStrandAReaderOnAReusedDescriptor() async throws {
        for iteration in 1...20 {
            // Torn down WITHOUT awaiting its stream — the shape every non-EOF test here leaves
            // behind, and the shape `WatchTransport` reconnects through.
            let abandoned = try makeSocketPair()
            let abandonedConnection = PosixSocketConnection(fd: abandoned.conn)
            abandonedConnection.close()
            Darwin.close(abandoned.peer)

            // A fresh connection, on the descriptor number that teardown may just have released.
            let pair = try makeSocketPair()
            let connection = PosixSocketConnection(fd: pair.conn)
            let collector = LineCollector(); collector.consume(connection.lines)

            writeBytes(pair.peer, "not-stolen")
            Darwin.close(pair.peer)

            try await XCTAssertNextLine(
                collector, "not-stolen",
                "iteration \(iteration): a torn-down connection's reader consumed these bytes")

            connection.close()
        }
    }

    // MARK: - Descriptor retirement (issue #899)

    // The reader RELEASES the descriptor number once its loop has ended. Issue #859 moved release off
    // `close()` and onto the reader thread, which bought the guarantee the test above rests on — and
    // created a failure mode the synchronous design could not have: never releasing at all. That went
    // ungated. Deleting `descriptor.retire()` from `startReader()` passed the whole suite, and
    // `testTeardownDoesNotStrandAReaderOnAReusedDescriptor` cannot see it BY CONSTRUCTION — it asserts
    // a fresh connection receives its own bytes, and a number that is never released can never be
    // reused, so leaking makes byte-stealing LESS likely. The two are complements: that one gates
    // release-too-EARLY, this one gates release-NEVER.
    //
    // Asking "is this number still a valid descriptor?" is only sound if the number cannot have been
    // REISSUED in between — this bundle runs amid Foundation/XPC activity that opens descriptors of its
    // own, and a reissued number reports "still open", failing a reader that retired correctly. So the
    // descriptor is FENCED first: `F_DUPFD` moves it to the lowest free number at or above a high
    // floor. That converts reuse from unlikely-by-timing into forbidden-by-contract, because POSIX
    // specifies allocation returns "the lowest numbered descriptor not currently open" — so this number
    // can only be handed out once EVERY number below it is open simultaneously. `headroom` measures how
    // many concurrent descriptors that would take, and is asserted rather than assumed — measured here
    // at 505 under the full suite and 506 for this class alone, against a floor of 64.
    //
    // What this gate does NOT assert is the other direction — that release is not too early. There is
    // no non-racy way to observe "still open" at an instant (the reader may legitimately finish first),
    // and that direction already has a behavioural gate in the test above.
    func testTheReaderRetiresTheDescriptorOnceItsLoopHasEnded() async throws {
        let pair = try makeSocketPair()
        defer { Darwin.close(pair.peer) }
        let fenced = try fence(pair.conn)

        let connection = PosixSocketConnection(fd: fenced)
        let collector = LineCollector(); collector.consume(connection.lines)

        // Proves the reader is really reading the FENCED descriptor — otherwise a released-looking
        // number could just be one the connection never used.
        writeBytes(pair.peer, "live\n")
        try await XCTAssertNextLine(collector, "live", "the connection is not reading fd \(fenced)")

        connection.close()
        let end = try await nextLine(collector)
        XCTAssertNil(end, "close() finishes the stream — the reader's loop has ended")

        let released = await awaitDescriptorRelease(fenced)
        // Measured AFTER the probe, so it describes the conditions the probe actually ran under; an
        // unmeasurable margin counts as fully collapsed, since being unable to open a socket at all is
        // exactly the descriptor pressure that would break the fence.
        let headroom = lowestFreeDescriptor().map { fenced - $0 } ?? 0
        // A collapsed margin makes the verdict below untrustworthy in EITHER direction, so it is checked
        // rather than assumed.
        XCTAssertGreaterThanOrEqual(
            headroom, 64,
            "fence margin collapsed to \(headroom) numbers below fd \(fenced) — reuse is no longer "
                + "excluded, so the retirement verdict is inconclusive rather than wrong")
        XCTAssertTrue(
            released,
            "fd \(fenced) is still open after the stream finished: the reader never retired it. The "
                + "lowest free number is \(fenced - headroom), so the fence held and this is a leak "
                + "rather than a number reissued to unrelated activity")
        // A leaked descriptor would otherwise accumulate across the rest of the run. Sound to reclaim
        // for the same reason the verdict is sound: with that much headroom the number is still ours.
        if !released { Darwin.close(fenced) }
    }

    // MARK: - Descriptor retirement without an explicit close (issue #1166)

    // The OTHER teardown path, and the more fragile one. `PosixSocketConnection` releases its
    // descriptor from exactly one place — the reader thread, after its loop ends — and for a
    // connection nobody ever closed, the only thing that ends that loop is `deinit { close() }`.
    // Delete that single line and everything above stays green — the #899 gate and the #859 soak
    // included — while every abandoned connection leaks a descriptor AND strands its reader blocked in
    // `read()` forever. Measured on `fc1bad4`, that mutant reported 924 tests, 4 skipped, 0 failures:
    // the same verdict, test for test, as the unmutated tree. This is the production ERROR route: a
    // subscribe-write failure discards the connection before anything attaches to it
    // (`WatchTransport`), which also makes it the path least likely to be walked by hand.
    //
    // Four things could end that reader's loop, and this test is only about `deinit` because the other
    // three are excluded by construction:
    //   * `close()` is never called — being dropped without one is the whole subject.
    //   * `continuation.onTermination` captures `self` WEAKLY (`WatchConnection.swift`), so when the
    //     drop releases `lines`, the callback's `self?.close()` finds a nil `self` and does nothing.
    //   * `FileDescriptorOwner.retire()` has exactly one caller in the app — the reader itself, after
    //     its loop — so it cannot be the thing that ENDS that loop.
    //   * EOF is kept off the table by holding the peer OPEN across the verdict: the `defer` below runs
    //     after it. Closing the peer early would end the reader's loop with no help from `deinit`, and
    //     this test would then pass just as happily on a tree with no backstop at all.
    // Each of those is an inspection, and inspection is not evidence (ADR-0031 § 4). The evidence is
    // the mutation, run in both directions on `fc1bad4`: with the backstop shipped, 925 tests, 4
    // skipped, 0 failures, this one passing in 0.003 s; with `deinit { close() }` deleted, 925 tests,
    // 4 skipped, and exactly ONE failure — this test. Every other test in the bundle returned the same
    // verdict on both sides, which is what rules out another release path quietly carrying this.
    //
    // Poll deadline: `awaitDescriptorRelease`'s 10 s default, shared with the #899 gate. The whole
    // probe — socket pair, fence, identity round-trip, drop and poll — runs in 0.002–0.003 s typical
    // and 0.016 s at the slowest of 1,104 measured iterations, so even that outlier sits ~600× inside
    // the deadline. The failure mode is a returned `false`, not a hang: the mutant above failed this
    // assertion at 10.007 s rather than wedging the suite.
    //
    // Soak: ONE iteration in the committed test, against the 20 of
    // `testTeardownDoesNotStrandAReaderOnAReusedDescriptor`. That test loops because it gates a RACE,
    // whose per-iteration failure rate leaves a single pass meaning little. This is not a race: with
    // the backstop deleted the descriptor is never released AT ALL, so the mutant fails
    // deterministically, and a loop would buy 20× the deadline on failure and no signal on success.
    // Non-flakiness was measured instead of looped — 1,104 executions, 0 failures: 1000 consecutive
    // iterations in ONE long-lived process (`-test-repetition-relaunch-enabled NO` — the harshest
    // setting for the fence's margin, since descriptor pressure accumulates rather than resetting),
    // 100 more with relaunch enabled so each is a fresh process, and four full-suite runs where it
    // shares a process with 924 other tests.
    //
    // The stranded reader THREAD is covered transitively, not separately: `descriptor.retire()` is the
    // reader's last statement after its loop ends, so observing the release IS observing that the
    // reader is no longer blocked. What that leaves uncovered is a backstop that frees the number while
    // a reader can still issue a `read()` against it — a bare `Darwin.close(raw)` in `deinit`, which
    // this test cannot see: freeing the number is exactly what its probe waits for, so that mutant
    // SATISFIES this test rather than failing it.
    //
    // THAT IS NOW GATED, one section down, by
    // `testDroppingAConnectionWithoutClosingItDoesNotStrandAReaderOnAReusedDescriptor` (issue #1187),
    // and deliberately not by widening this one. This comment has now been wrong about that gap twice,
    // and both corrections are recorded rather than quietly replaced. In flight on PR #1178 it claimed
    // `testTeardownDoesNotStrandAReaderOnAReusedDescriptor` already covered it — it does not, because
    // that test calls `close()` EXPLICITLY on its abandoned connection, so `shutdown()` wakes the reader
    // before `deinit` is ever reached; the judge gate that caught that is what filed #1187. The wording
    // that then landed with it, in the same commit, called the gap a backstop that releases the number
    // "WITHOUT waking the reader", which names the wrong mechanism. Measured out of
    // process on Darwin 25.5.0, with a thread first parked in `read()` on an AF_UNIX SOCK_STREAM fd for
    // 300 ms and confirmed still blocked: a plain `close()` of that descriptor DOES wake it, returning
    // -1/EBADF. The hazard never required the reader to stay asleep — only that the number becomes
    // reusable while a reader has yet to issue its next syscall. The gate below is written against that
    // instead, which is also why it needed no thread enumeration, the mechanism #1187 expected to be
    // the blocker.
    func testDroppingAConnectionWithoutClosingItRetiresTheDescriptor() async throws {
        let pair = try makeSocketPair()
        // Deliberately AFTER the verdict below — see the EOF bullet above.
        defer { Darwin.close(pair.peer) }
        let fenced = try fence(pair.conn)

        do {
            let abandonedConnection = PosixSocketConnection(fd: fenced)
            // Proves the connection really adopted fd `fenced`, WITHOUT consuming `lines`: a
            // released-looking number proves nothing if the connection never used that number, and
            // attaching a consumer would introduce a stream lifetime this test is not measuring.
            // `send()` writes to the connection's own descriptor and `pair.peer` is the only thing on
            // the far end of it, so the round-trip pins the identity.
            try abandonedConnection.send(Array("subscribe\n".utf8))
            XCTAssertEqual(
                readAvailable(pair.peer), "subscribe\n",
                "the connection is not writing to fd \(fenced) — the verdict below would be vacuous")
        }
        // The last reference is gone here: no `close()`, no `lines` consumer, no stored copy, and the
        // reader thread holds the descriptor and the continuation directly rather than through `self`.

        let released = await awaitDescriptorRelease(fenced)
        // Measured after the probe, for the reason the #899 gate measures it there: it describes the
        // conditions the probe actually ran under.
        let headroom = lowestFreeDescriptor().map { fenced - $0 } ?? 0
        XCTAssertGreaterThanOrEqual(
            headroom, 64,
            "fence margin collapsed to \(headroom) numbers below fd \(fenced) — a number reissued to "
                + "unrelated activity would read as 'still open', so the verdict below is inconclusive "
                + "rather than wrong")
        XCTAssertTrue(
            released,
            "fd \(fenced) is still open after the connection was dropped: the deinit backstop never "
                + "disconnected the socket, so the reader is still blocked in read() and never retired "
                + "the descriptor. The lowest free number is \(fenced - headroom), so the fence held "
                + "and this is a leak rather than a number reissued to unrelated activity")
        // No reclaiming `fenced` here, deliberately — the opposite of the #899 gate, and for the reason
        // that gate's reclaim is sound: there the reader has already finished, so a leaked number has
        // no owner left. Here a failure means the reader is STILL RUNNING and still owns the number, so
        // closing it from the test would hand a live reader a number the process can immediately
        // reissue — the exact #859 hazard this class exists to gate. The `defer` above is the reclaim:
        // closing the peer delivers the EOF that ends the reader's loop, and the reader retires its own
        // descriptor.
    }

    // MARK: - Deinit-only teardown does not strand a reader on a reused descriptor (issue #1187)

    // The soak above drives the EXPLICIT teardown path; this drives the IMPLICIT one. Both gate the
    // same #859 hazard — the descriptor NUMBER released while a reader can still `read()` it — and the
    // soak cannot reach this path by construction: it calls `close()` on its abandoned connection, so
    // `shutdown()` wakes that reader long before `deinit`.
    //
    // Why this needed a different OBSERVABLE rather than a stronger assertion on the one above, which
    // is what issue #1187 was filed to establish. The obvious candidate was the reader's own terminal
    // side-effect: `continuation.finish()` is the last thing it does that `lines` can observe, so a
    // stream that never finished would mean a reader that was never woken. It does not discriminate.
    // Measured out of process on Darwin 25.5.0, with a thread first parked in `read()` on an AF_UNIX
    // SOCK_STREAM fd for 300 ms and confirmed still blocked: a plain `close()` of that descriptor WAKES
    // it, returning -1/EBADF. So under `deinit { Darwin.close(descriptor.raw) }` the loop still ends,
    // the stream still finishes, and the number is still released — every reader-liveness observable is
    // green on the mutant, and a gate built on one passes it. That is how this was found: the first
    // version of this test asserted exactly that and survived its own canary.
    //
    // This does not contradict `FileDescriptorOwner`'s doc comment, and the distinction is the whole
    // point. That comment says `close()` is not a RELIABLE wake — "need not return at all" — which is a
    // statement about what teardown may not DEPEND on, not a promise that the reader stays blocked. A
    // reader that does wake is not evidence of safety either way, because the hazard was never the
    // stuck thread; it is the number becoming reusable while a reader can still issue a syscall against
    // it. So the only observable that separates the two deinits is the cross-wiring itself, and this is
    // the soak above with the explicit `close()` deleted:
    //   * shipped `deinit { close() }` → `shutdown()` keeps the number RESERVED until the reader
    //     retires it, so the fresh `socket()` below cannot be handed it while that reader lives.
    //   * `deinit { Darwin.close(descriptor.raw) }` → the number is free immediately, `socket()` returns
    //     the lowest free one, and the abandoned reader's first `read()` lands on the fresh connection
    //     — consuming its bytes, or retiring its number out from under it. This observable does not
    //     separate those two, and does not try to: every measured failure reported the fresh stream
    //     FINISHING with no payload, which is what both produce once the peer is closed. What is gated
    //     is the hazard, not which of its two shapes occurred.
    //
    // The connection is therefore constructed and dropped with nothing in between, rather than parked in
    // `read()` first: the drop should land while the reader thread is still starting, which is #859's
    // primary shape. Both shapes are hazardous under the mutant and this gate separates neither — a
    // reader that has NOT yet entered `read()` issues it against the reissued number and takes the
    // payload, while one already blocked wakes with EBADF, ends its loop, and then runs
    // `descriptor.retire()`, closing a number the process may already have handed to the fresh
    // connection. Both end that connection's stream with no payload, which is what is asserted.
    //
    // What this gate is SILENT on, stated so the pair is not misread as one gate: `deinit` deleted
    // outright. Then nothing frees the number at all, so it can never be reissued and nothing is
    // stolen — this test passes on that tree. That direction is
    // `testDroppingAConnectionWithoutClosingItRetiresTheDescriptor` above, and the two stand in the same
    // relation on this path that #859 and #899 do on the explicit one: that one gates release-NEVER,
    // this one gates release-TOO-EARLY. Neither subsumes the other.
    //
    // Iteration count: this gates a RACE, so a single pass would mean little, exactly as the soak above
    // records for its own 20. Measured with the mutant substituted: run ALONE, 18 of 20 iterations
    // failed in each of four runs — 72 of 80 — and the two survivors were a different pair every time
    // (6/7, then 4/10, 11/20, 8/19), so this is a per-iteration rate near 90% rather than two
    // structurally unreachable iterations. Run inside the full bundle it was 20 of 20, the rate being
    // load-dependent in the same direction the soak above records for its own. A spurious green needs
    // every iteration to miss at once. Against the shipped `deinit` the loop is deterministic, since
    // the number cannot be reissued while that reader can still read it, so the iterations cost only
    // their own microseconds.
    func testDroppingAConnectionWithoutClosingItDoesNotStrandAReaderOnAReusedDescriptor() async throws {
        for iteration in 1...20 {
            // Dropped with NO `close()`: `deinit` is the only teardown, which is the whole subject. The
            // `_ =` discard is what makes the drop happen HERE, at the end of this statement, rather
            // than at the end of the iteration, where an iteration-scoped local would put it — as in
            // the soak above, whose abandoned connection is therefore already torn down by its
            // explicit `close()` before `deinit` runs.
            do {
                let abandoned = try makeSocketPair()
                _ = PosixSocketConnection(fd: abandoned.conn)
                Darwin.close(abandoned.peer)
            }

            // A fresh connection, on the descriptor number a deinit-only teardown may just have freed.
            let pair = try makeSocketPair()
            let connection = PosixSocketConnection(fd: pair.conn)
            let collector = LineCollector(); collector.consume(connection.lines)

            writeBytes(pair.peer, "not-stolen")
            Darwin.close(pair.peer)

            try await XCTAssertNextLine(
                collector, "not-stolen",
                "iteration \(iteration): a connection dropped without close() left a reader on this "
                    + "descriptor number, which then consumed these bytes or retired the number")

            connection.close()
        }
    }

    // MARK: - Fixtures

    private struct SocketPairError: Error { let errnoValue: Int32 }

    private struct FenceError: Error { let floors: [Int32]; let errnoValue: Int32 }

    /// Move `fd` to the lowest free descriptor at or above a high floor, so that the number cannot be
    /// reissued under the test without the process first opening every descriptor beneath it. TAKES
    /// OWNERSHIP of `fd`: it is closed on both paths, so a caller only ever handles the returned number.
    /// The ladder descends because the floor must fall under `RLIMIT_NOFILE` (`F_DUPFD` reports `EINVAL`
    /// otherwise), which is not fixed across machines or CI images.
    private func fence(_ fd: Int32) throws -> Int32 {
        let floors: [Int32] = [512, 256, 128]
        for floor in floors {
            let moved = fcntl(fd, F_DUPFD, floor)
            if moved >= 0 {
                Darwin.close(fd)
                return moved
            }
        }
        let failure = FenceError(floors: floors, errnoValue: errno)
        Darwin.close(fd)
        throw failure
    }

    /// The number a fresh allocation would receive right now — the process's lowest free descriptor,
    /// which is what the fence's margin is measured against. `nil` when the process cannot spare one.
    private func lowestFreeDescriptor() -> Int32? {
        let probe = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard probe >= 0 else { return nil }
        Darwin.close(probe)
        return probe
    }

    /// Poll until `fd` is no longer a valid descriptor. Retirement happens on the reader thread AFTER
    /// `continuation.finish()`, so there is no completion to await (issue #859) — but the condition is
    /// a one-way step, so a slow machine only makes this slower, never flakier.
    private func awaitDescriptorRelease(_ fd: Int32, timeout: Duration = .seconds(10)) async -> Bool {
        func isReleased() -> Bool { fcntl(fd, F_GETFD) < 0 && errno == EBADF }
        let deadline = ContinuousClock.now.advanced(by: timeout)
        while ContinuousClock.now < deadline {
            if isReleased() { return true }
            try? await Task.sleep(for: .milliseconds(1))
        }
        return isReleased()
    }

    /// A connected AF_UNIX stream fd pair (no filesystem socket, no daemon). `conn` is wrapped by the
    /// production `PosixSocketConnection`; `peer` is the test's end to write / close directly.
    ///
    /// `SO_NOSIGPIPE` is set on both ends because `PosixSocketConnector.connect()` sets it on every
    /// socket production ever hands to a `PosixSocketConnection` — a fixture without it is not the
    /// production object. It also matters more since issue #859: teardown now SHUTS DOWN the socket
    /// instead of closing it, so a write after `close()` reaches a live-but-disconnected peer and
    /// raises SIGPIPE (killing the whole test process) where it previously returned EBADF. No test
    /// writes after closing today; this keeps the first one that does from taking down the run
    /// instead of failing one assertion.
    private func makeSocketPair() throws -> (conn: Int32, peer: Int32) {
        var fds: [Int32] = [-1, -1]
        guard socketpair(AF_UNIX, SOCK_STREAM, 0, &fds) == 0 else { throw SocketPairError(errnoValue: errno) }
        var noSigPipe: Int32 = 1
        for fd in fds {
            _ = setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size))
        }
        return (fds[0], fds[1])
    }

    @discardableResult
    private func writeBytes(_ fd: Int32, _ string: String) -> Int {
        let bytes = Array(string.utf8)
        return bytes.withUnsafeBytes { Darwin.write(fd, $0.baseAddress, $0.count) }
    }

    /// One blocking read of whatever is currently buffered on `fd`, decoded as UTF-8. Safe here because
    /// every call site reads AFTER a synchronous `send()` has already put the bytes in the socket
    /// buffer, so the read returns immediately.
    private func readAvailable(_ fd: Int32, max: Int = 4096) -> String {
        var buffer = [UInt8](repeating: 0, count: max)
        let n = buffer.withUnsafeMutableBytes { Darwin.read(fd, $0.baseAddress, $0.count) }
        guard n > 0 else { return "" }
        return String(decoding: buffer[0..<n], as: UTF8.self)
    }

    // MARK: - Stream awaiting helpers (timeout-guarded so a wiring bug fails instead of hanging)

    private enum WaitError: Error { case timeout }

    private func nextLine(_ collector: LineCollector, timeout: Duration = .seconds(5)) async throws -> String? {
        try await withThrowingTaskGroup(of: String?.self) { group in
            group.addTask { await collector.next() }
            group.addTask { try await Task.sleep(for: timeout); throw WaitError.timeout }
            let result = try await group.next()!
            group.cancelAll()
            return result
        }
    }

    private func XCTAssertNextLine(
        _ collector: LineCollector, _ expected: String, _ message: String = "",
        timeout: Duration = .seconds(5), file: StaticString = #filePath, line: UInt = #line
    ) async throws {
        let value = try await nextLine(collector, timeout: timeout)
        XCTAssertEqual(value, expected, message, file: file, line: line)
    }
}

// MARK: - Line collector

/// Consumes an `AsyncStream<String>` into a queue and hands lines out one at a time via `next()`
/// (`nil` = the stream finished) — so a test can assert an ordered sequence without arbitrary sleeps.
/// Mirrors `WatchTransportTests`' `EventRecorder`, specialized to `String`.
final class LineCollector: @unchecked Sendable {
    private let state = OSAllocatedUnfairLock(initialState: State())
    private struct State {
        var buffer: [String] = []
        var waiter: CheckedContinuation<String?, Never>?
        var finished = false
    }

    func consume(_ stream: AsyncStream<String>) {
        Task { [weak self] in
            for await line in stream { self?.push(line) }
            self?.finish()
        }
    }

    private func push(_ line: String) {
        let waiter: CheckedContinuation<String?, Never>? = state.withLock { st in
            if let w = st.waiter { st.waiter = nil; return w }
            st.buffer.append(line)
            return nil
        }
        waiter?.resume(returning: line)
    }

    private func finish() {
        let waiter: CheckedContinuation<String?, Never>? = state.withLock { st in
            st.finished = true
            let w = st.waiter; st.waiter = nil; return w
        }
        waiter?.resume(returning: nil)
    }

    func next() async -> String? {
        await withCheckedContinuation { continuation in
            let immediate: String?? = state.withLock { st -> String?? in
                if !st.buffer.isEmpty { return .some(st.buffer.removeFirst()) }
                if st.finished { return .some(nil) }
                st.waiter = continuation
                return nil
            }
            if let value = immediate { continuation.resume(returning: value) }
        }
    }
}
