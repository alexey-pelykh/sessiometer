// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The panel's SCROLL BOUNDARY (issue #818) — does content past the bottom of the screen stay reachable,
// and does the chrome that must never scroll away actually stay put?
//
// WHAT WAS BROKEN. The panel was fixed in width and INTRINSIC in height, and `NSPopover` sizes itself to
// that intrinsic height, so a tall enough body simply ran off the bottom of the screen with nothing to
// scroll. Measured on this render path at `f66a341`, BEFORE the boundary landed:
//
//   surface                      │ `.large` │ `.accessibility3`
//   ─────────────────────────────┼──────────┼──────────────────
//   healthy (3 accounts)         │   449.00 │           1031.00
//   stats (3 accounts)           │   562.00 │           1300.50
//   expiry (4 accounts)          │   637.00 │           1457.00
//   fault-systemic-refresh       │   524.00 │           1198.50
//
// SIXTEEN of the twenty-two rendered fixtures exceeded the 856 pt budget at `.accessibility3`; none did at
// the default text size. And `PanelRosterGeometryTests` measures the axis that makes it unbounded rather
// than merely large: one account costs a flat 96.00 pt at the default size and ≈221.30 pt at
// `.accessibility3`, linearly, with no plateau anywhere out to 50 accounts (4901.00 pt).
//
// ── WHY A HEIGHT MEASUREMENT IS NOT ENOUGH, AND WHAT THIS SUITE MEASURES INSTEAD ────────────────────────
//
// "The panel is now 856 pt" is satisfied by a fix that is strictly WORSE than the defect. Capping the
// panel without a scroll view crops it — measured on this render path, a bare `.frame(maxHeight:)` over the
// same content renders the panel's MIDDLE, with the header AND the footer gone. The bound would read green
// and the unreachable region would simply have moved. Asserting the presence of a `ScrollView` is no better
// in the other direction: it is satisfied by a boundary that never binds, which is today's panel with a
// wrapper around it.
//
// So the load-bearing assertion here is a RELATION between two independently measured heights —
// how much content the boundary holds, against how much of it the viewport can show:
//
//   • REACHABLE — `rosterContentHeight`, the roster's own intrinsic height, rendered OUTSIDE the panel so
//     no boundary can bound it. This is what the operator can get to by scrolling.
//   • AVAILABLE — `budget − pinnedChrome`, the viewport the boundary is left after every pinned element
//     has taken its ideal height. This is what the operator sees at rest.
//
// A fix that clipped would leave REACHABLE unchanged and lose the chrome; a fix that squeezed would shrink
// REACHABLE; a wrapper that never binds would leave the panel over budget. Each is a distinct failure and
// each has an assertion below.
//
// ── CONSTRAINT-A (issue #748) — NO GATE WITHOUT A PROVEN FALSIFIER ─────────────────────────────────────
//
// `testTheGateRejectsBothRivalLayouts` drives the suite's OWN two predicates — `fitsBudget` and
// `retainsPinnedChrome`, the exact functions every real assertion here gates on — over THREE compositions
// of the same content:
//
//   composition                             │ fitsBudget │ retainsPinnedChrome │ is
//   ────────────────────────────────────────┼────────────┼─────────────────────┼──────────────────────────
//   pinned chrome + SCROLLING body          │ pass       │ pass                │ what shipped
//   pinned chrome + CAPPED, unscrolling body│ pass       │ FAIL                │ the clipping near-miss
//   pinned chrome + UNBOUNDED body          │ FAIL       │ pass                │ the panel before this fix
//
// Neither predicate alone separates the three, which is the point: a suite carrying only the height check
// would have accepted the clipping rival, and one carrying only the chrome check would have accepted the
// unbounded panel it was written to replace. The two rivals fail DISJOINT predicates, so no constant can
// satisfy the pair — the same argument `PanelRosterGeometryTests`' canary makes for its bound.
//
// ── WHAT THIS SUITE CANNOT SEE, STATED RATHER THAN GLOSSED ─────────────────────────────────────────────
//
// `ImageRenderer` rasterizes at scroll offset ZERO and nothing here can scroll it. A `ScrollView` and a
// plain top-anchored clip of the same content are therefore PIXEL-IDENTICAL to this suite: both lay the
// body out at full height and show its top. What the measurements above DO establish is that the content
// is complete, that it exceeds the viewport, and that the pinned chrome survived — everything except that
// the container will move under a gesture.
//
// That last step is closed the way this project already closes an unrenderable structural claim
// (`PanelReachabilityLintTests`, which reads `StatusItemController.swift` as data): `testEveryUnboundedBody
// IsWrappedInTheScrollBoundary` reads `StatusPanelView.swift` as TEXT and requires each unbounded body to
// be inside a `PanelScrollBoundary`, and that `PanelScrollBoundary` be a `ScrollView`. It is a source
// predicate and carries a source predicate's honest reach — it says WIRED, not DELIVERED. The real
// popover's scroll gesture stays a manual pre-release check (design/README.md).

#if DEBUG
import AppKit
import SwiftUI
import XCTest

// MARK: - The measurement primitives and THE two predicates

@MainActor
enum ScrollBoundary {

    // The budget and the predicates are `nonisolated` — pure value work, the same split
    // `PanelRenderHarness` and `PanelGeometry` make for their naming/scale surfaces.

    /// The shipped bound, read from the ONE place it is defined. Never a second copy of the arithmetic:
    /// a suite that re-derived `900 − 24 − 20` here would keep passing after the panel's budget moved.
    nonisolated static var budget: Double { StatusPanelFormat.panelHeightBudget }

    // MARK: THE predicates

    /// `true` when a panel of `height` points fits the budget — the bound half of the gate.
    nonisolated static func fitsBudget(_ height: Double, budget: Double = budget) -> Bool {
        height <= budget
    }

