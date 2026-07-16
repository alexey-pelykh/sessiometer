// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Bar-glyph render-PARITY gate (issue #525) — the automated drift guard the panel harness could not
// provide for the menu-bar status item. `RenderPanelTool` / SwiftUI `ImageRenderer` draw a SwiftUI view;
// they never exercise `NSStatusItem` TEMPLATE TINTING, which the system applies. This suite drives AppKit
// directly (via `BarGlyphRenderer`), re-rendering every glyph as it actually appears in the bar — template-
// tinted, per appearance, @1x + @2x, plus the menu-open inverted state — and diffs the fresh renders
// against the committed references under `design/renders/bar-glyphs/` (emitted by the app's
// `--render-bar-glyphs` tool). It runs headless under `xcodebuild test`: unlike the panel's `ImageRenderer`
// (which needs a windowserver), `NSBitmapImageRep` + `NSImage.draw` rasterize a custom symbol with no GUI.
//
// The catalog is compiled INTO this test bundle (project.yml adds `Sources/Assets.xcassets`), so the REAL
// bespoke `.symbolset`s resolve via `StatusGauge.image(for:in:)` against `Bundle(for:)` — `NSImage(named:)`
// would swallow them into the fallbackRing here (the logic-test bundle's `Bundle.main` is the xctest
// runner). The `symbolsResolve` test below is the loud guard that this actually happened.
//
// THE BASELINE TRAP (issue #437, at cost of a near brand re-ratification): a golden-image gate blesses
// whatever the renderer produces on first run — if the renderer is broken, the broken output becomes the
// reference and the gate then DEFENDS the bug, reporting green. Three bugs once rendered all four glyphs as
// one identical white blob and were misread five times as "the DESIGN fails distinctness". So this suite is
// built to PROVE it can fail, not merely to pass:
//   • distinctness — the four glyphs must be pairwise different in every context (a blob collapse reddens);
//   • canary — a deliberately perturbed render must exceed the drift gate, and an identical re-render must
//     score exactly 0 (a pairwise 0.000 means the SAME image, a pipeline smell, never a finding);
//   • nearest-reference identity — each fresh glyph's closest committed reference must be ITSELF;
//   • non-blank — every render carries glyph ink, neither empty nor a solid fill.
//
// SCOPE — this is a DRIFT + DISTINCTNESS gate, NOT an artwork-FIDELITY oracle. It renders the same
// compiled `.symbolset` the app renders, so if actool ever miscompiles the artwork in a way that keeps the
// four glyphs pairwise distinct (the historical stroke→fill / dropped-`<circle>` class, #437), the
// references would bake the miscompiled shape and this suite would stay green on the wrong glyphs. The
// on-device `SESSIOMETER_GLYPH_GALLERY` capture remains #437's PRIORITY-1 fidelity falsifier; this gate
// certifies the rendered shape has not DRIFTED from its committed reference and that the four stay DISTINCT
// — do not read a green here as "the compiled artwork is correct".
//
// THE METRIC + THRESHOLDS — `BarGlyphRenderer.diffFraction` counts pixels whose largest channel delta
// exceeds 0.25. It is localized-change sensitive and antialiasing-tolerant, so thresholds are calibrated to
// MEASURED separations (issue #525), not guessed: identical renders 0.000; the closest real pair (healthy
// vs no-runway, @1x dark — both a diagonal stroke) 0.0226; a full interior-mark swap up to ~0.06; the
// canary blot ~0.11. Two DISTINCT drift gates, deliberately split by robustness:
//   • `testEachFreshGlyphIsNearestToItsOwnReference` — the PRIMARY, tight drift gate. Relative (a fresh
//     glyph's nearest committed reference must be itself), so it is IMMUNE to any uniform cross-machine
//     shift, and it catches the honesty-critical drift (a glyph morphing toward another state's shape) with
//     a full 0.0226-wide margin. This is what satisfies "the pass fails when a glyph drifts".
//   • `testEveryRenderMatchesItsCommittedReference` — a COARSE gross/uniform-drift tripwire at ceiling 0.05.
//     Its unique job is the case the relative gate is blind to: ALL glyphs drifting together (which keeps
//     each nearest to its own shifted reference). Set loose ON PURPOSE — the committed references are
//     rendered on one machine and this re-renders on the CI runner (unpinned `macos-latest`), so the
//     ceiling must clear cross-machine antialiasing; 0.05 clears it by a wide margin (the metric counts
//     near-full pixel flips, which deterministic vector rasterization does not produce across OS AA) while
//     the canary at ~0.11 proves it is still reachable. It intentionally does NOT try to catch a single-
//     glyph swap — the relative gate above already does, robustly.
// Distinctness floor is 0.01 (well under the 0.0226 real minimum, so no real pair is flagged identical, yet
// a 0.000 blob collapse still reddens); it is a same-run comparison, so cross-machine-immune.

