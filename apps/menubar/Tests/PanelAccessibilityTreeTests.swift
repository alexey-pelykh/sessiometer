// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The panel's ACCESSIBILITY-TREE gate (issue #758) — the a11y guard that was missing.
//
// WHAT WAS MISSING. Accessibility *labels* are deliberate and unit-tested as STRINGS
// (`StatusPanelFormatTests` `rowAccessibilityLabel` / blind / cornered / swap-callout). Nothing tested
// anything about a11y that is not a string: whether an element is REACHABLE at all, what ROLE it
// publishes, whether it is ENABLED, what ORDER the elements come in, and — the one issue #758 calls out
// as most likely to be faked — whether the elements marked `accessibilityHidden(true)` genuinely LEAVE
// the tree rather than merely carrying the modifier. A correct label on an unreachable or mis-typed
// element is not accessible.
//
// ─────────────────────────────────────────────────────────────────────────────────────────────────────
// AC-1: THE TOOLING QUESTION, MEASURED. Issue #758 made everything conditional on "can
// `performAccessibilityAudit` run, given it needs a macOS 14 host while the app targets 13.0
// (`project.yml:12`)?" Measured on 2026-07-28, that framing turns out to name the wrong blocker three
// times over:
//
//   1. THE HOST WAS NEVER THE PROBLEM. The dev machine is macOS 26.5.2 / Xcode 26.6, and CI's
//      `macos-latest` resolved to macOS 26.4 / Xcode 26.5 on issue #761's measured run (30350129101);
//      issue #749's ran on a `macos-26` runner likewise. Both are far above 14.
//
//   2. THE TEST BUNDLE *CAN* CARRY A HIGHER FLOOR THAN THE APP. Measured directly: adding
//      `deploymentTarget: "14.0"` to the `MenubarTests` target and regenerating yields
//      `MACOSX_DEPLOYMENT_TARGET = 14.0` for `MenubarTests` while the `Menubar` app target still reads
//      `13.0` (`xcodebuild -showBuildSettings`, both targets, same generated project). XcodeGen's
//      per-target `deploymentTarget` overrides the project-level `options.deploymentTarget` for that
//      target only. So raising the app's shipping floor is NOT required — the thing #758 correctly
//      refused to trade away was never on the table.
//
//   3. …AND WE DO NOT NEED IT ANYWAY. `performAccessibilityAudit` is a method on `XCUIApplication`, so it
//      needs a UI-TEST bundle, which drags in the whole cost surface issue #761 measured: its own scheme
//      (never the required `swift` job's), no run at all on a locked developer session, and — since
//      `Sources/` still has zero `accessibilityIdentifier`s — queries copy-coupled to rendered prose.
//      The accessibility tree is reachable IN-PROCESS from this existing headless bundle instead, with
//      no XCUITest, no scheme risk, no flake, and no TCC grant.
//
// So the automated branch is a GO, by a better route than the issue anticipated, and the app's 13.0
// deployment target is untouched by this item. What the in-process tree canNOT reach — VoiceOver's own
// rotor and focus traversal, and actual speech — is exactly the residue that stays manual, and
// `design/README.md` § VoiceOver pre-release checklist is where it lives.
//
// THE ACTIVATION RECIPE, bisected rather than guessed. A bare `NSHostingView` publishes NO children —
// AppKit builds the SwiftUI accessibility tree lazily, and the naive read returns an empty tree that
// would make every absence assertion below pass vacuously. A 2×2×2 bisect over {`finishLaunching`} ×
// {window} × {AX self-request} isolated the requirement to exactly TWO steps, both necessary, neither
// sufficient:
//
//   • `NSApplication.shared.finishLaunching()`, and
//   • one AX request against our OWN pid (`AXUIElementCreateApplication(getpid())` + a set of
//     `AXEnhancedUserInterface`).
//
// A window is NOT needed. The AX set itself RETURNS `-25208` (`kAXErrorNotImplemented`) and that is
// fine — it is the request reaching the accessibility runtime, not its return value, that switches
// in-process a11y on. `AXIsProcessTrusted()` reports **false** in this bundle, so no Accessibility TCC
// grant is involved; that is what makes the lane CI-safe.
//
// THE ABSENCE TRAP, and why `assertKnownPresent` is on every absence assertion. "Element X is not in the
// tree" is evidence only if the dump is COMPLETE and the query actually ran — a tree that is empty
// because activation silently failed satisfies every "is absent" check perfectly. Issue #761's spike lost
// a full round to the same shape: it published a `grep`-filtered tree as "the entire accessibility tree"
// and concluded a present element was absent. So no absence claim here stands alone; each one pins a
// known-PRESENT element in the SAME dump.
//
// PROVING THE GATE CAN FAIL (issue #748's batch constraint). A gate authored against already-passing
// code can be one that cannot fail — and issue #437 is the local precedent for how expensive that is:
// three render bugs were misread five times as "the DESIGN fails distinctness", and a golden blessed
// then would have DEFENDED them. So every predicate this suite asserts with is exercised by a
// MUTATION-driven canary that feeds a deliberately-broken view through the SAME function the real
// assertion calls — never an inspection-only argument. Same shape as `BarGlyphParityTests`' canaries.
// The canary the #761 spike lacked is specifically the activation one: a rig canary that validates the
// harness but not the QUERY cannot catch a wrong query.
//
// SCOPE. Status panel only. The Settings window is NOT covered — `SettingsView` is deliberately absent
// from this bundle (`project.yml`), compiling it in belongs to issue #762, and extending this gate over
// it is issue #840. `PanelA11y.tree(for:size:)` is written surface-agnostic so #840 is a re-point, not a
// rewrite.
//
// KNOWN-DEFECT PINS. Two real defects surfaced and were FILED rather than fixed here (this is an audit
// item): issue #838 (a decorative Stats icon that leaks into the tree) and issue #839 (non-interactive
// rows publishing `AXUnknown`). Neither is merely tolerated — #838 is pinned as set EQUALITY in
// `knownDecorativeLeaks`, #839 as the `AXUnknown` counts inside the role-histogram pin — so fixing
// either turns this suite RED and tells the fixer to drop the pin, the same discipline issue #830 uses
// against the #759 contrast gate. A pin that merely tolerated the defect would rot into a permanent
// allowance nobody revisits.

