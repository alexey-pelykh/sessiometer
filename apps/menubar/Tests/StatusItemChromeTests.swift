// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Coverage for the status item's pure decision layer (issue #764) — the half of `StatusItemController`
// that is a function of plain values rather than of `NSStatusBar` / `NSPanel` / a live `NSEvent`.
//
// WHY THESE FOUR DECISIONS, and not a state→glyph test. Issue #764's AC-1 asks for the controller's
// "state-to-glyph selection" to be extracted and covered "for every `ConnectionState`". Measured, the
// controller makes no such selection: `apply(_:)` is a verbatim forward of `presentation.glyph` into
// `StatusGauge.image(for:)`. The projection lives in `PresentationState.make(for:accountCount:...)` and
// is ALREADY locked exhaustively — `HonestStateMachineTests.testEveryConnectionStateProjectsOntoThe`
// `RatifiedAttentionGlyph` is a 10-row table over every `ConnectionState`, and
// `testNoRunwayIsGatedOnAFreshConnectedSnapshotExactlyLikeHealthy` covers all nine non-vouched states.
// The brand lock (one DISTINCT silhouette per state — the menu bar is monochrome, so shape alone carries
// state) is likewise already held, twice: `StatusGaugeTests.testEveryGlyphMapsToADistinctAsset` pins
// asset-name injectivity, and `BarGlyphParityTests.testTheFourGlyphsArePairwiseDistinctInEveryContext`
// pins RENDERED pairwise distinctness in every context and scale. Re-extracting the forward would add a
// tautological seam, so the gate is set at the measured boundary instead: the decisions that genuinely
// had no coverage. Because that upstream suite IS this item's answer to AC-1, it must not be able to rot
// silently — `testTheUpstreamStateToGlyphTableStillExists` below reads its source and reddens if the
// table is deleted or narrowed, so the claim is a live tripwire rather than a comment.
//
// CONSTRAINT-A (issue #748): every gate here whose verdict runs through a DERIVED predicate ships a canary
// proving that predicate can FAIL, driven through the SAME predicate the real assertion uses — never by
// inspection. That is the geometry gates (the canaries re-implement the pre-#446 and unclamped algorithms
// and run them through the same on-screen predicates) and the structural menu gates (the canaries mutate
// the spec and re-run the same checks). The click, dismissal and open-precondition gates compare a
// returned enum case directly — nothing sits between the function and the verdict that could be vacuously
// true, so a canary there would assert that `==` works.

import AppKit
import XCTest

final class StatusItemChromeTests: XCTestCase {

    // A 16" display's visible frame: menu bar excluded at the top, Dock at the bottom. macOS is
    // origin-bottom-left, so the status item's own window sits ABOVE `visible.maxY`.
    private let visible = NSRect(x: 0, y: 0, width: 1512, height: 944)
    /// A status item roughly two thirds across the bar.
    private let iconFrame = NSRect(x: 1200, y: 944, width: 32, height: 24)
    private let panelSize = NSSize(width: 360, height: 240)

    private var inset: CGFloat { StatusItemChrome.screenInset }
    private var gap: CGFloat { StatusItemChrome.panelGap }

    // MARK: - Shared predicates (the real assertions AND the canaries both run through these)

    /// The panel is fully inside the visible frame horizontally, with `screenInset` of margin.
    private func isHorizontallyOnScreen(_ frame: NSRect, within bounds: NSRect) -> Bool {
        frame.minX >= bounds.minX + inset - 0.001 && frame.maxX <= bounds.maxX - inset + 0.001
    }

    /// The panel's BOTTOM edge is on-screen — the #446 guarantee. Deliberately NOT a "fully on screen"
    /// check: the ratified trade clamps the bottom only, so a panel taller than the space below the icon
    /// grows UPWARD past the bar rather than clipping. `testATallPanelKeepsItsBottomOnScreenAndGrowsUpward`
    /// pins that asymmetry explicitly.
    private func isBottomOnScreen(_ frame: NSRect, within bounds: NSRect) -> Bool {
        frame.minY >= bounds.minY + inset - 0.001
    }

    /// Centered under the icon, to within half a point.
    private func isCenteredUnder(_ frame: NSRect, icon: NSRect) -> Bool {
        abs(frame.midX - icon.midX) < 0.5
    }

    // MARK: - The pre-#446 / unclamped algorithms, kept ONLY as canary mutants

