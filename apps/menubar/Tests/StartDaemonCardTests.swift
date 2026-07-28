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

    // MARK: - issue #779 — the failed state OFFERS the log instead of sending the operator to find it
    //
    // Issue #745 shipped "The daemon was registered but didn’t start. Check Console for details." That second
    // sentence was correct only for as long as the app had no path of its own; issue #776 built one. These
    // pin the replacement AND its honest-affordance gate, on the RENDERED surface, for the same reason the
    // rest of this file works there: a model that holds the right state and a card that draws nothing is the
    // defect class this suite exists for.

    /// AC-1 + AC-2. With a log to open, the manual instruction is GONE and a live `View log` stands in its
    /// place — while the diagnostic sentence, which is issue #745's honest statement of what happened, stays.
    ///
    /// Both halves are asserted together deliberately: "the button appeared" would pass while the stale
    /// instruction sat right beside it (the exact staleness issue #779 exists to remove), and "the
    /// instruction is gone" would pass on a card that dropped the whole line and offered nothing.
    func testTheNotStartedFailureOffersViewLogInPlaceOfTheConsoleInstruction() async {
        let model = await notStartedFailure()
        let nodes = tree(for: model, logPath: Self.seededLogPath)
        assertKnownPresent(nodes, Self.anchor, "the #779 affordance check")

        let button = nodes.interactiveNodes.first { $0.text == Self.viewLog }
        XCTAssertNotNil(button, """
            the not-started failure offers no `View log` action even with a log to open. Tree:
            \(nodes.map(\.description).joined(separator: "\n"))
            """)
        XCTAssertEqual(button?.enabled, true, "a visible action that cannot be activated is R10's dead button")

        XCTAssertNotNil(nodes.firstContaining(Self.diagnostic),
                        "issue #745's honest statement of WHAT happened must survive — the affordance "
                        + "replaces the navigation instruction, never the diagnosis")
        XCTAssertNil(nodes.firstContaining(Self.manualInstruction), """
            the card still tells the operator to go find the log by hand while ALSO offering to open it. \
            That instruction is what issue #779 replaces. Tree:
            \(nodes.map(\.description).joined(separator: "\n"))
            """)
    }

    /// AC-4 — the honest-affordance rule (issue #169) in the state most likely to trip it. "Registered but
    /// never started" is precisely the case where the daemon may have written nothing at all, so the no-log
    /// arm here is the common one, not a theoretical edge.
    ///
    /// And the fall-back is the SHIPPED sentence, byte-for-byte: the rule withholds the button, never the
    /// information, so an operator on a machine with no log is exactly as well-informed as before issue #779.
    func testTheNotStartedFailureFallsBackToTheInstructionWhenThereIsNoLog() async {
        let model = await notStartedFailure()
        let nodes = tree(for: model, logPath: nil)
        assertKnownPresent(nodes, Self.anchor, "the #779 no-log fallback check")

        XCTAssertNil(nodes.interactiveNodes.first { $0.text == Self.viewLog }, """
            a `View log` button was offered with NO log to open — a click would open nothing, which is the \
            dead affordance issue #169 forbids. Tree:
            \(nodes.map(\.description).joined(separator: "\n"))
            """)
        XCTAssertNotNil(nodes.firstContaining("\(Self.diagnostic) \(Self.manualInstruction)"),
                        "with no button to offer, the card owes the operator the manual route — the whole "
                        + "issue #745 sentence, unchanged")
    }

    /// The affordance is gated on the FAILURE, not merely on the log's existence. A register that THREW never
    /// spawned anything, so the daemon's log cannot mention it — offering to open it would be a dead button
    /// of a subtler kind: live and clickable, pointing at a file with nothing to say.
    ///
    /// The log is forced AVAILABLE here, so an absent button can only be the card's own gating. Seeded `nil`
    /// this test would pass on a card that offered the action unconditionally.
    func testARegisterErrorIsNotOfferedTheLogEvenWhenOneExists() async {
        let (model, _) = await failedLaunchRepair()
        let nodes = tree(for: model, logPath: Self.seededLogPath)
        assertKnownPresent(nodes, Self.anchor, "the #779 register-error gating check")

        XCTAssertNil(nodes.interactiveNodes.first { $0.text == Self.viewLog }, """
            a register error was offered `View log`. Nothing was spawned, so the daemon wrote no line about \
            this failure and the OS message is already on the card. Note the log was forced AVAILABLE, so \
            this is the card's state gating, not a seeding accident. Tree:
            \(nodes.map(\.description).joined(separator: "\n"))
            """)
    }

    /// AC-3 — "reuse whatever handler issue #776 establishes; do not fork a second Console-opening path."
    ///
    /// Asserted as IDENTITY between the two surfaces rather than as a property of this one: a check that the
    /// button here merely *has* the right label would pass on a hand-rolled copy that drifts the moment the
    /// mock's label, glyph or help text changes. Every user-visible attribute the tree can see must match the
    /// starting card's, because both render the one `ViewLogButton`.
    func testTheOfferedActionIsTheSameAffordanceTheStartingCardRenders() async {
        let model = await notStartedFailure()
        let mine = tree(for: model, logPath: Self.seededLogPath)
            .interactiveNodes.first { $0.text == Self.viewLog }
        XCTAssertNotNil(mine, "setup: the not-running card published no `View log` button")

        // Issue #776's own surface, same seeded path, hosted the same way.
        let starting = PanelA11y.tree(
            for: DaemonLogCard(state: .starting, actionStyle: .link)
                .environmentObject(WatchStatusStore.preview(state: .starting, rows: [],
                                                            nextSwap: nil, generatedAt: nil))
                .environment(\.daemonLogProbe, .fixed(Self.seededLogPath)),
            size: Self.cardSize)
            .interactiveNodes.first { $0.text == Self.viewLog }
        XCTAssertNotNil(starting, "setup: issue #776's starting card published no `View log` button")

        XCTAssertEqual(mine?.role, starting?.role, "the two must publish the same role")
        XCTAssertEqual(mine?.help, starting?.help,
                       "the help text carries the D3 destination (Console.app) — a second definition that "
                       + "drifted here would be exactly the fork issue #779 forbids")
        XCTAssertEqual(mine?.enabled, starting?.enabled)
    }

    /// AC-5 — redaction (issue #15). The not-started reason is a FIXED pair of literals that interpolates
    /// nothing, so there is no seam through which a credential could reach the card. Asserted as exact
    /// equality rather than a "does not contain a secret" scan: a containment check can only rule out the
    /// secrets it thinks to name, whereas equality rules out anything at all having been added.
    func testTheNotStartedFailureLineIsExactlyItsFixedCopyAndNothingElse() async {
        let model = await notStartedFailure()

        let offered = tree(for: model, logPath: Self.seededLogPath)
        assertKnownPresent(offered, Self.anchor, "the #779 redaction check (log offered)")
        XCTAssertNotNil(offered.first { $0.text == Self.diagnostic },
                        "the offered-affordance line must be the diagnostic and nothing more")

        let bare = tree(for: model, logPath: nil)
        assertKnownPresent(bare, Self.anchor, "the #779 redaction check (no log)")
        XCTAssertNotNil(bare.first { $0.text == "\(Self.diagnostic) \(Self.manualInstruction)" },
                        "and the fallback line must be the two fixed sentences and nothing more")
    }

    /// CANARY for the two absence assertions above. A replica that renders `View log` with NO regard for the
    /// probe — the shape a careless implementation takes — fed through the SAME query
    /// `testTheNotStartedFailureFallsBackToTheInstructionWhenThereIsNoLog` uses. It must be caught, or that
    /// test's "no button was offered" verdict proves nothing about the gate it claims to defend.
    func testTheNoLogAssertionCatchesAnUngatedButton() async {
        let model = await notStartedFailure()
        let ungated = PanelA11y.tree(for: UngatedViewLogCardReplica().environmentObject(model),
                                     size: Self.cardSize)
        assertKnownPresent(ungated, Self.anchor, "the ungated-button canary")
        XCTAssertNotNil(ungated.interactiveNodes.first { $0.text == Self.viewLog },
                        "the canary is not mutated: an ungated `View log` must be VISIBLE to this suite's "
                        + "query, or the no-log absence assertion is vacuous")

        // …and the real card, same model, same query, same absent probe, withholds it. The pair is the gate.
        XCTAssertNil(tree(for: model, logPath: nil).interactiveNodes.first { $0.text == Self.viewLog })
    }

    // MARK: - Helpers

    /// The `View log` label, the two halves of the #745 copy, and a stand-in log path — all DERIVED from the
    /// shipped constants rather than transcribed, so a reworded sentence reddens these tests instead of
    /// silently making them assert about a string the product no longer shows.
    private static let viewLog = StatusPanelFormat.viewLogButtonTitle
    private static let diagnostic = LoginItemModel.StartFailureReason.notStartedDiagnostic
    private static let manualInstruction = LoginItemModel.StartFailureReason.manualLogInstruction

    /// A FIXED stand-in log path, never a resolved one — same reasoning as `PanelRenderHarness.fixtureLogPath`
    /// (#776): nothing renders the path, only the button's presence, so a literal is a complete stand-in and
    /// it keeps the suite independent of whether the machine running it has ever started the daemon.
    private static let seededLogPath = "/Users/sessiometer/Library/Logs/sessiometer/sessiometer.log"

    /// A model driven into a genuine `.notStarted` failure through the real Start action: a registrable agent
    /// and a free lock (so `canStartDaemon` is true and the press is not a no-op), a register that SUCCEEDS,
    /// and a daemon that never comes up — so the bounded liveness wait elapses. Never a hand-set phase: the
    /// state has to be one the product can actually reach.
    private func notStartedFailure() async -> LoginItemModel {
        let fake = FakeLoginItemService(appStatus: .enabled, daemonAgentStatus: .notRegistered,
                                        cliManagedAgentPresent: false, daemonLockHeld: false)
        fake.daemonComesUpOnRegister = false  // the #745 case: register takes, nothing spawns
        let model = LoginItemModel(service: fake,
                                   registrationStore: ephemeralRegistrationStore(),
                                   agentIdentity: { "build-1" },
                                   livenessPollInterval: .milliseconds(1),
                                   livenessTimeout: .milliseconds(20))
        await model.startDaemon()
        guard case .failed(.notStarted, .operatorStart) = model.startPhase else {
            XCTFail("setup: expected an operator-attributed not-started failure, got \(model.startPhase)")
            return model
        }
        return model
    }

    /// The real card, hosted with `model` as its `@EnvironmentObject`. `panelScale` is left at its default
    /// (1.0, the `.large` Dynamic Type class) — the same factor the committed goldens render at.
    private func tree(for model: LoginItemModel) -> [A11yNode] {
        PanelA11y.tree(for: StartDaemonCard().environmentObject(model), size: Self.cardSize)
    }

    /// The same card with issue #776's availability seam pinned (issue #779). Separate from `tree(for:)` above
    /// so the pre-#779 tests keep hosting the card exactly as they did — with the environment default,
    /// `.unavailable`, which is what the shipped app falls back to and what renders no button.
    private func tree(for model: LoginItemModel, logPath: String?) -> [A11yNode] {
        PanelA11y.tree(for: StartDaemonCard()
                            .environmentObject(model)
                            .environment(\.daemonLogProbe, .fixed(logPath)),
                       size: Self.cardSize)
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
                    // `offeringLogAffordance: false` — the pre-#820 shape this replica models predates issue
                    // #779's affordance entirely, so it renders the reason's un-degraded copy. What the
                    // canary watches is whether the reason is DRAWN, which that argument cannot affect.
                    Text(StatusPanelFormat.startDaemonFailureText(
                        reason: reason.text(offeringLogAffordance: false), origin: origin))
                }
                Text(StatusPanelFormat.startDaemonHint)
            }
        }
    }
}

/// The not-running card with issue #779's affordance UNGATED — a `View log` offered on any failure, with no
/// regard for whether there is a log to open. This is the shape a careless implementation takes, and it is
/// what `testTheNoLogAssertionCatchesAnUngatedButton` requires this suite's absence query to catch.
///
/// A copy rather than a flag on the real card, for the same reason as the replica above: a production toggle
/// for "render the dead-button way" would be a code path shipping to operators for no product reason.
private struct UngatedViewLogCardReplica: View {
    @EnvironmentObject private var loginItem: LoginItemModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            BannerView(banner: StatusPanelFormat.banner(for: .notRunning, accountCount: 0))
            if case .failed(let reason, let origin) = loginItem.startPhase {
                Text(StatusPanelFormat.startDaemonFailureText(
                    reason: reason.text(offeringLogAffordance: true), origin: origin))
                Button(StatusPanelFormat.viewLogButtonTitle) {}
            }
        }
    }
}
#endif
