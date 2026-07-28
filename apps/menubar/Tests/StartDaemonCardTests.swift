// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The not-running card's RENDER-REACHABILITY gate (issue #820).
//
// WHAT THIS PINS THAT NOTHING ELSE DOES. `LoginItemModelTests` proves the model still HOLDS a failed
// repair's reason after a daemon takes the single-instance lock. That is necessary and not sufficient: the
// defect issue #820 fixes was never in the model — `startPhase` was always correct — it was that
// `StartDaemonCard` nested its reason inside `if loginItem.canStartDaemon`, so a reason the model held
// perfectly well was drawn nowhere. A phase nobody draws is not a reason the operator can reach, and no
// model test can tell the difference. So this file asserts on the RENDERED surface.
//
// THE INSTRUMENT. `PanelA11y.tree(for:size:)` (`PanelAccessibilityTreeTests`) is written surface-agnostic —
// "host any SwiftUI view offscreen and return its flattened accessibility tree" — so the card is hosted
// directly, with a real `LoginItemModel` driven into the state under test through the real
// `LoginItemService` seam. Nothing here stubs the model, the phase, or the card. This is a deliberate reuse
// of that harness rather than a second one: it already carries the activation recipe, and re-deriving it
// here is exactly the drift issue #504 paid for.
//
// AND THE ABSENCE TRAP COMES WITH IT. An empty tree — activation silently failing — satisfies "the button
// is withheld" perfectly, and would satisfy "the reason is absent" too if this file ever asserted that. So
// every assertion below, presence and absence alike, pins the always-present "Daemon not running" banner in
// the SAME dump. Same discipline, same reason (`PanelAccessibilityTreeTests` file header).
//
// PROVING THE GATE CAN FAIL. `testTheCoupledRenderIsCaughtByThisSuitesOwnPredicate` feeds a deliberately
// RE-COUPLED replica of the card — the pre-issue-#820 shape, reason nested inside the `canStartDaemon`
// branch — through the same query the real assertions use, and requires it to come out reason-less. Without
// that canary, a gate authored against already-fixed code is one nobody has watched fail.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

@MainActor
final class StartDaemonCardTests: XCTestCase {

    /// Wide enough that the reason wraps rather than being dropped, tall enough for banner + button + hint.
    private static let cardSize = CGSize(width: 380, height: 300)

    /// The banner is unconditional in this card, so it is the anchor every assertion here is judged against.
    private static let anchor = "Daemon not running"

    /// The liveness window the two MID-FLIGHT tests hold `.registering` open with. It is a CEILING that is
    /// never reached — both release the wait themselves — so its only job is to be comfortably longer than
    /// the work done while the beat is held: `settle` (≤2 s) plus `PanelA11y.tree`'s own poll-until-populated
    /// deadline (≤2 s). At the 5 s these started on, a CI host slow enough to spend that budget would time the
    /// wait out mid-render and flip the phase to `.failed` under the assertion — a flake that would read as
    /// "the card shows the wrong beat" rather than "the box was loaded".
    private static let heldBeatTimeout = Duration.seconds(30)

    // MARK: - AC1 / AC4 — the reason outlives `canStartDaemon`

    /// THE REGRESSION, at the surface that had it. A launch-time repair fails; a daemon THEN takes the lock,
    /// so `canStartDaemon` goes false and the Start affordance is withheld. The reason must still be drawn.
    ///
    /// Ordering is the point: staged the other way round (lock first) the repair would never run at all, and
    /// staged without the lock the old coupled render passes too. Only fail-then-lock separates them.
    func testAFailedLaunchRepairsReasonIsRenderedAfterADaemonTakesTheLock() async {
        let (model, fake) = await failedLaunchRepair()
        fake.daemonLockHeld = true
        XCTAssertFalse(model.canStartDaemon, "setup: the affordance is withheld, which is what used to hide "
                       + "the reason with it")

        let nodes = tree(for: model)
        assertKnownPresent(nodes, Self.anchor, "the withheld-affordance reason check")

        XCTAssertNotNil(nodes.firstContaining(StatusPanelFormat.startDaemonRepairAttribution),
                        "the launch repair's reason must survive the affordance it was nested inside — it is "
                        + "the operator's ONLY recovery signal, and nothing retries the repair behind it")
        XCTAssertNil(nodes.firstContaining(StatusPanelFormat.startDaemonButtonTitle),
                     "honest degradation is unchanged: the button still appears only where it can act")
    }

