// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Tests the client-side daemon-lock liveness probe (issue #742) against a real temp file — the
// Swift mirror of the Rust seam test `instance_lock_is_held_probe_reports_absent_held_and_freed`
// (`src/daemon/seams.rs`). The probe must read absent / held / freed WITHOUT disturbing a live
// holder (a separate non-blocking flock over its own open file description) and must never RETAIN
// the lock itself. Hermetic: each test uses an auto-cleaned temp dir, never a fixed shared path.

import XCTest
#if canImport(Darwin)
import Darwin
#endif

final class DaemonLockProbeTests: XCTestCase {

    private var tempDir: URL!
    private var lockPath: String!

    override func setUpWithError() throws {
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("daemon-lock-probe-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        lockPath = tempDir.appendingPathComponent("daemon.lock").path
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tempDir)
    }

    func testAbsentLockFileProbesAsNotHeld() {
        // No file created ⇒ the daemon never created it ⇒ not held (mirrors the Rust NotFound arm).
        XCTAssertFalse(DaemonLockProbe.isHeld(path: lockPath))
    }

    func testProbeOnAFreeLockReportsNotHeldAndDoesNotRetainIt() {
        // The file exists but nobody holds it: the probe acquires-then-releases and reports false,
        // and — crucially — must NOT keep the lock, so a SECOND probe also reports false.
        XCTAssertTrue(FileManager.default.createFile(atPath: lockPath, contents: nil))
        XCTAssertFalse(DaemonLockProbe.isHeld(path: lockPath))
        XCTAssertFalse(DaemonLockProbe.isHeld(path: lockPath),
                       "the probe must release the lock it briefly took, so a repeat probe is still free")
    }

    func testHeldLockProbesAsHeldThenFreedAfterRelease() {
        // Hold the lock from the test via a SEPARATE open + blocking flock, exactly as a live daemon
        // would (flock conflicts across distinct open file descriptions even within one process), then
        // assert the probe — its own non-blocking flock over its own open — sees it HELD.
        XCTAssertTrue(FileManager.default.createFile(atPath: lockPath, contents: nil))
        let fd = open(lockPath, O_RDONLY)
        XCTAssertGreaterThanOrEqual(fd, 0, "opening the temp lock file must succeed")
        XCTAssertEqual(flock(fd, LOCK_EX), 0, "the test acquires the lock like a live daemon")

        XCTAssertTrue(DaemonLockProbe.isHeld(path: lockPath),
                      "while the test holds LOCK_EX, the probe's non-blocking flock sees EWOULDBLOCK")

        // Release ⇒ the probe reads the lock as free again.
        XCTAssertEqual(flock(fd, LOCK_UN), 0)
        close(fd)
        XCTAssertFalse(DaemonLockProbe.isHeld(path: lockPath), "after release the lock probes as free")
    }
}
