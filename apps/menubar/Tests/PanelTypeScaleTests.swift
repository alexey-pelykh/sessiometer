// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The Dynamic Type scale layer's own gate (issue #756) — the curve, the ceiling, and the end-to-end
// behaviour of the panel under a size class.
//
// WHAT THIS SUITE IS FOR, and what its sibling covers instead. `PanelTextMetricsTests` (issue #750) asks
// "does the TEXT still fit its CELL at every size class" — the AC-3 clipping question, measured in points
// through CoreText. This suite asks the two questions underneath that one: is the FACTOR right (the pure
// curve, `.large == 1.0`, the ceiling), and does the panel actually CONSUME it (a render at
// `.accessibility3` differs from the default; a render at `.accessibility5` equals the ceiling's). Neither
// suite subsumes the other: a correct curve that no view reads would pass the metrics sweep vacuously,
// and a consumed-but-wrong curve would pass the render checks.
//
// WHY THE CURVE IS TESTED AGAINST LITERALS rather than recomputed. Re-deriving `14.0 / 17.0` in the test
// restates the implementation and asserts nothing; the published Dynamic Type body progression (14, 15,
// 16, **17**, 19, 21, 23, 28, 33, 40 pt) is an EXTERNAL fact, so it is written out as decimals here. A
// change to the curve then has to be made twice, deliberately, rather than sliding through.
//
// THE MEASURED PLATFORM FACT this whole layer exists for is recorded at length in
// `StatusPanelTypeScale.swift`'s header: on macOS both mechanisms issue #756 prescribes — `@ScaledMetric`
// and relative text styles — are INERT (`@ScaledMetric` base 100 returns 100.000 at all twelve size
// classes; `Text().font(.body)` rasterizes to 31×16 px at all twelve). `\.dynamicTypeSize` propagates, so
// an explicit consumer is what works, and `testThePanelActuallyConsumesTheSizeClass` below is the standing
// proof that the panel is one — the assertion that would have caught the original defect.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

@MainActor
final class PanelTypeScaleTests: XCTestCase {

    // MARK: - The curve

    /// The published Dynamic Type body point sizes ABOVE the default, as ratios against `.large`'s 17 pt.
    /// External facts, written as literals on purpose (see the header). The sub-default classes are not
    /// here because the panel floors at `.large` — see `testEveryClassAtOrBelowTheDefaultScalesByOne`.
    private let expected: [(DynamicTypeSize, Double)] = [
        (.large, 1.0),
        (.xLarge, 19.0 / 17.0), (.xxLarge, 21.0 / 17.0), (.xxxLarge, 23.0 / 17.0),
        (.accessibility1, 28.0 / 17.0), (.accessibility2, 33.0 / 17.0), (.accessibility3, 40.0 / 17.0),
    ]

    func testTheFactorCurveMatchesThePublishedDynamicTypeProgression() {
        var checked = 0
        for (size, want) in expected {
            XCTAssertEqual(PanelTypeScale.factor(for: size), want, accuracy: 1e-12,
                           "\(size)'s factor drifted from the published body-size progression")
            checked += 1
        }
        // Degenerate-subject guard: a pass over a partial set is not evidence.
        XCTAssertEqual(checked, 7, "expected 7 size classes from the default to the ceiling, checked \(checked)")
    }

    /// The panel GROWS but never shrinks. Measured rather than preferred: scaling below 1.0 pushed the
    /// realistic fleet label over its budget at all three sub-default classes (2.45 / 1.47 / 0.46 pt),
    /// because glyph advance does not shrink linearly while the budget does — so the label began eliding
    /// where it previously fit, which is what issue #699 spent a decision preventing. The rationale and
    /// the numbers live on `PanelTypeScale.floor`.
    func testEveryClassAtOrBelowTheDefaultScalesByOne() {
        for small in [DynamicTypeSize.xSmall, .small, .medium, .large] {
            XCTAssertEqual(PanelTypeScale.factor(for: small), 1.0,
                           "\(small) does not scale by exactly 1.0 — the panel shrank below its designed "
                           + "density, which measurably pushes the roster label into elision")
        }
        XCTAssertEqual(PanelTypeScale.floor, .large,
                       "the declared floor moved off the default size class — that is a change to the "
                       + "range the panel supports, so re-derive the #699 trade rather than re-tune this")
    }