#if DEBUG
import AppKit
import XCTest

final class BarGlyphParityTests: XCTestCase {

    private let glyphs = StatusGlyph.allCases
    private let contexts = BarGlyphContext.allCases
    private let scales = BarGlyphRenderer.scales

    /// Catches a blob collapse (all glyphs identical → 0), set well below the MEASURED closest real pair
    /// (0.0226) so no genuinely-distinct pair is ever flagged. Same-run comparison → cross-machine immune.
    private let distinctnessFloor = 0.01

    /// The COARSE gross/uniform-drift ceiling for the absolute committed-reference comparison — NOT the
    /// primary drift gate (that is the relative `testEachFreshGlyphIsNearestToItsOwnReference`, which is
    /// cross-machine-immune and catches single-glyph drift with margin). This absolute test exists only for
    /// the case the relative gate is blind to — all glyphs drifting together — so it is set LOOSE on purpose
    /// to clear cross-machine antialiasing (the references are committed from one machine and re-rendered on
    /// the unpinned `macos-latest` CI runner). 0.05 clears AA by a wide margin — the metric counts only
    /// near-full pixel flips, which deterministic vector rasterization does not produce across OS AA — while
    /// the canary (~0.11) proves it is still reachable.
    private let driftCeiling = 0.05

    /// The menu bar template is monochrome by construction: the brief's ≤ 30/255 chroma bound (over true
    /// ink) is antialiasing only — there is no colour channel carrying state.
    private let chromaBound = 30.0 / 255.0

    // MARK: - Rendering + reference helpers

    /// Resolve the REAL bespoke glyph from THIS test bundle's compiled catalog, through the exact
    /// configuration the live status item uses (`StatusGauge.image(for:in:)`).
    private func glyphImage(_ glyph: StatusGlyph) -> NSImage {
        StatusGauge.image(for: glyph, in: Bundle(for: Self.self))
    }

    private func render(_ glyph: StatusGlyph, _ context: BarGlyphContext, _ scale: Int) -> NSBitmapImageRep {
        BarGlyphRenderer.render(glyphImage(glyph), context: context, scale: scale)
    }

