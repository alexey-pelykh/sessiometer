// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Tests that the client-side socket-path resolver (issue #323) derives the EXACT path the daemon
// binds — pinned against the `src/paths.rs` contract — and that the non-sandbox tripwire (ADR-0011)
// behaves. The pure derivations are asserted without process state; `resolve()` is exercised against
// the (non-sandboxed) test process.

import XCTest

final class SocketPathResolverTests: XCTestCase {

    // AC: "The path resolver EQUALS the daemon's — unit test against `src/paths.rs`."
    //
    // Daemon source of truth (`src/paths.rs`):
    //   support_dir()    = home.join("Library/Application Support").join("sessiometer")
    //   control_socket() = support_dir().join("daemon.sock")
    // So for any home the control socket is `<home>/Library/Application Support/sessiometer/daemon.sock`.
    // This test pins the Swift resolver to that literal contract for a fixed home.
    func testSocketPathMatchesPathsRsContractForFixedHome() {
        XCTAssertEqual(
            SocketPathResolver.socketPath(home: "/Users/example"),
            "/Users/example/Library/Application Support/sessiometer/daemon.sock")
    }

    // The tail constant itself is exactly the `paths.rs` support_dir + control_socket leaf chain.
    func testSocketTailIsThePathsRsContract() {
        XCTAssertEqual(
            SocketPathResolver.socketTail,
            "Library/Application Support/sessiometer/daemon.sock")
    }

    // The daemon's `support_dir()` is NATIVE-LOCAL — it ignores `$XDG_CONFIG_HOME` (paths.rs issue
    // #7: the lock/socket must be machine-global), unlike `config_dir()`. The Swift resolver reads no
    // environment variable at all, so an XDG override cannot move the derived socket path.
    func testResolverIgnoresXdgConfigHome() {
        let previous = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"]
        setenv("XDG_CONFIG_HOME", "/tmp/should-be-ignored", 1)
        defer {
            if let previous { setenv("XDG_CONFIG_HOME", previous, 1) } else { unsetenv("XDG_CONFIG_HOME") }
        }
        // The derived path stays under native-local Library/Application Support — never the XDG dir.
        let path = SocketPathResolver.socketPath(home: "/Users/example")
        XCTAssertEqual(path, "/Users/example/Library/Application Support/sessiometer/daemon.sock")
        XCTAssertFalse(path.contains("should-be-ignored"))
    }

    // MARK: - Sandbox tripwire (ADR-0011)

    // Non-sandboxed: the passwd-DB home and `NSHomeDirectory()` agree → `.ok`.
    func testSandboxCheckOkWhenHomesAgree() {
        XCTAssertEqual(
            SocketPathResolver.sandboxCheck(passwdHome: "/Users/x", nsHome: "/Users/x"),
            .ok)
    }

    // Sandboxed: `NSHomeDirectory()` returns a CONTAINER path that diverges from the passwd-DB home
    // → `.sandboxed`, carrying both paths so the caller degrades loudly rather than connecting to a
    // wrong / denied path.
    func testSandboxCheckDetectsContainerDivergence() {
        let container = "/Users/x/Library/Containers/com.sessiometer.menubar/Data"
        XCTAssertEqual(
            SocketPathResolver.sandboxCheck(passwdHome: "/Users/x", nsHome: container),
            .sandboxed(passwdHome: "/Users/x", containerHome: container))
    }

    // MARK: - Live resolve() on the (non-sandboxed) test process

    // The test bundle runs non-sandboxed, so `resolve()` must succeed and yield exactly the path
    // derived from the passwd-DB home — the same home `src/paths.rs::home_dir()` uses.
    func testResolveSucceedsOnNonSandboxedProcessAndMatchesDerivation() throws {
        let passwd = try XCTUnwrap(SocketPathResolver.passwdHome(), "passwd-DB home must resolve")
        XCTAssertFalse(passwd.isEmpty)

        switch SocketPathResolver.resolve() {
        case .success(let path):
            XCTAssertEqual(path, SocketPathResolver.socketPath(home: passwd))
            XCTAssertTrue(path.hasSuffix("/Library/Application Support/sessiometer/daemon.sock"))
        case .failure(let error):
            XCTFail("non-sandboxed process should resolve, got \(error)")
        }
    }
}