    /// The algorithm as it shipped BEFORE issue #446: size once at open, clamp X, leave Y alone. Exists
    /// solely so `isBottomOnScreen` can be shown to reject something — a gate nothing can fail is not
    /// evidence.
    private func preIssue446Frame(iconFrame icon: NSRect, visibleFrame bounds: NSRect, contentSize size: NSSize) -> NSRect {
        var x = icon.midX - size.width / 2
        x = min(max(x, bounds.minX + inset), bounds.maxX - size.width - inset)
        let y = icon.minY - gap - size.height          // no floor clamp — the #446 bug
        return NSRect(x: x, y: y, width: size.width, height: size.height)
    }

    /// Centering with no horizontal clamp at all — the canary mutant for `isHorizontallyOnScreen`.
    private func unclampedFrame(iconFrame icon: NSRect, contentSize size: NSSize) -> NSRect {
        NSRect(x: icon.midX - size.width / 2, y: icon.minY - gap - size.height,
               width: size.width, height: size.height)
    }

    // MARK: - Panel geometry: the ordinary case

    func testTheOrdinaryPanelHangsCenteredBelowTheIconByExactlyTheGap() {
        let frame = StatusItemChrome.panelFrame(iconFrame: iconFrame, visibleFrame: visible, contentSize: panelSize)

        XCTAssertTrue(isCenteredUnder(frame, icon: iconFrame),
                      "an unclamped panel must be centered under the icon (got midX \(frame.midX) vs icon \(iconFrame.midX))")
        XCTAssertEqual(frame.maxY, iconFrame.minY - gap, accuracy: 0.001,
                       "the panel's top edge must sit exactly `panelGap` below the icon's bottom — the gap is what "
                       + "leaves the icon itself visible and clickable for the second-click toggle (#325/#326)")
        XCTAssertEqual(frame.size, panelSize, "an ordinary panel is sized to its content, never resized by placement")
        XCTAssertTrue(isHorizontallyOnScreen(frame, within: visible))
        XCTAssertTrue(isBottomOnScreen(frame, within: visible))
    }

    // MARK: - Panel geometry: horizontal clamping (both edges), and its canary

    func testAPanelNearEitherScreenEdgeIsClampedOnScreen() {
        // Icon hard against the right edge — centering alone would run the panel off-screen.
        let rightIcon = NSRect(x: visible.maxX - 30, y: visible.maxY, width: 28, height: 24)
        let rightFrame = StatusItemChrome.panelFrame(iconFrame: rightIcon, visibleFrame: visible, contentSize: panelSize)
        XCTAssertTrue(isHorizontallyOnScreen(rightFrame, within: visible),
                      "a panel under a right-edge icon must be clamped inside the visible frame, got \(rightFrame)")
        XCTAssertEqual(rightFrame.maxX, visible.maxX - inset, accuracy: 0.001,
                       "clamped right, the panel rests exactly `screenInset` from the edge")

        // Icon hard against the left edge (a narrow display, or the bar's left group).
        let leftIcon = NSRect(x: visible.minX + 4, y: visible.maxY, width: 28, height: 24)
        let leftFrame = StatusItemChrome.panelFrame(iconFrame: leftIcon, visibleFrame: visible, contentSize: panelSize)
        XCTAssertTrue(isHorizontallyOnScreen(leftFrame, within: visible),
                      "a panel under a left-edge icon must be clamped inside the visible frame, got \(leftFrame)")
        XCTAssertEqual(leftFrame.minX, visible.minX + inset, accuracy: 0.001,
                       "clamped left, the panel rests exactly `screenInset` from the edge")
    }

    // CANARY — the same `isHorizontallyOnScreen` predicate must REJECT an unclamped placement, or a green
    // above would mean nothing.
    func testTheHorizontalClampGateCanFail() {
        let rightIcon = NSRect(x: visible.maxX - 30, y: visible.maxY, width: 28, height: 24)
        let mutant = unclampedFrame(iconFrame: rightIcon, contentSize: panelSize)
        XCTAssertFalse(isHorizontallyOnScreen(mutant, within: visible),
                       "the unclamped canary (\(mutant)) did NOT fail the horizontal on-screen predicate — "
                       + "the gate cannot fail, so it is not evidence")
        // And the real function passes on the very same input — same predicate, opposite verdict.
        XCTAssertTrue(isHorizontallyOnScreen(
            StatusItemChrome.panelFrame(iconFrame: rightIcon, visibleFrame: visible, contentSize: panelSize),
            within: visible))
    }