    /// `true` when `tall` still draws the same pinned bottom chrome `short` does — the un-clipped half.
    ///
    /// Compares two bottom bands through `PanelRaster.diffFraction`, the metric the golden gate and the
    /// interaction-state gate already share: the fraction of pixels whose largest channel delta exceeds
    /// 64/255. Required to be EXACTLY zero — not a ceiling — so this stays a claim that no pixel moved
    /// meaningfully rather than a tuned tolerance.
    ///
    /// NOT byte equality, and the reason is measured rather than defensive. At the default text size the
    /// two bands differ on exactly ONE pixel by exactly 1/255 — the rasterizer's own start-up transient
    /// that `PanelRenderHarness` § Steady state documents at length (issue #824), which the settle reduces
    /// but does not eliminate. A byte-exact predicate reddens on that and says "the footer was cropped",
    /// which is a false alarm about the most alarming failure this suite has. The 64/255 threshold is 64×
    /// that noise and still ~1/4 of full scale, so a band that genuinely lost its chrome — flat colour
    /// against text and a callout — cannot slip under it; the CONSTRAINT-A canary measures a cropped band
    /// at a diff fraction of 1.
    ///
    /// Both bands come from the SAME process in the same run, so this comparison is cross-machine immune by
    /// construction — the same reasoning that keeps `PanelGoldenParityTests`' same-run assertions in the
    /// required job while its committed-golden comparison stays soft.
    nonisolated static func retainsPinnedChrome(tall: PanelRaster?, short: PanelRaster?) -> Bool {
        guard let tall, let short, tall.width > 0, tall.height > 0 else { return false }
        return PanelRaster.diffFraction(tall, short) == 0
    }

    // MARK: Rendering

    /// A healthy `n`-account roster whose only varying property is CARDINALITY — the same construction
    /// `PanelRosterGeometryTests.roster` uses, and for the same reason: every row carries identical
    /// percents and reset instants, so a height difference is attributable to the row count and nothing
    /// else. Reset offsets keep `PanelRenderHarness.boundaryGuardSecs` clear of a `humanizeUntil` rounding
    /// boundary so a sub-second delay between seeding and rasterizing cannot reflow a cell.
    static func roster(_ n: Int, now: Int64) -> [AccountRow] {
        let sessionReset = now + 2 * 3600 + 14 * 60 + PanelRenderHarness.boundaryGuardSecs
        let weeklyReset = now + 3 * 86_400 + PanelRenderHarness.boundaryGuardSecs
        return (0..<n).map { i in
            AccountRow(label: "Account-\(i)", isActive: i == 0, isEnabled: true,
                       isQuarantined: false, isRecovering: false, auth: .healthy,
                       sessionPct: 42, weeklyPct: 88,
                       sessionResetsAt: sessionReset, weeklyResetsAt: weeklyReset,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil)
        }
    }

    /// The WHOLE panel, rendered through `PanelRenderHarness` — the shared path the app's `--render-panel`
    /// tool and the golden gate both use, so every other way the panel is wired here is the way it ships.
    ///
    /// `composingScrollBoundaries: true` is the one departure from those two consumers, and it is the
    /// departure that makes this suite about #818 at all. They rasterize the panel with its boundaries
    /// bypassed because `ImageRenderer` cannot draw a `ScrollView`; measured through that default, the tree
    /// this suite sees would be an unbounded body under a hard height cap — which is `RivalLayout
    /// .cappedWithoutScrolling`, the layout `testTheGateRejectsBothRivalLayouts` exists to reject. The gate
    /// would then pass on the very defect it forbids, and every height below would be measuring a crop.
    ///
    /// The cost is that these rasters carry a BLANK body (that is the same `ImageRenderer` limitation seen
    /// from the other side), so nothing here may ask a question about scrolled CONTENT pixels. Heights and
    /// the pinned chrome are outside the boundary and draw normally, which is what this suite asks about;
    /// the content's own reachability is measured as height, and its presence in the a11y tree is pinned by
    /// `PanelAccessibilityTreeTests`.
    static func panel(_ n: Int, size: DynamicTypeSize, now: Int64,
                      nextSwap: NextSwap? = nil) -> CGImage? {
        let fixture = PanelRenderFixture(name: "scroll-boundary-\(n)", state: .connected,
                                         rows: roster(n, now: now), nextSwap: nextSwap,
                                         generatedAt: now - 12)
        return PanelRenderHarness.render(fixture, scheme: .light, dynamicTypeSize: size,
                                         composingScrollBoundaries: true)
    }

    static func panelHeight(_ n: Int, size: DynamicTypeSize, now: Int64,
                            nextSwap: NextSwap? = nil) -> Double? {
        panel(n, size: size, now: now, nextSwap: nextSwap)
            .map { Double($0.height) / Double(PanelRenderHarness.scale) }
    }

    /// The ROSTER's own intrinsic height — the content the boundary holds, and the "REACHABLE" half of the
    /// relation this suite asserts.
    ///
    /// Rendered STANDALONE rather than inside the panel, which is the whole point: inside, the boundary
    /// bounds it and the measurement would read back the viewport instead of the content. Laid out at the
    /// panel's own scaled width with the panel's own scale factor and accent, so it is the same roster the
    /// panel draws — only unbounded.
    /// Delegated to `PanelGeometry.contentHeight` rather than rendered here, because both suites now need
    /// this exact quantity and two copies of a measurement can disagree at a boundary — the reason
    /// `PanelRosterGeometryTests`' own header gives for keeping its predicate in one place, applied the
    /// moment a second consumer appeared.
    static func rosterContentHeight(_ n: Int, size: DynamicTypeSize, now: Int64) -> Double? {
        PanelGeometry.contentHeight(rows: roster(n, now: now), size: size, now: now)
    }

