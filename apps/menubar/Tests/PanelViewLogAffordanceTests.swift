// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The `View log` affordance's conformance + behaviour gate (issue #776).
//
// WHAT THIS DEFENDS. The ratified mock (`design/menubar-preview.html`) specifies a `View log` action in
// exactly two panel states — daemon-starting and crash-looping — and it was never built; the deferral note
// that tracked it pointed at issues #169/#171, both since delivered, so the spec sat orphaned with nothing
// watching it. This suite is what makes the mock's contract executable rather than a thing to remember.
//
// THE MOCK IS THE ORACLE, AND ONLY FOR WHAT IT AUTHORS. It fixes PLACEMENT (inside `msg-actions`, after the
// message copy, and in no other state), LABEL (`View log`), and PER-STATE STYLE (`.btn.link` alone in
// starting; `.btn` beside a sibling in crash-looping). It is deliberately SILENT on what a click DOES —
// that is umbrella decision D3 (open the log in Console.app via `NSWorkspace`), asserted here as its own
// pure-decision test rather than looked for in the reference.
//
// ONE PLACE THE MOCK IS KNOWN-STALE, and this suite must not "fix" it. The crash-looping frames also show a
// `Restart…` button. Measured evidence (`docs/findings/0777-manual-restart-under-conditional-keepalive.md`)
// showed a manual kickstart mid-throttle costs an EXTRA respawn cycle and blocks the caller for seconds —
// it lengthens the outage launchd is already ending on its own — so it is DROPPED, and issue #856 removes
// it from the mock. `testCrashLoopingOffersViewLogAndNoOtherAction` pins that absence deliberately: if a
// future change adds a second action to this card, this suite should go red and the fixer should come read
// the finding first.
//
// THE ROUTE. In-bundle, headless, no windowserver and no TCC — issue #749 (PR #771) measured `ImageRenderer`
// rasterizing inside this `TEST_HOST: ""` bundle, and issue #758 built the in-process accessibility tree
// (`PanelA11y` / `A11yNode`, `PanelAccessibilityTreeTests`) this file reuses rather than re-derives. Both
// halves are needed and neither substitutes for the other: the tree carries label / role / enablement /
// ORDER, which is placement; only a raster can see whether the two states' styles actually differ.
//
// PROVING THE GATE CAN FAIL (the issue #748 CONSTRAINT-A convention, and issue #437's expensive precedent —
// three render bugs misread five times as a design failure, which a golden blessed then would have
// DEFENDED). Every verdict in this file is computed by ONE function, and every axis of that function is
// exercised by a MUTATION canary fed through the SAME function the real assertion calls. A canary that
// exercised a parallel copy of the logic would prove nothing about the predicate actually in use.
//
// THE ABSENCE TRAP. "No `View log` in this state" is evidence only if the tree is non-empty and the query
// ran — an activation failure yields an empty tree that satisfies every absence check perfectly. So the
// conformance verdict treats a MISSING message anchor as its own failure case (`.noAnchor`), and the
// state-sweep pins a known-present element in the same dump. Same discipline as `PanelAccessibilityTreeTests`.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

// MARK: - The conformance predicate every assertion AND every canary routes through

/// The mock's contract for the `View log` action, as a verdict over a rendered accessibility tree.
///
/// Single-sourced deliberately (see the file header): this is the only place a conformance verdict is
/// computed, so the canaries below prove the real assertions' predicate rather than a look-alike.
enum ViewLogConformance {

    enum Verdict: Equatable {
        /// The action is present, correctly labelled, enabled, and sits after the message copy.
        case conforms
        /// No activatable element at all — the state offers nothing.
        case missing
        /// An action exists but does not carry the mock's label. Carries what WAS found, so a failure
        /// message can show the drift rather than merely assert it.
        case misLabelled(found: [String])
        /// The action renders BEFORE the message copy — the mock puts `msg-actions` after `msg-sub`.
        case misPlaced
        /// Present and correctly labelled, but not activatable. R10's "never a dead button" in its most
        /// literal form.
        case disabled
        /// The message anchor is absent, so this dump cannot support ANY verdict — the tree is empty or
        /// filtered, not clean. Distinct from `.missing` on purpose: one is a finding, this is a void.
        case noAnchor
    }

