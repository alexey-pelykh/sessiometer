// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Headless-rasterization probe for SwiftUI `ImageRenderer` (issue #749).
//
// WHY THIS EXISTS. `BarGlyphParityTests` used to state, as settled fact, that the panel's `ImageRenderer`
// "needs a windowserver" — which was the stated reason `RenderPanelTool` is an app-tool
// (`--render-panel <dir>`) rather than a test, and therefore the reason the PANEL had no automated
// visual gate while the bar GLYPH did. That claim had never been executed: it was load-bearing
// documentation for an untested belief, and the routing of the whole panel-golden lane (issues #753 /
// #754 / #760) hung off it. This suite is what executed it; that header now carries the correction.
//
// WHAT A GREEN RUN PROVES, exactly: `ImageRenderer` rasterizes inside THIS standalone logic-test bundle
// (`TEST_HOST: ""` — no host app, no `NSApplication` bootstrap, no popover) under `xcodebuild test`,
// which is the command CI runs verbatim. So an in-bundle panel golden gate is reachable; a red run
// routes issue #754 to its documented fallback (CI runs the built app's `--render-panel` as a build step
// and diffs in a script). It does NOT prove the stronger no-WINDOWSERVER claim — that was measured
// separately under `sandbox-exec` and is recorded in `BarGlyphParityTests`' header; nothing here re-runs
// it.
//
// NOT A THROWAWAY. Issue #749 called for a throwaway probe, but the answer is worth keeping executable:
// if a future Xcode / macOS revision withdraws headless `ImageRenderer`, the panel gate silently loses
// its foundation. This suite is that regression tripwire, and it is cheap (three small rasters).
//
// DEGENERATE-PASS GUARD. "Returned a non-nil `CGImage`" is NOT sufficient evidence. `ImageRenderer` can
// hand back a correctly-sized, entirely BLANK bitmap when SwiftUI declines to draw — which would read as
// a pass while proving nothing. Every probe therefore asserts INK as well as dimensions, reusing
// `BarGlyphRenderer.inkCoverage` (the same non-blank primitive the bar-glyph parity gate uses).
//
// How that primitive actually behaves here, measured rather than assumed: `inkCoverage` scores departure
// from the CORNER pixel, so BOTH degenerate cases — a blank raster and a uniform solid fill — collapse to
// coverage 0. The LOWER bound is therefore the load-bearing one, and it catches both. The upper bound is
// NOT the solid-fill guard it would be in `BarGlyphRenderer`'s own setting (opaque background, glyph ink
// over it); here it catches only the INVERTED degenerate, where every pixel but the corner differs.
// Stating this because the imported "strictly inside (0, 1) catches blank and blob" rationale does NOT
// transfer to these views, and a guard whose documented reason is wrong is a guard nobody can maintain.
//
// SCOPE OF EACH PROBE — deliberately split, because one composite view cannot honestly attest to all of
// it. A whole-row ink assertion is BLIND to any single missing element: removing both the symbol and the
// text from the composite row RAISES coverage (0.1196 → 0.1268, measured), because the freed width lets
// the capsule expand. So each construct that needs proving gets its own probe with its own subject.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

@MainActor
final class ImageRendererHeadlessProbeTests: XCTestCase {

    // MARK: - Stage 1: does `ImageRenderer` rasterize AT ALL, with no host app and no window?

    /// A deterministic, self-contained SwiftUI view (no environment, no asset catalog, no
    /// `@EnvironmentObject`) so a failure here is unambiguously the RASTERIZER and not our wiring.
    func testImageRendererRasterizesAMinimalViewHeadlessly() throws {
        let view = ZStack {
            Color.white
            Rectangle().fill(Color.black).frame(width: 32, height: 16)
        }
        .frame(width: 64, height: 32)

        let renderer = ImageRenderer(content: view)
        renderer.scale = 2

        let cg = try XCTUnwrap(
            renderer.cgImage,
            """
            ImageRenderer returned nil in a standalone logic-test bundle — it cannot rasterize with no \
            host app. That REINSTATES the pre-#749 belief this suite retired: route issue #754 to its \
            app-tool fallback (CI renders via the built app and diffs in a script).
            """)

        XCTAssertEqual(cg.width, 128, "scale 2 over a 64 pt width")
        XCTAssertEqual(cg.height, 64, "scale 2 over a 32 pt height")

        // Ink, not just pixels: a right-sized blank bitmap would satisfy the dimension assertions above
        // while proving nothing drew. The black rectangle covers exactly 1/4 of the white frame and
        // measures 0.2500; blank and uniform-fill both collapse to 0, which is why the lower bound is the
        // load-bearing one (see the header).
        let coverage = BarGlyphRenderer.inkCoverage(NSBitmapImageRep(cgImage: cg))
        XCTAssertGreaterThan(coverage, 0.05, "a blank raster is not a render — ImageRenderer drew nothing")
        XCTAssertLessThan(coverage, 0.95,
                          "every pixel but the corner differs — that is an inverted/blob raster, not a render")
    }

    // MARK: - Stage 1b: the SF-Symbol construct, probed ALONE so the assertion attests to it