    /// The committed-references directory, located from this source file (like `StatusGaugeTests` locates
    /// `Assets.xcassets`) — CI checks the tree out at the same path it compiled from.
    private func referenceURL(glyph: StatusGlyph, context: BarGlyphContext, scale: Int) -> URL {
        URL(fileURLWithPath: #filePath)                       // .../apps/menubar/Tests/BarGlyphParityTests.swift
            .deletingLastPathComponent()                      // .../apps/menubar/Tests
            .deletingLastPathComponent()                      // .../apps/menubar
            .appendingPathComponent("design/renders/bar-glyphs")
            .appendingPathComponent(BarGlyphRenderer.referenceName(glyph: glyph, context: context, scale: scale))
    }

    private func loadReference(glyph: StatusGlyph, context: BarGlyphContext, scale: Int,
                               file: StaticString = #filePath, line: UInt = #line) -> NSBitmapImageRep? {
        let url = referenceURL(glyph: glyph, context: context, scale: scale)
        guard let data = try? Data(contentsOf: url) else {
            XCTFail("missing committed reference \(url.lastPathComponent) — regenerate via --render-bar-glyphs",
                    file: file, line: line)
            return nil
        }
        guard let rep = NSBitmapImageRep(data: data) else {
            XCTFail("reference \(url.lastPathComponent) is not a decodable PNG", file: file, line: line)
            return nil
        }
        return rep
    }

    // MARK: - Guard: the real symbols resolved (not the fallbackRing)

    // The whole suite is meaningless if the custom symbols did not resolve — `image(for:in:)` would return
    // the 16×16 fallbackRing for all four, and every glyph would be an identical ring. The configured
    // bespoke symbol is ~22×21 pt; the fallbackRing is 16×16. A width well above 16 proves the catalog
    // compiled into this bundle and resolved. (Distinctness below is the second, independent guard.)
    func testTheRealBespokeSymbolsResolveInThisBundle() {
        for glyph in glyphs {
            let size = glyphImage(glyph).size
            XCTAssertGreaterThan(size.width, 18,
                "\(glyph) resolved at \(size) — that is the 16×16 fallbackRing, not the bespoke symbol; "
                + "the compiled Assets.xcassets is missing from the MenubarTests bundle (project.yml)")
        }
    }

    // MARK: - AC: each bar-glyph state has a template-tinted render matching its reference (24 cells)

    func testEveryRenderMatchesItsCommittedReference() {
        var checked = 0
        for glyph in glyphs {
            for context in contexts {
                for scale in scales {
                    guard let reference = loadReference(glyph: glyph, context: context, scale: scale) else { continue }
                    let fresh = render(glyph, context, scale)
                    XCTAssertEqual(fresh.pixelsWide, reference.pixelsWide,
                        "\(glyph)/\(context)@\(scale)x width drifted from reference")
                    XCTAssertEqual(fresh.pixelsHigh, reference.pixelsHigh,
                        "\(glyph)/\(context)@\(scale)x height drifted from reference")
                    let drift = BarGlyphRenderer.diffFraction(fresh, reference)
                    XCTAssertLessThan(drift, driftCeiling,
                        "\(glyph)/\(context)@\(scale)x drifted \(drift) from its reference (> \(driftCeiling)) — "
                        + "the rendered shape changed; regenerate the references if intentional")
                    checked += 1
                }
            }
        }
        // Degenerate-subject guard: the pass is only evidence if it evaluated the full planned 4×3×2 set.
        XCTAssertEqual(checked, glyphs.count * contexts.count * scales.count,
                       "expected 24 (glyph × context × scale) reference comparisons, ran \(checked)")
    }

    // MARK: - AC: the pass fails when a glyph drifts — nearest-reference identity (cross-machine immune)

    // The robust drift gate: a fresh glyph's CLOSEST committed reference (over the four, in its own
    // context/scale) must be ITSELF. Relative, so a uniform cross-machine antialiasing shift (which nudges
    // every comparison equally) cannot flip the winner. Catches a glyph drifting toward a sibling's shape,
    // and a blob collapse (an ambiguous nearest).
    func testEachFreshGlyphIsNearestToItsOwnReference() {
        for context in contexts {
            for scale in scales {
                let references = glyphs.compactMap { g in
                    loadReference(glyph: g, context: context, scale: scale).map { (g, $0) }
                }
                guard references.count == glyphs.count else {
                    XCTFail("missing references for \(context)@\(scale)x — cannot run nearest-reference identity")
                    continue
                }
                for glyph in glyphs {
                    let fresh = render(glyph, context, scale)
                    let ranked = references
                        .map { (glyph: $0.0, drift: BarGlyphRenderer.diffFraction(fresh, $0.1)) }
                        .sorted { $0.drift < $1.drift }
                    XCTAssertEqual(ranked.first?.glyph, glyph,
                        "\(glyph)/\(context)@\(scale)x is closest to \(ranked.first?.glyph as Any)'s reference, "
                        + "not its own — the glyph drifted toward another state's shape")
                }
            }
        }
    }

    // MARK: - Baseline trap: the four glyphs must be pairwise DISTINCT in every context (light is riskiest)

    // A monochrome template has only shape to carry state; two states sharing a silhouette are
    // indistinguishable. The LIGHT context is the one #437 left unproven (black ink on a light bar THINS
    // where white ink on a dark bar BLOOMS); distinctness here is the evidence the interior marks survive
    // light tinting — the inherited #437 PRIORITY-1 shape-distinctness check, now automated for light.
    func testTheFourGlyphsArePairwiseDistinctInEveryContext() {
        for context in contexts {
            for scale in scales {
                let reps = glyphs.map { render($0, context, scale) }
                for i in 0..<glyphs.count {
                    for j in (i + 1)..<glyphs.count {
                        let diff = BarGlyphRenderer.diffFraction(reps[i], reps[j])
                        XCTAssertGreaterThan(diff, distinctnessFloor,
                            "\(glyphs[i]) and \(glyphs[j]) are near-identical (\(diff)) in \(context)@\(scale)x — "
                            + "shapes collapsed, or the renderer is broken (a 0.000 means the SAME image)")
                    }
                }
            }
        }
    }

    // The consequential poles the design record flags — Healthy (ignore me) vs No-runway (act now), the
    // ✓/⊘ diagonal-stroke pair — must never render as the same shape, in any context.
    func testHealthyAndNoRunwayNeverShareAShape() {
        for context in contexts {
            for scale in scales {
                let diff = BarGlyphRenderer.diffFraction(render(.healthy, context, scale),
                                                         render(.noRunway, context, scale))
                XCTAssertGreaterThan(diff, distinctnessFloor,
                    "Healthy and No-runway rendered near-identical (\(diff)) in \(context)@\(scale)x — "
                    + "opposite verdicts must not share a shape")
            }
        }
    }

    // MARK: - Baseline trap: the gate PROVES it can fail (canary), and identical renders score exactly 0

    func testTheCanaryDriftsAndAnIdenticalRenderDoesNot() {
        let clean = render(.healthy, .light, 2)

        // Identical re-render scores exactly 0 — the metric is deterministic and 0.000 means the same image.
        let identical = render(.healthy, .light, 2)
        XCTAssertEqual(BarGlyphRenderer.diffFraction(clean, identical), 0.0, accuracy: 0.0,
                       "two renders of the same glyph must be byte-identical (drift 0) — else the rig is nondeterministic")

        // A deliberately perturbed copy MUST exceed the drift ceiling — proof the gate can fail.
        let perturbed = render(.healthy, .light, 2)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: perturbed)
        NSColor.red.setFill()
        NSRect(x: 8, y: 8, width: 8, height: 8).fill()   // ~11% of the 24-pt grid
        NSGraphicsContext.restoreGraphicsState()
        let canary = BarGlyphRenderer.diffFraction(clean, perturbed)
        XCTAssertGreaterThan(canary, driftCeiling,
                             "canary \(canary) did NOT exceed the drift ceiling \(driftCeiling) — the gate cannot fail, so it is not evidence")
    }

