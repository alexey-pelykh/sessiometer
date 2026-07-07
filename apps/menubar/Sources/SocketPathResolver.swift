// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Resolves the daemon's AF_UNIX control-socket path CLIENT-side — the SAME path the daemon binds in
// `src/paths.rs::control_socket()` — and enforces the non-sandboxed invariant ADR-0011 requires.
//
// The pure derivations (`socketPath(home:)`, `sandboxCheck(passwdHome:nsHome:)`) are split from the
// process-global lookups (`resolve()`), so the path CONTRACT and the sandbox TRIPWIRE are unit-
// testable without touching real process state — and a unit test can assert the derived path equals
// the `paths.rs` contract for any home string.

import Foundation
#if canImport(Darwin)
import Darwin
#endif

enum SocketPathResolver {

    /// The fixed tail under the user's home the daemon binds:
    /// `Library/Application Support/sessiometer/daemon.sock`. This mirrors `src/paths.rs` exactly —
    /// `support_dir()` is `home + "Library/Application Support" + "sessiometer"` and
    /// `control_socket()` appends `daemon.sock`. It is NATIVE-LOCAL: like the daemon's `support_dir()`
    /// (and unlike `config_dir()`), it ignores `$XDG_CONFIG_HOME` — the lock/socket must be
    /// machine-global (paths.rs issue #7). This resolver reads no environment variable at all, so
    /// that property holds by construction.
    static let socketTail = "Library/Application Support/sessiometer/daemon.sock"

    /// The daemon's control-socket path for a given home directory — a pure string derivation, so a
    /// test can assert it against the `paths.rs` contract for any home without process state.
    static func socketPath(home: String) -> String {
        (home as NSString).appendingPathComponent(socketTail)
    }

    /// The outcome of the non-sandbox tripwire (pure). The passwd-DB home and `NSHomeDirectory()`
    /// MUST agree for a non-sandboxed app; under App Sandbox `NSHomeDirectory()` returns the
    /// CONTAINER path — a divergence — and the native-local daemon socket is unreachable, so the
    /// transport must degrade loudly rather than target a wrong / denied path (ADR-0011).
    enum SandboxCheck: Equatable {
        case ok
        case sandboxed(passwdHome: String, containerHome: String)
    }

    /// Compare the two home-resolution routes a non-sandboxed app has. Pure.
    static func sandboxCheck(passwdHome: String, nsHome: String) -> SandboxCheck {
        passwdHome == nsHome ? .ok : .sandboxed(passwdHome: passwdHome, containerHome: nsHome)
    }

    /// Why a resolve failed — both are "degrade loudly", never "connect anyway".
    enum ResolveError: Error, Equatable {
        /// `getpwuid(getuid())` yielded no usable home directory.
        case homeUnresolved
        /// The passwd-DB home diverges from `NSHomeDirectory()` ⇒ the app is sandboxed ⇒ the
        /// native-local daemon socket is unreachable. Carries both paths for a loud log.
        case sandboxed(passwdHome: String, containerHome: String)
    }

    /// Resolve the daemon socket path from live process state, applying the sandbox tripwire. The
    /// home is read from the password database (`getpwuid(getuid())->pw_dir`) — the SAME source
    /// `src/paths.rs::home_dir()` uses (never `$HOME`, never XDG, never a sandbox container).
    static func resolve() -> Result<String, ResolveError> {
        guard let passwd = passwdHome() else { return .failure(.homeUnresolved) }
        switch sandboxCheck(passwdHome: passwd, nsHome: NSHomeDirectory()) {
        case .ok:
            return .success(socketPath(home: passwd))
        case .sandboxed(let passwd, let container):
            return .failure(.sandboxed(passwdHome: passwd, containerHome: container))
        }
    }

    /// The current user's home from the password database (`getpwuid(getuid())->pw_dir`), or `nil`
    /// if unresolved. Matches `src/paths.rs::home_dir()`; a non-sandboxed process gets the real home.
    static func passwdHome() -> String? {
        let uid = getuid()
        guard let pw = getpwuid(uid), let dir = pw.pointee.pw_dir else { return nil }
        return String(cString: dir)
    }
}