#if DEBUG
import AppKit
import ApplicationServices
import SwiftUI
import XCTest

// MARK: - The tree walker

/// One accessibility element, flattened out of the live AppKit tree.
struct A11yNode: CustomStringConvertible {
    let depth: Int
    let role: String
    let label: String
    let value: String
    let identifier: String
    let help: String
    let enabled: Bool

    /// The user-facing string, wherever it lives. SwiftUI publishes a COMBINED `Text` element's string as
    /// `value` while `Button` and friends use `label` — the exact per-element-type split that cost the
    /// issue #761 spike a round when it queried one and concluded the element was absent.
    var text: String { label.isEmpty ? value : label }

    var description: String {
        "\(String(repeating: "  ", count: depth))role=\(role) enabled=\(enabled) "
            + "label='\(label)' value='\(value)' id='\(identifier)'"
    }
}

@MainActor
enum PanelA11y {

    // MARK: Activation

    private static var activated = false

    /// Switch on in-process accessibility. Idempotent; see the file header for the bisect that established
    /// that BOTH steps are required and a window is not.
    static func activate() {
        guard !activated else { return }
        activated = true
        NSApplication.shared.setActivationPolicy(.accessory)
        NSApplication.shared.finishLaunching()
        // The return value is deliberately discarded — it is `kAXErrorNotImplemented` and always will be.
        // The REQUEST is what activates the tree, not its result (file header).
        _ = AXUIElementSetAttributeValue(AXUIElementCreateApplication(getpid()),
                                         "AXEnhancedUserInterface" as CFString,
                                         kCFBooleanTrue)
    }

    // MARK: Walking

    private static func string(_ object: AnyObject, _ selectorName: String) -> String {
        guard let n = object as? NSObject, n.responds(to: NSSelectorFromString(selectorName)) else { return "" }
        return (n.perform(NSSelectorFromString(selectorName))?.takeUnretainedValue() as? String) ?? ""
    }

    private static func children(_ object: AnyObject) -> [Any] {
        guard let n = object as? NSObject,
              n.responds(to: NSSelectorFromString("accessibilityChildren")) else { return [] }
        return (n.perform(NSSelectorFromString("accessibilityChildren"))?.takeUnretainedValue() as? [Any]) ?? []
    }

    private static func walk(_ object: AnyObject, depth: Int, into out: inout [A11yNode]) {
        guard depth < 16 else { return }
        let enabled = ((object as? NSObject)?.value(forKey: "isAccessibilityEnabled") as? Bool) ?? true
        out.append(A11yNode(depth: depth,
                            role: string(object, "accessibilityRole"),
                            label: string(object, "accessibilityLabel"),
                            value: string(object, "accessibilityValue"),
                            identifier: string(object, "accessibilityIdentifier"),
                            help: string(object, "accessibilityHelp"),
                            enabled: enabled))
        for child in children(object) { walk(child as AnyObject, depth: depth + 1, into: &out) }
    }

