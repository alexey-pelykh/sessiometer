// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The launch-at-login model (issue #170): the pure `@MainActor` decision layer over `SMAppService` login-item
// + LaunchAgent registration, exposing the "Launch at login" toggle intent and the "Start daemon" affordance
// to the Settings form and the not-running panel card. It is the login-item SIBLING of `SettingsModel` (#268)
// and `AccountSwapModel` (#169) — the same tested-shell / untested-OS-wrapper split, only the surface differs.
//
// AppKit- AND ServiceManagement-FREE by design (Foundation + Combine + os only) so it compiles into the
// headless `MenubarTests` bundle and its toggle derivation, idempotent first-launch registration,
// `.requiresApproval` handling, the two-owner guard, and the Start-daemon phase machine are all driven
// hermetically against a fake `LoginItemService` — NO real `SMAppService` registration, no login item ever
// written to the operator's account by a test run. The concrete `SMAppServiceLoginItemService` (which imports
// `ServiceManagement` and touches the OS) stays in the app target, the same split `SettingsModel` (tested) vs
// `SettingsView`/`SMAppServiceLoginItem` (app-only) uses.
//
// NO credential handling of any kind (C-001 / issue #15): the whole surface is registration state (login item
// + LaunchAgent), which carries no token, email, or oauth blob. A `.failed` reason is a redacted registration
// message, never a secret.
//
// TWO-OWNER INVARIANT (issue #170 / #329, load-bearing): the `org.sessiometer.agent` LaunchAgent can be
// registered by the Rust CLI (`sessiometer service install`, `~/Library/LaunchAgents`, `src/service.rs`) AND
// by this app (a bundled `SMAppService.agent` plist) — ONE identity, deliberately shared. Two plists on one
// label collide. The app is the newcomer, so it YIELDS: when the CLI already owns the label
// (`cliManagedAgentPresent`), the app never registers its bundled agent and the Start affordance stands down.

import Combine
import Foundation
import os

private let loginItemLog = Logger(subsystem: "org.sessiometer.menubar", category: "login-item")

// MARK: - The OS seam

/// One `SMAppService` registration status, mirrored into an AppKit-free enum so the pure model can reason over
/// it without importing `ServiceManagement`. The concrete `SMAppServiceLoginItemService` maps
/// `SMAppService.Status` (`.notRegistered` / `.enabled` / `.requiresApproval` / `.notFound`) onto these cases.
enum LoginItemStatus: Equatable {
    /// Not registered — the toggle reads OFF.
    case notRegistered
    /// Registered and active — the toggle reads ON.
    case enabled
    /// Registered but the user must approve it in System Settings › General › Login Items. This is a SUCCESS
    /// (the register call worked) with a pending approval gate — the toggle reads ON, never a failure.
    case requiresApproval
    /// No such registrable item — for the daemon agent in #170 this is the expected state until #171 embeds
    /// the daemon binary + ships the bundled `Contents/Library/LaunchAgents` plist.
    case notFound
}

/// The OS surface the `LoginItemModel` drives, behind a protocol so the model's decisions are tested against a
/// fake. `SMAppService` exposes no injectable state, so this seam is the ONLY testability boundary; the concrete
/// implementation wraps `SMAppService.mainApp` / `SMAppService.agent(plistName:)` and the CLI-owner probe.
protocol LoginItemService: AnyObject {
    /// The app's own login-item (`SMAppService.mainApp`) status.
    var appStatus: LoginItemStatus { get }
    /// Register the app as a login item (idempotent at the OS layer — a re-register of an enabled item is a
    /// no-op). Throws the `SMAppService` error on failure.
    func registerApp() throws
    /// Unregister the app login item. Throws on failure.
    func unregisterApp() throws

    /// The bundled daemon LaunchAgent (`SMAppService.agent`) status. `.notFound` until #171 ships the plist.
    var daemonAgentStatus: LoginItemStatus { get }
    /// Whether the Rust CLI already owns the `org.sessiometer.agent` LaunchAgent (a plist at the CLI's
    /// `~/Library/LaunchAgents` path). When true, the app defers — the two-owner guard (issue #170 / #329).
    var cliManagedAgentPresent: Bool { get }
    /// Whether ANY daemon currently holds the single-instance lock (`daemon.lock`) — a fresh
    /// client-side `flock` liveness probe (issue #742). Unlike `cliManagedAgentPresent` (which only
    /// checks whether the CLI's LaunchAgent PLIST FILE exists), this detects a LIVE daemon of any
    /// provenance — including a developer's manually-run `sessiometer run` that has no LaunchAgent —
    /// so the Start affordance can stand down honestly rather than register an agent that would just
    /// lose the lock.
    var daemonLockHeld: Bool { get }
    /// Register (and, via the plist's `RunAtLoad`, start) the embedded daemon LaunchAgent. Throws on failure.
    func registerDaemonAgent() throws
    /// Unregister the bundled daemon LaunchAgent. Throws on failure.
    func unregisterDaemonAgent() throws