    // MARK: - Panel geometry: the #446 bottom floor, and its canary

    func testATallPanelKeepsItsBottomOnScreenAndGrowsUpward() {
        // The Stats tab on a short display: taller than the space between the icon and the Dock.
        let tall = NSSize(width: 360, height: 1200)
        let frame = StatusItemChrome.panelFrame(iconFrame: iconFrame, visibleFrame: visible, contentSize: tall)

        XCTAssertTrue(isBottomOnScreen(frame, within: visible),
                      "issue #446: a tall panel must keep its bottom on-screen, got \(frame)")
        XCTAssertEqual(frame.minY, visible.minY + inset, accuracy: 0.001,
                       "clamped, the panel rests exactly `screenInset` above the visible frame's bottom")

        // The ratified asymmetry, pinned so it is a decision and not an accident: only the BOTTOM is
        // clamped, so a panel this tall overlaps upward past the icon rather than being clipped.
        // Visible-but-overlapping beats correctly-placed-but-cut-off (#446).
        XCTAssertGreaterThan(frame.maxY, visible.maxY,
                             "a 1200 pt panel on a 944 pt visible frame MUST overlap upward — if this ever "
                             + "stops holding, the #446 trade changed and the placement rule needs re-ratifying")
    }

    // CANARY — the same `isBottomOnScreen` predicate must REJECT the pre-#446 algorithm on the same input.
    func testTheBottomFloorGateCanFail() {
        let tall = NSSize(width: 360, height: 1200)
        let mutant = preIssue446Frame(iconFrame: iconFrame, visibleFrame: visible, contentSize: tall)
        XCTAssertFalse(isBottomOnScreen(mutant, within: visible),
                       "the pre-#446 canary (\(mutant)) did NOT fail the bottom-on-screen predicate — "
                       + "the gate cannot fail, so it is not evidence")
        XCTAssertTrue(isBottomOnScreen(
            StatusItemChrome.panelFrame(iconFrame: iconFrame, visibleFrame: visible, contentSize: tall),
            within: visible))
    }

    // A short panel must NOT be moved by the floor clamp — the fix must be inert on the ordinary path.
    // (This is what makes "switching back to the shorter Status tab restores the original look" true.)
    func testTheBottomFloorIsInertWhenThePanelAlreadyFits() {
        let frame = StatusItemChrome.panelFrame(iconFrame: iconFrame, visibleFrame: visible, contentSize: panelSize)
        XCTAssertEqual(frame, preIssue446Frame(iconFrame: iconFrame, visibleFrame: visible, contentSize: panelSize),
                       "when the panel fits below the icon the #446 clamp must change nothing — a Status↔Stats "
                       + "round trip has to land back on the original placement")
    }

    // MARK: - Panel geometry: clamp ORDER on an over-wide panel (pinned, not fixed)

    // `min(max(x, lo), hi)` resolves to `hi` when `hi < lo` — i.e. a panel wider than the visible frame
    // minus both insets ends up hanging off the LEFT edge, not the right. Unreachable at shipped sizes
    // (a ~360 pt panel against a ≥1280 pt display), so this pins the behaviour rather than "fixing" a
    // state no operator can reach. If the panel ever becomes width-flexible, this test is the tripwire.
    func testAnOverWidePanelResolvesToTheRightBoundByClampOrder() {
        let narrowScreen = NSRect(x: 0, y: 0, width: 300, height: 900)
        let icon = NSRect(x: 150, y: 900, width: 24, height: 24)
        let frame = StatusItemChrome.panelFrame(iconFrame: icon, visibleFrame: narrowScreen, contentSize: panelSize)

        XCTAssertEqual(frame.minX, narrowScreen.maxX - panelSize.width - inset, accuracy: 0.001,
                       "clamp order: with hi < lo the `min(max(...))` resolves to hi")
        XCTAssertFalse(isHorizontallyOnScreen(frame, within: narrowScreen),
                       "documenting the consequence — an over-wide panel is NOT fully on screen; it is "
                       + "unreachable at shipped sizes, which is why it is pinned rather than changed")
    }

    // MARK: - Degenerate fitting size