    /// The control the assertion above needs to be evidence rather than a coincidence: the SAME failed
    /// repair with the lock free renders the reason AND the button. Without this, a card that had simply
    /// stopped rendering the button entirely would satisfy the test above.
    func testTheSameFailureStillRendersBothReasonAndButtonWhileTheAffordanceCanAct() async {
        let (model, _) = await failedLaunchRepair()
        XCTAssertTrue(model.canStartDaemon, "setup: the lock is free, so Start is offered")

        let nodes = tree(for: model)
        assertKnownPresent(nodes, Self.anchor, "the offered-affordance control")

        XCTAssertNotNil(nodes.firstContaining(StatusPanelFormat.startDaemonRepairAttribution))
        XCTAssertNotNil(nodes.firstContaining(StatusPanelFormat.startDaemonButtonTitle),
                        "the button is still offered here — the reason's render is what was decoupled, not it")
    }

    /// The resting state is untouched: no failure, no reason line — so decoupling the render did not turn
    /// the card into one that always shows an error strip. (This is also what keeps the committed panel
    /// goldens valid: the render harness seeds exactly this state.)
    func testAnIdleCardRendersNoReasonAtAll() {
        // The store is injected even though this test never reaches a path that writes one: the model's
        // default is the operator's REAL `UserDefaults`, so leaving it out would make hermeticity a property
        // of what this test happens not to call rather than of how it is wired.
        let fake = FakeLoginItemService(appStatus: .enabled, daemonAgentStatus: .notRegistered,
                                        cliManagedAgentPresent: false, daemonLockHeld: false)
        let model = LoginItemModel(service: fake, registrationStore: ephemeralRegistrationStore())

        let nodes = tree(for: model)
        assertKnownPresent(nodes, Self.anchor, "the idle-card check")

        XCTAssertNil(nodes.firstContaining(StatusPanelFormat.startDaemonRepairAttribution),
                     "a resting card invents no failure")
        XCTAssertNil(nodes.firstContaining(StatusPanelFormat.startDaemonRepairPendingText),
                     "nor an in-flight beat")
        XCTAssertNotNil(nodes.firstContaining(StatusPanelFormat.startDaemonButtonTitle))
        // NOT asserted here: the absence of the warning GLYPH. `Label(_:systemImage:)` publishes only its
        // text, so an SF Symbol name never enters the tree and `firstContaining("exclamation")` is nil in
        // the FAILED state too — an assertion that cannot fail, reading as coverage it does not provide.
        // The reason's own text, asserted above, is the honest proxy for "no failure is being shown".
    }

    // MARK: - AC2 — the copy distinguishes the two writers

    /// The PENDING beat's other half, pinned at the render layer for the same reason the failure line is.
    /// `StatusPanelFormatTests` proves `startDaemonPendingText(for:)` returns different strings; only this
    /// proves the CARD asks it. Without it, reverting the button's one line to the bare
    /// `startDaemonOperatorPendingText` constant — the pre-issue-#820 shape — leaves the whole suite green
    /// while the launch repair once again reads exactly like a press the operator made.
    ///
    /// Staged on a repair caught MID-FLIGHT: the beat is transient, so the repair is started concurrently and
    /// held open by its post-register liveness wait (no daemon comes up), rendered while it waits, and then
    /// released by letting a daemon take the lock.
    func testALaunchRepairInFlightRendersItsOwnPendingBeatNotTheOperators() async {
        let fake = FakeLoginItemService(appStatus: .enabled, daemonAgentStatus: .enabled,
                                        cliManagedAgentPresent: false, daemonLockHeld: false,
                                        daemonAgentRunState: .notRunning)
        fake.daemonComesUpOnRegister = false  // the liveness wait is what holds `.registering` open
        let store = ephemeralRegistrationStore()
        store.lastRegisteredIdentity = "build-1"
        let model = LoginItemModel(service: fake,
                                   registrationStore: store,
                                   agentIdentity: { "build-2" },
                                   livenessPollInterval: .milliseconds(5),
                                   livenessTimeout: Self.heldBeatTimeout)

        async let repair: Void = model.reconcileDaemonAgentRegistration()
        guard await settle(model, until: { if case .registering(.launchRepair) = $0 { return true }
                                           return false }) else {
            fake.daemonLockHeld = true
            await repair
            return XCTFail("setup: the repair never held its in-flight beat; got \(model.startPhase)")
        }

        let nodes = tree(for: model)
        assertKnownPresent(nodes, Self.anchor, "the in-flight pending-beat check")
        XCTAssertNotNil(nodes.firstContaining(StatusPanelFormat.startDaemonRepairPendingText),
                        "a repair no press stands behind must not borrow the button's own wording")
        XCTAssertNil(nodes.firstContaining(StatusPanelFormat.startDaemonOperatorPendingText),
                     "\"Starting…\" here would credit the operator with a press they never made — the exact "
                     + "indistinguishability issue #820 exists to remove")

        // Release the wait: a daemon takes the lock, so the liveness poll resolves on its next read.
        fake.daemonLockHeld = true
        await repair
    }