    /// Judge one tree against the mock's contract.
    ///
    /// - Parameters:
    ///   - tree: the flattened accessibility tree of a rendered panel or card.
    ///   - expectedLabel: the mock's label for the action.
    ///   - anchorText: a substring of the message copy this card must render, which doubles as the
    ///     absence-evidence anchor and as the placement reference the action must follow.
    static func verdict(tree: [A11yNode], expectedLabel: String, anchorText: String) -> Verdict {
        guard let anchorIndex = tree.firstIndex(where: { $0.text.contains(anchorText) }) else {
            return .noAnchor
        }
        let buttons = tree.enumerated().filter { $0.element.role == "AXButton" }
        guard !buttons.isEmpty else { return .missing }
        guard let match = buttons.first(where: { $0.element.text == expectedLabel }) else {
            return .misLabelled(found: buttons.map(\.element.text))
        }
        guard match.element.enabled else { return .disabled }
        guard match.offset > anchorIndex else { return .misPlaced }
        return .conforms
    }
}

/// The mock's PER-STATE style divergence, as a verdict over two rasters.
///
/// The tree cannot see this: `.btn` and `.btn.link` publish the identical role, label and enablement. What
/// separates them in the reference is DRAWN — `.btn` carries a hairline border and a `--btn-bg` fill that
/// `.btn.link` (border:0, background:transparent) does not. So the measurable, falsifiable form of "the two
/// states are styled differently, and in the direction the mock specifies" is: the bordered render carries
/// materially MORE ink than the borderless one.
///
/// This is deliberately a DIRECTIONAL check, not a pixel oracle. It cannot certify "matches the mock's
/// border radius"; the committed panel goldens plus `build-comparison.py` and a human eye remain the
/// fidelity path (`PanelGoldenParityTests` makes the same distinction for the panel as a whole). What it
/// CAN certify is the thing this item is most at risk of getting wrong: quietly normalizing the two states
/// to one style because the divergence looks like an inconsistency.
enum ViewLogStyleDivergence {

    /// The minimum ink gap that counts as a real difference rather than antialiasing. The measured gap is
    /// far larger (see the assertion's message); this floor sits low so a future control-metric revision
    /// cannot redden the suite for a change that regressed nothing.
    static let minimumInkGap = 0.005

    static func honorsMockDivergence(linkInk: Double, borderedInk: Double) -> Bool {
        borderedInk - linkInk >= minimumInkGap
    }
}

// MARK: - Rendering the card in isolation

@MainActor
private enum ViewLogCardHarness {

    static let cardSize = CGSize(width: 320, height: 150)

    /// One `DaemonLogCard`, wired with the minimum the view actually reads: a store (for the banner's
    /// account count), the #776 availability probe, and the accent pin.
    ///
    /// `.tint` is pinned for the same reason `PanelRenderHarness` pins it (issue #754): `Color.accentColor`
    /// resolves through a build setting present on the APP target only, so an unpinned render in THIS bundle
    /// would take the operator's macOS system accent — a machine-dependent hue, and this file compares two
    /// renders by ink.
    static func card(state: ConnectionState,
                     style: DaemonLogCard.ActionStyle,
                     logPath: String?,
                     scheme: ColorScheme = .dark) -> some View {
        DaemonLogCard(state: state, actionStyle: style)
            .padding(14)
            .environmentObject(WatchStatusStore.preview(state: state, rows: [],
                                                        nextSwap: nil, generatedAt: nil))
            .environment(\.daemonLogProbe, .fixed(logPath))
            .environment(\.colorScheme, scheme)
            .tint(Color.panelAccent)
    }

    /// Ink coverage of a rendered card, or `nil` if it did not rasterize at all.
    ///
    /// `BarGlyphRenderer.inkCoverage` scores departure from the CORNER pixel, so a blank raster and a
    /// uniform fill both collapse to 0 — which is exactly why a caller must reject 0 rather than read it as
    /// "a little ink" (the degenerate-pass guard `ImageRendererHeadlessProbeTests` documents).
    static func inkCoverage(of view: some View, size: CGSize = cardSize) -> Double? {
        let renderer = ImageRenderer(content: view.frame(width: size.width, height: size.height))
        renderer.scale = 2
        guard let cg = renderer.cgImage else { return nil }
        return BarGlyphRenderer.inkCoverage(NSBitmapImageRep(cgImage: cg))
    }
}

// MARK: - Tests

@MainActor
final class PanelViewLogAffordanceTests: XCTestCase {

