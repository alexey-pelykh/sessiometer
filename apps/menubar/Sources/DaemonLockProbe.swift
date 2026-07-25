// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Client-side liveness probe for the daemon's single-instance lock (issue #742). It performs the
// SAME non-blocking `flock(LOCK_EX|LOCK_NB)` over a fresh read-only open that the daemon's own
// `src/daemon/seams.rs::InstanceLock::is_held` does — mirrored here so the app can detect a LIVE
// daemon (whether launchd-managed OR a developer's manually-run `sessiometer run`) WITHOUT issuing
// a daemon command. The app stays a pure IPC client: this is a filesystem `flock`, not a new socket
// verb or credential path. `InstanceLock::is_held` is `pub(crate)` and not Swift-reachable, so the
// probe is reimplemented rather than called.
//
// It NEVER holds the lock: a successful acquire of a FREE lock is released the instant the fd
// closes (the `defer` below) — the same acquire-then-release the Rust probe documents as benign,
// because in practice a live daemon already holds the lock so the probe fails to acquire and never
// contends. Pure over its `path` argument (no process-global state), so it is unit-tested
// hermetically against a temp file, exactly like the Rust seam test.

import Foundation
#if canImport(Darwin)
import Darwin
#endif

enum DaemonLockProbe {

    /// Whether ANY process currently holds the daemon's single-instance lock at `path`.
    ///
    /// - an ABSENT lock file ⇒ the daemon has never created it ⇒ not held (`false`).
    /// - a successful `flock` acquire ⇒ no live holder; the lock is released the instant the fd
    ///   closes (`defer`), so nothing is started, stopped, or signalled ⇒ `false`.
    /// - `EWOULDBLOCK` ⇒ another process holds it ⇒ a daemon is alive ⇒ `true`.
    /// - ANY other error (open denied, flock failure) ⇒ conservatively `false`: a probe error must
    ///   NEVER newly SUPPRESS the Start affordance. Reporting "held" on an unexpected error would
    ///   block a legitimate Start when no daemon is running; reporting "not held" falls back to the
    ///   pre-#742 gate (and the daemon-side exit-0 stand-down backstops the misleading-success case
    ///   this gate otherwise prevents). Production is non-sandboxed (ADR-0011), so the lock dir is
    ///   reachable and this branch is a defensive tripwire, not the common path.
    static func isHeld(path: String) -> Bool {
        let fd = open(path, O_RDONLY)
        if fd < 0 {
            // ENOENT ⇒ never created ⇒ not held. Any other open error ⇒ can't probe ⇒ permissive.
            return false
        }
        defer { close(fd) }
        if flock(fd, LOCK_EX | LOCK_NB) == 0 {
            // Acquired ⇒ no live holder; the LOCK_EX just taken is released by `close(fd)` (defer).
            return false
        }
        return errno == EWOULDBLOCK
    }
}