    // MARK: Raster banding

    /// The bottom `points` of `image`, normalized into the same sRGB / RGBA8 representation every other
    /// panel raster comparison in this bundle uses.
    ///
    /// A BAND rather than the whole raster, deliberately, and its DEPTH is load-bearing in both directions.
    /// Too shallow and a footer that survived while the swap callout above it did not would still read
    /// green. Too deep and the band reaches up past the pinned chrome into the boundary itself, where two
    /// rosters legitimately differ — measured at `.accessibility3`, an 80 pt band already scores 0.012 and
    /// a 140 pt one 0.018 against a 0.000 at 40 pt, because the deeper bands are comparing two rows against
    /// fifty. It must also never reach the header, whose sub-line states the account COUNT
    /// (`StatusPanelFormat.headerSubtitle`) and so differs by design.
    nonisolated static func bottomBand(_ image: CGImage, points: Double) -> PanelRaster? {
        let rows = Int(points * Double(PanelRenderHarness.scale))
        guard rows > 0, image.height >= rows, image.width > 0,
              let space = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        var bytes = [UInt8](repeating: 0, count: image.width * rows * 4)
        let ok: Bool = bytes.withUnsafeMutableBytes { raw -> Bool in
            guard let base = raw.baseAddress,
                  let ctx = CGContext(data: base, width: image.width, height: rows,
                                      bitsPerComponent: 8, bytesPerRow: image.width * 4, space: space,
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            ctx.setBlendMode(.copy)
            // Draw the full image into a context only `rows` tall, anchored so the image's BOTTOM edge
            // lands on the context's bottom edge — CoreGraphics' origin is bottom-left, so a y of 0 keeps
            // the bottom band and lets the rest overflow the top.
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            return true
        }
        return ok ? PanelRaster(width: image.width, height: rows, bytes: bytes) : nil
    }
}

// MARK: - The rival layouts (CONSTRAINT-A's subjects)

/// The three ways a panel-shaped stack can meet — or fail to meet — a height budget. Rendered from the
/// SAME content so the predicates are separating the COMPOSITION and nothing else.
private enum RivalLayout: String, CaseIterable {
    /// Pinned chrome, with the unbounded body inside a scroll boundary. What shipped.
    case scrolling
    /// Pinned chrome, with the whole stack capped and no boundary. The near-miss: it meets the budget by
    /// cropping, so the bound reads green while the chrome is gone.
    case cappedWithoutScrolling
    /// Pinned chrome, unbounded body, no cap at all. The panel before this fix.
    case unbounded
}

@MainActor
private enum RivalRenderer {
    /// A three-part stack in the panel's own shape: fixed chrome above, an unbounded body, fixed chrome
    /// below. The bands are flat colours so a lost one is unambiguous — this canary is about the LAYOUT,
    /// and borrowing the real panel's views would only add ways for it to be inconclusive.
    static func render(_ layout: RivalLayout, body: CGFloat) -> CGImage? {
        let stack = VStack(spacing: 0) {
            Color.red.frame(height: 40)                             // "pinned header"
            if layout == .cappedWithoutScrolling {
                Color.blue.frame(height: body)                      // unbounded body, no boundary
            } else {
                ScrollView(.vertical) { Color.blue.frame(height: body) }
            }
            Color.green.frame(height: 40)                           // "pinned footer"
        }
        .frame(width: CGFloat(StatusPanelFormat.panelContentWidth), alignment: .leading)

        let content: AnyView = layout == .unbounded
            ? AnyView(stack.fixedSize(horizontal: false, vertical: true))
            : AnyView(stack.frame(maxHeight: CGFloat(ScrollBoundary.budget))
                .fixedSize(horizontal: false, vertical: true))
        let renderer = ImageRenderer(content: content)
        renderer.scale = PanelRenderHarness.scale
        return renderer.cgImage
    }
}

// MARK: - The suite

@MainActor
final class PanelScrollBoundaryTests: XCTestCase {

    private static func wallClock() -> Int64 { Int64(Date().timeIntervalSince1970) }

    /// Both ends of the range the panel declares support over. The scaled case is the one that forces the
    /// boundary — measured, the default text size fits every committed fixture and `.accessibility3` fits
    /// almost none.
    private static let sizeClasses: [(DynamicTypeSize, String)] = [(.large, "large"),
                                                                   (.accessibility3, "accessibility3")]

    /// How deep a bottom band the chrome comparison reads — 40 pt, MEASURED rather than picked. See
    /// `ScrollBoundary.bottomBand` for the two directions it is bounded in; at this depth the two panels
    /// score 0.000000 at `.accessibility3` and differ on a single pixel by 1/255 at the default size,
    /// while 80 pt already reaches into the boundary and scores 0.012.
    private static let chromeBandPoints: Double = 40

    // MARK: - Degenerate-subject guard (issue #748)

    /// Deliberately first. Everything below reads heights off rasters; a rasterizer returning nil, or a
    /// zero-height image, would make every comparison here a comparison of nothing — and a suite that
    /// measured nothing would report green.
    func testTheSubjectsRasterizeAndCarryHeight() throws {
        let now = Self.wallClock()
        for (size, name) in Self.sizeClasses {
            let panel = try XCTUnwrap(ScrollBoundary.panelHeight(3, size: size, now: now),
                                      "the 3-account panel did not rasterize at \(name)")
            let content = try XCTUnwrap(ScrollBoundary.rosterContentHeight(3, size: size, now: now),
                                        "the standalone roster did not rasterize at \(name)")
            XCTAssertGreaterThan(panel, 0, "the panel rasterized to ZERO height at \(name)")
            XCTAssertGreaterThan(content, 0, "the roster rasterized to ZERO height at \(name)")
            XCTAssertLessThan(content, panel,
                              "the standalone roster (\(content) pt) is not SHORTER than the panel that "
                              + "contains it (\(panel) pt) at \(name) — the two measurements are not "
                              + "measuring what this suite thinks they are")
        }
    }