    // MARK: - Every render carries glyph ink (neither blank nor a solid blob)

    func testEveryRenderIsNonBlank() {
        for glyph in glyphs {
            for context in contexts {
                for scale in scales {
                    let coverage = BarGlyphRenderer.inkCoverage(render(glyph, context, scale))
                    XCTAssertGreaterThan(coverage, 0.03,
                        "\(glyph)/\(context)@\(scale)x has almost no ink (\(coverage)) — the glyph did not draw")
                    XCTAssertLessThan(coverage, 0.97,
                        "\(glyph)/\(context)@\(scale)x is a near-solid fill (\(coverage)) — the glyph mushed into a blob")
                }
            }
        }
    }

    // MARK: - Monochrome by construction (the deuteranopia note — asserted once)

    // The resting bar contexts (light/dark) tint a template with `labelColor`, a gray — so every pixel is
    // achromatic (r≈g≈b) to within antialiasing. There is no colour channel carrying state. (menuOpen's
    // background is the accent by design — the highlight — so it is not part of this monochrome claim; its
    // glyph ink is white, itself monochrome.)
    func testTheRestingBarGlyphsAreMonochrome() {
        for context in [BarGlyphContext.light, .dark] {
            for glyph in glyphs {
                let rep = render(glyph, context, 2)
                var maxChroma = 0.0
                for y in 0..<rep.pixelsHigh {
                    for x in 0..<rep.pixelsWide {
                        guard let c = rep.colorAt(x: x, y: y) else { continue }
                        let chroma = max(c.redComponent, c.greenComponent, c.blueComponent)
                                   - min(c.redComponent, c.greenComponent, c.blueComponent)
                        maxChroma = max(maxChroma, chroma)
                    }
                }
                XCTAssertLessThanOrEqual(maxChroma, chromaBound,
                    "\(glyph)/\(context) carries chroma \(maxChroma) (> \(chromaBound)) — a template bar glyph must be monochrome")
            }
        }
    }
}
#endif
