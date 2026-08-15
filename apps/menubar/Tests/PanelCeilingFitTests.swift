// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Does the panel FIT at its own ceiling (issue #983)? Rendered at `PanelTypeScale.ceiling`, measured
// against a named display, and answered — rather than reasoned about.
//
// WHY THE QUESTION WAS STILL OPEN. `StatusPanelTypeScale`'s header states a fit-adjacent number ("at
// ×2.3529 the healthy panel is already 894 pt wide"), and its neighbouring paragraph argues about what
// growing PAST the ceiling would cost. Neither establishes that the panel AT the ceiling lands on a
// screen: the first is a width with nothing to compare it to, the second is about a size class the panel
// declines to render. Issue #983 asks for the missing half and names the header comment as the thing NOT
// to assert it from. So every number below is read off a render in this process.
//
// WHAT "FITS" NEEDS, and why a bare verdict would not be one. A footprint fits a DISPLAY, so the claim is
// meaningless without naming which — and it has two axes, which have completely different answers here:
//
//   * WIDTH had no verdict anywhere. The panel is `.frame(width:)`-pinned at `panelContentWidth × k`, so
//     it grows 380.00 → 894.12 pt at the ceiling with no bound of any kind. Nothing compared that to a
//     screen. The ONE place that reasons about panel-width-against-screen is `StatusItemChrome.panelFrame`,
//     whose clamp-order comment called the over-wide branch unreachable on the ground that "the panel is
//     ~360 pt" — a DEFAULT-size figure, 2.35× short of the ceiling. The conclusion survives the correction
//     (see below); the ground it rested on did not, which is exactly the newly-reachable-state shape
//     issue #983 was filed under.
//   * HEIGHT already had one, from issue #818: the panel is clamped at `StatusPanelFormat.panelHeightBudget`,
//     and that budget IS the derived available height (900 − 24 − 20 = 856). So "height fits" is true BY
//     CONSTRUCTION and asserting it alone would be a tautology. What the render still adds is a SECOND,
//     INDEPENDENT construction of that number: `availableHeight` below recomposes it from the three display
//     constants instead of reading `panelHeightBudget`, so the assertion reds the moment the two disagree —
//     re-tune the budget alone and every roster-bearing state measures past this suite's figure while the
//     panel itself stays happily within its own. That is a contribution `PanelScrollBoundaryTests
//     .testThePanelNeverExceedsItsBudgetAndTheBudgetActuallyBinds` cannot make: it reads the constant, so it
//     moves WITH such a re-tune and stays green. Both halves are measured — mutating the budget body by
//     +200 reds this suite on 16 states and leaves that one passing.
//
//     WHAT IT DOES NOT CATCH, stated because the assertion's own wording used to claim otherwise: a state
//     whose PINNED chrome — header, tab bar, footer, all outside the boundary — exceeded the budget on its
//     own would NOT "sail past" the bound in this measurement. `.frame(maxHeight:)` sizes the frame to the
//     cap and lets an inflexible child overflow OUTSIDE it, and `ImageRenderer` rasterizes the reported
//     size, so the raster is pinned at the cap either way. Measured: driving the display height to 200
//     (a 156 pt budget, far under the leanest state's 262.50 pt of chrome) rasterizes all 22 states at
//     exactly 156.00 pt and leaves the fit assertion GREEN — the roster-partition assertion below is what
//     reds there, on every state being at the bound. Overflow-past-the-cap is a question this render path
//     cannot ask; it would need a clipping or intrinsic-height probe, and nothing here claims one.
//
// ── WHAT WAS MEASURED (macOS 26.5.2 / Xcode 26.6, all 22 harness fixtures, `composingScrollBoundaries`) ──
//
//   axis   │ at `.large` │ at `.accessibility3` │ available on 1440 × 900 │ spare
//   ───────┼─────────────┼──────────────────────┼─────────────────────────┼────────
//   width  │      380.00 │               894.50 │ 1424.00 (1440 − 2 × 8)  │  529.50
//   height │ ≤    637.00 │            ≤  856.00 │  856.00 (900 − 24 − 20) │    0.00
//
// The width is ONE number across all 22 states (a root `.frame(width:)` cannot vary per state). It reads
// 894.50 rather than the 894.12 the arithmetic gives because a raster is whole pixels: 894.1176… pt at @2x
// is 1788.24 px, which lands as 1789. The check below uses the LARGER, rasterized figure on purpose — the
// panel's real footprint, and the conservative direction.
//
// EVERY FIGURE HERE IS RENDERED IN THE LIGHT SCHEME ONLY, while the committed golden corpus is 44 cells
// (22 states × light/dark). That is sufficient rather than a gap, and it is measured rather than argued
// from construction: rendering all 22 states in BOTH schemes at the ceiling gives identical widths and
// heights, 0 of 22 differing. The scheme changes colour, not layout, so "every state" below means every
// state — a dark pass would restate these same 22 numbers.
//
// The height column is an upper bound because the states differ: SIXTEEN saturate the 856 pt budget
// exactly and SIX come in under it (`connecting` and `unsupported` at 262.50, `crash-looping` 309.00,
// `starting` 315.00, `not-running` 357.00, `empty-roster` 499.50). That partition is not arbitrary and is
// asserted below rather than tabulated: the six are precisely the ROSTER-LESS states. Every state that
// draws even the harness's three-account roster is at the bound at this size class.
//
// ── THE VERDICT ─────────────────────────────────────────────────────────────────────────────────────────
//
// IT FITS, and the ceiling stands where `PanelTypeScale.ceiling` puts it. On the smallest display this
// app's deployment target PLAUSIBLY presents, the ceiling panel clears the width by 529.50 pt and meets the
// height budget exactly; fed the measured footprint, `StatusItemChrome.panelFrame` — the app's own
// placement — returns a rect wholly inside the visible frame from every status-item position.
//
// PLAUSIBLY IS NOT "SMALLEST", and this suite must not be read as having established a worst case. macOS 13
// also supports the 12-inch MacBook (2017), which defaults to 1280 × 800 pt; 1440 × 900 is that machine's
// largest scaled mode. `StatusPanelFormat.smallestPlausibleDisplayHeight` carries the full statement. What
// it costs each axis differs: the WIDTH verdict holds anywhere at or above 1024 pt (894.50 pt clears the
// 1008 pt such a display leaves), so it is a worst case in practice. The HEIGHT verdict is 1440 × 900's
// alone and does not generalise downward — 800 pt would leave 756 pt against an 856 pt budget. That is
// #818's fixed-budget decision behaving as RECORDED, which is not the same as ratified: `design/README.md`
// § The scroll boundary (#818) carries that bound as DECIDED IN CODE, PENDING RATIFICATION, and
// `StatusPanelFormat.panelHeightBudget` says the same beside the constant. Not a hole in this measurement
// either way, and issue #1176 carries deriving the budget from the live screen.
//
// READ THE HEIGHT HALF AS "BOUNDED AND SCROLLABLE", NOT "FITS WITHOUT SCROLLING". They are different
// answers and only the first is true here: at the ceiling, sixteen of the twenty-two states show their
// first screenful and reach the rest by scrolling. "The panel fits" is a claim about the WINDOW landing on
// the screen, never about the content being visible at once. Issue #983's phrasing predates that
// distinction existing — #818 created it — so it is spelled out rather than left to the reader.
//
// ── WHAT THIS SUITE DOES NOT SAY ────────────────────────────────────────────────────────────────────────
//
//   * NOT that the panel fits a display with a DOCK. The 856 pt budget charges 44 pt of chrome; a real
//     1080 pt display reports a 960 pt `visibleFrame`, i.e. 120 pt. That gap is scale-INDEPENDENT — a
//     10-account roster reaches the same bound at the DEFAULT text size (`PanelRosterGeometryTests`
//     measures 1061.00 pt) — so it is not the ceiling's defect and not this suite's question. It is
//     `popoverChromeAllowance`'s own recorded caveat, and issue #1176 carries deriving the budget from the
//     live screen.
//   * NOT a photograph of a panel on a screen. The panel is `NSPanel`-hosted and cannot be opened
//     programmatically or captured without Screen-Recording TCC (`RenderPanelTool`'s header), so what is
//     measured is the LAYOUT FOOTPRINT in points, from the same `ImageRenderer` path the app's design
//     oracle and the committed goldens use, against a display's geometry in points.
//   * NOT that elements inside the panel are laid out correctly at the ceiling — overlap is issue #896 and
//     clipping was issue #757. This suite measures the panel's outer box and nothing within it.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