    /// The property the whole change rests on: at the DEFAULT size class the factor is EXACTLY 1.0, so
    /// every `points * scale` is the identity in IEEE-754 and the panel renders byte-for-byte what it did
    /// before issue #756. This is what let the committed goldens (issue #754) stay valid without a
    /// re-baseline — `testTheDefaultSizeClassRendersTheUnscaledPanel` is the end-to-end half of it.
    ///
    /// `XCTAssertEqual` without an accuracy, deliberately: "close to 1.0" is not the claim. A factor of
    /// 0.9999999 would still move pixels and churn every golden.
    func testTheDefaultSizeClassFactorIsExactlyOne() {
        XCTAssertEqual(PanelTypeScale.factor(for: .large), 1.0,
                       "the default size class no longer scales by exactly 1.0 — the panel's default "
                       + "rendering has moved, and every committed panel golden is now stale")
    }

    /// Strictly increasing from the default up to the ceiling, and never DEcreasing anywhere across the
    /// full twelve — the clamped ends are flat, and a larger text setting must never shrink the panel.
    func testTheCurveNeverDecreasesAndStrictlyRisesAboveTheDefault() {
        let ordered = expected.map(\.0)
        for (a, b) in zip(ordered, ordered.dropFirst()) {
            XCTAssertLessThan(PanelTypeScale.factor(for: a), PanelTypeScale.factor(for: b),
                              "\(a) → \(b) is not an increase — a larger text setting must never shrink "
                              + "the panel")
        }
        let all = DynamicTypeSize.allCases.sorted()
        for (a, b) in zip(all, all.dropFirst()) {
            XCTAssertLessThanOrEqual(PanelTypeScale.factor(for: a), PanelTypeScale.factor(for: b),
                                     "\(a) → \(b) DECREASES the panel's scale")
        }
    }

    /// Sizes above the ceiling clamp to it rather than growing further. The ceiling is AC-3's own — the
    /// panel is required to render correctly *to* `.accessibility3` — and `StatusPanelTypeScale`'s header
    /// records why the panel declares a limit instead of scaling without bound (at ×2.3529 the Stats
    /// tab's content is already 1300.50 pt against the panel's 856 pt budget, so every screenful past the
    /// first is reached by scrolling).
    func testSizesAboveTheCeilingClampToIt() {
        let ceiling = PanelTypeScale.factor(for: PanelTypeScale.ceiling)
        for over in [DynamicTypeSize.accessibility4, .accessibility5] {
            XCTAssertEqual(PanelTypeScale.factor(for: over), ceiling, accuracy: 1e-12,
                           "\(over) does not clamp to \(PanelTypeScale.ceiling) — the panel would size "
                           + "past the range it declares support for")
        }
        XCTAssertEqual(PanelTypeScale.ceiling, .accessibility3,
                       "the declared ceiling moved away from AC-3's `.accessibility3` — if that is "
                       + "intended, the range this panel claims to support changed and the claim in "
                       + "StatusPanelTypeScale's header needs re-deriving, not this number re-tuning")
    }

    // MARK: - End-to-end: the panel CONSUMES the size class

    private func render(_ fixture: String, _ size: DynamicTypeSize,
                        file: StaticString = #filePath, line: UInt = #line) -> PanelRaster? {
        let now = Int64(Date().timeIntervalSince1970)
        guard let f = PanelRenderHarness.fixtures(now: now).first(where: { $0.name == fixture }) else {
            XCTFail("no fixture named \(fixture)", file: file, line: line); return nil
        }
        guard let cg = PanelRenderHarness.render(f, scheme: .light, dynamicTypeSize: size) else {
            XCTFail("\(fixture) did not render at \(size)", file: file, line: line); return nil
        }
        return PanelRaster.normalize(cg)
    }