    /// The control for the assertion above: an OPERATOR-initiated start in flight renders the button's own
    /// wording. Without this, a card that had simply started saying "Repairing…" for every writer would
    /// satisfy the test above — the pair is what pins a DISTINCTION rather than a relabelling.
    func testAnOperatorInitiatedStartInFlightRendersTheButtonsOwnWording() async {
        let fake = FakeLoginItemService(appStatus: .enabled, daemonAgentStatus: .notRegistered,
                                        cliManagedAgentPresent: false, daemonLockHeld: false,
                                        daemonAgentRunState: .notRunning)
        fake.daemonComesUpOnRegister = false
        let model = LoginItemModel(service: fake,
                                   registrationStore: ephemeralRegistrationStore(),
                                   agentIdentity: { "build-1" },
                                   livenessPollInterval: .milliseconds(5),
                                   livenessTimeout: Self.heldBeatTimeout)

        async let start: Void = model.startDaemon()
        guard await settle(model, until: { if case .registering(.operatorStart) = $0 { return true }
                                           return false }) else {
            fake.daemonLockHeld = true
            await start
            return XCTFail("setup: the start never held its in-flight beat; got \(model.startPhase)")
        }

        let nodes = tree(for: model)
        assertKnownPresent(nodes, Self.anchor, "the operator in-flight control")
        XCTAssertNotNil(nodes.firstContaining(StatusPanelFormat.startDaemonOperatorPendingText),
                        "the operator's own press keeps issue #170's shipped wording")
        XCTAssertNil(nodes.firstContaining(StatusPanelFormat.startDaemonRepairPendingText),
                     "nothing is being repaired — this is the button doing what it was asked to do")

        fake.daemonLockHeld = true
        await start
    }

    /// An OPERATOR-initiated Start that fails renders its reason with NO repair attribution — the operator
    /// pressed the button a moment ago, so telling them they pressed it is noise. Paired with the launch
    /// repair above, this is the distinction AC2 asks for, asserted on the rendered strings.
    func testAnOperatorInitiatedFailureCarriesNoRepairAttribution() async {
        let fake = FakeLoginItemService(appStatus: .enabled, daemonAgentStatus: .notRegistered,
                                        cliManagedAgentPresent: false, daemonLockHeld: false)
        fake.daemonRegisterError = FakeLoginItemError.denied
        let model = LoginItemModel(service: fake,
                                   registrationStore: ephemeralRegistrationStore(),
                                   agentIdentity: { "build-1" },
                                   livenessPollInterval: .milliseconds(1),
                                   livenessTimeout: .milliseconds(20))
        await model.startDaemon()
        guard case .failed(_, .operatorStart) = model.startPhase else {
            return XCTFail("setup: expected an operator-attributed failure, got \(model.startPhase)")
        }

        let nodes = tree(for: model)
        assertKnownPresent(nodes, Self.anchor, "the operator-attribution check")

        XCTAssertNil(nodes.firstContaining(StatusPanelFormat.startDaemonRepairAttribution),
                     "a press the operator just made is not an \"automatic repair\" — the two must not read "
                     + "the same, in either direction")
    }

    // MARK: - Canary: the predicate is proven able to FAIL