@MainActor
final class PanelCeilingFitTests: XCTestCase {

    // MARK: - The display, and the room it leaves

    /// The smallest display this app's deployment target plausibly presents, as the two hardware
    /// assumptions `StatusPanelFormat` already owns — read, never restated, so this suite cannot come to
    /// describe a different screen than the shipped height budget was derived from.
    private static var displaySize: CGSize {
        CGSize(width: StatusPanelFormat.smallestPlausibleDisplayWidth,
               height: StatusPanelFormat.smallestPlausibleDisplayHeight)
    }

    /// The width a panel may occupy there: the display less `StatusItemChrome.screenInset` on BOTH edges,
    /// because that is the margin the app's own placement clamp keeps.
    private static var availableWidth: Double {
        displaySize.width - 2 * Double(StatusItemChrome.screenInset)
    }

    /// The height a panel may occupy there. Deliberately the SAME arithmetic `panelHeightBudget` performs,
    /// composed from the same three constants rather than reading the budget — so if someone re-tunes the
    /// budget alone, this suite disagrees with it instead of silently agreeing.
    private static var availableHeight: Double {
        displaySize.height - StatusPanelFormat.menuBarHeight - StatusPanelFormat.popoverChromeAllowance
    }

    /// The display's visible frame under the SAME chrome model the budget assumes: the menu bar excluded,
    /// the Dock deliberately not modelled (see the header's caveat and issue #1176).
    private static var visibleFrame: NSRect {
        NSRect(x: 0, y: 0,
               width: displaySize.width, height: displaySize.height - StatusPanelFormat.menuBarHeight)
    }