    /// Open System Settings › General › Login Items (`SMAppService.openSystemSettingsLoginItems()`), for the
    /// `.requiresApproval` deep-link.
    func openLoginItemsSettings()
}

// MARK: - LoginItemModel

@MainActor
final class LoginItemModel: ObservableObject {

    /// The Start-daemon affordance's interaction phase. `registering` is the transient beat that persists from
    /// the button press THROUGH the register AND the post-register liveness wait (issue #745). `failed` carries a
    /// card-rendered reason — either a REDACTED register error (no credential surface — issue #15) OR the
    /// "registered but never started" liveness timeout. `idle` is the resting state and TRUE success: register
    /// succeeded AND a daemon took the single-instance lock, so the panel will leave `.notRunning` on the next
    /// `watch` snapshot, exactly as a swap's new active row arrives.
    enum StartPhase: Equatable {
        case idle
        case registering
        case failed(reason: String)
    }

    // MARK: Published state

    /// The app login-item status — drives the "Launch at login" toggle. `private(set)`: it only changes via a
    /// register/unregister or an explicit refresh, never a direct write.
    @Published private(set) var appStatus: LoginItemStatus
    /// The bundled daemon LaunchAgent status — gates the Start affordance. `.notFound` until #171.
    @Published private(set) var daemonStatus: LoginItemStatus
    /// The Start-daemon interaction phase the not-running card observes.
    @Published private(set) var startPhase: StartPhase = .idle

    private let service: LoginItemService

    /// The post-register liveness wait (issue #745): how often to re-probe the single-instance lock, and how
    /// long to wait for a daemon to take it, before declaring the start failed. Injectable so tests drive the
    /// poll deterministically without real-time sleeps; the production defaults give a slow launchd `RunAtLoad`
    /// spawn ample room while still surfacing a dead start within a few seconds.
    private let livenessPollInterval: Duration
    private let livenessTimeout: Duration

    init(service: LoginItemService,
         livenessPollInterval: Duration = .milliseconds(500),
         livenessTimeout: Duration = .seconds(8)) {
        self.service = service
        self.livenessPollInterval = livenessPollInterval
        self.livenessTimeout = livenessTimeout
        self.appStatus = service.appStatus
        self.daemonStatus = service.daemonAgentStatus
    }

    // MARK: App login item (fully shippable in #170 — no embedded binary needed)

    /// Whether the app is registered to launch at login — the toggle's ON state. BOTH `.enabled` and
    /// `.requiresApproval` read ON: the register succeeded in each; `.requiresApproval` is a separate,
    /// non-failure approval gate the view surfaces (never a reason to show the toggle off).
    var launchAtLoginEnabled: Bool { appStatus == .enabled || appStatus == .requiresApproval }

    /// Whether launch-at-login is enabled but the user must still approve it in System Settings › Login Items.
    /// The view shows an inline hint + a deep-link when true; the toggle stays ON.
    var needsApproval: Bool { appStatus == .requiresApproval }

    /// The toggle intent (bound by the Settings form). Register when turning ON and not already on; unregister
    /// when turning OFF and currently on. Idempotent — a set to the current state is a no-op, so it never
    /// double-registers — and it re-reads the true status afterwards, so a `.requiresApproval` result (or a
    /// failed register that left the item off) is reflected HONESTLY rather than optimistically.
    func setLaunchAtLogin(_ desired: Bool) {
        if desired {
            guard !launchAtLoginEnabled else { return }
            do {
                try service.registerApp()
            } catch {
                loginItemLog.error("login item register failed: \(String(describing: error), privacy: .public)")
            }
        } else {
            guard launchAtLoginEnabled else { return }
            do {
                try service.unregisterApp()
            } catch {
                loginItemLog.error("login item unregister failed: \(String(describing: error), privacy: .public)")
            }
        }
        appStatus = service.appStatus
    }

    /// Idempotent first-launch registration, called from `main.swift` on every launch. A no-op when the app is
    /// already a login item (`.enabled` / `.requiresApproval`) — so re-launches never re-register — otherwise it
    /// registers the app login item. Non-fatal: a register failure is logged and the app carries on (the toggle
    /// still reflects the true, un-registered status). This registers ONLY the app login item, never the daemon
    /// agent (that stays user-initiated via the Start affordance — issue #170 keystone).
    func registerAppLoginItemOnLaunch() {
        guard !launchAtLoginEnabled else { return }
        do {
            try service.registerApp()
        } catch {
            loginItemLog.error("first-launch login item register failed: \(String(describing: error), privacy: .public)")
        }
        appStatus = service.appStatus
    }