    /// Host any SwiftUI view offscreen and return its flattened accessibility tree, ROOT INCLUDED at depth 0.
    ///
    /// Surface-agnostic on purpose — issue #840 re-points this at `SettingsView` without touching it.
    ///
    /// Polls rather than sleeping a fixed interval: the tree populates asynchronously after layout, so this
    /// spins the run loop until a child appears or the deadline passes. A timeout is NOT an error here — an
    /// empty tree is a legitimate measurement that `assertKnownPresent` is responsible for rejecting, and
    /// swallowing it into an `XCTFail` inside the walker would hide it from the canary that must observe it.
    static func tree(for view: some View, size: CGSize) -> [A11yNode] {
        activate()
        let host = NSHostingView(rootView: view.frame(width: size.width, height: size.height))
        host.frame = NSRect(origin: .zero, size: size)
        host.layoutSubtreeIfNeeded()

        let deadline = Date().addingTimeInterval(2.0)
        while (host.accessibilityChildren() ?? []).isEmpty, Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }

        var nodes: [A11yNode] = []
        walk(host, depth: 0, into: &nodes)
        return nodes
    }

    /// The status panel for one render fixture, wired through the SAME `statusPanelEnvironment` seam
    /// (issue #504) the app and the golden gate use, so a newly-required environment object breaks this
    /// suite too rather than silently rendering a degraded tree.
    static func panelTree(fixture: PanelRenderFixture, scheme: ColorScheme = .dark) -> [A11yNode] {
        let store = WatchStatusStore.preview(state: fixture.state, rows: fixture.rows,
                                             nextSwap: fixture.nextSwap, generatedAt: fixture.generatedAt,
                                             canonicalScrub: fixture.canonicalScrub,
                                             keychainLocked: fixture.keychainLocked,
                                             systemicRefreshFailure: fixture.systemicRefreshFailure,
                                             systemicRefreshSource: fixture.systemicRefreshSource)
        let stats = fixture.statsWire.map { PanelStatsModel.loadedPreview($0) } ?? PanelStatsModel(client: nil)
        let view = StatusPanelView()
            .statusPanelEnvironment(store: store,
                                    capture: AccountCaptureModel(client: nil),
                                    swap: AccountSwapModel(client: nil),
                                    stats: stats,
                                    loginItem: LoginItemModel(service: A11yProbeLoginItemService()))
            .environment(\.colorScheme, scheme)
            .tint(Color.panelAccent)
        return tree(for: view, size: CGSize(width: 380, height: 420))
    }

    /// Every panel fixture, seeded from the real clock exactly as `PanelRenderHarness` does.
    static var allFixtures: [PanelRenderFixture] {
        PanelRenderHarness.fixtures(now: Int64(Date().timeIntervalSince1970))
    }
}

// MARK: - The predicates every assertion AND every canary routes through
//
// Single-sourced deliberately: a canary that exercises a PARALLEL copy of the logic proves nothing about
// the predicate the real assertion uses. These are the only place a verdict is computed.

extension Array where Element == A11yNode {
    /// Elements published as images. The panel's images are ALL decorative (glyphs, meters, sparklines,
    /// pills) and are meant to be `accessibilityHidden`, so any survivor here is a leak.
    var decorativeImageLeaks: [A11yNode] { filter { $0.role == "AXImage" } }

    /// Elements the accessibility runtime treats as activatable.
    var interactiveNodes: [A11yNode] { filter { $0.role == "AXButton" } }

    /// The first element whose user-facing text contains `needle`, wherever the string lives.
    func firstContaining(_ needle: String) -> A11yNode? {
        first { $0.text.contains(needle) }
    }

    /// The tree's shape as a role histogram, excluding the hosting root — e.g. `"AXButton:5 AXStaticText:3"`.
    ///
    /// Deliberately role-only and string-free. It catches ANY element entering or leaving the tree —
    /// including a decorative **Text**, which `decorativeImageLeaks` structurally cannot see — while
    /// staying immune to the copy-coupling issue #761 warned about, where assertions pinned to rendered
    /// prose break on a wording change that regressed nothing.
    var roleHistogram: String {
        Dictionary(grouping: dropFirst(), by: \.role)
            .mapValues(\.count)
            .sorted { $0.key < $1.key }
            .map { "\($0.key):\($0.value)" }
            .joined(separator: " ")
    }
}

// MARK: - Tests

