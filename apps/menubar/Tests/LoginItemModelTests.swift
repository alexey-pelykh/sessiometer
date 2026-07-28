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

    /// A TRUE success: the register lands AND the daemon comes up and takes the single-instance lock within the
    /// liveness window (issue #745), so the phase lands `.idle` and the panel then leaves `.notRunning` via the
    /// next watch snapshot.
    func testStartDaemonSuccessWhenDaemonComesUp() async {
        let (model, fake) = makeModel(daemonAgentStatus: .notRegistered,  // registrable (plist present, #171)
                                      daemonComesUpOnRegister: true)
        XCTAssertTrue(model.canStartDaemon)
        await model.startDaemon()
        XCTAssertEqual(fake.registerDaemonCount, 1)
        XCTAssertEqual(model.daemonStatus, .enabled)
        XCTAssertEqual(model.startPhase, .idle)
    }

    /// The #745 fix: register SUCCEEDS but the daemon never takes the lock (a bad launchd config / crash-loop /
    /// sandbox denial — register does NOT throw). The old code landed `.idle` and the card sat silent; now the
    /// bounded liveness wait elapses with the lock still free and the phase lands `.failed` with the actionable
    /// "registered but didn't start" reason, so the click is never a silent no-op.
    func testStartDaemonRegistersButDaemonNeverComesUpSurfacesFailure() async {
        let (model, fake) = makeModel(daemonAgentStatus: .notRegistered,
                                      daemonComesUpOnRegister: false)  // register succeeds, daemon never starts
        XCTAssertTrue(model.canStartDaemon)
        await model.startDaemon()
        XCTAssertEqual(fake.registerDaemonCount, 1, "register was attempted and SUCCEEDED (no throw)")
        guard case .failed(let reason) = model.startPhase else {
            return XCTFail("expected .failed (registered but never started), got \(model.startPhase)")
        }
        XCTAssertTrue(reason.contains("registered but"),
                      "the failure names the not-started condition, not a register error")
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

    // MARK: - Stale-registration repair after an app update (issue #788)

    /// T4 / AC3 — the header's explicit recommendation: on a changed executable, unregister BEFORE
    /// re-registering. Asserted on the ORDERED call log, not on counts, because "both were called" is exactly
    /// what a wrong order also satisfies. T1 rides here too: this is the unregister path's first live exercise.
    func testChangedExecutableUnregistersBeforeReRegistering() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [.unregister, .register],
                       "the SDK header requires unregister BEFORE re-register when the executable changed")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-2",
                       "a successful re-registration records WHAT was registered")
    }

    /// T5 (positive) / AC2 — a changed executable is detected and repaired, so launchd ends up holding a
    /// registration for the NEW binary.
    func testDetectorFiresOnAChangedExecutable() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.registerDaemonCount, 1)
        XCTAssertEqual(fake.unregisterDaemonCount, 1)
    }

    /// T5 (negative) — a detector that always fires is as wrong as one that never does. An UNCHANGED identity
    /// must touch nothing: no unregister (which would kill the daemon), no register.
    func testDetectorDoesNotFireOnAnUnchangedExecutable() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-1")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [], "an unchanged executable is a no-op — nothing to repair")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-1", "the recorded identity is left untouched")
        XCTAssertEqual(model.startPhase, .idle, "a no-op reconcile paints no beat at all")
    }

    /// T6 — no re-register storm: once a change is repaired, every subsequent launch at that same identity is
    /// inert. Three reconciles, one registration.
    func testRepeatedLaunchesAtTheSameVersionRegisterAtMostOnce() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()
        await model.reconcileDaemonAgentRegistration()
        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.registerDaemonCount, 1, "the repair happens once, not once per launch")
        XCTAssertEqual(fake.unregisterDaemonCount, 1)
    }

    /// T6 (at the source) — `startDaemon()` records what it registered, so the NEXT launch's reconcile sees an
    /// unchanged identity. Without this the app would unregister→re-register an agent registered seconds ago:
    /// the storm, seeded by the very act of starting.
    func testStartDaemonRecordsTheRegisteredIdentitySoTheNextLaunchIsInert() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .notRegistered, lastRegisteredIdentity: nil, agentIdentity: "build-2",
            daemonComesUpOnRegister: true)

        await model.startDaemon()
        XCTAssertEqual(store.lastRegisteredIdentity, "build-2")

        // Simulate the next launch finding the daemon gone and the lock free. BOTH must be cleared: leaving
        // the job `.running` would make the reconcile below defer at gate 4 and pass this test vacuously,
        // proving nothing about the identity short-circuit it is actually here to pin (issue #819).
        fake.daemonAgentRunState = .notRunning
        fake.daemonLockHeld = false
        await model.reconcileDaemonAgentRegistration()
        XCTAssertEqual(fake.unregisterDaemonCount, 0, "a freshly-registered agent is never churned")
        XCTAssertEqual(fake.registerDaemonCount, 1)
    }

    /// T9 / AC4 — the two-owner guard OUTRANKS the SDK's "must be re-registered". When the Rust CLI owns
    /// `org.sessiometer.agent`, the app performs NO re-registration at all, even with a detected change:
    /// asserted in the affirmative on BOTH halves (no unregister, no register).
    func testNoReRegistrationAtAllWhenTheAgentIsCLIManaged() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, cliManagedAgentPresent: true,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [],
                       "a CLI-managed LaunchAgent is not the app's to unregister or re-register")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-1",
                       "no registration happened, so nothing is recorded as registered")
    }

    /// T2 / AC4 — the unregister half of the two-owner guard, stated on its own: the app never unregisters a
    /// CLI-managed agent, whatever the detector says.
    func testCLIManagedAgentIsNeverUnregisteredByTheApp() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, cliManagedAgentPresent: true,
            lastRegisteredIdentity: nil, agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.unregisterDaemonCount, 0)
    }

    /// T7 / AC5 — a re-registration that would displace a LIVE daemon running the old executable is DEFERRED,
    /// never silently performed: `unregister()` unloads the launchd job and terminates that daemon. The
    /// deferral leaves the recorded identity stale ON PURPOSE, so the next launch retries.
    ///
    /// Staged with OUR job running (issue #819): that — not the bare lock — is the state in which the
    /// unregister would actually kill something, and it is what the gate now branches on. The lock is held
    /// too, because a running daemon of ours is holding it.
    func testLiveDaemonIsDeferredToNextLaunchNeverSilentlyKilled() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: true, daemonAgentRunState: .running,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [], "a live daemon is never displaced mid-launch")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-1",
                       "a deferral is not a repair — the stale identity stands so the next launch retries")
        XCTAssertEqual(model.startPhase, .idle,
                       "a deferral is a healthy postponement, not a failure — it paints no error card")
    }

    /// T7 (the retry half) — the deferral above is genuinely self-healing: the same model, on a later launch
    /// that finds our job stopped, performs the repair it postponed.
    func testDeferredRepairIsPerformedOnTheNextLaunchOnceOurJobHasStopped() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: true, daemonAgentRunState: .running,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()
        XCTAssertEqual(fake.daemonCalls, [])

        // Our daemon exited, releasing the lock — the state a later launch finds.
        fake.daemonAgentRunState = .notRunning
        fake.daemonLockHeld = false
        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [.unregister, .register])
        XCTAssertEqual(store.lastRegisteredIdentity, "build-2")
    }

    /// T3 / AC6 — an unregister that THROWS surfaces a reason on the existing not-running card rather than
    /// being swallowed, and never proceeds to register on top of a failed unregister.
    func testUnregisterFailureSurfacesAReasonInsteadOfBeingSwallowed() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2")
        fake.daemonUnregisterError = FakeLoginItemError.denied

        await model.reconcileDaemonAgentRegistration()

        guard case .failed(let reason) = model.startPhase else {
            return XCTFail("an unregister throw must surface, not vanish; got \(model.startPhase)")
        }
        XCTAssertFalse(reason.isEmpty, "the card needs something to say")
        XCTAssertEqual(fake.registerDaemonCount, 0, "a failed unregister does not proceed to register")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-1",
                       "a failed repair is never recorded as done — the next launch retries")
    }

    /// AC6 — the other failure half: unregister lands, register throws. The agent is left honestly
    /// unregistered with a reason shown AND the Start affordance available, so the operator can recover —
    /// never a silent half-state.
    func testRegisterFailureAfterUnregisterLeavesAnHonestRecoverableState() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2")
        fake.daemonRegisterError = FakeLoginItemError.denied

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [.unregister, .register])
        guard case .failed = model.startPhase else {
            return XCTFail("a register throw must surface; got \(model.startPhase)")
        }
        XCTAssertEqual(model.daemonStatus, .notRegistered, "the honest post-unregister status")
        XCTAssertTrue(model.canStartDaemon, "the operator can recover via the existing Start affordance")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-1",
                       "a failed register records nothing new — the identity of the registration that is now "
                       + "gone stands, and it is inert: the next launch finds `.notRegistered` and repairs "
                       + "nothing, because first registration is the Start affordance's job")
    }

    /// AC6 / issue #745 parity — a re-registration launchd ACCEPTS still has to be spawned by `RunAtLoad`, and
    /// that spawn can fail silently. No daemon within the liveness window ⇒ the same honest "registered but
    /// didn't start" reason the Start affordance uses, not a card that sits quietly forever.
    func testReRegisterThatNeverStartsADaemonSurfacesTheSilentStartFailure() async {
        let (model, _, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2",
            daemonComesUpOnRegister: false)

        await model.reconcileDaemonAgentRegistration()

        guard case .failed(let reason) = model.startPhase else {
            return XCTFail("a re-register whose daemon never came up must surface; got \(model.startPhase)")
        }
        XCTAssertTrue(reason.contains("didn’t start"), "the #745 silent-start copy, reused verbatim")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-2",
                       "the REGISTRATION succeeded — the spawn is a separate, separately-surfaced failure")
    }

    /// A successful repair whose daemon DOES come up lands `.idle` — true success, no residual error card.
    func testSuccessfulReRegistrationWhoseDaemonComesUpLandsIdle() async {
        let (model, _, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2",
            daemonComesUpOnRegister: true)

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(model.startPhase, .idle)
    }

    /// The reconcile REPAIRS, it never INITIATES (the #170 keystone: the app does not enroll a daemon nobody
    /// asked for). With nothing of ours registered — `.notRegistered` (never started) or `.notFound` (no
    /// bundled plist) — a detected change registers nothing.
    func testReconcileNeverInitiatesARegistrationTheOperatorNeverAskedFor() async {
        for status in [LoginItemStatus.notRegistered, .notFound] {
            let (model, fake, store) = makeReconcileModel(
                daemonAgentStatus: status, lastRegisteredIdentity: nil, agentIdentity: "build-2")

            await model.reconcileDaemonAgentRegistration()

            XCTAssertEqual(fake.daemonCalls, [], "nothing of ours is registered, so nothing is repaired (\(status))")
            XCTAssertNil(store.lastRegisteredIdentity)
        }
    }

    /// A `.requiresApproval` agent IS a live registration (issue #170 treats it as success everywhere else), so
    /// it is repairable too — the reconcile must not mistake the pending approval gate for "not registered".
    func testRequiresApprovalAgentIsRepairedLikeAnEnabledOne() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .requiresApproval, lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [.unregister, .register])
    }

    /// An agent registered by a build that PREDATES this bookkeeping has no recorded identity. Nil reads as
    /// CHANGED — that unknown executable is exactly the stale registration issue #788 exists to repair — and
    /// the repair then records, so it happens once rather than on every launch.
    func testNoRecordedIdentityCountsAsChangedAndIsRepairedExactlyOnce() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: nil, agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()
        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.registerDaemonCount, 1)
        XCTAssertEqual(store.lastRegisteredIdentity, "build-2")
    }

    /// Two reconciles running CONCURRENTLY repair once, not twice. Today there is a single call site, and the
    /// reverse direction is already safe (`startDaemon()` records before its liveness wait, so a concurrent
    /// reconcile no-ops at the identity gate). This pins the guard so that adding a second call site — an
    /// `applicationDidBecomeActive` refresh being the obvious one — cannot silently introduce a double
    /// unregister→register.
    func testConcurrentReconcilesRepairOnce() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2",
            daemonComesUpOnRegister: false)  // the liveness wait holds `.registering` open across the overlap

        async let first: Void = model.reconcileDaemonAgentRegistration()
        async let second: Void = model.reconcileDaemonAgentRegistration()
        _ = await (first, second)

        XCTAssertEqual(fake.daemonCalls, [.unregister, .register],
                       "the second reconcile must find one in flight and stand down, not repeat the repair")
    }

    /// The two deferral paths — the gate before the yield and the re-probe after it — must both leave
    /// `startPhase` exactly as they found it. The re-probe path is the one that can regress: it has already
    /// painted `.registering`, so putting back a literal `.idle` rather than the ENTRY phase would erase a
    /// pre-existing `.failed` reason. Unreachable from the single launch-time call site (the phase is always
    /// `.idle` there), so this pins the symmetry for the second call site the guard above anticipates.
    func testDeferralRestoresTheEntryPhaseRatherThanForcingIdle() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        // Land a real `.failed` first: a register that throws, with the lock free.
        fake.daemonRegisterError = FakeLoginItemError.denied
        await model.reconcileDaemonAgentRegistration()
        guard case .failed(let reason) = model.startPhase else {
            return XCTFail("setup: expected a failed repair, got \(model.startPhase)")
        }

        // Re-arm so the NEXT reconcile actually reaches the re-probe. Without these two lines the SUCCESSFUL
        // unregister above has left the agent `.notRegistered`, so the second call bails at gate 2 long before
        // the re-probe and the assertion below passes no matter what the deferral does to the phase — vacuous,
        // not passing.
        fake.daemonRegisterError = nil
        fake.daemonAgentStatus = .enabled
        // OUR agent comes up DURING the next repair — not running at gate 4, running by the time of the
        // re-probe (and holding the lock it took). That is the launch-time race the re-probe exists for:
        // the app is a login item and the agent is `RunAtLoad`, so both start at once.
        fake.agentSpawnsOnNextProbe = true
        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls.count, 2,
                       "the repair must have DEFERRED at the re-probe — no second unregister/register pair")

        guard case .failed(let preserved) = model.startPhase else {
            return XCTFail("the deferral erased the prior reason; got \(model.startPhase)")
        }
        XCTAssertEqual(preserved, reason, "a deferral reports nothing of its own — it restores what it found")
    }

    // MARK: - Whose daemon is it (issue #819)

    // The two directions AC4 names, stated as a pair. They differ in ONE input — who is running — and land on
    // opposite verdicts, which is what makes them a gate rather than two agreeing assertions.

    /// AC1 / AC4 (the proceed direction) — THE ISSUE. A FOREIGN daemon holds the single-instance lock (a
    /// hand-run `sessiometer run`, the provenance issue #742 explicitly supports) while OUR registered agent
    /// is not running. Issue #788's any-provenance gate deferred here forever, leaving the repair permanently
    /// inert. It must now proceed: unloading a job with no process behind it terminates nothing, whoever
    /// holds the lock.
    func testRepairProceedsWhenOnlyAForeignDaemonHoldsTheLock() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: true, daemonAgentRunState: .notRunning,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2",
            // Our re-registered agent's `RunAtLoad` spawn stands down (exit 0) because that foreign daemon
            // still holds the lock — the realistic outcome, and the reason the liveness wait is skipped below.
            daemonComesUpOnRegister: false)

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [.unregister, .register],
                       "a foreign lock holder is not ours to unload, so it is no reason to defer the repair")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-2", "the repair happened, so it is recorded")
        XCTAssertTrue(fake.daemonLockHeld,
                      "and the foreign daemon is untouched — our unregister unloaded OUR job, not its process")
    }

    /// AC2 / AC4 (the defer direction) — the same lock, the same detected change, the ONLY difference being
    /// that the running daemon is OURS. Here `unregister()` really would kill it, so the repair still defers.
    /// Issue #788's live-daemon gate is narrowed, not removed.
    func testRepairStillDefersWhenOurOwnAgentIsLive() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: true, daemonAgentRunState: .running,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [], "our own live daemon is never displaced mid-launch")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-1",
                       "a deferral is not a repair — the next launch retries")
        XCTAssertEqual(model.startPhase, .idle, "a healthy postponement paints no error card")
    }

    /// The proceed direction's OTHER half: a repair performed behind a foreign lock holder must not CLAIM a
    /// liveness it cannot observe. `daemonBecameLive()` decides by polling the single-instance lock, and its
    /// premise is written down — the lock was FREE at the decision point, so a lock taken inside the window is
    /// the daemon we just started. Behind a foreign holder that premise is false: the poll would return true on
    /// its first read and attribute a stranger's daemon to this repair.
    ///
    /// ASSERTED ON THE PROBE COUNT, not the phase, and that is the point. Both the correct path and a path
    /// that polled anyway land `.idle` — the foreign daemon's lock satisfies the poll immediately — so the end
    /// state cannot tell them apart, and a test that asserted only `.idle` would pass with the skip removed.
    /// (It did: that mutant survived until this counter was added.) What distinguishes them is whether the
    /// unanswerable question was asked at all.
    func testForeignHolderRepairDoesNotEnterALivenessWaitItCannotAttribute() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: true, daemonAgentRunState: .notRunning,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2",
            daemonComesUpOnRegister: false)  // our agent never comes up — it stood down behind the foreign one

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [.unregister, .register], "precondition: the repair did happen")
        XCTAssertEqual(fake.lockProbeCount, 2,
                       "exactly the two gate reads — `repairDisplacementCheck()` at gate 4 and again at the "
                       + "re-probe. A third would mean the liveness wait ran on a lock it cannot attribute.")
        XCTAssertEqual(model.startPhase, .idle,
                       "the registration succeeded and a daemon IS serving — neither a spinner nor the #745 "
                       + "\"didn't start\" card belongs here")
    }

    /// The counter's own control, so the assertion above cannot pass by the wait being unreachable in general.
    /// Same repair with the lock FREE: the liveness wait DOES run, so the probe count exceeds the two gate
    /// reads. Without this, a change that broke `daemonBecameLive()` entirely would leave the test above green.
    func testTheLivenessWaitIsStillEnteredWhenTheLockIsFree() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: false, daemonAgentRunState: .notRunning,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2",
            daemonComesUpOnRegister: true)

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [.unregister, .register], "precondition: the repair did happen")
        XCTAssertGreaterThan(fake.lockProbeCount, 2,
                            "with the lock free the wait is attributable, so it must actually be performed")
        XCTAssertEqual(model.startPhase, .idle, "and our daemon came up, so the repair truly succeeded")
    }

    /// The `.unknown` fallback, both directions. A probe that cannot answer must land EXACTLY on issue #788's
    /// any-provenance behaviour — defer iff any daemon holds the lock — never guess. Without this, a
    /// `launchctl` output-format drift would silently become either permanent inertness or, far worse, an
    /// unregister that kills a live daemon.
    func testUnknownRunStateFallsBackToTheAnyProvenanceLockGate() async {
        let (deferred, deferredFake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: true, daemonAgentRunState: .unknown,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2")
        await deferred.reconcileDaemonAgentRegistration()
        XCTAssertEqual(deferredFake.daemonCalls, [],
                       "cannot tell whose daemon holds the lock ⇒ defer, exactly as issue #788 did")

        let (repaired, repairedFake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: false, daemonAgentRunState: .unknown,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2")
        await repaired.reconcileDaemonAgentRegistration()
        XCTAssertEqual(repairedFake.daemonCalls, [.unregister, .register],
                       "a free lock ⇒ repair, exactly as issue #788 did — the fallback is never MORE inert")
    }

    /// AC3 — the two-owner guard outranks the new signal too. Even with our job demonstrably stopped (the
    /// state that now green-lights a repair), a CLI-owned label is still not the app's to touch.
    ///
    /// PRECEDENCE, not just outcome. "The repair did nothing" is satisfied by a guard that runs LAST as
    /// readily as by one that runs first, so the probe counters are what pin the ordering — and the ordering
    /// has teeth here: the CLI and the app share ONE launchd label (`org.sessiometer.agent`), so a demoted
    /// guard would have the run-state probe reading the CLI'S job and reporting it as "our LaunchAgent's job",
    /// plus a needless `launchctl` spawn on every launch of a CLI-managed machine.
    func testTwoOwnerGuardIsConsultedBeforeAnyOtherSignal() async {
        let (model, fake, _) = makeReconcileModel(
            daemonAgentStatus: .enabled, cliManagedAgentPresent: true,
            daemonLockHeld: true, daemonAgentRunState: .notRunning,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [],
                       "a stopped job does not license the app to re-point a LaunchAgent the CLI owns")
        XCTAssertEqual(fake.runStateProbeCount, 0,
                       "the two-owner guard must short-circuit BEFORE the run-state probe — a CLI-owned label "
                       + "means launchd's answer about that label is not about us at all")
        XCTAssertEqual(fake.lockProbeCount, 0, "and before the lock probe, for the same reason")
    }

    /// The one cell of `repairDisplacementCheck()`'s table that is STRICTER than issue #788: our job running
    /// while the lock is free. #788 would have proceeded there and unloaded a live job of ours; the narrowed
    /// gate postpones. Documented at length in the model, and — until this test — unguarded: every other
    /// `.running` case stages a held lock, so reverting this cell alone left the suite green.
    ///
    /// The state is real, if transient: our daemon is starting up and has not taken the lock yet, or it has
    /// stood down and not yet exited. Unloading it there is exactly the silent kill gate 4 exists to prevent.
    func testOurRunningJobDefersEvenWhenTheLockIsFree() async {
        let (model, fake, store) = makeReconcileModel(
            daemonAgentStatus: .enabled, daemonLockHeld: false, daemonAgentRunState: .running,
            lastRegisteredIdentity: "build-1", agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [],
                       "a running job of ours is never unloaded, whatever the lock says — the lock is not the "
                       + "thing that dies when launchd unloads the job")
        XCTAssertEqual(store.lastRegisteredIdentity, "build-1",
                       "a deferral is not a repair — the next launch retries")
    }

    // MARK: - The identity the detector compares (issue #788)

    /// The detector is only as good as the identity feeding it. Driving the model with hand-written strings
    /// proves the BRANCHING; this proves the SIGNAL — that rewriting the embedded daemon (what
    /// `embed-daemon.sh` does on every Release build) actually produces a different identity.
    func testIdentityChangesWhenTheEmbeddedDaemonIsRewritten() throws {
        let bundle = try makeTemporaryBundle(helperContents: "old-daemon")
        let before = DaemonAgentIdentity.current(bundleURL: bundle, infoDictionary: ["CFBundleVersion": "7"])

        // A re-`lipo` of a different-sized universal binary, with a moved mtime.
        let helper = bundle.appendingPathComponent(DaemonAgentIdentity.helperRelativePath)
        try "a-much-longer-new-daemon-binary".write(to: helper, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes(
            [.modificationDate: Date().addingTimeInterval(600)], ofItemAtPath: helper.path)

        let after = DaemonAgentIdentity.current(bundleURL: bundle, infoDictionary: ["CFBundleVersion": "7"])
        XCTAssertNotEqual(before, after, "a rewritten executable must change the identity, same version or not")
    }

    /// The negative direction at the identity layer: an untouched bundle reads the same identity on every
    /// launch, so the detector stays quiet.
    func testIdentityIsStableForAnUntouchedBundle() throws {
        let bundle = try makeTemporaryBundle(helperContents: "daemon")
        let info: [String: Any] = ["CFBundleShortVersionString": "0.2.0", "CFBundleVersion": "7"]
        XCTAssertEqual(DaemonAgentIdentity.current(bundleURL: bundle, infoDictionary: info),
                       DaemonAgentIdentity.current(bundleURL: bundle, infoDictionary: info))
    }

    /// The SEAM, not the halves. Both halves of the detector are covered above — `DaemonAgentIdentity.current`
    /// by the temp-bundle tests, the branching by injected strings — but every one of those injects
    /// `agentIdentity` explicitly, so NOTHING pinned the wire between them: replacing the production default
    /// with a constant left the whole suite green while making the repair fire once and go inert forever.
    /// This is the only test that builds a model WITHOUT injecting an identity, so it is the only one that
    /// fails if that default is ever unwired.
    func testProductionIdentityProviderIsTheOneTheModelActuallyUses() async {
        let fake = FakeLoginItemService(
            appStatus: .enabled, daemonAgentStatus: .notRegistered,
            cliManagedAgentPresent: false, daemonLockHeld: false)
        fake.daemonComesUpOnRegister = true
        let store = ephemeralRegistrationStore()

        // No `agentIdentity:` argument — the production default is under test.
        let model = LoginItemModel(service: fake,
                                   registrationStore: store,
                                   livenessPollInterval: .milliseconds(1),
                                   livenessTimeout: .milliseconds(20))
        await model.startDaemon()

        XCTAssertEqual(store.lastRegisteredIdentity, DaemonAgentIdentity.current(),
                       "the model must record the identity of the REAL running bundle — a default wired to a "
                       + "constant would make every later launch see no change and never repair anything")
    }

    /// A version bump alone changes the identity — the shipped-update signal, which holds even where the
    /// helper is absent (a bundle predating issue #171, or a Debug build whose embed step no-ops). Degrading
    /// to the version half is deliberate: coarser, never broken.
    func testIdentityFallsBackToTheVersionWhenTheHelperIsAbsent() throws {
        let bundle = try makeTemporaryBundle(helperContents: nil)
        let before = DaemonAgentIdentity.current(bundleURL: bundle, infoDictionary: ["CFBundleVersion": "7"])
        let after = DaemonAgentIdentity.current(bundleURL: bundle, infoDictionary: ["CFBundleVersion": "8"])
        XCTAssertNotEqual(before, after)
    }

    // MARK: - Render-harness seed behaviour (T8)

    // SCOPE — what these two do NOT prove. `PanelRenderHarness`'s `PanelRenderLoginItemService` is `private`
    // to the app target, so no test here can reach it: both drive a REPLICA of its seed, hand-copied, and
    // editing the harness's own seed would fail neither. T8's other half — "the stub stays in sync with the
    // protocol" — is structurally unfailable here: this change adds no protocol member (all three conformances
    // are byte-unchanged), so no conformance CAN drift, and one that did would simply not compile. The real
    // guard against the harness rendering something unexpected is `PanelGoldenParityTests`, which re-renders
    // every panel state and diffs it against the committed goldens. What these two DO pin is the model-side
    // contract that seed depends on.

    /// The replica seed still yields the Start affordance and the resting `.idle` phase — no spinner or error
    /// text leaking into a golden. If a model change makes that seed derive something else, this names the
    /// cause here rather than surfacing as an unexplained golden diff.
    func testRenderHarnessSeedStillYieldsTheStartAffordance() {
        let (model, _, _) = makeReconcileModel(
            appStatus: .enabled, daemonAgentStatus: .notRegistered,
            cliManagedAgentPresent: false, daemonLockHeld: false)

        XCTAssertTrue(model.canStartDaemon, "the not-running fixture renders the Start-daemon affordance")
        XCTAssertEqual(model.startPhase, .idle, "no pending or failed beat bleeds into a design render")
    }

    /// The mutation half: a design render must never mutate registration state. The seed is `.notRegistered`,
    /// so even if a render path reached the reconcile it would register nothing — both register and unregister
    /// stay untouched.
    func testRenderHarnessSeedMakesReconcileInert() async {
        let (model, fake, _) = makeReconcileModel(
            appStatus: .enabled, daemonAgentStatus: .notRegistered, agentIdentity: "build-2")

        await model.reconcileDaemonAgentRegistration()

        XCTAssertEqual(fake.daemonCalls, [], "a design render never mutates login-item state")
    }

    // MARK: - Helpers

    /// A model + fake + store wired for the issue #788 reconcile tests: the identity the bundle reports now
    /// (`agentIdentity`) and the one last recorded as registered (`lastRegisteredIdentity`) are both injected,
    /// so a test states the change it is exercising instead of simulating a build.
    private func makeReconcileModel(
        appStatus: LoginItemStatus = .enabled,
        daemonAgentStatus: LoginItemStatus = .enabled,
        cliManagedAgentPresent: Bool = false,
        daemonLockHeld: Bool = false,
        daemonAgentRunState: DaemonAgentRunState = .notRunning,
        lastRegisteredIdentity: String? = nil,
        agentIdentity: String = "build-1",
        daemonComesUpOnRegister: Bool = true
    ) -> (model: LoginItemModel, fake: FakeLoginItemService, store: DaemonAgentRegistrationStore) {
        let fake = FakeLoginItemService(
            appStatus: appStatus,
            daemonAgentStatus: daemonAgentStatus,
            cliManagedAgentPresent: cliManagedAgentPresent,
            daemonLockHeld: daemonLockHeld,
            daemonAgentRunState: daemonAgentRunState)
        fake.daemonComesUpOnRegister = daemonComesUpOnRegister

        let store = ephemeralRegistrationStore()
        store.lastRegisteredIdentity = lastRegisteredIdentity

        // Tiny liveness timings (issue #745) so the post-register lock poll runs in ~ms rather than stalling
        // the suite on the 8 s production window.
        let model = LoginItemModel(service: fake,
                                   registrationStore: store,
                                   agentIdentity: { agentIdentity },
                                   livenessPollInterval: .milliseconds(1),
                                   livenessTimeout: .milliseconds(20))
        return (model, fake, store)
    }

    /// A per-test volatile `UserDefaults` domain, so a recorded identity never touches the operator's real
    /// defaults or leaks into another test.
    private func ephemeralRegistrationStore() -> DaemonAgentRegistrationStore {
        let suite = "org.sessiometer.menubar.login-item-tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        return DaemonAgentRegistrationStore(defaults: defaults)
    }

    /// A throwaway `.app`-shaped directory, optionally holding an embedded daemon at the real
    /// `Contents/Helpers/sessiometer` path, for the identity-composition tests. Removed on teardown.
    private func makeTemporaryBundle(helperContents: String?) throws -> URL {
        let bundle = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("Sessiometer-\(UUID().uuidString).app")
        let helper = bundle.appendingPathComponent(DaemonAgentIdentity.helperRelativePath)
        try FileManager.default.createDirectory(
            at: helper.deletingLastPathComponent(), withIntermediateDirectories: true)
        if let helperContents {
            try helperContents.write(to: helper, atomically: true, encoding: .utf8)
        }
        addTeardownBlock { try? FileManager.default.removeItem(at: bundle) }
        return bundle
    }

    @discardableResult
    private func makeModel(
        appStatus: LoginItemStatus = .notRegistered,
        daemonAgentStatus: LoginItemStatus = .notFound,
        cliManagedAgentPresent: Bool = false,
        daemonLockHeld: Bool = false,
        daemonComesUpOnRegister: Bool = false
    ) -> (model: LoginItemModel, fake: FakeLoginItemService) {
        let fake = FakeLoginItemService(
            appStatus: appStatus,
            daemonAgentStatus: daemonAgentStatus,
            cliManagedAgentPresent: cliManagedAgentPresent,
            daemonLockHeld: daemonLockHeld)
        fake.daemonComesUpOnRegister = daemonComesUpOnRegister
        // Tiny liveness timings (issue #745) so the post-register lock poll runs in ~ms: the timeout-path test
        // (no daemon comes up) resolves fast instead of stalling the suite on the 8s production window.
        return (LoginItemModel(service: fake,
                               livenessPollInterval: .milliseconds(1),
                               livenessTimeout: .milliseconds(20)),
                fake)
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

    /// Whether a daemon holds the single-instance lock — and how many times that was ASKED.
    ///
    /// The count is the only instrument that can tell the foreign-holder repair path from the lock-polling
    /// one (issue #819), because the two land on the SAME `startPhase`: with another daemon holding the lock,
    /// `daemonBecameLive()` returns true on its first read, so `.idle` results either way. The difference is
    /// whether the liveness wait was ENTERED AT ALL — i.e. whether the model asked a question whose answer it
    /// could not attribute. Same reasoning as `daemonCalls` recording ORDER: the thing that distinguishes
    /// right from wrong here is not the end state, so the end state cannot be the assertion.
    var daemonLockHeld: Bool {
        get {
            lockProbeCount += 1
            return lockHeldStorage
        }
        set { lockHeldStorage = newValue }
    }

    /// How many times `daemonLockHeld` has been read. See its doc comment for why this exists.
    ///
    /// A TEST reading the property counts too: this and `runStateProbeCount` tally EVERY access, including
    /// one made from an `XCTAssert`. So assert a count BEFORE reading the property it counts, or the
    /// assertion perturbs its own instrument. Unlike `daemonCalls`, these are not inert to observation.
    private(set) var lockProbeCount = 0

    /// What launchd reports about OUR agent's job (issue #819). A COMPUTED probe rather than a plain stored
    /// value, so a test can stage the check-then-act window the reconcile's re-probe narrows: with
    /// `agentSpawnsOnNextProbe` set, the FIRST read reports the job not running and every read after reports
    /// it running — and the same transition takes the lock, because one `RunAtLoad` spawn does both. That is
    /// exactly how our agent coming up between two probes looks to the app. Reading it is therefore NOT
    /// side-effect-free while armed; that one-shot transition is the whole point.
    ///
    /// It is the RUN STATE that carries the one-shot, not the lock (which it did before issue #819), because
    /// the run state is what the gate now branches on — arming the lock alone would stage a race the gate no
    /// longer reacts to, and the test would pass without exercising anything.
    var daemonAgentRunState: DaemonAgentRunState {
        get {
            runStateProbeCount += 1
            guard agentSpawnsOnNextProbe else { return runStateStorage }
            // The spawn lands between this probe and the next: report the pre-spawn state, then hold the
            // post-spawn one — running, and holding the lock it just took.
            agentSpawnsOnNextProbe = false
            let before = runStateStorage
            runStateStorage = .running
            lockHeldStorage = true
            return before
        }
        set { runStateStorage = newValue }
    }

    /// Arms the one-shot spawn-mid-window race `daemonAgentRunState` describes; disarms itself on the next read.
    var agentSpawnsOnNextProbe = false

    /// How many times `daemonAgentRunState` has been read. The instrument for gate PRECEDENCE: the two-owner
    /// guard's invariant is that no other signal is consulted BEFORE it, and "the repair did nothing" is
    /// satisfied by a demoted guard too. Zero reads is what says "not consulted at all".
    private(set) var runStateProbeCount = 0

    private var lockHeldStorage: Bool
    private var runStateStorage: DaemonAgentRunState

    /// The status `registerApp()` lands on when it does not throw (default `.enabled`; set `.requiresApproval`).
    var appRegisterResult: LoginItemStatus = .enabled
    /// The status `registerDaemonAgent()` lands on when it does not throw.
    var daemonRegisterResult: LoginItemStatus = .enabled
    /// Whether a successful `registerDaemonAgent()` simulates the plist's `RunAtLoad` bringing the daemon up (it
    /// then holds the single-instance lock, so `daemonLockHeld` flips true). False models the #745 case: register
    /// succeeds but the daemon never starts, so `daemonLockHeld` stays false and the liveness wait times out.
    var daemonComesUpOnRegister = false

    var appRegisterError: Error?
    var appUnregisterError: Error?
    var daemonRegisterError: Error?
    var daemonUnregisterError: Error?

    private(set) var registerAppCount = 0
    private(set) var unregisterAppCount = 0
    private(set) var registerDaemonCount = 0
    private(set) var unregisterDaemonCount = 0
    private(set) var openSettingsCount = 0

    /// Every daemon-agent registration call, in the order it arrived — the order-sensitive evidence the
    /// unregister-before-register requirement needs (issue #788 / AC3), which call counts alone cannot express:
    /// "both were called" is exactly what the WRONG order also satisfies.
    private(set) var daemonCalls: [DaemonAgentCall] = []

    init(
        appStatus: LoginItemStatus,
        daemonAgentStatus: LoginItemStatus,
        cliManagedAgentPresent: Bool,
        daemonLockHeld: Bool = false,
        daemonAgentRunState: DaemonAgentRunState = .notRunning
    ) {
        self.appStatus = appStatus
        self.daemonAgentStatus = daemonAgentStatus
        self.cliManagedAgentPresent = cliManagedAgentPresent
        self.lockHeldStorage = daemonLockHeld
        self.runStateStorage = daemonAgentRunState
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
        daemonCalls.append(.register)
        if let daemonRegisterError { throw daemonRegisterError }
        daemonAgentStatus = daemonRegisterResult
        // Simulate the plist's `RunAtLoad`: a register that "takes" brings the daemon up, which then holds the
        // single-instance lock and makes our job running. When false, the daemon never appears — the #745
        // silent-start failure mode.
        //
        // The `lockHeldStorage` guard models the stand-down (issue #742 / #819): if ANOTHER daemon already
        // holds the single-instance lock, our freshly-spawned one exits 0 rather than fighting for it, and
        // the conditional KeepAlive does not restart it — so the job ends up NOT running and the lock stays
        // that other daemon's. Without the guard the fake could reach a state the product cannot (two live
        // daemons, one lock), and a test staged on it would prove nothing.
        if daemonComesUpOnRegister && !lockHeldStorage {
            lockHeldStorage = true
            runStateStorage = .running
        }
    }

    func unregisterDaemonAgent() throws {
        unregisterDaemonCount += 1
        daemonCalls.append(.unregister)
        if let daemonUnregisterError { throw daemonUnregisterError }
        daemonAgentStatus = .notRegistered
        // Unloading the launchd job stops the daemon THAT JOB was running — and only that one. If our job was
        // not running, whatever holds the lock is somebody else's process and unloading ours does not touch it
        // (issue #819). Modelling that faithfully is what lets a test tell the foreign-holder path apart from
        // the old any-provenance one: a fake that cleared the lock unconditionally would show a foreign daemon
        // being killed by our unregister, which is precisely what does NOT happen.
        if runStateStorage == .running {
            runStateStorage = .notRunning
            lockHeldStorage = false
        }
    }

    func openLoginItemsSettings() { openSettingsCount += 1 }
}

/// One daemon-agent registration call, recorded in order by `FakeLoginItemService` (issue #788).
private enum DaemonAgentCall: String, Equatable {
    case unregister
    case register
}

/// A stand-in for an `SMAppService` registration error (denied / not permitted) — its exact reason is irrelevant
/// to the model, which only routes a throw into `.failed` with a redacted message.
private enum FakeLoginItemError: Error {
    case denied
}