    /// THE regression guard for issue #756's actual defect. Before this change the panel rendered
    /// **760×898 px at every one of the twelve size classes** — measured, not inferred — because nothing
    /// consumed `\.dynamicTypeSize`. If this assertion ever goes green-by-collapse (the two renders
    /// becoming identical again), the panel has silently stopped scaling and the defect is back.
    func testThePanelActuallyConsumesTheSizeClass() throws {
        let base = try XCTUnwrap(render("healthy", .large))
        let big = try XCTUnwrap(render("healthy", .accessibility3))
        XCTAssertGreaterThan(big.width, base.width,
                             "the panel is \(big.width) px wide at .accessibility3 and \(base.width) px "
                             + "at .large — it is not scaling. This is exactly the issue #756 defect: on "
                             + "macOS neither @ScaledMetric nor a relative text style moves anything, so "
                             + "the panel must consume \\.dynamicTypeSize explicitly")
        XCTAssertGreaterThan(big.height, base.height,
                             "the panel did not grow in height at .accessibility3 — the fonts scaled but "
                             + "the layout did not follow, which is the clipping bug AC-2 names")
        // The width is a `.frame(width:)` on a known constant, so it is exact rather than approximate:
        // 380 pt × k, at the harness's @2x. Asserting the exact number (not merely "bigger") is what
        // distinguishes real uniform scaling from something that merely grew.
        let k = PanelTypeScale.factor(for: .accessibility3)
        let expected = Int((Double(PanelMetrics.width) * k * PanelRenderHarness.scale).rounded())
        XCTAssertEqual(big.width, expected, accuracy: 1,
                       "the panel is \(big.width) px wide at .accessibility3; uniform scaling of the "
                       + "\(PanelMetrics.width) pt panel by \(k) at @\(Int(PanelRenderHarness.scale))x "
                       + "requires \(expected) px")
    }

    /// A regression guard on the DEFAULT ARGUMENT, and deliberately no more than that: rendering at an
    /// explicit `.large` must agree to the byte with the default-argument path that `--render-panel` and
    /// the golden gate call. It would still pass if `factor(for: .large)` were 1.5 — because both paths
    /// route through the same size class — so it is NOT what protects the committed goldens. That load
    /// is carried by `testTheDefaultSizeClassFactorIsExactlyOne` (the arithmetic) and the goldens
    /// themselves (the bytes). What this catches is the default drifting off `.large` later.
    func testTheDefaultSizeClassRendersTheUnscaledPanel() throws {
        let explicit = try XCTUnwrap(render("healthy", .large))
        let now = Int64(Date().timeIntervalSince1970)
        let fixture = try XCTUnwrap(PanelRenderHarness.fixtures(now: now).first { $0.name == "healthy" })
        // The default-argument path — what `--render-panel` and the golden gate call.
        let byDefault = try XCTUnwrap(PanelRaster.normalize(
            try XCTUnwrap(PanelRenderHarness.render(fixture, scheme: .light))))
        XCTAssertEqual(PanelRaster.diffFraction(explicit, byDefault), 0.0, accuracy: 0.0,
                       "rendering at an explicit .large differs from the default-argument render")
        XCTAssertEqual(explicit.bytes, byDefault.bytes,
                       "the two default-size render paths do not agree to the byte")
    }