@MainActor
final class PanelAccessibilityTreeTests: XCTestCase {

    private static let panelSize = CGSize(width: 380, height: 420)

    // MARK: Absence-trap guard
    //
    // Every absence assertion below calls this against the SAME dump it is judging. An activation failure
    // yields an empty tree, which would satisfy "no decorative element leaked" perfectly — this is what
    // makes the absence claims evidence rather than vacuity (file header).

    private func assertKnownPresent(_ nodes: [A11yNode], _ needle: String,
                                    _ context: String,
                                    file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertNotNil(
            nodes.firstContaining(needle), """
            ABSENCE EVIDENCE VOID for \(context): the known-present anchor '\(needle)' is missing from a \
            tree of \(nodes.count) node(s). Any "element is absent" verdict from this dump is vacuous — \
            the tree is empty or filtered, not clean. Check PanelA11y.activate() first.
            """, file: file, line: line)
    }

    // MARK: - Rig canaries — every predicate proven able to FAIL, by mutation
    //
    // Each pair is one MUTATED view (predicate must fire) and one CONTROL view differing only in the
    // property under test (predicate must stay silent). Both go through the production predicate.

    /// The canary the issue #761 spike did not have: it validated its shell harness but not its QUERY, and
    /// so reported a present element as absent with full confidence. This one fails if activation regresses
    /// — which is the single failure that would silently void every absence assertion in this file.
    func testTheWalkerActuallyReachesAKnownPresentElement() {
        struct V: View {
            var body: some View { Text("CANARY_PRESENT_ANCHOR") }
        }
        let nodes = PanelA11y.tree(for: V(), size: CGSize(width: 200, height: 80))
        XCTAssertGreaterThan(nodes.count, 1,
                             "the walker returned only the root — in-process a11y did not activate")
        XCTAssertNotNil(nodes.firstContaining("CANARY_PRESENT_ANCHOR"),
                        "the walker cannot see a plain Text element; every absence claim here would be void")
    }

    /// MUTATION: an image WITHOUT `accessibilityHidden` must be caught by `decorativeImageLeaks`.
    func testTheDecorativeLeakPredicateFiresOnAnUnhiddenImage() {
        struct Mutated: View {
            var body: some View {
                VStack {
                    Text("ANCHOR")
                    Image(systemName: "chart.line.uptrend.xyaxis")  // deliberately NOT hidden
                }
            }
        }
        let nodes = PanelA11y.tree(for: Mutated(), size: CGSize(width: 200, height: 120))
        assertKnownPresent(nodes, "ANCHOR", "the decorative-leak canary")
        XCTAssertFalse(nodes.decorativeImageLeaks.isEmpty, """
            the leak predicate did NOT fire on an image with no accessibilityHidden — it cannot detect the \
            defect it exists to detect, so its green verdict on the real panel means nothing.
            """)
    }

    /// CONTROL for the above: the SAME view with the modifier applied must come back clean, so the canary
    /// is testing `accessibilityHidden` and not merely "images exist".
    func testTheDecorativeLeakPredicateIsSilentOnAHiddenImage() {
        struct Control: View {
            var body: some View {
                VStack {
                    Text("ANCHOR")
                    Image(systemName: "chart.line.uptrend.xyaxis").accessibilityHidden(true)
                }
            }
        }
        let nodes = PanelA11y.tree(for: Control(), size: CGSize(width: 200, height: 120))
        assertKnownPresent(nodes, "ANCHOR", "the decorative-leak control")
        XCTAssertEqual(nodes.decorativeImageLeaks.count, 0,
                       "accessibilityHidden(true) did not remove the image: \(nodes.decorativeImageLeaks)")
    }

    /// MUTATION: a non-interactive element must NOT satisfy the interactive-role predicate — otherwise
    /// "the switch chip is a button" would be true of any element at all.
    func testTheInteractiveRolePredicateDistinguishesButtonsFromText() {
        struct TextOnly: View {
            var body: some View { Text("NOT_A_BUTTON") }
        }
        struct RealButton: View {
            var body: some View { Button("IS_A_BUTTON") {} }
        }
        let textNodes = PanelA11y.tree(for: TextOnly(), size: CGSize(width: 200, height: 80))
        assertKnownPresent(textNodes, "NOT_A_BUTTON", "the interactive-role canary")
        XCTAssertTrue(textNodes.interactiveNodes.isEmpty,
                      "a plain Text was classified interactive — the role predicate cannot discriminate")

        let buttonNodes = PanelA11y.tree(for: RealButton(), size: CGSize(width: 200, height: 80))
        XCTAssertEqual(buttonNodes.interactiveNodes.count, 1,
                       "a real Button was not classified interactive: \(buttonNodes)")
    }