    /// Deep-link to System Settings › General › Login Items — the action behind the `.requiresApproval` hint.
    func openLoginItemsSettings() { service.openLoginItemsSettings() }

    // MARK: Daemon agent (Start affordance; #171 activates the bundled plist)

    /// Whether the Start-daemon affordance can act, as the conjunction of three gates: the bundled
    /// agent is registrable (NOT `.notFound` — i.e. #171 has shipped the plist + embedded binary),
    /// the Rust CLI is not already the LaunchAgent owner (the two-owner guard), AND no daemon
    /// currently holds the single-instance lock (the liveness gate, issue #742). The liveness gate
    /// catches what the two-owner check cannot: a manually-run `sessiometer run` has no LaunchAgent
    /// plist, but it DOES hold `daemon.lock`. While any gate fails — the #170 no-plist state, a
    /// CLI-managed daemon, or ANY live daemon — the Start affordance is withheld rather than
    /// offering a misleading action that would register an agent and silently win nothing.
    var canStartDaemon: Bool {
        daemonStatus != .notFound && !service.cliManagedAgentPresent && !service.daemonLockHeld
    }

    /// The "Start daemon" action: register (and, via `RunAtLoad`, start) the embedded daemon LaunchAgent. A
    /// no-op when `canStartDaemon` is false (the button is only offered when it is) or a start is already in
    /// flight. After register succeeds it WAITS for a daemon to actually take the single-instance lock
    /// (issue #745) — landing `.idle` (the panel then leaves `.notRunning` on the next `watch` snapshot) only if
    /// one comes up, else `.failed`, so a dead start is never silent; a register throw surfaces a redacted reason
    /// inline. The `registering` beat is painted before the synchronous register (the `Task.yield()`) and HELD
    /// across the liveness wait, mirroring the swap/capture affordances' pending state.
    func startDaemon() async {
        guard canStartDaemon else { return }
        if case .registering = startPhase { return }

        startPhase = .registering
        await Task.yield()  // let the "Starting…" beat paint before the synchronous register
        do {
            try service.registerDaemonAgent()
            daemonStatus = service.daemonAgentStatus
            // register() SUCCEEDING only means launchd accepted the plist — the plist's `RunAtLoad` still has to
            // SPAWN the daemon, and that spawn can fail (a bad launchd config, a crash-loop, a sandbox denial)
            // while register() reports success (issue #745). So don't assume `.idle`: wait for a daemon to
            // actually take the single-instance lock, and surface a failure if none does — otherwise the card
            // sits silent forever, since the panel only leaves `.notRunning` on a `watch` snapshot that, with no
            // daemon, never arrives.
            startPhase = await daemonBecameLive()
                ? .idle
                : .failed(reason: Self.notStartedReason)
        } catch {
            loginItemLog.error("daemon agent register failed: \(String(describing: error), privacy: .public)")
            startPhase = .failed(reason: Self.startFailureReason(error))
        }
    }

    /// Poll for a daemon to take the single-instance lock after a successful register (issue #745). Reuses the
    /// same `daemonLockHeld` flock probe `canStartDaemon` gates on (issue #742): `canStartDaemon` guaranteed the
    /// lock was FREE at the button press, so a lock that becomes held within the window is the daemon our
    /// register just started (via `RunAtLoad`). Returns `true` as soon as the lock is held, `false` if the
    /// bounded window elapses with no daemon — the honest "registered but never started" signal. Bounded and
    /// cancellation-aware so it never spins the UI.
    private func daemonBecameLive() async -> Bool {
        if service.daemonLockHeld { return true }
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: livenessTimeout)
        while clock.now < deadline {
            do { try await Task.sleep(for: livenessPollInterval) }
            catch { return service.daemonLockHeld }  // cancelled — report last-known state, stop polling
            if service.daemonLockHeld { return true }
        }
        return false
    }

    /// Re-read both statuses from the OS — called when the Settings window (re)opens and when the app becomes
    /// active, so a login-item change the user made directly in System Settings is reflected without a relaunch.
    func refreshStatus() {
        appStatus = service.appStatus
        daemonStatus = service.daemonAgentStatus
    }

    /// The #745 silent-failure copy: register succeeded but no daemon took the lock within the liveness window.
    /// There is no OS error to surface (register did NOT throw), so this is a plain, actionable statement rather
    /// than a redacted message — and, like every `.failed` reason, it carries no credential (issue #15).
    private static let notStartedReason =
        "The daemon was registered but didn’t start. Check Console for details."

    /// A redacted, non-secret reason for a failed daemon start (issue #15) — a registration error carries no
    /// credential, so the OS message is safe to surface, with a plain fallback when it is empty.
    private static func startFailureReason(_ error: Error) -> String {
        let message = (error as NSError).localizedDescription
        return message.isEmpty ? "The daemon couldn’t be started." : message
    }
}