    /// The bordered controls ride `ControlSize`, not `.font` — `.borderedProminent` substitutes its own
    /// font and ignores an outer `.font()`, which is why the three prominent buttons rendered at the SAME
    /// pixel size at `.large` and `.accessibility3` before this mapping existed. The curve must therefore
    /// (a) hand back `.small` at the default, so the goldens do not move, and (b) never step DOWN as the
    /// text grows. It saturates at `.large` — macOS 13 has no `.extraLarge` — and asserting that here
    /// keeps the ceiling an acknowledged property rather than a silent one.
    func testTheControlSizeCurveStepsUpAndNeverDown() {
        XCTAssertEqual(PanelTypeScale.controlSize(for: PanelTypeScale.factor(for: .large)), .small,
                       "the default size class must keep the control size the goldens were shot at")

        let ordered: [ControlSize] = [.mini, .small, .regular, .large]
        func rank(_ size: ControlSize) -> Int { ordered.firstIndex(of: size) ?? ordered.count }

        var previous = rank(.small)
        var sawAStepUp = false
        for size in DynamicTypeSize.allCases.sorted() {
            let current = rank(PanelTypeScale.controlSize(for: PanelTypeScale.factor(for: size)))
            XCTAssertGreaterThanOrEqual(current, previous,
                                        "the control size steps DOWN at \(size) — bigger text, smaller button")
            if current > previous { sawAStepUp = true }
            previous = current
        }
        // Degenerate-subject guard: a curve that returned `.small` for everything would satisfy every
        // assertion above while scaling nothing at all.
        XCTAssertTrue(sawAStepUp, "the control size never changes across the whole range — it is a constant")
        XCTAssertEqual(PanelTypeScale.controlSize(for: PanelTypeScale.factor(for: .accessibility3)), .large,
                       "the accessibility classes must reach the largest control size macOS 13 offers")

        // The MIDDLE of the curve, which the two ends plus monotonicity above leave unpinned: re-tuning
        // the 1.09 / 1.30 thresholds could move `.xLarge` down to `.small` or `.xxLarge` up to `.large`
        // and still satisfy every assertion so far. This pins the exact map `controlSize(for:)`'s own
        // comment documents, so moving a threshold has to be a deliberate edit in both places.
        let middle: [(DynamicTypeSize, ControlSize)] = [
            (.xLarge, .regular), (.xxLarge, .regular),
            (.xxxLarge, .large), (.accessibility1, .large), (.accessibility2, .large),
        ]
        for (size, want) in middle {
            XCTAssertEqual(PanelTypeScale.controlSize(for: PanelTypeScale.factor(for: size)), want,
                           "\(size) no longer takes \(want) — the thresholds inside `controlSize(for:)` "
                           + "moved off the map its own comment states")
        }
    }

    /// The ceiling, end to end: `.accessibility5` must rasterize IDENTICALLY to `.accessibility3`. A
    /// same-run comparison, so it is cross-machine immune (the reasoning `PanelGoldenParityTests` uses for
    /// its own always-on half).
    func testRendersAboveTheCeilingAreIdenticalToTheCeiling() throws {
        let ceiling = try XCTUnwrap(render("healthy", .accessibility3))
        for over in [DynamicTypeSize.accessibility4, .accessibility5] {
            let beyond = try XCTUnwrap(render("healthy", over))
            XCTAssertEqual(PanelRaster.diffFraction(ceiling, beyond), 0.0, accuracy: 0.0,
                           "\(over) rendered differently from the declared ceiling "
                           + "\(PanelTypeScale.ceiling) — the clamp in `factor(for:)` and the "
                           + "`.dynamicTypeSize(...ceiling)` modifier on the panel root disagree")
        }
    }

    // MARK: - AC-4: the menu-bar status item is UNAFFECTED

    // The status item is a monochrome TEMPLATE glyph at a fixed bar size, locked by the brand identity —
    // explicitly NOT body text, and explicitly not governed by the panel's typography. It is also an
    // AppKit `NSImage` built by `StatusGauge`, which takes no `DynamicTypeSize` and sits outside the
    // SwiftUI hierarchy the panel's `\.panelScale` is injected into, so the scale CANNOT reach it by
    // construction.
    //
    // "By construction" is the argument, so this test exists to check the one thing construction does not
    // guarantee: that rendering the panel at a large size class in the same process leaves no shared
    // AppKit state behind that changes the glyph. The STANDING gate on the glyph's appearance is
    // `BarGlyphParityTests`, which diffs it against committed references and is untouched by this change.
    func testTheStatusItemGlyphIsUnchangedByPanelScaling() throws {
        let before = try XCTUnwrap(StatusGauge.image(for: .healthy, in: Bundle(for: Self.self)))
        let beforeSize = before.size
        _ = render("healthy", .accessibility3)
        let after = try XCTUnwrap(StatusGauge.image(for: .healthy, in: Bundle(for: Self.self)))
        XCTAssertEqual(after.size, beforeSize,
                       "the status-item glyph's size changed after the panel rendered at "
                       + ".accessibility3 — the panel's Dynamic Type scaling reached the menu-bar item, "
                       + "which is a fixed-bar-size template glyph and must never scale with text")
        XCTAssertTrue(after.isTemplate,
                      "the status-item glyph stopped being a template image — the menu-bar item is "
                      + "monochrome by brand lock and relies on template rendering")
    }
}
#endif