    /// Split out of the composite row below on purpose: a whole-row ink assertion cannot detect a missing
    /// symbol (removing it *raises* coverage — see the header's measurement), so a composite-only probe
    /// would have claimed SF-Symbol support it never tested.
    func testImageRendererRasterizesAnSFSymbolHeadlessly() throws {
        let view = ZStack {
            Color.white
            Image(systemName: "checkmark.circle.fill")
                .resizable()
                .frame(width: 24, height: 24)
                .foregroundStyle(Color.black)
        }
        .frame(width: 32, height: 32)

        let renderer = ImageRenderer(content: view)
        renderer.scale = 2

        let cg = try XCTUnwrap(
            renderer.cgImage,
            """
            ImageRenderer returned nil for an SF Symbol. If the minimal-view probe above is green, \
            symbol resolution is the blocker, not the rasterizer.
            """)

        XCTAssertEqual(cg.width, 64, "scale 2 over a 32 pt width")
        XCTAssertEqual(cg.height, 64, "scale 2 over a 32 pt height")

        // The symbol is the ONLY thing that can draw here, so ink IS symbol ink. A filled 24 pt symbol in
        // a 32 pt frame measures 0.4287; 0 means the symbol did not resolve or did not render — the exact
        // failure the composite row below cannot see. The floor sits far below the measurement on purpose:
        // this asks "did the symbol draw", not "did it draw at this exact weight", so a future SF Symbol
        // metric revision must not redden it.
        let coverage = BarGlyphRenderer.inkCoverage(NSBitmapImageRep(cgImage: cg))
        XCTAssertGreaterThan(coverage, 0.10, "the SF Symbol did not draw — nothing else in this view can")
        XCTAssertLessThan(coverage, 0.95,
                          "every pixel but the corner differs — that is an inverted/blob raster, not a symbol")
    }

    // MARK: - Stage 2: environment injection — `@EnvironmentObject`, `@Published`, `colorScheme`

    /// The question issue #754 actually needs answered: does rasterization survive ENVIRONMENT INJECTION,
    /// which issue #749 flagged as the likelier failure point than the rasterizer? An `@EnvironmentObject`
    /// resolved through a view modifier, `@Published` state read during render, and an explicit
    /// `colorScheme` override — WITHOUT importing `StatusPanelView` itself (not compiled into this bundle;
    /// adding it is issue #754's job, not this spike's).
    ///
    /// What this probe DOES attest: environment resolution, because SwiftUI traps at render time on an
    /// unresolved `@EnvironmentObject` — so reaching a non-nil `cgImage` at all is the proof, and the ink
    /// assertion is a secondary non-blank check. What it does NOT attest is any individual child drawing;
    /// the SF Symbol has its own probe above precisely because this one cannot see it.
    ///
    /// If Stage 1 passes and Stage 2 fails, the verdict is NOT a flat no-go: it localizes the blocker to
    /// environment resolution, which is a fixable wiring problem rather than a platform limit.
    func testImageRendererRasterizesPanelShapedConstructsHeadlessly() throws {
        let model = ProbeModel(label: "probe", fraction: 0.42)

        let view = ProbePanelRow()
            .environmentObject(model)
            .environment(\.colorScheme, .dark)
            .frame(width: 240, height: 44)

        let renderer = ImageRenderer(content: view)
        renderer.scale = 2

        let cg = try XCTUnwrap(
            renderer.cgImage,
            """
            ImageRenderer returned nil once an @EnvironmentObject and @Published state were involved. \
            If the plain-view probes above are green, the blocker is environment resolution, not the \
            rasterizer — issue #754 should treat this as a wiring problem, not a platform limit.
            """)

        XCTAssertEqual(cg.width, 480, "scale 2 over a 240 pt width")
        XCTAssertEqual(cg.height, 88, "scale 2 over a 44 pt height")

        // Secondary and deliberately weak: it rules out a wholly blank row, nothing finer (see the
        // docstring). An intact row measures 0.1196 — the same figure the header's composite-blindness
        // measurement starts from.
        let coverage = BarGlyphRenderer.inkCoverage(NSBitmapImageRep(cgImage: cg))
        XCTAssertGreaterThan(coverage, 0.01, "the row is wholly blank — nothing in the hierarchy drew")
        XCTAssertLessThan(coverage, 0.95,
                          "every pixel but the corner differs — that is an inverted/blob raster, not a row")
    }
}

// MARK: - Probe fixtures (panel-shaped, but deliberately NOT the panel)

/// Stands in for `WatchStatusStore` — an `ObservableObject` injected through the environment and read
/// during render, which is the construct issue #749 named as the likely failure point.
@MainActor
private final class ProbeModel: ObservableObject {
    @Published var label: String
    @Published var fraction: Double

    init(label: String, fraction: Double) {
        self.label = label
        self.fraction = fraction
    }
}

/// A row shaped like `AccountRow`'s essentials — environment-resolved state, an SF Symbol, a text run,
/// and a proportionally-filled capsule — without depending on any panel source file.
private struct ProbePanelRow: View {
    @EnvironmentObject private var model: ProbeModel

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "checkmark.circle.fill")
            Text(model.label).font(.system(size: 12, weight: .semibold))
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.gray)
                    Capsule().fill(Color.green).frame(width: geo.size.width * model.fraction)
                }
            }
            .frame(height: 6)
        }
        .padding(8)
        .background(Color.black)
    }
}
#endif