    func testADegenerateFittingSizeFallsBackOnEitherAxis() {
        XCTAssertEqual(StatusItemChrome.contentSize(fitting: .zero), StatusItemChrome.fallbackContentSize,
                       "a 0×0 first-layout measurement must not size the panel")
        XCTAssertEqual(StatusItemChrome.contentSize(fitting: NSSize(width: 360, height: 0)),
                       StatusItemChrome.fallbackContentSize,
                       "zero on ONE axis is as unusable as zero on both — a 360×0 panel shows nothing")
        XCTAssertEqual(StatusItemChrome.contentSize(fitting: NSSize(width: 0, height: 240)),
                       StatusItemChrome.fallbackContentSize,
                       "zero on ONE axis is as unusable as zero on both")
        // Sub-point but non-zero is still degenerate.
        XCTAssertEqual(StatusItemChrome.contentSize(fitting: NSSize(width: 0.4, height: 0.4)),
                       StatusItemChrome.fallbackContentSize)
    }

    func testARealFittingSizeIsPassedThroughUntouched() {
        let measured = NSSize(width: 372.5, height: 618.25)
        XCTAssertEqual(StatusItemChrome.contentSize(fitting: measured), measured,
                       "a real measurement must never be rounded or substituted — the panel is sized to its content")
    }

    func testTheFallbackSizeIsItselfUsable() {
        // The fallback is only worth having if placing it yields a sane, on-screen frame.
        let frame = StatusItemChrome.panelFrame(iconFrame: iconFrame, visibleFrame: visible,
                                                contentSize: StatusItemChrome.fallbackContentSize)
        XCTAssertTrue(isHorizontallyOnScreen(frame, within: visible))
        XCTAssertTrue(isBottomOnScreen(frame, within: visible))
        XCTAssertGreaterThan(StatusItemChrome.fallbackContentSize.width, 0)
        XCTAssertGreaterThan(StatusItemChrome.fallbackContentSize.height, 0)
    }

    // MARK: - Click routing

    func testARightMouseUpIsSecondary() {
        XCTAssertEqual(StatusItemChrome.click(forEventType: .rightMouseUp, modifiers: []), .secondary)
        XCTAssertEqual(StatusItemChrome.click(forEventType: .rightMouseUp, modifiers: .control), .secondary,
                       "modifiers must not change a right-click's meaning")
    }

    func testAControlHeldLeftMouseUpIsSecondary() {
        XCTAssertEqual(StatusItemChrome.click(forEventType: .leftMouseUp, modifiers: .control), .secondary,
                       "control+click is macOS's second contextual-menu gesture")
        XCTAssertEqual(StatusItemChrome.click(forEventType: .leftMouseUp, modifiers: [.control, .shift]), .secondary,
                       "control combined with other modifiers is still control-click")
    }

    func testAPlainLeftMouseUpIsPrimary() {
        XCTAssertEqual(StatusItemChrome.click(forEventType: .leftMouseUp, modifiers: []), .primary)
        for modifier in [NSEvent.ModifierFlags.command, .shift, .option, .capsLock, .function] {
            XCTAssertEqual(StatusItemChrome.click(forEventType: .leftMouseUp, modifiers: modifier), .primary,
                           "only CONTROL promotes a left-click to secondary — \(modifier) must not")
        }
    }

    // The `nil`-event path is not a curiosity: `showLifecycleMenu` presents the transient menu via
    // `performClick(nil)`, which leaves `NSApp.currentEvent` without a mouse event. Classifying that as
    // secondary would make the menu re-enter itself.
    func testANilEventIsPrimarySoTheProgrammaticClickCannotReEnterTheMenu() {
        XCTAssertEqual(StatusItemChrome.click(forEventType: nil, modifiers: []), .primary)
        XCTAssertEqual(StatusItemChrome.click(forEventType: nil, modifiers: .control), .primary,
                       "a nil event has no modifiers to honour — control must not resurrect it as secondary")
    }

    // The item fires on mouse-UP only (`sendAction(on: [.leftMouseUp, .rightMouseUp])`); anything else
    // reaching the classifier is not a click and must not open the menu.
    func testNonMouseUpEventsAreNeverSecondary() {
        for type in [NSEvent.EventType.leftMouseDown, .rightMouseDown, .otherMouseUp, .keyDown, .mouseMoved] {
            XCTAssertEqual(StatusItemChrome.click(forEventType: type, modifiers: .control), .primary,
                           "\(type) is not a status-item click — it must never raise the lifecycle menu")
        }
    }

    // MARK: - Lifecycle menu spec

