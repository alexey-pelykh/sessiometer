// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Hermetic tests for the launch-at-login model (issue #170): toggle-state derivation, the register/unregister
// intent, idempotent first-launch registration, `.requiresApproval` as a non-failure gate, the Start-daemon
// phase machine, and the two-owner guard. Each maps to an acceptance criterion.
//
// The model is driven by a `FakeLoginItemService` conforming to the same `LoginItemService` seam the concrete
// `SMAppServiceLoginItemService` implements — NO real `SMAppService`, so a test run can NEVER write a login
// item to the operator's account or register a LaunchAgent. The fake records call counts and lets each test
// script the status a register lands on (e.g. `.requiresApproval`) or an error it throws.

import Foundation
import XCTest

@MainActor
final class LoginItemModelTests: XCTestCase {

    // MARK: - Toggle-state derivation (AC: the toggle reflects the true SMAppService status)

    /// The toggle reads ON for `.enabled` AND `.requiresApproval` (both are successful registrations), OFF for
    /// `.notRegistered` / `.notFound`; `needsApproval` is true ONLY for `.requiresApproval`.
    func testToggleStateDerivationPerStatus() {
        XCTAssertFalse(makeModel(appStatus: .notRegistered).model.launchAtLoginEnabled)
        XCTAssertTrue(makeModel(appStatus: .enabled).model.launchAtLoginEnabled)

        let approval = makeModel(appStatus: .requiresApproval).model
        XCTAssertTrue(approval.launchAtLoginEnabled, "requiresApproval is a successful registration — toggle ON")
        XCTAssertTrue(approval.needsApproval, "requiresApproval surfaces the approval hint")

        let enabled = makeModel(appStatus: .enabled).model
        XCTAssertFalse(enabled.needsApproval, "an enabled item needs no approval")
        XCTAssertFalse(makeModel(appStatus: .notFound).model.launchAtLoginEnabled)
    }

    // MARK: - Register / unregister intent

    /// Turning the toggle ON from off registers the app login item exactly once and reflects the new status.
    func testTurningOnRegistersAppOnce() {
        let (model, fake) = makeModel(appStatus: .notRegistered)
        model.setLaunchAtLogin(true)
        XCTAssertEqual(fake.registerAppCount, 1)
        XCTAssertEqual(model.appStatus, .enabled)
        XCTAssertTrue(model.launchAtLoginEnabled)
    }

    /// Turning the toggle OFF from on unregisters the app login item exactly once.
    func testTurningOffUnregistersApp() {
        let (model, fake) = makeModel(appStatus: .enabled)
        model.setLaunchAtLogin(false)
        XCTAssertEqual(fake.unregisterAppCount, 1)
        XCTAssertEqual(model.appStatus, .notRegistered)
        XCTAssertFalse(model.launchAtLoginEnabled)
    }

    // MARK: - Idempotency / re-entrancy (no double-register; first-launch is safe every launch)

    /// Setting the toggle to its CURRENT state is a no-op — never a second register/unregister.
    func testSettingToCurrentStateIsNoOp() {
        let (onModel, onFake) = makeModel(appStatus: .enabled)
        onModel.setLaunchAtLogin(true)
        XCTAssertEqual(onFake.registerAppCount, 0, "already on → no re-register")

        let (offModel, offFake) = makeModel(appStatus: .notRegistered)
        offModel.setLaunchAtLogin(false)
        XCTAssertEqual(offFake.unregisterAppCount, 0, "already off → no unregister")
    }

    /// A rapid double turn-off unregisters exactly once — the second is a no-op (the status guard).
    func testDoubleTurnOffUnregistersOnce() {
        let (model, fake) = makeModel(appStatus: .enabled)
        model.setLaunchAtLogin(false)
        model.setLaunchAtLogin(false)
        XCTAssertEqual(fake.unregisterAppCount, 1)
    }

    /// First-launch registration registers when off — safe to call from `main.swift` every launch.
    func testFirstLaunchRegistersWhenOff() {
        let (model, fake) = makeModel(appStatus: .notRegistered)
        model.registerAppLoginItemOnLaunch()
        XCTAssertEqual(fake.registerAppCount, 1)
        XCTAssertEqual(model.appStatus, .enabled)
    }

    /// First-launch registration is a NO-OP when the app is already a login item — a relaunch never
    /// re-registers (nor does it re-register while an approval is pending).
    func testFirstLaunchNoOpWhenAlreadyEnabled() {
        let (enabled, enabledFake) = makeModel(appStatus: .enabled)
        enabled.registerAppLoginItemOnLaunch()
        XCTAssertEqual(enabledFake.registerAppCount, 0, "already enabled → no re-register on relaunch")

        let (pending, pendingFake) = makeModel(appStatus: .requiresApproval)
        pending.registerAppLoginItemOnLaunch()
        XCTAssertEqual(pendingFake.registerAppCount, 0, "approval pending is still ON → no re-register")
    }