    // MARK: - Measuring

    /// The panel's rasterized footprint in POINTS at `size`, composed exactly as it ships.
    ///
    /// `composingScrollBoundaries: true` is the whole point: the height being measured is the BOUNDED
    /// panel's, so a bypassed render would report the unbounded intrinsic height and answer a question
    /// nobody asked. Such a render carries a blank body by construction — heights and the pinned chrome are
    /// all it can be asked, which is all this suite asks (`\.panelScrollBoundaryEnabled`).
    private static func footprint(_ fixture: PanelRenderFixture, _ size: DynamicTypeSize) -> CGSize? {
        guard let cg = PanelRenderHarness.render(fixture, scheme: .light, dynamicTypeSize: size,
                                                 composingScrollBoundaries: true) else { return nil }
        return CGSize(width: Double(cg.width) / Double(PanelRenderHarness.scale),
                      height: Double(cg.height) / Double(PanelRenderHarness.scale))
    }

    private static func wallClock() -> Int64 { Int64(Date().timeIntervalSince1970) }

    private func fixtures() -> [PanelRenderFixture] { PanelRenderHarness.fixtures(now: Self.wallClock()) }

    // MARK: - Degenerate-subject guard (issue #748)

    /// Deliberately first. Every verdict below is a comparison against a measured footprint, so a fixture
    /// that failed to rasterize — or rasterized to nothing — would make its "fits" a comparison of nothing,
    /// and a suite that measured nothing would report green.
    func testEveryFixtureRasterizesAtTheCeiling() throws {
        var measured = 0
        for fixture in fixtures() {
            let box = try XCTUnwrap(Self.footprint(fixture, PanelTypeScale.ceiling),
                                    "\(fixture.name) did not rasterize at \(PanelTypeScale.ceiling)")
            XCTAssertGreaterThan(box.width, 0, "\(fixture.name) rasterized to ZERO width at the ceiling")
            XCTAssertGreaterThan(box.height, 0, "\(fixture.name) rasterized to ZERO height at the ceiling")
            measured += 1
        }
        XCTAssertEqual(measured, 22,
                       "measured \(measured) fixtures, not the 22 the harness catalogs. Every verdict in "
                       + "this suite is 'no fixture exceeds …', which a shrunken catalog satisfies by "
                       + "having less to check — re-derive the count against `PanelRenderHarness.fixtures`")
    }

    // MARK: - AC-1 / AC-2: the verdict

