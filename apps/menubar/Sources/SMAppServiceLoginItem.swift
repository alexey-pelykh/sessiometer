// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The concrete `SMAppService`-backed `LoginItemService` (issue #170): the ONLY place `ServiceManagement` is
// touched. It wraps `SMAppService.mainApp` (the app login item) and `SMAppService.agent(plistName:)` (the
// embedded daemon LaunchAgent), maps `SMAppService.Status` onto the AppKit-free `LoginItemStatus`, and probes
// for a CLI-owned agent so the app can yield (the two-owner guard).
//
// It stays in the APP target (NOT the headless `MenubarTests` bundle): `SMAppService` performs real OS
// registration with no injectable state, so the pure decisions live in `LoginItemModel` (tested against a fake
// `LoginItemService`) and only this thin adapter runs against the live framework — the same split
// `SettingsModel` (tested) vs `SettingsView` (app-only) uses. Its own correctness is covered by the local
// dev-signed E2E the #170 spike validated, not by a unit test.
//
// #170 vs #171: `SMAppService.mainApp` needs no embedded binary, so the login-item surface is fully live now.
// The daemon agent needs the bundled `Contents/Library/LaunchAgents/org.sessiometer.agent.plist` + the embedded
// daemon binary that #171 ships; until then `agent(plistName:).status` is `.notFound`, which the model reads as
// "not registrable" and the Start affordance degrades honestly.

import Foundation
import os
import ServiceManagement

private let loginItemServiceLog = Logger(subsystem: "org.sessiometer.menubar", category: "login-item")

/// The bundled daemon LaunchAgent's plist filename — resolved by `SMAppService.agent(plistName:)` relative to
/// the app bundle's `Contents/Library/LaunchAgents/`. It is `<label>.plist` for the ratified daemon label
/// (`org.sessiometer.agent`, `src/service.rs` `AGENT_LABEL`), so the app and the CLI register ONE identity.
private let daemonAgentPlistName = "org.sessiometer.agent.plist"

final class SMAppServiceLoginItemService: LoginItemService {

    /// `SMAppService.agent` for the embedded daemon LaunchAgent — one instance reused across status reads and
    /// register/unregister so every call targets the same bundled plist.
    private let daemonAgent = SMAppService.agent(plistName: daemonAgentPlistName)

    // MARK: App login item (SMAppService.mainApp — no embedded binary, live in #170)

    var appStatus: LoginItemStatus { Self.map(SMAppService.mainApp.status) }

    func registerApp() throws { try SMAppService.mainApp.register() }

    func unregisterApp() throws { try SMAppService.mainApp.unregister() }

    // MARK: Daemon agent (SMAppService.agent — #171 ships the bundled plist)

    var daemonAgentStatus: LoginItemStatus { Self.map(daemonAgent.status) }

    /// Whether the Rust CLI already owns the `org.sessiometer.agent` LaunchAgent — the two-owner guard (issue
    /// #170 / #329). `sessiometer service install` writes `~/Library/LaunchAgents/org.sessiometer.agent.plist`
    /// (`src/service.rs` `agent_plist()`); the app's OWN `SMAppService.agent` registration lives in the bundle +
    /// the ServiceManagement database, never that folder — so a file there means the CLI is the owner and the
    /// app must not register a second plist onto the same launchd label. Non-sandboxed (ADR-0011), so the real
    /// `~/Library/LaunchAgents` is reachable.
    var cliManagedAgentPresent: Bool {
        FileManager.default.fileExists(atPath: Self.cliAgentPlistPath.path)
    }

    func registerDaemonAgent() throws { try daemonAgent.register() }

    func unregisterDaemonAgent() throws { try daemonAgent.unregister() }

    // MARK: Approval deep-link

    func openLoginItemsSettings() { SMAppService.openSystemSettingsLoginItems() }

    // MARK: Mapping

    /// `~/Library/LaunchAgents/org.sessiometer.agent.plist` — the exact path the Rust CLI's `service install`
    /// writes (`src/service.rs`: `launch_agents_dir()/{AGENT_LABEL}.plist`).
    private static let cliAgentPlistPath: URL =
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/LaunchAgents/\(daemonAgentPlistName)")

    /// Map `SMAppService.Status` onto the AppKit-free `LoginItemStatus` the model reasons over. An unknown
    /// future case degrades to `.notRegistered` — the SAFE direction (toggle reads off, honest), never a
    /// fabricated "enabled".
    private static func map(_ status: SMAppService.Status) -> LoginItemStatus {
        switch status {
        case .notRegistered: return .notRegistered
        case .enabled: return .enabled
        case .requiresApproval: return .requiresApproval
        case .notFound: return .notFound
        @unknown default:
            loginItemServiceLog.error("unknown SMAppService.Status raw=\(status.rawValue, privacy: .public)")
            return .notRegistered
        }
    }
}