    /// MUTATION: `.disabled(true)` must be observable, so the blocked-row assertion below is not asserting
    /// a constant.
    func testTheEnabledFlagTracksDisabledState() {
        struct Subject: View {
            let off: Bool
            var body: some View { Button("SUBJECT_BUTTON") {}.disabled(off) }
        }
        let enabled = PanelA11y.tree(for: Subject(off: false), size: CGSize(width: 200, height: 80))
        let disabled = PanelA11y.tree(for: Subject(off: true), size: CGSize(width: 200, height: 80))

        XCTAssertEqual(enabled.interactiveNodes.first?.enabled, true, "an enabled Button reported disabled")
        XCTAssertEqual(disabled.interactiveNodes.first?.enabled, false, """
            a .disabled(true) Button still reported enabled — the flag is a constant here, so the \
            blocked-row assertion would pass no matter what the panel does.
            """)
    }

    // MARK: - AC-4: interactive elements expose correct traits

    /// The issue's headline question: issue #448 made the persistent switch chip interactive, so it must
    /// read as a BUTTON and not as static text.
    func testThePersistentSwitchChipIsExposedAsAButton() throws {
        let fixture = try XCTUnwrap(PanelA11y.allFixtures.first { $0.name == "healthy" })
        let nodes = PanelA11y.panelTree(fixture: fixture)
        assertKnownPresent(nodes, "Sessiometer.", "the switch-chip trait check")

        let chip = try XCTUnwrap(nodes.firstContaining("Switch to Temp"), """
            the #448 persistent switch chip is not in the tree at all: \(nodes)
            """)
        XCTAssertEqual(chip.role, "AXButton", """
            the switch chip publishes '\(chip.role)', not AXButton — issue #448 made it a real button, so \
            VoiceOver announcing it as static text would hide the panel's primary action.
            """)
        XCTAssertTrue(chip.enabled, "the switch chip is present but disabled in the healthy fixture")
    }

    /// The tabs, the switchable rows, and the account-less states' primary actions.
    func testEveryInteractiveSurfaceIsExposedAsAButton() throws {
        let fixtures = PanelA11y.allFixtures
        let healthy = try XCTUnwrap(fixtures.first { $0.name == "healthy" })
        let healthyNodes = PanelA11y.panelTree(fixture: healthy)
        assertKnownPresent(healthyNodes, "Sessiometer.", "the interactive-surface check")

        for label in ["Status", "Stats"] {
            let tab = try XCTUnwrap(healthyNodes.first { $0.label == label }, "tab '\(label)' missing")
            XCTAssertEqual(tab.role, "AXButton", "the \(label) tab publishes \(tab.role)")
        }
        // The two SWITCHABLE rows (the active row is deliberately not interactive — see issue #839).
        for account in ["Personal,", "Temp,"] {
            let row = try XCTUnwrap(healthyNodes.firstContaining(account), "row \(account) missing")
            XCTAssertEqual(row.role, "AXButton", "the switchable row \(account) publishes \(row.role)")
        }

        let notRunning = try XCTUnwrap(fixtures.first { $0.name == "not-running" })
        let notRunningNodes = PanelA11y.panelTree(fixture: notRunning)
        assertKnownPresent(notRunningNodes, "Daemon not running", "the Start-daemon trait check")
        let start = try XCTUnwrap(notRunningNodes.firstContaining("Start daemon"))
        XCTAssertEqual(start.role, "AXButton", "the Start-daemon affordance publishes \(start.role)")

        let empty = try XCTUnwrap(fixtures.first { $0.name == "empty-roster" })
        let emptyNodes = PanelA11y.panelTree(fixture: empty)
        assertKnownPresent(emptyNodes, "Capture your first account", "the onboarding trait check")
        let field = try XCTUnwrap(emptyNodes.firstContaining("Account label"))
        XCTAssertEqual(field.role, "AXTextField", "the capture label field publishes \(field.role)")
        let capture = try XCTUnwrap(emptyNodes.firstContaining("Capture the active account"))
        XCTAssertEqual(capture.role, "AXButton", "the capture affordance publishes \(capture.role)")
    }