    /// THE answer issue #983 asks for: rendered at the ceiling, does the panel fit the smallest display
    /// this app's deployment target PLAUSIBLY presents?
    ///
    /// Both axes, every state, and the measured margin reported either way — a verdict with no number
    /// behind it is the judgement call AC-2 refuses.
    ///
    /// The name says PLAUSIBLE where AC-1 says "a supported display", and that gap is the AC mapping rather
    /// than a slip: macOS 13 also supports the 12-inch MacBook (2017) at 1280 × 800 pt, so what is measured
    /// here is a display small enough to be worth designing against, never the deployment target's floor.
    /// The width verdict holds there too; the height verdict does not generalise below 900 pt and is not
    /// claimed to.
    func testThePanelFitsTheSmallestPlausibleDisplayAtTheCeiling() throws {
        var report: [String] = []
        for fixture in fixtures() {
            let box = try XCTUnwrap(Self.footprint(fixture, PanelTypeScale.ceiling),
                                    "\(fixture.name) did not rasterize at \(PanelTypeScale.ceiling)")
            report.append(String(format: "%@=%.2f×%.2f", fixture.name, box.width, box.height))

            XCTAssertLessThanOrEqual(box.width, Self.availableWidth,
                                     "\(fixture.name) is " + String(format: "%.2f pt", box.width)
                                     + " wide at \(PanelTypeScale.ceiling), past the "
                                     + String(format: "%.2f pt", Self.availableWidth)
                                     + " a \(Int(Self.displaySize.width))-pt-wide display leaves after "
                                     + "\(StatusItemChrome.screenInset) pt of inset per edge. The declared "
                                     + "ceiling no longer fits the display this suite measures against, "
                                     + "which is issue #983's other branch: LOWER `PanelTypeScale.ceiling` "
                                     + "and re-derive `StatusPanelTypeScale`'s header in the same change. "
                                     + "Measured so far: \(report.joined(separator: "; "))")

            XCTAssertLessThanOrEqual(box.height, Self.availableHeight,
                                     "\(fixture.name) is " + String(format: "%.2f pt", box.height)
                                     + " tall at \(PanelTypeScale.ceiling), past the "
                                     + String(format: "%.2f pt", Self.availableHeight)
                                     + " the display leaves. NOT a ceiling verdict: height is bounded by "
                                     + "#818, and this suite recomposes that bound from the three display "
                                     + "constants rather than reading `panelHeightBudget`, so the reachable "
                                     + "cause is that the two now disagree — was the budget re-tuned "
                                     + "without moving the display it is derived from? It is NOT oversized "
                                     + "pinned chrome: `.frame(maxHeight:)` caps the rasterized size, so "
                                     + "chrome that exceeds the budget overflows outside the frame rather "
                                     + "than growing this number. "
                                     + "Measured so far: \(report.joined(separator: "; "))")
        }
        XCTAssertEqual(report.count, 22, "expected 22 footprints, took \(report.count)")
    }

    /// The fit above is worthless if the panel is not actually AT the ceiling — a panel that stopped
    /// consuming the size class would sit at 380.00 pt and clear every bound with room to spare, reporting
    /// the same green. So the subject is pinned: the ceiling footprint must be the DEFAULT one scaled by
    /// the published factor, on the axis that has no bound to hide behind.
    ///
    /// `PanelTypeScaleTests.testThePanelActuallyConsumesTheSizeClass` makes the same check for its own
    /// purpose (issue #756's defect, on one fixture, in pixels). Repeated here rather than cited because it
    /// is this suite's anti-vacuity guard: were it deleted there, every verdict here would go quietly
    /// vacuous instead of red.
    func testTheFitVerdictHasTheCeilingPanelAsItsSubject() throws {
        let fixture = try XCTUnwrap(fixtures().first { $0.name == "healthy" }, "no `healthy` fixture")
        let base = try XCTUnwrap(Self.footprint(fixture, .large))
        let ceiling = try XCTUnwrap(Self.footprint(fixture, PanelTypeScale.ceiling))

        XCTAssertEqual(base.width, StatusPanelFormat.panelContentWidth, accuracy: 0.51,
                       "the default-size panel measured " + String(format: "%.2f pt", base.width)
                       + " rather than its \(StatusPanelFormat.panelContentWidth) pt content width")

        let k = PanelTypeScale.factor(for: PanelTypeScale.ceiling)
        // Half a point of tolerance, and only in the ROUNDING direction: a raster is whole pixels, so
        // 380 × 2.3529… = 894.12 pt lands at 894.50 pt when @2x rounds 1788.24 px up to 1789.
        XCTAssertEqual(ceiling.width, base.width * k, accuracy: 0.51,
                       "the panel is " + String(format: "%.2f pt", ceiling.width) + " wide at "
                       + "\(PanelTypeScale.ceiling); uniform scaling of "
                       + String(format: "%.2f pt", base.width) + " by \(k) requires "
                       + String(format: "%.2f pt", base.width * k) + ". If the two widths have become "
                       + "EQUAL the panel has stopped consuming the size class, and every 'fits' verdict "
                       + "in this suite is about the default-size panel rather than the ceiling one")
        XCTAssertGreaterThan(ceiling.width, base.width,
                             "the ceiling panel is no wider than the default one — this suite is measuring "
                             + "the wrong subject and its verdicts are vacuous")
    }