    // MARK: - AC-1: the bound holds, and it actually binds

    /// The panel never exceeds its budget, at any cardinality, at either end of the type-scale range.
    ///
    /// Both halves matter. A bound nothing reaches is vacuous, so the large cardinalities must land EXACTLY
    /// on the budget — that is the boundary doing work rather than the content happening to be small. And a
    /// bound that fires on everything would be a panel permanently pinned at 856 pt, so the small
    /// cardinalities must come in strictly UNDER it.
    func testThePanelNeverExceedsItsBudgetAndTheBudgetActuallyBinds() throws {
        let now = Self.wallClock()
        var report: [String] = []
        for (size, name) in Self.sizeClasses {
            for n in [1, 3, 10, 20, 50] {
                let height = try XCTUnwrap(ScrollBoundary.panelHeight(n, size: size, now: now),
                                           "the \(n)-account panel did not rasterize at \(name)")
                report.append("\(n)@\(name)=" + String(format: "%.2f", height))
                XCTAssertTrue(ScrollBoundary.fitsBudget(height),
                              "the \(n)-account panel measured " + String(format: "%.2f pt", height)
                              + " against the \(ScrollBoundary.budget) pt budget at \(name). The scroll "
                              + "boundary is not bounding the panel — content past the budget is off the "
                              + "bottom of the screen and unreachable, which is issue #818 reopening. "
                              + "Measured so far: \(report.joined(separator: "; "))")
            }
            // The bound BINDS: a 50-account roster is far past any budget, so the panel must sit exactly
            // on it. Without this the whole test above is satisfied by a panel that never grows.
            let saturated = try XCTUnwrap(ScrollBoundary.panelHeight(50, size: size, now: now))
            XCTAssertEqual(saturated, ScrollBoundary.budget, accuracy: 1.0,
                           "a 50-account panel measured " + String(format: "%.2f pt", saturated)
                           + " rather than saturating the \(ScrollBoundary.budget) pt budget at \(name). "
                           + "Either the panel stopped growing with its roster — in which case every "
                           + "'fits' verdict above is vacuous — or the budget moved")
            // And it does NOT fire on everything: one account is comfortably inside.
            let single = try XCTUnwrap(ScrollBoundary.panelHeight(1, size: size, now: now))
            XCTAssertLessThan(single, ScrollBoundary.budget,
                              "a ONE-account panel measured " + String(format: "%.2f pt", single)
                              + " at \(name), i.e. at or over the budget. The boundary is binding on a "
                              + "panel that fits, which would mean it is scrolling content that has room")
        }
        XCTAssertEqual(report.count, 2 * 5,
                       "expected \(2 * 5) (size class × cardinality) measurements, took \(report.count)")
    }

    // MARK: - AC-1 / AC-3: REACHABLE against AVAILABLE, at both size classes