    /// A row that cannot be switched to must be exposed as DISABLED, not merely carry the reason in its
    /// label — a VoiceOver user should hear the control is unavailable without parsing prose.
    func testBlockedRowsAreExposedAsDisabled() throws {
        let fixture = try XCTUnwrap(PanelA11y.allFixtures.first { $0.name == "blind-cornered" })
        let nodes = PanelA11y.panelTree(fixture: fixture)
        assertKnownPresent(nodes, "Sessiometer.", "the blocked-row check")

        for account in ["Personal,", "Temp,"] {
            let row = try XCTUnwrap(nodes.firstContaining(account), "row \(account) missing")
            XCTAssertEqual(row.role, "AXButton", "blocked row \(account) publishes \(row.role)")
            XCTAssertFalse(row.enabled, """
                the weekly-exhausted row \(account) is still enabled in the tree. Its label says \
                "Can't switch", but a VoiceOver user navigating by control state would be told it is \
                actionable.
                """)
        }
    }

    // MARK: - AC-5: decorative elements are verified ABSENT, not merely annotated

    /// Issue #758 names this the assertion most likely to be faked — grepping for `accessibilityHidden(true)`
    /// and calling it verified. `Sources/` carries 20+ applications of the modifier, so the grep looks
    /// healthy in aggregate while a single missed view leaks. This walks the real tree of EVERY fixture.
    ///
    /// Asserted as set EQUALITY against the known-defect pin, so fixing issue #838 turns this RED and says
    /// so — the pin cannot rot into a permanent allowance.
    func testNoUnexpectedDecorativeElementReachesTheTree() throws {
        /// The one decorative element currently leaking, filed as issue #838 and deliberately not fixed
        /// here (this is an audit item). Identified by its SF Symbol name, which the runtime publishes as
        /// the element's `accessibilityIdentifier`.
        let knownDecorativeLeaks: Set<String> = ["chart.line.uptrend.xyaxis"]

        var observed: Set<String> = []
        for fixture in PanelA11y.allFixtures {
            let nodes = PanelA11y.panelTree(fixture: fixture)
            // Absence is evidence only against a populated tree — every fixture publishes the header.
            assertKnownPresent(nodes, "Sessiometer.", "fixture '\(fixture.name)'")
            for leak in nodes.decorativeImageLeaks {
                observed.insert(leak.identifier.isEmpty ? leak.text : leak.identifier)
            }
        }

        XCTAssertEqual(observed, knownDecorativeLeaks, """
            The set of decorative elements reaching the accessibility tree changed.

              observed: \(observed.sorted())
              pinned:   \(knownDecorativeLeaks.sorted())

            • Something NEW leaked → add `.accessibilityHidden(true)` to it; decorative glyphs, meters, \
            sparklines and pills must not be focusable, their information is already in the owning \
            element's label.
            • The pinned leak is GONE → issue #838 is fixed. Delete it from `knownDecorativeLeaks` above \
            (and the comment) so the set is empty again.
            """)
    }

    /// The structural companion to the leak test above, and the reason it is not redundant with it.
    ///
    /// `decorativeImageLeaks` can only see images. A decorative **Text** — the whole `SignalLegend`, say,
    /// which is hidden as one subtree at `StatusPanelStats.swift:391` — would leak straight past it. This
    /// pins the role histogram of every fixture instead, so ANY element appearing or disappearing reddens
    /// the gate, whatever its type.
    ///
    /// Why a histogram and not the element text: issue #761's spike found that keying assertions on
    /// rendered prose couples them to `StatusPanelFormat`'s exact wording, so a copy edit breaks tests that
    /// were not testing copy while a real regression that preserves the sentence slips through. Roles carry
    /// the structure without the prose.
    ///
    /// Note `stats` legitimately carries `AXImage:1` — that is the issue #838 leak, pinned here too.
    func testTheAccessibilityShapeOfEveryFixtureIsUnchanged() {
        let pinned: [String: String] = [
            "healthy": "AXButton:5 AXStaticText:3 AXUnknown:1",
            "stats": "AXButton:2 AXImage:1 AXStaticText:2 AXUnknown:3",
            "stale": "AXButton:5 AXStaticText:3 AXUnknown:1",
            "disconnected": "AXStaticText:3 AXUnknown:3",
            "connecting": "AXStaticText:2",
            "starting": "AXStaticText:2",
            "not-running": "AXButton:1 AXStaticText:3",
            "crash-looping": "AXStaticText:2",
            "unsupported": "AXStaticText:2",
            "empty-roster": "AXButton:1 AXStaticText:4 AXTextField:1",
            "blind-ok": "AXButton:5 AXStaticText:3 AXUnknown:1",
            "blind-degraded": "AXButton:5 AXStaticText:3 AXUnknown:1",
            "blind-cornered": "AXButton:4 AXStaticText:2 AXUnknown:1",
            "fault-keychain-locked": "AXButton:5 AXStaticText:4 AXUnknown:1",
            "fault-scrub-exhausted": "AXButton:5 AXStaticText:4 AXUnknown:1",
            "fault-systemic-refresh": "AXButton:5 AXStaticText:4 AXUnknown:1",
            "fault-scrub-recovering": "AXButton:5 AXStaticText:4 AXUnknown:1",
        ]

        let fixtures = PanelA11y.allFixtures
        // Cardinality guard: a shrunken fixture catalog would let this pass while covering almost nothing.
        XCTAssertEqual(fixtures.count, pinned.count, """
            the fixture catalog has \(fixtures.count) entries but \(pinned.count) are pinned — a new panel \
            state needs its shape recorded here, or this gate silently stops covering it.
            """)

        for fixture in fixtures {
            let nodes = PanelA11y.panelTree(fixture: fixture)
            assertKnownPresent(nodes, "Sessiometer.", "fixture '\(fixture.name)'")
            guard let expected = pinned[fixture.name] else {
                XCTFail("fixture '\(fixture.name)' has no pinned shape — add it: \"\(nodes.roleHistogram)\"")
                continue
            }
            XCTAssertEqual(nodes.roleHistogram, expected, """
                fixture '\(fixture.name)' changed accessibility shape.
                  observed: \(nodes.roleHistogram)
                  pinned:   \(expected)
                An element entered or left the tree. If a decorative view lost its \
                `.accessibilityHidden(true)`, restore it. If the panel genuinely gained or lost a control, \
                update the pin above in the same commit.
                """)
        }
    }