    // MARK: - The qualifier: fitting means BOUNDED, not fully visible

    /// "It fits" is a claim about the WINDOW, and at this size class it is emphatically not a claim about
    /// the content being visible at once. The partition is asserted rather than tabulated — but the rule it
    /// asserts is scoped to roster CARDINALITY, not roster PRESENCE. An earlier draft of this comment
    /// claimed the looser thing — "adding a fixture does not rot it" — and measurement falsified it.
    ///
    /// WHAT ACTUALLY SATURATES THE BUDGET IS THREE ACCOUNTS. From the committed curve in
    /// `PanelRosterGeometryTests`, at `.accessibility3`: one account measures 451.00 pt and two 672.00 pt —
    /// both comfortably under the 856 pt bound — and only three, at 893.50 pt, clears it. Every
    /// roster-bearing fixture the harness ships happens to carry three (twelve of them) or four (`expiry`,
    /// `pathological-label`, `same-local-part`, `degenerate-label`), which is the only reason the rule holds
    /// across all 22 today. NOTHING PINS THAT. So the assertion below is guarded on the count rather than on
    /// the roster being non-empty: a one- or two-account fixture is under budget by that curve, which is
    /// this rule's SCOPE rather than a regression in the panel, and it records as under-budget instead of
    /// failing. Measured: adding one ordinary one-account roster fixture reds the unguarded form.
    ///
    /// Both halves must be non-empty, and each rules out a different way of being wrong. An empty
    /// saturating half would mean the bound never binds at the ceiling — in which case #818's boundary is
    /// decoration here and the fit above is trivial. An empty under-budget half would mean the panel is
    /// permanently pinned at 856 pt, which is a panel that stopped measuring its content.
    func testAtTheCeilingEveryRosterBearingStateIsAtTheBoundAndOnlyThoseAre() throws {
        let budget = StatusPanelFormat.panelHeightBudget
        // The smallest roster that saturates the budget at the ceiling, from the curve cited above: three.
        // This is what scopes the at-the-bound arm — roster CARDINALITY governs, roster presence does not.
        let saturatingRoster = 3
        var saturating: [String] = []
        var underBudget: [String] = []

        for fixture in fixtures() {
            let box = try XCTUnwrap(Self.footprint(fixture, PanelTypeScale.ceiling),
                                    "\(fixture.name) did not rasterize at \(PanelTypeScale.ceiling)")
            let atBound = abs(box.height - budget) < 1.0
            if atBound { saturating.append(fixture.name) } else { underBudget.append(fixture.name) }

            if fixture.rows.isEmpty {
                XCTAssertFalse(atBound,
                               "\(fixture.name) draws NO roster and is nonetheless at the \(budget) pt "
                               + "bound at \(PanelTypeScale.ceiling) ("
                               + String(format: "%.2f pt", box.height) + "). A state with no unbounded "
                               + "content should not reach the bound — its fixed chrome grew past a whole "
                               + "screen, or the bound stopped tracking content")
            } else if fixture.rows.count >= saturatingRoster {
                XCTAssertTrue(atBound,
                              "\(fixture.name) draws \(fixture.rows.count) accounts and measured "
                              + String(format: "%.2f pt", box.height) + " at \(PanelTypeScale.ceiling), "
                              + "short of the \(budget) pt bound. The recorded verdict is that every state "
                              + "drawing \(saturatingRoster) accounts or more is at the bound at this size "
                              + "class, i.e. that its 'fits' means bounded-and-scrollable. This roster is "
                              + "INSIDE that scope, so the panel's behaviour really has changed — "
                              + "re-derive the verdict in `StatusPanelTypeScale`'s header, do not relax "
                              + "this assertion. (RE-SCOPING is the answer only for a roster SMALLER than "
                              + "\(saturatingRoster), which the curve puts under budget; such a fixture is "
                              + "guarded out above rather than failed here, so it cannot reach this message)")
            }
            // A one- or two-account roster is under the bound by the curve cited above, so it is outside
            // this rule's scope: recorded in whichever half it lands in, asserted neither way. The panel
            // being BOUNDED there is still checked — by the fit verdict, on every state.
        }

        XCTAssertFalse(saturating.isEmpty,
                       "NO state reaches the \(budget) pt bound at \(PanelTypeScale.ceiling), so the "
                       + "boundary never binds here and 'the panel fits' is true of a panel that was never "
                       + "going to overflow. Under budget: \(underBudget.joined(separator: ", "))")
        XCTAssertFalse(underBudget.isEmpty,
                       "EVERY state sits at the \(budget) pt bound at \(PanelTypeScale.ceiling) — the "
                       + "panel is pinned at its budget rather than measuring its content")
    }