    /// The load-bearing assertion, and the one the issue actually asks for: how much content the boundary
    /// holds, against how much of it the viewport can show.
    ///
    /// REACHABLE is the roster's own intrinsic height, measured outside the panel. AVAILABLE is what the
    /// viewport is left after every pinned element takes its ideal height, derived from a cardinality small
    /// enough that the bound does not bind — so it is measured, not assumed.
    ///
    /// Three claims, and each rules out a different wrong fix:
    ///   • REACHABLE is the FULL roster — every row is laid out, so the boundary is carrying content rather
    ///     than a squeezed version of it.
    ///   • REACHABLE strictly EXCEEDS AVAILABLE at the large cardinalities — there genuinely is content
    ///     beyond the viewport, which is what makes a scroll boundary the fix rather than decoration.
    ///   • the panel is nonetheless within budget — the excess is held, not displayed.
    func testTheBoundaryHoldsMoreRosterThanTheViewportCanShowAtBothSizeClasses() throws {
        let now = Self.wallClock()
        var report: [String] = []
        for (size, name) in Self.sizeClasses {
            // Chrome is measured where nothing scrolls: at ONE account the panel is inside the budget, so
            // its height is the honest sum of pinned chrome and the whole roster.
            let onePanel = try XCTUnwrap(ScrollBoundary.panelHeight(1, size: size, now: now))
            let oneRoster = try XCTUnwrap(ScrollBoundary.rosterContentHeight(1, size: size, now: now))
            let twoRoster = try XCTUnwrap(ScrollBoundary.rosterContentHeight(2, size: size, now: now))
            let pinnedChrome = onePanel - oneRoster
            let available = ScrollBoundary.budget - pinnedChrome
            let rowCost = twoRoster - oneRoster

            XCTAssertGreaterThan(rowCost, 0,
                                 "an account costs " + String(format: "%.2f pt", rowCost) + " of roster at "
                                 + "\(name) — the roster is not reaching the measurement, so every "
                                 + "comparison below would be comparing constants")
            XCTAssertGreaterThan(available, 0,
                                 "the pinned chrome alone is " + String(format: "%.2f pt", pinnedChrome)
                                 + " at \(name), leaving the boundary no viewport inside the "
                                 + "\(ScrollBoundary.budget) pt budget. Pinning has outgrown the screen: "
                                 + "the chrome itself would now be clipped, which is a worse defect than "
                                 + "the one this boundary fixes — re-open the pin/scroll split in "
                                 + "design/README.md § The scroll boundary rather than widening the budget")

            for n in [10, 20, 50] {
                let reachable = try XCTUnwrap(ScrollBoundary.rosterContentHeight(n, size: size, now: now),
                                              "the \(n)-account roster did not rasterize at \(name)")
                let panel = try XCTUnwrap(ScrollBoundary.panelHeight(n, size: size, now: now))
                report.append("\(n)@\(name): reachable=" + String(format: "%.2f", reachable)
                              + " available=" + String(format: "%.2f", available)
                              + " panel=" + String(format: "%.2f", panel))

                // Every row is still laid out — the boundary holds the whole roster, not a truncation of it.
                //
                // The tolerance is the RASTER QUANTUM accumulated over the rows, not a round number: at
                // `PanelRenderHarness.scale` = 2 a row's height can land half a point either side of its
                // ideal, and at `.accessibility3` the scale factor is fractional (×2.3529) so it genuinely
                // does — measured, the 50-row roster comes in 10.00 pt under a flat 50 × 221.50 pt
                // prediction, an accumulation of ~0.2 pt per row and not a missing row. Half a point per
                // row admits exactly that and nothing else: the smallest defect this can hide is 24.50 pt
                // at n = 50, against the 221.50 pt a single dropped row would cost.
                let predicted = oneRoster + Double(n - 1) * rowCost
                XCTAssertEqual(reachable, predicted, accuracy: Double(n) * 0.5,
                               "the \(n)-account roster measured "
                               + String(format: "%.2f pt", reachable) + " against the "
                               + String(format: "%.2f pt", predicted)
                               + " that \(n) rows cost at " + String(format: "%.2f pt", rowCost)
                               + " each (\(name)), a gap of "
                               + String(format: "%.2f pt", abs(reachable - predicted)) + " against the "
                               + String(format: "%.2f pt", Double(n) * 0.5) + " half-a-point-per-row "
                               + "quantum. That is more than rounding: rows are being dropped or "
                               + "compressed rather than held, so the content beyond the viewport is NOT "
                               + "reachable by scrolling to it")

                // And there is genuinely something beyond the viewport to scroll to.
                XCTAssertGreaterThan(reachable, available,
                                     "the \(n)-account roster is "
                                     + String(format: "%.2f pt", reachable) + " against a "
                                     + String(format: "%.2f pt", available) + " viewport at \(name) — it "
                                     + "FITS, so this cardinality no longer exercises the boundary and the "
                                     + "assertion above proves nothing about overflow. Raise the "
                                     + "cardinality rather than deleting the check")

                XCTAssertTrue(ScrollBoundary.fitsBudget(panel),
                              "the \(n)-account panel is over budget at \(name): \(report.last ?? "")")
            }
        }
        XCTAssertEqual(report.count, 2 * 3,
                       "expected \(2 * 3) (size class × cardinality) measurements, took \(report.count)")
    }

    // MARK: - AC-2: the pinned chrome survives a roster that overflows

    /// The chrome an operator must never have to scroll for is still DRAWN when the roster overflows.
    ///
    /// This is the assertion that separates a scroll boundary from a crop, and it is a pixel claim rather
    /// than an arithmetic one: the bottom band of a 50-account panel must be byte-identical to the bottom
    /// band of a 2-account panel. That band is the swap callout, its status line and the snapshot-age
    /// footer — the panel's ONE primary action and its freshness signal. Under a crop the band is roster
    /// rows, or nothing at all.
    ///
    /// The `nextSwap` target is the SAME account in both panels, so the callout's own copy is identical and
    /// a difference in the band can only be a difference in what survived.
    func testTheSwapActionAndFooterStillRenderUnderAnOverflowingRoster() throws {
        let now = Self.wallClock()
        let target = NextSwap.target(to: "Account-1",
                                     reason: .soonestReset(resetsAt: now + 3 * 86_400
                                                           + PanelRenderHarness.boundaryGuardSecs))
        for (size, name) in Self.sizeClasses {
            let short = try XCTUnwrap(ScrollBoundary.panel(2, size: size, now: now, nextSwap: target),
                                      "the 2-account panel did not rasterize at \(name)")
            let tall = try XCTUnwrap(ScrollBoundary.panel(50, size: size, now: now, nextSwap: target),
                                     "the 50-account panel did not rasterize at \(name)")
            // The two panels must be the same WIDTH for a band comparison to mean anything.
            XCTAssertEqual(tall.width, short.width,
                           "the panel changed width with its roster at \(name), so the bands below are "
                           + "not comparable")
            let shortBand = ScrollBoundary.bottomBand(short, points: Self.chromeBandPoints)
            let tallBand = ScrollBoundary.bottomBand(tall, points: Self.chromeBandPoints)
            XCTAssertTrue(ScrollBoundary.retainsPinnedChrome(tall: tallBand, short: shortBand),
                          "the bottom \(Self.chromeBandPoints) pt of a 50-account panel at \(name) differs "
                          + "from the same band of a 2-account one. The swap callout, its status line and "
                          + "the snapshot-age footer are pinned OUTSIDE the scroll boundary precisely so a "
                          + "long roster cannot push them off — a difference here means the panel met its "
                          + "budget by CROPPING rather than by scrolling, which relocates issue #818's "
                          + "unreachable region instead of removing it")
        }
    }

    // MARK: - CONSTRAINT-A: the gate rejects both rival layouts