    /// MUTATION for the shape predicate: two trees differing by exactly one element must produce different
    /// histograms — otherwise the pin above would be blind to the leak class it exists to catch.
    func testTheShapePredicateFiresOnASingleExtraElement() {
        struct Subject: View {
            let extra: Bool
            var body: some View {
                VStack {
                    Text("ANCHOR")
                    // Mirrors `SignalLegend`: a decorative TEXT subtree hidden as a whole — the exact
                    // shape `decorativeImageLeaks` cannot see.
                    Text("DECORATIVE_LEGEND").accessibilityHidden(!extra)
                }
            }
        }
        let clean = PanelA11y.tree(for: Subject(extra: false), size: CGSize(width: 200, height: 120))
        let leaked = PanelA11y.tree(for: Subject(extra: true), size: CGSize(width: 200, height: 120))

        assertKnownPresent(clean, "ANCHOR", "the shape canary (clean)")
        assertKnownPresent(leaked, "ANCHOR", "the shape canary (leaked)")
        XCTAssertTrue(clean.decorativeImageLeaks.isEmpty && leaked.decorativeImageLeaks.isEmpty,
                      "precondition: this canary is about a TEXT leak, which the image predicate must miss")
        XCTAssertNotEqual(clean.roleHistogram, leaked.roleHistogram, """
            the histogram is identical with and without an extra focusable Text — the shape pin cannot \
            detect a leaked decorative text subtree, which is the whole reason it exists alongside \
            `decorativeImageLeaks`.
            """)
    }

    // MARK: - Focus / navigation order

    /// Focus order is the sequence `accessibilityChildrenInNavigationOrder()` publishes — the order a
    /// VoiceOver user traverses. Asserted as a SHAPE (header → tabs → roster → footer) rather than exact
    /// prose, so a wording change in `StatusPanelFormat` cannot redden it while a genuine re-ordering does.
    func testNavigationOrderRunsHeaderThenTabsThenRosterThenFooter() throws {
        let fixture = try XCTUnwrap(PanelA11y.allFixtures.first { $0.name == "healthy" })
        let nodes = PanelA11y.panelTree(fixture: fixture)
        assertKnownPresent(nodes, "Sessiometer.", "the navigation-order check")

        let top = nodes.filter { $0.depth == 1 }
        XCTAssertGreaterThan(top.count, 4, "the healthy panel published only \(top.count) top-level elements")

        func index(_ needle: String) throws -> Int {
            try XCTUnwrap(top.firstIndex { $0.text.contains(needle) }, "'\(needle)' not at top level: \(top)")
        }
        let header = try index("Sessiometer.")
        let statusTab = try index("Status")
        let statsTab = try index("Stats")
        let firstRow = try index("Work,")
        let footer = try index("updated")

        XCTAssertEqual(header, 0, "the header is not the first element a VoiceOver user reaches")
        XCTAssertLessThan(statusTab, statsTab, "the Status tab must precede Stats")
        XCTAssertLessThan(statsTab, firstRow, "the tab bar must precede the roster")
        XCTAssertLessThan(firstRow, footer, "the roster must precede the footer")
        XCTAssertEqual(footer, top.count - 1, "the footer is not the last element")
    }