    // MARK: - AC-1's "on a supported display": the app's own placement

    /// The strongest form of the verdict available without a screen capture: hand the MEASURED ceiling
    /// footprint to `StatusItemChrome.panelFrame` — the function the app actually places the panel with —
    /// and require the rect it returns to lie wholly inside the display's visible frame.
    ///
    /// The status item's position is swept, because the placement is centred under the icon and then
    /// clamped: the interesting cases are the two edges, where a panel this wide is pushed back on-screen.
    ///
    /// It also puts a standing gate under a comment that had none. `panelFrame`'s clamp-order note records
    /// that `min(max(x, lo), hi)` inverts when `hi < lo` — i.e. when the panel is wider than the visible
    /// frame less both insets — and called that unreachable because "the panel is ~360 pt". At the ceiling
    /// it is 894.50 pt, so the ground moved by ×2.35 even though the conclusion held. Asserted here rather
    /// than left as prose.
    ///
    /// The display in the name is the PLAUSIBLE one; the MARK above quotes AC-1's "supported". That seam is
    /// the header's `PLAUSIBLY IS NOT "SMALLEST"` paragraph, which owns what it costs each axis — and this
    /// verdict reads BOTH, since `visibleFrame` carries the 900 pt height as well as the 1440 pt width.
    func testTheAppsOwnPlacementKeepsTheCeilingPanelOnTheSmallestPlausibleDisplay() throws {
        let fixture = try XCTUnwrap(fixtures().first { $0.name == "healthy" }, "no `healthy` fixture")
        let box = try XCTUnwrap(Self.footprint(fixture, PanelTypeScale.ceiling))
        let size = NSSize(width: box.width, height: box.height)
        let visible = Self.visibleFrame

        // The clamp-order precondition, stated as its own assertion so a failure names the cause rather
        // than the symptom: below this, `panelFrame`'s x-clamp inverts and hangs the panel off the LEFT.
        XCTAssertLessThanOrEqual(size.width, visible.width - 2 * Double(StatusItemChrome.screenInset),
                                 "the ceiling panel (" + String(format: "%.2f pt", size.width) + ") is "
                                 + "wider than the visible frame less both "
                                 + "\(StatusItemChrome.screenInset) pt insets, so "
                                 + "`StatusItemChrome.panelFrame`'s x-clamp INVERTS — the documented "
                                 + "`hi < lo` branch its comment calls unreachable at shipped sizes. It is "
                                 + "reachable now; fix the clamp or lower the ceiling")

        // A status item sits in the menu bar, directly above the visible frame.
        let barHeight = StatusPanelFormat.menuBarHeight
        let iconWidth: CGFloat = 30
        let positions: [(String, CGFloat)] = [("hard left", 0),
                                              ("centre", (visible.width - iconWidth) / 2),
                                              ("hard right", visible.width - iconWidth)]
        var checked = 0
        for (where_, x) in positions {
            let icon = NSRect(x: x, y: visible.maxY, width: iconWidth, height: barHeight)
            let frame = StatusItemChrome.panelFrame(iconFrame: icon, visibleFrame: visible, contentSize: size)
            XCTAssertTrue(visible.contains(frame),
                          "with the status item \(where_), the ceiling panel is placed at \(frame), which "
                          + "is not wholly inside the \(visible) visible frame of a "
                          + "\(Int(Self.displaySize.width))×\(Int(Self.displaySize.height)) pt display. "
                          + "Part of the panel is off-screen at the declared ceiling")
            checked += 1
        }
        XCTAssertEqual(checked, positions.count,
                       "swept \(checked) status-item positions, expected \(positions.count)")
    }
}
#endif