    func testEveryLifecycleActionAppearsExactlyOnce() {
        let actions = StatusItemChrome.lifecycleMenu.compactMap(\.action)
        XCTAssertEqual(Set(actions).count, actions.count, "no lifecycle action may appear twice")
        // Exhaustive BY CONSTRUCTION over the enum — a new action fails here until it is placed.
        XCTAssertEqual(Set(actions), Set(StatusItemChrome.MenuAction.allCases),
                       "every MenuAction must appear in the menu — an unplaced action is unreachable")
    }

    func testTheMenuOrderAndCopyAreLocked() {
        XCTAssertEqual(StatusItemChrome.lifecycleMenu, [
            .item(.addAccount, "Add account…"),
            .item(.openSettings, "Settings…", keyEquivalent: ","),
            .separator,
            .item(.quit, "Quit Sessiometer"),
        ], "the secondary-click menu's rows, order, copy and shortcuts are the shipped contract")
    }

    func testQuitIsSeparatedFromTheOrdinaryActions() {
        let entries = StatusItemChrome.lifecycleMenu
        guard let separatorIndex = entries.firstIndex(where: \.isSeparator),
              let quitIndex = entries.firstIndex(where: { $0.action == .quit }) else {
            return XCTFail("the menu must carry both a separator and a Quit row")
        }
        XCTAssertLessThan(separatorIndex, quitIndex,
                          "Quit is the one destructive row — it must sit below the separator, not adjacent to "
                          + "Settings where a mis-click lands on it")
        XCTAssertEqual(entries.filter(\.isSeparator).count, 1, "exactly one separator")
    }

    func testEveryVisibleRowIsNonEmptyAndUsesTheEllipsisConventionForRowsThatOpenAFurtherSurface() {
        for entry in StatusItemChrome.lifecycleMenu where !entry.isSeparator {
            guard let title = entry.title else { return XCTFail("a non-separator row must carry a title") }
            XCTAssertFalse(title.isEmpty, "a menu row may not be blank")
            XCTAssertFalse(title.hasSuffix("..."), "\(title): use the ellipsis CHARACTER '…', not three periods")
        }
        // Apple's HIG: a row that opens a further surface before acting ends in an ellipsis; one that acts
        // immediately does not. Add account… and Settings… open surfaces; Quit acts.
        for action in [StatusItemChrome.MenuAction.addAccount, .openSettings] {
            let title = StatusItemChrome.lifecycleMenu.first { $0.action == action }?.title
            XCTAssertEqual(title?.hasSuffix("…"), true, "\(action) opens a further surface — its row ends in '…'")
        }
        let quit = StatusItemChrome.lifecycleMenu.first { $0.action == .quit }?.title
        XCTAssertEqual(quit?.hasSuffix("…"), false, "Quit acts immediately — no ellipsis")
    }

    // Quit terminates the MENU-BAR APP only; the daemon's lifecycle is #170. The copy has to say which
    // thing is quitting, or the operator cannot tell whether their sessions keep being watched.
    func testQuitNamesTheAppSoItCannotBeReadAsQuittingTheDaemon() {
        let quit = StatusItemChrome.lifecycleMenu.first { $0.action == .quit }?.title
        XCTAssertEqual(quit, "Quit Sessiometer")
        XCTAssertFalse(quit?.lowercased().contains("daemon") ?? true,
                       "Quit must not claim to stop the daemon — it is a pure-client control")
    }

    // CANARY — the structural menu checks must reject a mutated spec, run through the SAME predicates.
    func testTheMenuGatesCanFail() {
        // (a) a duplicated action defeats the once-each check
        let duplicated: [StatusItemChrome.MenuEntry] = [
            .item(.addAccount, "Add account…"), .item(.addAccount, "Add another…"), .separator, .item(.quit, "Quit Sessiometer"),
        ]
        let dupActions = duplicated.compactMap(\.action)
        XCTAssertNotEqual(Set(dupActions).count, dupActions.count,
                          "the duplicate canary did not trip the once-each predicate — that gate cannot fail")

        // (b) a dropped action defeats exhaustiveness
        let incomplete: [StatusItemChrome.MenuEntry] = [.item(.addAccount, "Add account…"), .item(.quit, "Quit Sessiometer")]
        XCTAssertNotEqual(Set(incomplete.compactMap(\.action)), Set(StatusItemChrome.MenuAction.allCases),
                          "the dropped-action canary did not trip the exhaustiveness predicate")

        // (c) Quit above the separator defeats the ordering check
        let misordered: [StatusItemChrome.MenuEntry] = [
            .item(.quit, "Quit Sessiometer"), .separator, .item(.addAccount, "Add account…"), .item(.openSettings, "Settings…"),
        ]
        let sepIdx = misordered.firstIndex(where: \.isSeparator)
        let quitIdx = misordered.firstIndex { $0.action == .quit }
        XCTAssertFalse(sepIdx! < quitIdx!, "the misordered canary did not trip the separator-before-Quit predicate")

        // (d) three-period ellipsis defeats the copy check
        XCTAssertTrue("Add account...".hasSuffix("..."), "the ASCII-ellipsis canary did not trip the copy predicate")
    }