    /// MUTATION. A replica of the card in its PRE-issue-#820 shape — the reason nested inside the
    /// `canStartDaemon` branch — fed through the same query the assertions above use. It must come out
    /// reason-less, which is what proves those assertions would catch a regression to it rather than
    /// passing on any card at all.
    func testTheCoupledRenderIsCaughtByThisSuitesOwnPredicate() async {
        let (model, fake) = await failedLaunchRepair()
        fake.daemonLockHeld = true

        let coupled = PanelA11y.tree(for: CoupledStartDaemonCardReplica().environmentObject(model),
                                     size: Self.cardSize)
        assertKnownPresent(coupled, Self.anchor, "the coupled-render canary")
        XCTAssertNil(coupled.firstContaining(StatusPanelFormat.startDaemonRepairAttribution),
                     "the canary is not mutated: the pre-#820 shape must LOSE the reason here, or this "
                     + "suite's assertions prove nothing about the coupling they exist to catch")

        // …and the real card, same model, same query, keeps it. The pair is the gate.
        let real = tree(for: model)
        XCTAssertNotNil(real.firstContaining(StatusPanelFormat.startDaemonRepairAttribution))
    }

    // MARK: - Helpers

    /// The real card, hosted with `model` as its `@EnvironmentObject`. `panelScale` is left at its default
    /// (1.0, the `.large` Dynamic Type class) — the same factor the committed goldens render at.
    private func tree(for model: LoginItemModel) -> [A11yNode] {
        PanelA11y.tree(for: StartDaemonCard().environmentObject(model), size: Self.cardSize)
    }

    /// A model driven into a genuinely-failed LAUNCH REPAIR through the real reconcile: a changed identity
    /// (so the detector fires), a free lock and a stopped job (so it proceeds past gate 4), and a register
    /// that throws. Never a hand-set phase — the state has to be one the product can actually reach.
    private func failedLaunchRepair() async -> (LoginItemModel, FakeLoginItemService) {
        let fake = FakeLoginItemService(appStatus: .enabled, daemonAgentStatus: .enabled,
                                        cliManagedAgentPresent: false, daemonLockHeld: false,
                                        daemonAgentRunState: .notRunning)
        fake.daemonRegisterError = FakeLoginItemError.denied
        let store = ephemeralRegistrationStore()
        store.lastRegisteredIdentity = "build-1"
        let model = LoginItemModel(service: fake,
                                   registrationStore: store,
                                   agentIdentity: { "build-2" },
                                   livenessPollInterval: .milliseconds(1),
                                   livenessTimeout: .milliseconds(20))
        await model.reconcileDaemonAgentRegistration()
        return (model, fake)
    }

    /// Spin the cooperative pool until `model.startPhase` satisfies `until`, or a bounded number of turns
    /// elapses. Both the model and this test are `@MainActor`, so a concurrently-started register only makes
    /// progress while this yields — hence sleeping rather than busy-looping. Returns whether it arrived, so a
    /// caller reports a MISSED beat as a setup failure instead of asserting against a state it never reached.
    private func settle(_ model: LoginItemModel,
                        until predicate: (LoginItemModel.StartPhase) -> Bool) async -> Bool {
        for _ in 0..<400 {
            if predicate(model.startPhase) { return true }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return false
    }

    /// A per-test volatile `UserDefaults` domain, so a recorded identity never touches the operator's real
    /// defaults or leaks into another test. `LoginItemModelTests` carries the same three lines against its
    /// OWN suite prefix, deliberately: the prefix is what makes a domain that somehow outlives its teardown
    /// traceable to the suite that wrote it, so the two are siblings rather than a copy. (The absence-trap
    /// guard, which has no such per-suite half, is shared instead — see `assertKnownPresent`.)
    private func ephemeralRegistrationStore() -> DaemonAgentRegistrationStore {
        let suite = "org.sessiometer.menubar.start-card-tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        return DaemonAgentRegistrationStore(defaults: defaults)
    }
}

// MARK: - The mutation

/// `StartDaemonCard` as it stood BEFORE issue #820: the `.failed` reason nested inside the `canStartDaemon`
/// branch. Deliberately a copy rather than a flag on the real card — a production toggle for "render the old
/// way" would be a code path shipping to operators for no product reason. Its ONLY job is to be the thing the
/// canary above watches fail; if the real card is restructured, this replica does not need to follow, because
/// what it models is a shape that no longer exists.
private struct CoupledStartDaemonCardReplica: View {
    @EnvironmentObject private var loginItem: LoginItemModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            BannerView(banner: StatusPanelFormat.banner(for: .notRunning, accountCount: 0))
            if loginItem.canStartDaemon {
                Text(StatusPanelFormat.startDaemonButtonTitle)
                if case .failed(let reason, let origin) = loginItem.startPhase {
                    Text(StatusPanelFormat.startDaemonFailureText(reason: reason, origin: origin))
                }
                Text(StatusPanelFormat.startDaemonHint)
            }
        }
    }
}
#endif