    /// Both predicates, driven over three compositions of the same content — the proof that this suite can
    /// fail, and that it takes BOTH halves to separate the shipped layout from its two near-misses.
    ///
    /// Run rather than argued. The clipping rival is not hypothetical: it is what a `.frame(maxHeight:)`
    /// applied without a boundary actually does on this render path, and it is the fix a reader reaches for
    /// first because it satisfies "the panel must not exceed the screen" completely.
    func testTheGateRejectsBothRivalLayouts() throws {
        // Tall enough to blow the budget several times over, so no composition passes by accident.
        let tallBody = CGFloat(ScrollBoundary.budget * 3)
        let shortBody: CGFloat = 60

        var verdicts: [RivalLayout: (fits: Bool, chrome: Bool)] = [:]
        for layout in RivalLayout.allCases {
            let tall = try XCTUnwrap(RivalRenderer.render(layout, body: tallBody),
                                     "the \(layout.rawValue) rival did not rasterize")
            let short = try XCTUnwrap(RivalRenderer.render(layout, body: shortBody),
                                      "the short \(layout.rawValue) rival did not rasterize")
            let height = Double(tall.height) / Double(PanelRenderHarness.scale)
            verdicts[layout] = (
                fits: ScrollBoundary.fitsBudget(height),
                chrome: ScrollBoundary.retainsPinnedChrome(
                    tall: ScrollBoundary.bottomBand(tall, points: Self.chromeBandPoints),
                    short: ScrollBoundary.bottomBand(short, points: Self.chromeBandPoints))
            )
        }

        let shipped = try XCTUnwrap(verdicts[.scrolling])
        XCTAssertTrue(shipped.fits && shipped.chrome,
                      "the SHIPPED composition — pinned chrome around a scrolling body — must satisfy both "
                      + "predicates, or the two real assertions above are failing for a reason that has "
                      + "nothing to do with the panel. Measured: \(verdicts)")

        let cropped = try XCTUnwrap(verdicts[.cappedWithoutScrolling])
        XCTAssertTrue(cropped.fits,
                      "the clipping rival is supposed to MEET the budget — that is what makes it dangerous. "
                      + "If it does not, this canary is not exercising the near-miss it names")
        XCTAssertFalse(cropped.chrome,
                       "a stack capped WITHOUT a scroll boundary kept its pinned bottom chrome. Measured on "
                       + "this render path a bare cap renders the stack's MIDDLE, losing both ends — if it "
                       + "no longer does, then `retainsPinnedChrome` can no longer tell a crop from a "
                       + "scroll, and AC-2's assertion above is decoration")

        let unbounded = try XCTUnwrap(verdicts[.unbounded])
        XCTAssertFalse(unbounded.fits,
                       "the UNBOUNDED composition — the panel as it was before this fix — fits the "
                       + "\(ScrollBoundary.budget) pt budget. Either the budget grew past three times "
                       + "itself, or `fitsBudget` cannot fail")
        XCTAssertTrue(unbounded.chrome,
                      "the unbounded composition lost its chrome, which it cannot do — nothing crops it. "
                      + "That would mean `retainsPinnedChrome` fires on something other than cropping, so "
                      + "the clipping verdict above proves nothing")

        // The two rivals fail DISJOINT predicates. Stated as its own assertion because it is the reason
        // the suite needs both: either predicate alone accepts one of the two layouts this fix replaced.
        XCTAssertNotEqual(cropped.fits, unbounded.fits)
        XCTAssertNotEqual(cropped.chrome, unbounded.chrome)
    }

    // MARK: - The structural half: WIRED, not DELIVERED