    // MARK: - Outside-click dismissal

    func testAClickOnOurOwnStatusItemIsIgnoredSoTheSecondClickStillToggles() {
        // Checked FIRST, and independently of what the models are doing: the global monitor also sees the
        // menu-bar mouse-DOWN on our own item. Closing here would let the button's mouse-UP reopen it —
        // the classic "won't close on the second click" bug (#325/#326).
        for capture in [false, true] {
            for swap in [false, true] {
                XCTAssertEqual(
                    StatusItemChrome.outsideClick(landedOnStatusItem: true, captureBusy: capture, swapBusy: swap),
                    .ignoreOwnStatusItem,
                    "own-item clicks are ignored regardless of busy state (capture=\(capture) swap=\(swap))")
            }
        }
    }

    func testAnInFlightCaptureOrSwapRetainsThePanel() {
        XCTAssertEqual(StatusItemChrome.outsideClick(landedOnStatusItem: false, captureBusy: true, swapBusy: false),
                       .retain, "#360: a typed-but-unsubmitted label or an in-flight capture must not be dropped")
        XCTAssertEqual(StatusItemChrome.outsideClick(landedOnStatusItem: false, captureBusy: false, swapBusy: true),
                       .retain, "#169: an in-flight swap WRITES the active account — its outcome must be readable")
        XCTAssertEqual(StatusItemChrome.outsideClick(landedOnStatusItem: false, captureBusy: true, swapBusy: true),
                       .retain)
    }

    func testAnIdleOutsideClickDismisses() {
        XCTAssertEqual(StatusItemChrome.outsideClick(landedOnStatusItem: false, captureBusy: false, swapBusy: false),
                       .dismiss, "the ordinary path — click away from an idle panel closes it")
    }

    // Exhaustive over all 8 input combinations, so the truth table is locked rather than sampled.
    func testTheDismissTruthTableIsTotal() {
        var seen: [StatusItemChrome.DismissDecision: Int] = [:]
        for onItem in [false, true] {
            for capture in [false, true] {
                for swap in [false, true] {
                    let decision = StatusItemChrome.outsideClick(landedOnStatusItem: onItem,
                                                                 captureBusy: capture, swapBusy: swap)
                    let expected: StatusItemChrome.DismissDecision =
                        onItem ? .ignoreOwnStatusItem : (capture || swap ? .retain : .dismiss)
                    XCTAssertEqual(decision, expected, "onItem=\(onItem) capture=\(capture) swap=\(swap)")
                    seen[decision, default: 0] += 1
                }
            }
        }
        XCTAssertEqual(seen.values.reduce(0, +), 8, "all 8 combinations must be evaluated")
        XCTAssertEqual(seen.count, 3, "every decision case must be reachable — an unreachable case is dead policy")
    }

    // MARK: - Open precondition

    func testThePanelOnlyOpensOnceTheIconHasAWindow() {
        XCTAssertFalse(StatusItemChrome.canOpenPanel(iconHasWindow: false),
                       "with no icon window there is nothing to position against — showing would flash the "
                       + "panel at a stale, unpositioned frame")
        XCTAssertTrue(StatusItemChrome.canOpenPanel(iconHasWindow: true))
    }

    // MARK: - The AC-1 claim, asserted rather than left as prose