    // MARK: - requiresApproval is a success, not a failure

    /// A register that lands in `.requiresApproval` leaves the toggle ON and surfaces `needsApproval` — never
    /// treated as a failure — and the deep-link forwards to System Settings.
    func testRequiresApprovalIsOnAndDeepLinks() {
        let (model, fake) = makeModel(appStatus: .notRegistered)
        fake.appRegisterResult = .requiresApproval
        model.setLaunchAtLogin(true)

        XCTAssertEqual(model.appStatus, .requiresApproval)
        XCTAssertTrue(model.launchAtLoginEnabled, "the register succeeded — the toggle stays ON")
        XCTAssertTrue(model.needsApproval)

        model.openLoginItemsSettings()
        XCTAssertEqual(fake.openSettingsCount, 1, "the approval hint deep-links to Login Items")
    }

    /// A register that THROWS leaves the item off and the toggle off (honest — never optimistically ON).
    func testFailedRegisterLeavesToggleOff() {
        let (model, fake) = makeModel(appStatus: .notRegistered)
        fake.appRegisterError = FakeLoginItemError.denied
        model.setLaunchAtLogin(true)
        XCTAssertEqual(fake.registerAppCount, 1)
        XCTAssertFalse(model.launchAtLoginEnabled, "a failed register is reflected honestly — toggle stays off")
    }

    // MARK: - Start-daemon phase machine

    /// A successful start registers the daemon agent once and lands `.idle`, with the daemon status reflecting
    /// the registration (the panel then leaves `.notRunning` via the next watch snapshot).
    func testStartDaemonSuccess() async {
        let (model, fake) = makeModel(daemonAgentStatus: .notRegistered)  // registrable (plist present, #171)
        XCTAssertTrue(model.canStartDaemon)
        await model.startDaemon()
        XCTAssertEqual(fake.registerDaemonCount, 1)
        XCTAssertEqual(model.daemonStatus, .enabled)
        XCTAssertEqual(model.startPhase, .idle)
    }

    /// A daemon register that throws lands `.failed` with a redacted reason (never a crash, never a silent no-op).
    func testStartDaemonFailureSurfacesReason() async {
        let (model, fake) = makeModel(daemonAgentStatus: .notRegistered)
        fake.daemonRegisterError = FakeLoginItemError.denied
        await model.startDaemon()
        XCTAssertEqual(fake.registerDaemonCount, 1)
        guard case .failed = model.startPhase else {
            return XCTFail("expected .failed, got \(model.startPhase)")
        }
    }

    /// The #170 shipped state: no bundled plist → `.notFound` → the Start action is inert (canStartDaemon false,
    /// no register attempted), the honest degradation before #171 activates the agent.
    func testStartDaemonInertWhenNotFound() async {
        let (model, fake) = makeModel(daemonAgentStatus: .notFound)
        XCTAssertFalse(model.canStartDaemon)
        await model.startDaemon()
        XCTAssertEqual(fake.registerDaemonCount, 0, "no plist bundled (#170) → the Start action never registers")
        XCTAssertEqual(model.startPhase, .idle)
    }

    // MARK: - Two-owner guard (the app yields to a CLI-managed daemon agent)

    /// When the Rust CLI already owns `org.sessiometer.agent`, the app defers: canStartDaemon is false and a
    /// Start attempt registers nothing — never a second owner on one launchd label (issue #170 / #329).
    func testTwoOwnerGuardBlocksDaemonRegister() async {
        let (model, fake) = makeModel(daemonAgentStatus: .notRegistered, cliManagedAgentPresent: true)
        XCTAssertFalse(model.canStartDaemon, "a CLI-managed agent means the app stands down")
        await model.startDaemon()
        XCTAssertEqual(fake.registerDaemonCount, 0, "the app never registers a second owner for the label")
    }

    // MARK: - Liveness gate (issue #742: the app yields to ANY live daemon, even a manual one)