    private let label = StatusPanelFormat.viewLogButtonTitle
    private static let panelSize = CGSize(width: 380, height: 420)

    /// A log path for the "there IS something to view" arm. A literal, never a resolved one — see
    /// `PanelRenderHarness.fixtureLogPath` for why nothing here may touch the real filesystem.
    private let seededLogPath = PanelRenderHarness.fixtureLogPath

    private func fixture(named name: String) throws -> PanelRenderFixture {
        try XCTUnwrap(PanelRenderHarness.fixtures(now: Int64(Date().timeIntervalSince1970))
            .first { $0.name == name }, "no panel render fixture named '\(name)'")
    }

    // MARK: - AC-1 / AC-2: the two states the mock specifies

    /// AC-1. `daemon-starting` renders the action, after the message copy, correctly labelled and live.
    func testDaemonStartingRendersTheMockSpecifiedViewLogAction() throws {
        let tree = PanelA11y.panelTree(fixture: try fixture(named: "starting"))
        XCTAssertEqual(
            ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: "Starting"),
            .conforms,
            """
            the daemon-starting card does not conform to the mock's `View log` action \
            (menubar-preview.html, data-frame="daemon-starting-light" / "-dark"). Tree:
            \(tree.map(\.description).joined(separator: "\n"))
            """)
    }

    /// AC-2. `crash-looping` renders the same action, on the same terms.
    func testCrashLoopingRendersTheMockSpecifiedViewLogAction() throws {
        let tree = PanelA11y.panelTree(fixture: try fixture(named: "crash-looping"))
        XCTAssertEqual(
            ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: "crash-looping"),
            .conforms,
            """
            the crash-looping card does not conform to the mock's `View log` action \
            (menubar-preview.html, data-frame="crash-looping-light" / "-dark"). Tree:
            \(tree.map(\.description).joined(separator: "\n"))
            """)
    }

    /// The `Restart…` pin (see the file header). The mock shows two buttons here; exactly ONE is built, on
    /// measured evidence that the other is harmful. Asserting the count — rather than only asserting that
    /// `View log` is present — is what turns a future "let's add Restart for conformance" into a red test
    /// that sends the author to `docs/findings/0777-manual-restart-under-conditional-keepalive.md`.
    func testCrashLoopingOffersViewLogAndNoOtherAction() throws {
        let tree = PanelA11y.panelTree(fixture: try fixture(named: "crash-looping"))
        XCTAssertEqual(tree.interactiveNodes.map(\.text), [label], """
            the crash-looping card's action set changed. The mock ALSO shows `Restart…`, and it is dropped \
            deliberately: issue #777 measured that a manual kickstart mid-throttle costs an extra respawn \
            cycle and blocks the caller for seconds, lengthening the outage launchd is already ending. If \
            you are adding it back, read docs/findings/0777-manual-restart-under-conditional-keepalive.md \
            first; if you are adding something else, update this pin deliberately.
            """)
    }

    // MARK: - AC-3: no other state offers it

    /// The mock's entire action inventory is three labels across three frames; `View log` belongs to two
    /// states and no more. Sweeping EVERY fixture is what makes that a contract rather than a spot check.
    ///
    /// TWO SEPARATE VACUITIES, and the second is the one that nearly got through. The first is the familiar
    /// empty-tree void — an activation failure yields no nodes, which satisfies any absence check
    /// perfectly; `assertKnownPresent`-style anchoring is what closes it. The second is subtler and
    /// specific to this affordance: the availability probe is seeded PER FIXTURE, and every non-offering
    /// fixture is seeded `nil`. Judged against its own seed, each of those states would render no button
    /// EVEN IF the view offered it unconditionally — so the sweep would be measuring the fixture catalog,
    /// not the view. This is why the probe is forced ON for every fixture below: with a log always
    /// available, an absent button can only mean the VIEW declined to render it, which is the actual claim.
    func testNoOtherPanelStateOffersViewLog() {
        let offering = Set(["starting", "crash-looping"])
        for fixture in PanelA11y.allFixtures where !offering.contains(fixture.name) {
            // Force availability: absence must be the view's decision, never the fixture's seed.
            let tree = PanelA11y.panelTree(fixture: fixture,
                                           daemonLogOverride: .fixed(seededLogPath))
            // Absence evidence: a tree that is empty because activation failed would pass the assertion
            // below vacuously. Every fixture publishes the app name in its header, so that is the anchor.
            XCTAssertNotNil(tree.firstContaining("Sessiometer"), """
                ABSENCE EVIDENCE VOID for fixture '\(fixture.name)': the known-present anchor 'Sessiometer' \
                is missing from a tree of \(tree.count) node(s), so no "View log is absent" verdict from \
                this dump means anything. Check PanelA11y.activate().
                """)
            XCTAssertNil(tree.interactiveNodes.first { $0.text == label }, """
                fixture '\(fixture.name)' offers a `View log` action even though the mock specifies it in \
                exactly two states — daemon-starting and crash-looping — and in no other. Note the log was \
                forced AVAILABLE here, so this is the view's own state-gating failing, not a seeding \
                accident.
                """)
        }
    }

    /// The canary for the sweep above, and the reason it is not decorative. It renders a state the mock
    /// gives NO action (`connecting`) through the same tree helper with the probe forced on, and asserts
    /// the sweep's own predicate would catch a button there. Without this, the sweep could be passing
    /// because the probe silently withheld every button rather than because the view gates by state — the
    /// exact vacuity the sweep's docstring describes.
    func testTheStateSweepCanSeeAButtonWhenOneIsPresent() throws {
        // A state the mock DOES give the action, forced available, judged by the sweep's predicate.
        let offering = PanelA11y.panelTree(fixture: try fixture(named: "starting"),
                                           daemonLogOverride: .fixed(seededLogPath))
        XCTAssertNotNil(offering.interactiveNodes.first { $0.text == label }, """
            the sweep's predicate cannot see a `View log` button that IS present, so every "absent" verdict \
            it returns for the other 15 fixtures is vacuous.
            """)
        // And the control: a non-offering state, same forcing, must genuinely have none.
        let notOffering = PanelA11y.panelTree(fixture: try fixture(named: "connecting"),
                                              daemonLogOverride: .fixed(seededLogPath))
        XCTAssertNotNil(notOffering.firstContaining("Sessiometer"),
                        "the connecting tree is empty — this control proves nothing")
        XCTAssertNil(notOffering.interactiveNodes.first { $0.text == label },
                     "`connecting` offers a `View log` action; the mock gives it none")
    }

    // MARK: - AC-5 / R10: never a dead button

    /// The honest-affordance rule (issue #169), in the state it is most likely to be violated: the daemon
    /// has not written a log line yet. The card must degrade to exactly the inert banner it was before.
    func testTheActionIsAbsentWhenThereIsNoLogToView() {
        for (state, anchor) in [(ConnectionState.starting, "Starting"),
                                (ConnectionState.crashLooping, "crash-looping")] {
            let tree = PanelA11y.tree(
                for: ViewLogCardHarness.card(state: state, style: .link, logPath: nil),
                size: ViewLogCardHarness.cardSize)
            XCTAssertEqual(
                ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: anchor), .missing,
                """
                \(state) rendered a `View log` action with NO log to view. That is the dead button R10 \
                forbids: a click would open nothing. Tree:
                \(tree.map(\.description).joined(separator: "\n"))
                """)
        }
    }

    /// The other half of the same rule: where the action IS offered, it is offered live. `.disabled` is a
    /// distinct verdict from `.missing` precisely so a greyed-out-but-present button cannot pass as honest.
    func testTheOfferedActionIsEnabledRatherThanGreyedOut() {
        let tree = PanelA11y.tree(
            for: ViewLogCardHarness.card(state: .starting, style: .link, logPath: seededLogPath),
            size: ViewLogCardHarness.cardSize)
        let button = tree.interactiveNodes.first { $0.text == label }
        XCTAssertNotNil(button, "the seeded card published no `View log` button at all")
        XCTAssertEqual(button?.enabled, true,
                       "`View log` is published disabled — a visible action that cannot be activated")
    }

    // MARK: - AC-6: accessibility

    /// VoiceOver reaches it, names it, and can activate it. What an in-process tree CAN attest: the element
    /// is in the tree (not `accessibilityHidden`), publishes the activatable `AXButton` role — which is what
    /// puts a control in the keyboard loop rather than leaving it a mouse-only decoration — is enabled, and
    /// carries a label naming the action. What it canNOT attest is VoiceOver's own rotor traversal and
    /// actual speech; that residue stays the manual pre-release checklist in `design/README.md`, exactly as
    /// issue #758 scoped it.
    func testTheActionExposesAnAccessibilityLabelAndAnActivatableRole() {
        let tree = PanelA11y.tree(
            for: ViewLogCardHarness.card(state: .crashLooping, style: .bordered, logPath: seededLogPath),
            size: ViewLogCardHarness.cardSize)
        let button = tree.first { $0.role == "AXButton" && $0.text == label }
        XCTAssertNotNil(button, """
            no AXButton labelled '\(label)'. Either the control is absent, hidden from the tree, or it \
            publishes a non-activatable role — in the last case it is not keyboard-reachable. Tree:
            \(tree.map(\.description).joined(separator: "\n"))
            """)
        XCTAssertEqual(button?.enabled, true, "the control is not activatable")
        // Not colour-alone (WCAG 1.4.1): the affordance is carried by a TEXT label plus a glyph, and the
        // link treatment differs from the bordered one by border and fill, not by hue. The label being the
        // published name is the machine-checkable half of that.
        XCTAssertEqual(button?.text, label, "the spoken name must be the action's name")
    }

    // MARK: - AC-7: per-state style conformance (the raster half)

    /// The mock styles this action DIFFERENTLY in its two states — `.btn.link` (borderless, transparent) in
    /// daemon-starting, `.btn` (hairline border over `--btn-bg`) in crash-looping. Honour that divergence;
    /// it is the reference's intent, not an inconsistency to normalize away.
    func testThePerStateStylesDivergeInTheDirectionTheMockSpecifies() throws {
        let link = try XCTUnwrap(
            ViewLogCardHarness.inkCoverage(of: ViewLogCardHarness.card(
                state: .starting, style: .link, logPath: seededLogPath)),
            "the .btn.link card did not rasterize")
        let bordered = try XCTUnwrap(
            ViewLogCardHarness.inkCoverage(of: ViewLogCardHarness.card(
                state: .starting, style: .bordered, logPath: seededLogPath)),
            "the .btn card did not rasterize")

        // Degenerate-pass guard: `inkCoverage` scores departure from the corner pixel, so a blank raster
        // scores 0 and would satisfy any "they differ" test paired with a non-zero sibling.
        XCTAssertGreaterThan(link, 0.01, "the borderless card rendered blank — nothing drew")
        XCTAssertGreaterThan(bordered, 0.01, "the bordered card rendered blank — nothing drew")

        XCTAssertTrue(
            ViewLogStyleDivergence.honorsMockDivergence(linkInk: link, borderedInk: bordered),
            """
            the two states' `View log` styles do not diverge as the mock specifies. `.btn` (crash-looping) \
            draws a hairline border over a `--btn-bg` fill that `.btn.link` (daemon-starting) explicitly \
            drops (border:0, background:transparent), so the bordered render must carry more ink. \
            Measured: link=\(link), bordered=\(bordered), gap=\(bordered - link), required \
            ≥ \(ViewLogStyleDivergence.minimumInkGap). A gap at or below zero means the two states were \
            normalized to one style.
            """)
    }

    /// The other half, and the one the raster CANNOT give: which state gets which treatment.
    ///
    /// The ink assertion above proves the two `ActionStyle` cases look different; it is completely blind to
    /// them being ASSIGNED the wrong way round. Crash-looping wrongly styled `.link` is a direct AC-2
    /// violation that a "the styles diverge" check passes without complaint — which is exactly why the
    /// mapping is a pure function (`ActionStyle.forState`) and is pinned here against the mock's frames:
    /// `class="btn link"` at daemon-starting, `class="btn"` at crash-looping.
    func testEachStateTakesTheTreatmentTheMockGivesIt() {
        XCTAssertEqual(DaemonLogCard.ActionStyle.forState(.starting), .link,
                       "the mock renders daemon-starting's action `class=\"btn link\"` (borderless)")
        XCTAssertEqual(DaemonLogCard.ActionStyle.forState(.crashLooping), .bordered,
                       "the mock renders crash-looping's action `class=\"btn\"` (bordered)")
    }

    // MARK: - AC-4 / D3: what a click does (the mock is silent; D3 is the oracle)

    /// D3: open the log in Console.app. The decision is split from its `NSWorkspace` shell so it is testable
    /// without a workspace — the same split issue #764 made for the status item's own imperative shell.
    func testAClickPlansToOpenTheLogInConsole() {
        let console = URL(fileURLWithPath: "/System/Applications/Utilities/Console.app")
        let plan = DaemonLogOpen.plan(logPath: "/tmp/x/sessiometer.log", consoleApp: console)
        XCTAssertEqual(plan, .console(app: console,
                                      log: URL(fileURLWithPath: "/tmp/x/sessiometer.log")))
    }

    /// Console.app is a system app, so this arm should never fire in practice — but "never in practice" is
    /// not a reason to drop the click on the floor. Falling back to the system's handler still yields a VIEW
    /// of the log, which keeps the affordance honest under the one failure it can hit.
    func testAnUnresolvableConsoleStillOpensTheLogRatherThanDoingNothing() {
        let plan = DaemonLogOpen.plan(logPath: "/tmp/x/sessiometer.log", consoleApp: nil)
        XCTAssertEqual(plan, .systemDefault(log: URL(fileURLWithPath: "/tmp/x/sessiometer.log")))
    }

    // MARK: - The path contract (mirrors src/paths.rs)

    /// The LABEL, pinned to the mock's literal string rather than to the constant the panel renders.
    ///
    /// Everything else in this file compares `StatusPanelFormat.viewLogButtonTitle` against itself — the
    /// rendered button's text against the same constant that produced it. That is a self-referential
    /// oracle: it survives ANY rename, so on its own it cannot attest the mock's label at all. The mock is
    /// the oracle for this string (`menubar-preview.html`, `>View log</button>` ×4), so the literal belongs
    /// here, exactly as the daemon's path contract is pinned as a literal below.
    func testTheLabelIsTheMocksLabel() {
        XCTAssertEqual(StatusPanelFormat.viewLogButtonTitle, "View log", """
            the `View log` button title drifted from the design mock, which is the oracle for it \
            (menubar-preview.html renders `>View log</button>` in all four affected frames). Change the \
            mock first if the label is meant to change.
            """)
    }

    /// `logPath(home:)` must equal `src/paths.rs::logs_dir()`'s macOS branch (`apple_logs_dir_from`) joined
    /// with the filename `src/observability.rs` documents. Asserted as a literal for a known home, so a
    /// drift in either file surfaces here rather than as a button that opens nothing.
    func testTheLogPathMirrorsTheDaemonsOwnContract() {
        XCTAssertEqual(DaemonLogLocation.logPath(home: "/Users/x"),
                       "/Users/x/Library/Logs/sessiometer/sessiometer.log")
        XCTAssertEqual(DaemonLogLocation.logTail, "Library/Logs/sessiometer/sessiometer.log")
    }

    /// Both arms of the existence gate, driven through the injected predicate so no test creates, deletes,
    /// or depends on a real file.
    func testExistenceGatesThePathRatherThanTheOtherWayAround() {
        let expected = "/Users/x/Library/Logs/sessiometer/sessiometer.log"
        XCTAssertEqual(DaemonLogLocation.existingLogPath(home: "/Users/x", fileExists: { $0 == expected }),
                       expected)
        XCTAssertNil(DaemonLogLocation.existingLogPath(home: "/Users/x", fileExists: { _ in false }),
                     "a missing log must resolve to nil — that nil is what withholds the button")
    }

    // MARK: - Rig canaries: every verdict axis proven able to FAIL, by mutation
    //
    // Each feeds a deliberately-broken view through the SAME `ViewLogConformance.verdict` the real
    // assertions call. See the file header for why a parallel copy would prove nothing.

    /// CONTROL: the real card, seeded, must come back `.conforms` — so the mutations below are testing the
    /// mutation and not some ambient breakage.
    func testTheConformancePredicateIsSilentOnTheRealCard() {
        let tree = PanelA11y.tree(
            for: ViewLogCardHarness.card(state: .starting, style: .link, logPath: seededLogPath),
            size: ViewLogCardHarness.cardSize)
        XCTAssertEqual(
            ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: "Starting"), .conforms)
    }

    /// MUTATION — LABEL: a plausible synonym must be caught. Without this, `.conforms` could be satisfied by
    /// any button at all and the "label matches the mock" claim would be empty.
    func testTheConformancePredicateFiresOnAMisLabelledAction() {
        struct Mutated: View {
            var body: some View {
                VStack(alignment: .leading) {
                    Text("Starting…")
                    Button("Show log") {}
                }
            }
        }
        let tree = PanelA11y.tree(for: Mutated(), size: ViewLogCardHarness.cardSize)
        XCTAssertEqual(
            ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: "Starting"),
            .misLabelled(found: ["Show log"]), """
            the predicate accepted a differently-labelled action, so it cannot attest the mock's label. \
            Tree: \(tree.map(\.description).joined(separator: "\n"))
            """)
    }

    /// MUTATION — PLACEMENT: the mock puts `msg-actions` AFTER `msg-sub`. An action rendered above the
    /// message reads as a header control, not as the message's response.
    func testTheConformancePredicateFiresOnAMisPlacedAction() {
        struct Mutated: View {
            var body: some View {
                VStack(alignment: .leading) {
                    Button(StatusPanelFormat.viewLogButtonTitle) {}
                    Text("Starting…")
                }
            }
        }
        let tree = PanelA11y.tree(for: Mutated(), size: ViewLogCardHarness.cardSize)
        XCTAssertEqual(
            ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: "Starting"),
            .misPlaced, """
            the predicate accepted an action rendered before the message copy, so its placement claim is \
            empty. Tree: \(tree.map(\.description).joined(separator: "\n"))
            """)
    }

    /// MUTATION — PRESENCE: a card with no action must read `.missing`. This is the axis
    /// `testTheActionIsAbsentWhenThereIsNoLogToView` depends on, so it needs its own proof.
    func testTheConformancePredicateFiresOnAMissingAction() {
        struct Mutated: View {
            var body: some View { Text("Starting…") }
        }
        let tree = PanelA11y.tree(for: Mutated(), size: ViewLogCardHarness.cardSize)
        XCTAssertEqual(
            ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: "Starting"), .missing)
    }

    /// MUTATION — DEAD BUTTON: present and correctly labelled but disabled. R10's literal case, and the one
    /// a label-only check would wave through.
    func testTheConformancePredicateFiresOnADisabledAction() {
        struct Mutated: View {
            var body: some View {
                VStack(alignment: .leading) {
                    Text("Starting…")
                    Button(StatusPanelFormat.viewLogButtonTitle) {}.disabled(true)
                }
            }
        }
        let tree = PanelA11y.tree(for: Mutated(), size: ViewLogCardHarness.cardSize)
        XCTAssertEqual(
            ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: "Starting"),
            .disabled, """
            the predicate accepted a disabled action as conforming — R10's "never a dead button" would be \
            unenforced. Tree: \(tree.map(\.description).joined(separator: "\n"))
            """)
    }

    /// MUTATION — ABSENCE-EVIDENCE VOID: a tree with no message anchor must refuse to render a verdict at
    /// all. Without this, an empty tree would score `.missing` and every absence claim in this file would
    /// be vacuous rather than evidenced — the exact failure issue #761's spike lost a round to.
    func testTheConformancePredicateRefusesToJudgeAnAnchorlessTree() {
        struct Unrelated: View {
            var body: some View { Text("nothing to do with the panel") }
        }
        let tree = PanelA11y.tree(for: Unrelated(), size: ViewLogCardHarness.cardSize)
        XCTAssertEqual(
            ViewLogConformance.verdict(tree: tree, expectedLabel: label, anchorText: "Starting"),
            .noAnchor, """
            the predicate returned a substantive verdict for a tree that does not contain the card at all, \
            so its absence verdicts prove nothing.
            """)
    }

    /// MUTATION — STYLE: two identical inks (what normalizing the two states to one treatment would
    /// produce) must NOT satisfy the divergence predicate.
    func testTheStyleDivergencePredicateFiresWhenBothStatesAreStyledAlike() {
        XCTAssertFalse(ViewLogStyleDivergence.honorsMockDivergence(linkInk: 0.12, borderedInk: 0.12),
                       "identical renders passed the divergence check — it cannot detect normalization")
        XCTAssertFalse(ViewLogStyleDivergence.honorsMockDivergence(linkInk: 0.20, borderedInk: 0.12),
                       "an INVERTED gap passed — the check must be directional, not merely 'they differ'")
        XCTAssertTrue(ViewLogStyleDivergence.honorsMockDivergence(linkInk: 0.12, borderedInk: 0.20),
                      "a real gap in the mock's direction must pass, or the predicate rejects everything")
    }
}
#endif