    /// WHAT IS INSIDE THE BOUNDARY AND WHAT IS OUTSIDE IT — the pin/scroll split, asserted.
    ///
    /// A source predicate, and it carries a source predicate's honest reach — the same one
    /// `PanelReachabilityLintTests` states for its own subject: this says the container is WIRED, never
    /// that a gesture DELIVERS.
    ///
    /// TWO CLAIMS NEITHER RENDER NOR ARITHMETIC CAN MAKE, which is why this file needs a lint at all:
    ///
    ///   1. That the container SCROLLS rather than clips. `ImageRenderer` rasterizes at scroll offset
    ///      zero, so a `ScrollView` and a top-anchored clip of the same content are pixel-identical to
    ///      every measurement above.
    ///   2. That the honest-state banners are PINNED. The fault banner sits ABOVE the roster in the view
    ///      tree, so at offset zero it draws at the top of the panel whether it is pinned or merely the
    ///      first thing inside the boundary. A render cannot separate those two; only scrolling could,
    ///      and nothing here can scroll. The swap callout and the footer are the other way round — they
    ///      sit BELOW, so `testTheSwapActionAndFooterStillRenderUnderAnOverflowingRoster` proves their
    ///      pinning empirically and this only corroborates it.
    ///
    /// CONTAINMENT IS RESOLVED BY BRACE MATCHING, not by a proximity window: each `PanelScrollBoundary {`
    /// is paired with its closing brace through `PanelReachabilityLint.matchingBrace` — the same walker
    /// that file's gate uses, over the same redacted bytes — and a body is INSIDE when its offset falls in
    /// one of those spans. A "is there a boundary in the preceding N characters" test would call the
    /// pinned swap callout wrapped, since a boundary closes a few lines above it.
    func testTheBoundaryContainsTheUnboundedBodiesAndNothingThatMustStayPinned() throws {
        let url = URL(fileURLWithPath: #filePath)      // …/apps/menubar/Tests/PanelScrollBoundaryTests.swift
            .deletingLastPathComponent()               // …/apps/menubar/Tests
            .deletingLastPathComponent()               // …/apps/menubar
            .appendingPathComponent("Sources")
            .appendingPathComponent("StatusPanelView.swift")
        let source = try XCTUnwrap(try? String(contentsOf: url, encoding: .utf8),
                                   "StatusPanelView.swift must be readable as data — this gate reads it, "
                                 + "it does not compile it")
        // Comments and string literals are redacted through the SAME scanner the other panel lints use, so
        // the boundary's own long doc comment — which names every one of the tokens below — cannot satisfy
        // or defeat the gate.
        let scanned = PanelScaleLint.scan(source)
        XCTAssertFalse(scanned.unmodelledLiteral,
                       "the scanner does not model a literal in StatusPanelView.swift, so what follows is "
                       + "not a measurement")
        let code = scanned.code
        let text = String(decoding: code, as: UTF8.self)

        // Degenerate-subject guards. Without these, an emptied span list makes every "outside" assertion
        // below pass trivially — the shape of green-over-nothing this project treats as a failure.
        XCTAssertTrue(text.contains("ScrollView(.vertical) { content }"),
                      "`PanelScrollBoundary` must BE a `ScrollView`. If its body became a clip, a frame or "
                      + "an overlay, every render assertion in this suite still passes — none of them can "
                      + "see the difference — while the overflow stops being reachable")

        // Matched on `PanelScrollBoundary(` and then the NEXT brace, rather than on a fixed
        // `PanelScrollBoundary {` — every call site names its spoken region (`label:`), so the trailing
        // closure does not follow the type name directly. Searching for the literal open-brace form finds
        // nothing, and "nothing" is indistinguishable from "no boundaries left" for every check below,
        // which is what the count guard underneath is for.
        let opens = PanelReachabilityLint.offsets(of: Array("PanelScrollBoundary(".utf8), in: code)
        XCTAssertGreaterThan(opens.count, 1,
                             "found \(opens.count) scroll-boundary call sites. Every unbounded state body "
                             + "routes through one, so a single site (or none) means the boundary stopped "
                             + "being reached and the containment checks below are asserting over nothing")
        var spans: [Range<Int>] = []
        for open in opens {
            let brace = try XCTUnwrap(code[open...].firstIndex(of: UInt8(ascii: "{")), """
                a `PanelScrollBoundary(` at offset \(open) opens no closure — its body is where the \
                scrolled content is, so a call site without one is not a boundary
                """)
            let close = try XCTUnwrap(PanelReachabilityLint.matchingBrace(in: code, openAt: brace),
                                      "a `PanelScrollBoundary(` at offset \(open) has no matching brace")
            spans.append(brace..<close)
        }

        func offsets(_ token: String) -> [Int] {
            PanelReachabilityLint.offsets(of: Array(token.utf8), in: code)
        }
        func inside(_ offset: Int) -> Bool { spans.contains { $0.contains(offset) } }

        // Bodies that grow without bound with something the panel does not control. Every construction of
        // one must sit inside a boundary.
        for body in ["RosterView(", "StatsView()", "CaptureCard(", "DaemonLogCard(", "StartDaemonCard()"] {
            let sites = offsets(body)
            XCTAssertFalse(sites.isEmpty, "`\(body)` is no longer constructed in StatusPanelView.swift — "
                                        + "this row of the pin/scroll rule has no subject")
            for site in sites where !inside(site) {
                XCTFail("`\(body)` is constructed OUTSIDE every `PanelScrollBoundary` in "
                        + "StatusPanelView.swift (offset \(site)). It grows without bound, so once the "
                        + "budget binds it is clipped rather than scrolled — the unreachable region issue "
                        + "#818 removed, back in a new place. If this body became fixed chrome, take it "
                        + "off this list AND off `stateBody`'s pin/scroll rule, which is where the reason "
                        + "belongs")
            }
        }

        // Chrome that must never scroll away. AC-2's decision, in the form a reader can check.
        for chrome in ["PanelHeader(", "HonestStrip(", "SwapCalloutCard(", "SwapStatusLine()",
                       "FooterView("] {
            let sites = offsets(chrome)
            XCTAssertFalse(sites.isEmpty, "`\(chrome)` is no longer constructed in StatusPanelView.swift — "
                                        + "this row of the pin/scroll rule has no subject")
            for site in sites where inside(site) {
                XCTFail("`\(chrome)` is constructed INSIDE a `PanelScrollBoundary` in "
                        + "StatusPanelView.swift (offset \(site)). It is pinned deliberately: the header "
                        + "carries the honest-state sub-line and the tab switcher, `HonestStrip` says the "
                        + "roster below it is last-known rather than live, the callout is the panel's one "
                        + "primary action, and the footer is the freshness signal. Each becomes missable "
                        + "the moment a roster can push it out of view — see `stateBody`'s doc comment and "
                        + "design/README.md § The scroll boundary (#818)")
            }
        }

        // `BannerView` is the one token on BOTH sides, and that is the rule rather than an exception being
        // tolerated: a banner that IS the body scrolls (nothing below it to push it away, and pinning it
        // would clip the honest message itself), while the daemon-level fault banner is pinned because a
        // roster below it would otherwise carry it off the top.
        let banners = offsets("BannerView(banner:")
        XCTAssertEqual(banners.count, 2,
                       "expected exactly two `BannerView` call sites — the honest message card and the "
                       + "daemon-fault banner. Found \(banners.count), so the split asserted below no "
                       + "longer describes the file")
        XCTAssertEqual(banners.filter(inside).count, 1,
                       "exactly one `BannerView` belongs inside the boundary (the message card that IS the "
                       + "body of `.connecting` / `.unsupported`) and exactly one outside it (the "
                       + "daemon-level fault banner, pinned above the roster). Measured: "
                       + "\(banners.filter(inside).count) inside of \(banners.count)")
    }

    // MARK: - The render seam (#818): what the goldens see, and why that is still this panel

    /// The premise the render bypass rests on, kept running so it cannot rot into folklore.
    ///
    /// `PanelRenderHarness` rasterizes the panel with `\.panelScrollBoundaryEnabled` FALSE because
    /// `ImageRenderer` draws a `ScrollView`'s frame and none of its content. That is a claim about the
    /// PLATFORM, and platforms change — so it is measured here rather than asserted in a comment. The
    /// control is the load-bearing half: the identical body rendered WITHOUT a scroll view must carry ink,
    /// or a rig that draws nothing at all would satisfy the ScrollView half perfectly and this test would
    /// be pinning its own brokenness.
    ///
    /// Three ScrollView cases, because the tempting reading — "it clips what is scrolled out of view" —
    /// would predict that a viewport TALLER than its content renders fine. Measured, it does not: 300 pt of
    /// viewport around 100 pt of content is as blank as the rest. It is the container.
    ///
    /// WHEN THIS GOES RED, the bypass has become unnecessary and should be DELETED, not re-pinned: drop
    /// `composingScrollBoundaries`, let the harness render the panel as it composes, re-bless the goldens
    /// (the bodies will move — that is the point), and delete this test. A pin that merely tolerated the
    /// limitation would rot into a permanent allowance nobody revisits.
    func testImageRendererStillCannotDrawAScrollView() throws {
        let body = VStack(spacing: 4) {
            ForEach(0..<6) { i in
                Text("ROW \(i) ————————").font(.system(size: 14)).foregroundStyle(.black)
            }
        }
        .frame(maxWidth: .infinity)

        func ink(_ view: some View) throws -> Double {
            // Over an OPAQUE background: `inkCoverage` scores departure from the corner pixel, and black
            // text over a transparent corner agrees with it on every colour channel — measured, that scores
            // 0.0 for a perfectly-drawn render and would have made this whole test vacuous.
            let renderer = ImageRenderer(content: view.background(Color.white))
            renderer.scale = PanelRenderHarness.scale
            let cg = try XCTUnwrap(renderer.cgImage, "the rig rendered nothing at all")
            return PanelRaster.inkCoverage(try XCTUnwrap(PanelRaster.normalize(cg)))
        }

        let control = try ink(body.frame(width: 200).fixedSize(horizontal: false, vertical: true))
        XCTAssertGreaterThan(control, 0.02, """
            the CONTROL drew nothing (\(control) ink) — six rows of black text over white. Nothing below \
            is evidence about `ScrollView`; the rig itself is broken.
            """)

        let cases: [(String, Double)] = [
            ("fixedSize", try ink(ScrollView { body }.frame(width: 200)
                .fixedSize(horizontal: false, vertical: true))),
            ("definite 200×120, shorter than content", try ink(ScrollView { body }
                .frame(width: 200, height: 120))),
            ("definite 200×300, TALLER than content", try ink(ScrollView { body }
                .frame(width: 200, height: 300))),
        ]
        for (shape, measured) in cases {
            XCTAssertEqual(measured, 0, accuracy: 0.0001, """
                `ImageRenderer` DREW a ScrollView (\(shape) scored \(measured) ink against a \(control) \
                control) — the platform limitation the render bypass works around is gone. Delete the \
                bypass rather than re-pin it; see this test's doc comment for the sequence.
                """)
        }
    }

    /// What the bypass costs, bounded by measurement instead of by argument.
    ///
    /// Rendering the panel without its boundaries is only honest while the two trees DRAW THE SAME PANEL,
    /// and they do exactly while no state's body reaches the budget: a `ScrollView` whose content fits
    /// scrolls nothing, so composing it changes no pixel. At the default size class — the only one the
    /// committed goldens capture — that holds for every fixture, and this asserts it fixture by fixture
    /// rather than in general: both trees must report the SAME height, and that height must be under the
    /// budget, which is the condition that makes the equality mean "nothing was scrollable" rather than
    /// "both happened to be clamped to 856".
    ///
    /// It reddens the day a fixture grows past the bound at `.large`. That is precisely the day the
    /// goldens would otherwise start lying — rasterizing content the shipped panel puts behind a scroll —
    /// and the fix then is a decision (shrink the state, or accept that its golden shows more than the
    /// popover does), not a nudge to this number.
    func testTheRenderBypassIsANoOpAtTheGoldenSizeClass() throws {
        let now = Self.wallClock()
        var checked = 0
        for fixture in PanelRenderHarness.fixtures(now: now) {
            for scheme in PanelRenderHarness.themes {
                guard let composed = PanelRenderHarness.render(fixture, scheme: scheme,
                                                               composingScrollBoundaries: true),
                      let bypassed = PanelRenderHarness.render(fixture, scheme: scheme,
                                                               composingScrollBoundaries: false)
                else {
                    XCTFail("\(fixture.name) did not rasterize in both compositions")
                    continue
                }
                let scale = Double(PanelRenderHarness.scale)
                let composedHeight = Double(composed.height) / scale
                let bypassedHeight = Double(bypassed.height) / scale
                XCTAssertEqual(composedHeight, bypassedHeight, accuracy: 0.5, """
                    \(fixture.name) is \(composedHeight) pt as it ships and \(bypassedHeight) pt as the \
                    goldens rasterize it — the bypass is no longer a no-op, so the committed golden for \
                    this fixture shows a panel the popover does not.
                    """)
                XCTAssertLessThan(composedHeight, ScrollBoundary.budget, """
                    \(fixture.name) reaches \(composedHeight) pt at the DEFAULT text size, against a \
                    \(ScrollBoundary.budget) pt budget — so this state now scrolls in the popover while \
                    its golden still rasterizes the whole body. Decide which is right; do not relax this.
                    """)
                XCTAssertEqual(composed.width, bypassed.width,
                               "\(fixture.name) changed WIDTH between compositions — the boundary is "
                               + "vertical only and must not touch the panel's fixed width")
                checked += 1
            }
        }
        // Degenerate-subject guard: an empty catalog would satisfy every assertion above.
        XCTAssertGreaterThan(checked, 20,
                             "expected the whole (fixture × theme) catalog, compared \(checked) cells")
    }
}
#endif