    // Issue #764 AC-1 asks for state→glyph coverage "for every ConnectionState". It already exists,
    // upstream — which is this item's whole answer to AC-1, so that answer must not be able to rot
    // silently. This asserts the upstream suite STILL CARRIES the table, by reading its source (the same
    // technique `AppLaunchPlanTests.testTheSwapBudgetStillClearsTheRustLockBound` uses for `src/swap.rs`).
    //
    // Reading the SOURCE rather than re-deriving `make` locally is the load-bearing choice: a local
    // re-derivation stays green when the upstream table is deleted, which makes it a decoration, not a
    // link. This version reddens when the table goes away — verified by deleting both upstream tests.
    func testTheUpstreamStateToGlyphTableStillExists() throws {
        let suite = URL(fileURLWithPath: #filePath)          // .../Tests/StatusItemChromeTests.swift
            .deletingLastPathComponent()
            .appendingPathComponent("HonestStateMachineTests.swift")
        let source = try XCTUnwrap(try? String(contentsOf: suite, encoding: .utf8),
                                   "could not read \(suite.lastPathComponent) — AC-1's coverage claim cannot be checked")

        for name in ["func testEveryConnectionStateProjectsOntoTheRatifiedAttentionGlyph",
                     "func testNoRunwayIsGatedOnAFreshConnectedSnapshotExactlyLikeHealthy"] {
            XCTAssertTrue(source.contains(name),
                          "\(name) is gone from HonestStateMachineTests. That suite is issue #764's ENTIRE "
                          + "answer to AC-1 (the controller only forwards `presentation.glyph`), so its "
                          + "removal re-opens the hole this item closed — restore it, or re-home the table "
                          + "and re-point this assertion.")
        }

        // …and that the table still enumerates every connection input, not a narrowed subset. Each state
        // must appear as a row in the expected-table literal.
        guard let tableStart = source.range(of: "func testEveryConnectionStateProjectsOntoTheRatifiedAttentionGlyph"),
              let tableEnd = source.range(of: "func testNoRunwayIsGatedOnAFreshConnectedSnapshotExactlyLikeHealthy") else {
            return XCTFail("could not bound the upstream table — re-point this assertion")
        }
        let table = source[tableStart.lowerBound..<tableEnd.lowerBound]
        let everyConnectionInput = ["(.connected,", "(.connecting,", "(.starting,", "(.reconnecting(",
                                    "(.emptyRoster,", "(.stale,", "(.disconnected(", "(.notRunning,",
                                    "(.unsupported,", "(.crashLooping,"]
        for input in everyConnectionInput {
            XCTAssertTrue(table.contains(input),
                          "the upstream #524 table no longer carries a row for \(input) — the state→glyph "
                          + "mapping has been narrowed, and the menu bar's glyph for that state is now unpinned")
        }
        XCTAssertEqual(everyConnectionInput.count, 10, "the 10 connection inputs of the #524 axis")
    }

    // The brand lock's consequence (monochrome bar ⇒ shape alone carries state): no two states may select
    // the same artwork. Held upstream by `StatusGaugeTests` + `BarGlyphParityTests`; re-derived here
    // because #764's caller names it as the exact failure mode an extraction could introduce ("if your
    // extraction would let two states select the same glyph, that is a brand-lock violation").
    func testNoTwoStatesCanSelectTheSameGlyph() {
        let assets = StatusGlyph.allCases.map(StatusGauge.assetName(for:))
        XCTAssertEqual(Set(assets).count, StatusGlyph.allCases.count,
                       "two states selecting the same glyph asset is a brand-lock violation, not a detail — "
                       + "the menu-bar item is a monochrome template, so SHAPE is the only channel carrying state")

        // Reached through the projection the controller actually forwards, so this covers the composed
        // path (state → presentation → asset), not just the factory in isolation. Asserting that every
        // reached asset is IN the family would be vacuous — `assetName` is total over `StatusGlyph`, so
        // that holds by the type system. These two can actually fail.
        let everyConnectionState: [ConnectionState] = [
            .connected, .connecting, .starting, .reconnecting(reason: "EOF"), .emptyRoster,
            .stale, .disconnected(reason: "EOF"), .notRunning, .unsupported, .crashLooping,
        ]
        let reachedGlyphs = everyConnectionState.map { PresentationState.make(for: $0, accountCount: 1).glyph }
        let reachedAssets = Set(reachedGlyphs.map(StatusGauge.assetName(for:)))
        XCTAssertEqual(reachedAssets.count, Set(reachedGlyphs).count,
                       "two glyphs the projection actually reaches collapsed onto ONE asset — the composed "
                       + "path, not just the factory, has to keep the silhouettes apart")
        XCTAssertGreaterThan(reachedAssets.count, 1,
                             "every connection state reached the SAME silhouette — the bar would carry no "
                             + "state at all, which asset-name injectivity above cannot detect")
    }
}