    /// Every element a VoiceOver user can focus must SAY something. An element in the tree with no label
    /// and no value announces silence, which is worse than being hidden.
    func testNoFocusableElementIsSilent() throws {
        for fixture in PanelA11y.allFixtures {
            let nodes = PanelA11y.panelTree(fixture: fixture)
            assertKnownPresent(nodes, "Sessiometer.", "fixture '\(fixture.name)'")
            // Depth 0 is the hosting root, which legitimately carries no text of its own.
            let silent = nodes.filter { $0.depth > 0 && $0.text.isEmpty }
            XCTAssertTrue(silent.isEmpty, """
                fixture '\(fixture.name)' publishes \(silent.count) focusable element(s) with neither a \
                label nor a value — VoiceOver would land on them and say nothing: \(silent)
                """)
        }
    }

    // MARK: - #813 — the episode's provenance survives the machine → store → VIEW chain

    /// AC1's PANEL leg, guarded where it is actually reachable: through the real `StatusPanelView`.
    ///
    /// `StatusPanelFormatTests` pins the banner FUNCTION and `HonestStateMachineTests` pins the machine's
    /// projection, but neither sees the `StatusPanelView` → `daemonFaultBanner` argument list in between.
    /// Dropping `systemicRefreshSource:` at that call site leaves both suites green while the panel renders
    /// "1 consecutive sweep failed" over an episode in which zero sweeps ran — the exact defect #813
    /// exists to remove. This walks the rendered tree, so that line is load-bearing for a passing suite.
    ///
    /// The fixture is built HERE rather than added to `PanelRenderHarness.fixtures`: a shipped
    /// `fault-systemic-refresh-preflight` capture needs a matching `.pop` state in the mock to pair
    /// against (the design-SSOT prerequisite that also defers the canary faults, #571), and this guard
    /// needs no capture — only the tree.
    func testPreflightProvenanceReachesTheRenderedPanelBanner() throws {
        let swept = try XCTUnwrap(PanelA11y.allFixtures.first { $0.name == "fault-systemic-refresh" })
        var preflight = swept
        // The count a preflight-opened episode actually carries: the seeded floor of one, kept only so a
        // pre-#813 client stays grammatical. If the provenance is lost anywhere in the chain, THIS is what
        // gets rendered as "1 consecutive sweep failed".
        preflight.systemicRefreshFailure = 1
        preflight.systemicRefreshSource = .preflight

        let nodes = PanelA11y.panelTree(fixture: preflight)
        assertKnownPresent(nodes, "Sessiometer.", "the #813 provenance check")

        XCTAssertNotNil(nodes.firstContaining("startup preflight could not resolve"), """
            the preflight banner never reached the rendered panel — the provenance is dropped somewhere in \
            machine → store → StatusPanelView. Tree: \(nodes)
            """)
        XCTAssertNil(nodes.firstContaining("consecutive sweep"), """
            the panel still asserts a sweep for a PREFLIGHT-opened episode (zero sweeps ran) — issue #813's \
            defect, reachable again. Tree: \(nodes)
            """)

        // AC2's panel leg, through the same rendered chain: the sweep arm is untouched.
        let sweptNodes = PanelA11y.panelTree(fixture: swept)
        assertKnownPresent(sweptNodes, "Sessiometer.", "the #813 sweep-arm regression check")
        XCTAssertNotNil(sweptNodes.firstContaining("3 consecutive sweeps failed"),
                        "the #378 sweep phrasing regressed in the rendered panel: \(sweptNodes)")
    }
}

/// A hermetic `LoginItemService` for the tree walk — no `SMAppService`, no OS calls. Mirrors
/// `PanelRenderHarness`' own private stand-in (it cannot be reused: it is `private` to that file).
private final class A11yProbeLoginItemService: LoginItemService {
    let appStatus: LoginItemStatus = .enabled
    let daemonAgentStatus: LoginItemStatus = .notRegistered
    let cliManagedAgentPresent: Bool = false
    let daemonLockHeld: Bool = false
    let daemonAgentRunState: DaemonAgentRunState = .notRunning
    func registerApp() throws {}
    func unregisterApp() throws {}
    func registerDaemonAgent() throws {}
    func unregisterDaemonAgent() throws {}
    func openLoginItemsSettings() {}
}
#endif