    /// When ANY daemon holds the single-instance lock — including a manually-run `sessiometer run`
    /// with no LaunchAgent plist (so `cliManagedAgentPresent` is false, and the two-owner guard
    /// alone would NOT catch it) — the app stands down: `canStartDaemon` is false and a Start attempt
    /// registers nothing, so the app never registers an agent that would just lose the lock and
    /// silently stand down.
    func testLivenessGateBlocksStartWhenADaemonHoldsTheLock() async {
        let (model, fake) = makeModel(daemonAgentStatus: .notRegistered, daemonLockHeld: true)
        XCTAssertFalse(model.canStartDaemon,
                       "a live daemon (lock held) means the app stands down, even with no CLI plist")
        await model.startDaemon()
        XCTAssertEqual(fake.registerDaemonCount, 0,
                       "the app never registers an agent that would just lose the lock")
    }

    /// `canStartDaemon` is the three-way conjunction: registrable (not `.notFound`) AND no CLI owner
    /// (two-owner guard) AND no daemon holding the lock (liveness gate, issue #742).
    func testCanStartDaemonDerivation() {
        XCTAssertFalse(makeModel(daemonAgentStatus: .notFound).model.canStartDaemon)
        XCTAssertTrue(makeModel(daemonAgentStatus: .notRegistered).model.canStartDaemon)
        XCTAssertTrue(makeModel(daemonAgentStatus: .enabled).model.canStartDaemon)
        XCTAssertFalse(
            makeModel(daemonAgentStatus: .notRegistered, cliManagedAgentPresent: true).model.canStartDaemon)
        // The liveness gate is independent of the two-owner gate: a registrable agent + no CLI plist,
        // but a daemon holds the lock ⇒ still blocked (the manual-daemon case).
        XCTAssertFalse(
            makeModel(daemonAgentStatus: .notRegistered, daemonLockHeld: true).model.canStartDaemon)
    }

    // MARK: - Helpers

    @discardableResult
    private func makeModel(
        appStatus: LoginItemStatus = .notRegistered,
        daemonAgentStatus: LoginItemStatus = .notFound,
        cliManagedAgentPresent: Bool = false,
        daemonLockHeld: Bool = false
    ) -> (model: LoginItemModel, fake: FakeLoginItemService) {
        let fake = FakeLoginItemService(
            appStatus: appStatus,
            daemonAgentStatus: daemonAgentStatus,
            cliManagedAgentPresent: cliManagedAgentPresent,
            daemonLockHeld: daemonLockHeld)
        return (LoginItemModel(service: fake), fake)
    }
}

// MARK: - Test doubles

/// A hermetic `LoginItemService`: no `SMAppService`, so a test never writes a login item or a LaunchAgent. It
/// records call counts, lets a test script the status a register LANDS on (`appRegisterResult` /
/// `daemonRegisterResult` — e.g. `.requiresApproval`), and lets a test make any register/unregister THROW.
private final class FakeLoginItemService: LoginItemService {
    var appStatus: LoginItemStatus
    var daemonAgentStatus: LoginItemStatus
    var cliManagedAgentPresent: Bool
    var daemonLockHeld: Bool

    /// The status `registerApp()` lands on when it does not throw (default `.enabled`; set `.requiresApproval`).
    var appRegisterResult: LoginItemStatus = .enabled
    /// The status `registerDaemonAgent()` lands on when it does not throw.
    var daemonRegisterResult: LoginItemStatus = .enabled

    var appRegisterError: Error?
    var appUnregisterError: Error?
    var daemonRegisterError: Error?

    private(set) var registerAppCount = 0
    private(set) var unregisterAppCount = 0
    private(set) var registerDaemonCount = 0
    private(set) var openSettingsCount = 0

    init(
        appStatus: LoginItemStatus,
        daemonAgentStatus: LoginItemStatus,
        cliManagedAgentPresent: Bool,
        daemonLockHeld: Bool = false
    ) {
        self.appStatus = appStatus
        self.daemonAgentStatus = daemonAgentStatus
        self.cliManagedAgentPresent = cliManagedAgentPresent
        self.daemonLockHeld = daemonLockHeld
    }

    func registerApp() throws {
        registerAppCount += 1
        if let appRegisterError { throw appRegisterError }
        appStatus = appRegisterResult
    }

    func unregisterApp() throws {
        unregisterAppCount += 1
        if let appUnregisterError { throw appUnregisterError }
        appStatus = .notRegistered
    }

    func registerDaemonAgent() throws {
        registerDaemonCount += 1
        if let daemonRegisterError { throw daemonRegisterError }
        daemonAgentStatus = daemonRegisterResult
    }

    func unregisterDaemonAgent() throws {
        daemonAgentStatus = .notRegistered
    }

    func openLoginItemsSettings() { openSettingsCount += 1 }
}

/// A stand-in for an `SMAppService` registration error (denied / not permitted) — its exact reason is irrelevant
/// to the model, which only routes a throw into `.failed` with a redacted message.
private enum FakeLoginItemError: Error {
    case denied
}
