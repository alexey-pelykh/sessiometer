// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The app-icon grid gate (issue #991) — every raster `AppIcon.appiconset` ships sits on Apple's published
// macOS grid, and keeps the baked corner radius that grid assumes.
//
// The predicates and the reason each constant has the value it has live next door in `AppIconGrid.swift`;
// this file is the assertions and the canaries. What it adds over "measure the icon" is a subject set that
// cannot silently shrink: the files are enumerated from `Contents.json`, the manifest is cross-checked
// against the directory, and each raster's decoded canvas is checked against the size the manifest claims
// — so a size dropped from either side is a failure rather than a smaller green run. PRD AC-2 asks for
// exactly that ("BUT NOT at only some sizes — all of 16→1024 conform or the change is incomplete").
//
// CONSTRAINT-A (ADR-0031 § Decision 4): a gate ships only with a canary proving it can fail, verified by
// mutation rather than by inspection. Five mutants, every one driven through the SAME `AppIconGrid.measure`,
// `cornerAlphas`, `idealBodyFill` and `peakAlpha` calls the real assertions above use:
//
//   • a FULL-BLEED raster — the pre-#952 shape, and the exact regression this gate exists to catch. The
//     grid predicate must reject it.
//   • a HARD-CORNERED SQUARE filling the measured box — the per-corner read must reject it, and the grid
//     predicate must ACCEPT it. That asymmetry is the evidence the corner assertion is load-bearing rather
//     than decorative: the box metric is structurally blind to it, the same way a whole-row ink metric was
//     blind to two deleted elements (ADR-0031's "composite blindness"). A canary that everything rejects
//     would prove nothing about either half.
//   • a HARD-CORNERED SQUARE rendered at the IDEAL body edges — what dropping `icon.svg`'s `rx` would
//     actually ship. The fill CEILING must reject it, at every size. It is a second subject rather than a
//     replacement because the measured-box square inverts off the pixel grid: at canvas 16 it reads under
//     the fill FLOOR, so it can canary the corner read there but not the ceiling (issue #1160).
//   • that ideal-body square DIMMED to alpha 249 — inside both fill bounds at all ten, and accepted by
//     every predicate that shipped before issue #1148 added the opacity floor. Only the floor rejects it.
//   • a HOLLOW BODY at 60 % of its box, held on-grid by four alpha-254 pixels. Everything accepts it, the
//     floor included, and only the fill floor rejects it.
//
// The last two are the same asymmetry one layer down: each isolates a bound #1148 added by asserting that
// every OTHER predicate accepts the mutant, so a green cannot be coming from somewhere else. Every mutant
// is checked to have genuinely mutated before its verdict is read — a canary that has quietly stopped
// mutating passes forever (ADR-0031's "no-op mutation", which fired for real in #768).
//
// THE LIST ABOVE IS NOT A COVERAGE CLAIM — several assertions below have no mutant at all, and the one
// worth naming here is the EQUALITY half of the grid test, which runs only at the five rasters whose canvas
// `256` divides. The full-bleed mutant is driven through the TOLERANCE, and the two are not the same
// assertion — `testEveryEmittedSizeSitsOnTheMacOSAppIconGrid` carries the mutation results that establish it.

#if DEBUG
import XCTest

final class AppIconGridTests: XCTestCase {

    // MARK: - The subject set

    /// `apps/menubar/Sources/Assets.xcassets/AppIcon.appiconset/`, resolved from this file's own location.
    ///
    /// `#filePath`-relative for the same reason `WireGoldenTests` reads its goldens that way: it works
    /// identically under `xcodebuild test` in CI and locally, with no working-directory assumption. The
    /// rasters are read from the REPO rather than from a bundled resource on purpose — the committed PNG is
    /// the artifact under test, and a copy staged into a test bundle would be one indirection away from it.
    private static func appIconSet(file: StaticString = #filePath) -> URL {
        URL(fileURLWithPath: "\(file)")
            .deletingLastPathComponent()   // Tests/
            .deletingLastPathComponent()   // menubar/
            .appendingPathComponent("Sources/Assets.xcassets/AppIcon.appiconset", isDirectory: true)
    }

    /// One `Contents.json` entry, with the canvas it claims.
    private struct Declared {
        let filename: String
        let canvas: Int
    }

    /// Every image the manifest declares, in manifest order.
    private func declaredImages() throws -> [Declared] {
        let url = Self.appIconSet().appendingPathComponent("Contents.json")
        let root = try JSONSerialization.jsonObject(with: try Data(contentsOf: url))
        let images = try XCTUnwrap((root as? [String: Any])?["images"] as? [[String: Any]],
                                   "AppIcon.appiconset/Contents.json has no `images` array")
        return try images.map { entry in
            let filename = try XCTUnwrap(entry["filename"] as? String, "manifest entry with no filename")
            let size = try XCTUnwrap(entry["size"] as? String, "\(filename) declares no size")
            let scale = try XCTUnwrap(entry["scale"] as? String, "\(filename) declares no scale")
            let points = try XCTUnwrap(Int(size.split(separator: "x").first.map(String.init) ?? ""),
                                       "\(filename) has an unparseable size \(size)")
            let factor = try XCTUnwrap(Int(scale.replacingOccurrences(of: "x", with: "")),
                                       "\(filename) has an unparseable scale \(scale)")
            return Declared(filename: filename, canvas: points * factor)
        }
    }

    private func raster(_ filename: String) throws -> PanelRaster {
        try XCTUnwrap(PanelRaster.normalize(pngAt: Self.appIconSet().appendingPathComponent(filename)),
                      "\(filename) did not decode to a raster")
    }

    /// The manifest and the directory must describe the same set of files.
    ///
    /// This is the gate's cardinality guard. Every assertion below iterates the manifest, so a size deleted
    /// from `Contents.json` would leave the suite green over a smaller subject — the shape where a check
    /// that evaluates nothing still reports a pass.
    func testTheManifestAndTheDirectoryDescribeTheSameFiles() throws {
        let declared = Set(try declaredImages().map(\.filename))
        let onDisk = Set(try FileManager.default
            .contentsOfDirectory(atPath: Self.appIconSet().path)
            .filter { $0.hasSuffix(".png") })

        XCTAssertFalse(declared.isEmpty, "the manifest declares no images — every assertion below would "
                       + "evaluate an empty subject and report a pass")
        XCTAssertEqual(declared, onDisk,
                       "AppIcon.appiconset/Contents.json and the directory disagree: "
                       + "declared-but-absent \(declared.subtracting(onDisk).sorted()), "
                       + "present-but-undeclared \(onDisk.subtracting(declared).sorted()). "
                       + "Regenerate with brand/generate.sh rather than editing either side by hand")
    }

    /// Each raster is a square canvas of exactly the size × scale its manifest entry claims.
    ///
    /// The grid is a FRACTION of the canvas, so a wrong canvas silently rescales the whole measurement:
    /// a 512 raster filed as `icon_512x512@2x.png` would measure a perfect 80.47 % and still ship an icon
    /// macOS renders at half resolution.
    func testEveryRasterIsTheCanvasItsManifestEntryClaims() throws {
        for image in try declaredImages() {
            let raster = try raster(image.filename)
            XCTAssertEqual(raster.width, image.canvas,
                           "\(image.filename) is \(raster.width)px wide, manifest claims \(image.canvas)")
            XCTAssertEqual(raster.height, image.canvas,
                           "\(image.filename) is \(raster.height)px tall, manifest claims \(image.canvas)")
        }
    }

    // MARK: - The gate

    /// Every emitted size reaches full opacity somewhere in its body — the anchor the absolute reads need.
    ///
    /// This is the regime the rest of the instrument assumes, made falsifiable (issue #1148). `opaqueAlpha`,
    /// `cornerAlphas < 255` and `idealBodyFill`'s /255 all read the byte 255 as full coverage, so a uniform
    /// alpha scale slides every one of them at once and nothing else here can see it: the IDEAL-BODY square
    /// at alpha 249 is accepted by all three at all ten rasters, which is what
    /// `testOnlyTheOpacityFloorRejectsAUniformlyDimmedSquare` asserts. The third read was `bodyFill` until
    /// issue #1160 re-based the bounds, and it is named as `idealBodyFill` here because the substitution is
    /// not cosmetic: the MEASURED-box square dimmed by the same 2.4 % is no longer accepted anywhere off the
    /// pixel grid (84.8256 % at canvas 16, 99.5523 % at 32 and 64, 98.5951 % at 128, each outside a bound).
    /// `AppIconGrid.peakAlpha` carries why this is the right anchor and what it does not close.
    ///
    /// Asserted at EVERY declared size, deliberately unscoped — unlike the fill bounds, it needs no model of
    /// the body's area, so the `isExactOnPixelGrid` precondition does not apply to it.
    func testEveryEmittedSizeIsFullyOpaqueSomewhereInItsBody() throws {
        var lines: [String] = []
        for image in try declaredImages() {
            let raster = try raster(image.filename)
            let box = try XCTUnwrap(AppIconGrid.bodyBox(of: raster), "\(image.filename) carries no body")
            let peak = AppIconGrid.peakAlpha(raster, box: box)
            lines.append("[app-icon-grid] \(image.filename) — peak alpha in body box \(peak)")

            XCTAssertEqual(
                peak, 255,
                "\(image.filename) never reaches full opacity — its body box peaks at alpha \(peak). Every "
                + "absolute read in this instrument (the alpha ≥ \(AppIconGrid.opaqueAlpha) contour, the "
                + "corner < 255 test, and body-fill's /255) treats 255 as full coverage, so at a lower peak "
                + "they are measuring against an anchor that is not there and a hard-cornered square passes "
                + "as rounded. brand/src/icon.svg fills its body opaque and brand/generate.sh runs one "
                + "rsvg-convert pass per size, so a peak below 255 means the artwork or the pipeline gained "
                + "a transparency neither declares — do not relax this to match the output"
            )
        }
        print(lines.joined(separator: "\n"))
    }

    /// Every emitted size sits on the 824/1024 grid, within the half-pixel each edge quantises by — and
    /// where `256` divides the canvas, sits on it EXACTLY.
    ///
    /// The second half is the derived consequence `AppIconGrid.edgeTolerance` states and
    /// `AppIconGrid.isExactOnPixelGrid` names: there the ideal edges are integers, so `|measured − ideal|
    /// ≤ 0.5` between two integers forces equality. Asserting only the tolerance at those five rasters
    /// would leave a strictly stronger, already-true claim unread — and since issue #1160 moved the fill
    /// bounds onto `idealBodyFill`, this is the one place that predicate is still consumed rather than
    /// merely described.
    ///
    /// **The equality half has no canary, and the full-bleed mutant is not one** — a correction, since an
    /// earlier form of this comment claimed the opposite. That mutant is driven through `onGrid`, the
    /// TOLERANCE, inside `testTheGridPredicateRejectsAFullBleedRaster`, which reads `worstEdgeError` only into
    /// its printed line; nothing but the ten shipped rasters ever evaluates the `XCTAssertEqual` below, and the
    /// `guard` above it means only five of those reach it. Measured by mutation rather than read off the code:
    /// perturbing the expected value to 0.25 reddens 5 assertions, every one of them inside THIS test, with
    /// that canary still passing (11 tests, 5 failures); inverting `AppIconGrid.isExactOnPixelGrid` reddens 5
    /// again, this time at the four off-grid canvases, and again leaves it green. What stands in for a canary
    /// is containment rather than a mutant: this claim is strictly stronger than the tolerance beside it, which
    /// IS canaried, so what it can catch alone is a sub-half-pixel move. CONSTRAINT-A is satisfied for the
    /// tolerance and is NOT satisfied for this.
    func testEveryEmittedSizeSitsOnTheMacOSAppIconGrid() throws {
        var lines: [String] = []
        for image in try declaredImages() {
            let measurement = try XCTUnwrap(AppIconGrid.measure(try raster(image.filename)),
                                            "\(image.filename) is not a square canvas carrying opaque content")
            lines.append("[app-icon-grid] \(image.filename) — \(measurement)")
            XCTAssertTrue(
                measurement.onGrid,
                "\(image.filename) is OFF the macOS app-icon grid: \(measurement). Expected the body to "
                + "occupy \(AppIconGrid.bodySpan)/\(AppIconGrid.templateCanvas) of the canvas "
                + "(80.46875 %) at alpha ≥ \(AppIconGrid.opaqueAlpha), with every edge within "
                + "\(AppIconGrid.edgeTolerance) px of ideal. A body at 100 % is the pre-#952 full-bleed "
                + "regression: check that brand/generate.sh still routes the AppIcon set through "
                + "inset_app_icon, then regenerate"
            )
            guard AppIconGrid.isExactOnPixelGrid(canvas: image.canvas) else { continue }
            XCTAssertEqual(
                measurement.worstEdgeError, 0,
                "\(image.filename) is on the grid but not EXACTLY on it: \(measurement). At canvas "
                + "\(image.canvas) the ideal edges are integers (256 divides the canvas), so the "
                + "half-pixel tolerance collapses to equality — a non-zero error here is not quantisation, "
                + "it is a body of the wrong size or in the wrong place"
            )
        }
        print(lines.joined(separator: "\n"))
    }

    /// The baked corner radius survived into the emitted artwork — the body is a rounded rect, not a square.
    ///
    /// macOS applies no mask of its own (it is not iOS), so the artwork's own `rx` is the only thing keeping
    /// the icon from shipping hard-cornered. `docs/specs/app-icon-grid.feature.md` asks for that `rx` to be
    /// DROPPED and is stale — `brand/generate.sh` and `brand/README.md` § "The baked `rx` stays" both record
    /// why, and issue **#1141** tracks reconciling the spec. This assertion is the executable form of the
    /// correction, which is what keeps a reader who follows the stale scenario from landing the defect.
    ///
    /// Three reads, none of which catches what the others do — and they cover different ladders:
    ///
    ///   • **per-corner, every declared size**: a body-box corner pixel that is FULLY opaque is a square
    ///     corner. Catches ONE squared corner, which moves the aggregate by a quarter of the deficit and
    ///     would stay inside its threshold.
    ///   • **aggregate upper bound, every declared size**: the ideal body is filled to 95.71 %, not 100 %.
    ///     Reads the deficit's MAGNITUDE, so it catches a radius that shrank without vanishing — still wide
    ///     enough to keep the corner pixel off a full 255, which is the whole of what the per-corner read
    ///     above asks. Note the direction, because this prose had it backwards until issue #1149: shrinking
    ///     `rx` drives the corner pixel TOWARD fully covered, not toward empty. At the declared radius it
    ///     measures 0 from canvas 32 up and ~150 at canvas 16 — the arc has taken the pixel — and it climbs
    ///     as the radius falls, reaching 255 at 16/256/512/1024 but capping under it at 32/64/128, where the
    ///     box's corner pixel is only partly covered by the body to begin with (`AppIconGrid.cornerAlphas`
    ///     measures that, and what it costs). Unscoped since issue #1160: it reads
    ///     `AppIconGrid.idealBodyFill`, whose denominator is exact at every canvas, so the five rasters the
    ///     measured-box estimator could not speak about are now under it. `squareFillThreshold` carries the
    ///     re-derived margin, and the radius set it therefore admits.
    ///   • **aggregate LOWER bound, the same sizes**: the ideal body is filled to 95.71 %, not to *anything
    ///     less*. Without it the fill is bounded from above only, so a body can be arbitrarily empty and
    ///     still pass — a real icon shrunk to 60 % of its box, with the box held by four alpha-254 pixels,
    ///     measured 34.41 % and was accepted everywhere (issue #1148). `circleFillThreshold` carries the
    ///     derivation and the margin.
    ///
    /// The complementarity above is a claim about SHAPE, and it holds only inside the opacity regime
    /// `testEveryEmittedSizeIsFullyOpaqueSomewhereInItsBody` asserts — which is exactly the bound the prose
    /// used to state it without. All three reads key on opacity magnitude, so outside that regime one
    /// global alpha scale slides them together and none of them is evidence about the corner radius at all.
    func testEveryEmittedSizeKeptItsBakedCornerRadius() throws {
        var lines: [String] = []
        var asserted = 0
        for image in try declaredImages() {
            let raster = try raster(image.filename)
            let box = try XCTUnwrap(AppIconGrid.bodyBox(of: raster), "\(image.filename) carries no body")
            let corners = AppIconGrid.cornerAlphas(raster, box: box)
            let fill = try XCTUnwrap(AppIconGrid.idealBodyFill(raster),
                                     "\(image.filename) cannot carry the ideal body region")
            lines.append(String(format: "[app-icon-grid] %@ — ideal-body fill %.4f%% (rounded %.4f%%, "
                                + "admitted band %.4f%%…%.4f%% between an inscribed circle %.4f%% and a "
                                + "square %.4f%%), corner alphas %@",
                                image.filename, fill * 100, AppIconGrid.roundedFill * 100,
                                AppIconGrid.circleFillThreshold * 100, AppIconGrid.squareFillThreshold * 100,
                                AppIconGrid.circleFill * 100, AppIconGrid.squareFill * 100, "\(corners)"))

            XCTAssertTrue(
                corners.allSatisfy { $0 < 255 },
                "\(image.filename) is FULLY opaque at a corner of its own body box (alphas \(corners)) — "
                + "that corner is square. macOS does NOT mask app icons, so a square ships as a square. Do "
                + "not 'fix' it by dropping icon.svg's rx=\"229\": that IS this defect — see brand/README.md "
                + "§ The baked `rx` stays. Partial coverage is expected and fine: at canvas 16 the radius is "
                + "2.88 px and the arc cuts the corner pixel to ~150"
            )
            XCTAssertLessThanOrEqual(
                fill, AppIconGrid.squareFillThreshold,
                String(format: "%@ fills %.4f%% of its ideal body — at or past the %.4f%% boundary "
                       + "between a rounded tile (%.4f%%, the rx=\"229\" the artwork declares) and a hard "
                       + "SQUARE (100%%). The corner radius has been dropped or shrunk in the artwork",
                       image.filename, fill * 100, AppIconGrid.squareFillThreshold * 100,
                       AppIconGrid.roundedFill * 100)
            )
            XCTAssertGreaterThanOrEqual(
                fill, AppIconGrid.circleFillThreshold,
                String(format: "%@ fills only %.4f%% of its ideal body — at or under the %.4f%% boundary "
                       + "between a rounded tile (%.4f%%, the rx=\"229\" the artwork declares) and an "
                       + "inscribed CIRCLE (%.4f%%). The body is hollow, over-rounded, or partly "
                       + "transparent. Note this reads the IDEAL body — canvas × 824/1024 square, rounded "
                       + "outward to whole pixels — and never the measured box, so stray opaque pixels "
                       + "cannot hold a correct-looking denominator around almost nothing. Check that "
                       + "brand/generate.sh still renders the whole body rather than an inset of it, and "
                       + "that no step introduced a global alpha",
                       image.filename, fill * 100, AppIconGrid.circleFillThreshold * 100,
                       AppIconGrid.roundedFill * 100, AppIconGrid.circleFill * 100)
            )
            asserted += 1
        }
        let declaredCount = try declaredImages().count
        XCTAssertEqual(asserted, declaredCount,
                       "the fill bounds ran on \(asserted) of \(declaredCount) declared rasters — since "
                       + "issue #1160 they are asserted at every size, so a shortfall means a raster was "
                       + "skipped rather than carved out")
        print(lines.joined(separator: "\n"))
    }

    // MARK: - CONSTRAINT-A: the gate proves it can fail

    /// A full-bleed raster — the pre-#952 shape — driven through the same grid predicate, must be rejected.
    func testTheGridPredicateRejectsAFullBleedRaster() throws {
        var rejected = 0
        for image in try declaredImages() {
            let mutant = try XCTUnwrap(AppIconGrid.fullBleed(try raster(image.filename)),
                                       "\(image.filename): the full-bleed mutation produced nothing")
            let measurement = try XCTUnwrap(AppIconGrid.measure(mutant),
                                            "\(image.filename): the mutant is not measurable")

            // The mutation genuinely happened, before any conclusion is drawn from the verdict.
            XCTAssertEqual(measurement.box.width, image.canvas,
                           "\(image.filename): the canary is not actually full-bleed (\(measurement)) — "
                           + "the mutation has become a no-op and the rejection below proves nothing")
            XCTAssertEqual(measurement.bodyFraction, 1.0, accuracy: 1e-9)

            XCTAssertFalse(
                measurement.onGrid,
                "\(image.filename): a FULL-BLEED raster passed the grid predicate (\(measurement)) — the "
                + "gate cannot fail on the exact regression it exists to catch, so its green is not evidence"
            )
            print(String(format: "[app-icon-grid] canary full-bleed %@ — %.4f%% of canvas, worst edge "
                         + "error %.4f px (tolerance %.4f) → REJECTED",
                         image.filename, measurement.bodyFraction * 100,
                         measurement.worstEdgeError, AppIconGrid.edgeTolerance))
            rejected += 1
        }
        XCTAssertEqual(rejected, try declaredImages().count, "not every size was canaried")
    }

    /// A hard-cornered square filling the measured box must PASS the grid predicate and FAIL the per-corner
    /// one — which is what makes the per-corner assertion evidence rather than decoration.
    ///
    /// Scoped to the PER-CORNER half since issue #1160. The aggregate half moved to
    /// `testTheFillCeilingRejectsAHardCorneredSquareAtEverySize`, with a different subject, because this
    /// mutant stops being a square once the fill ceiling runs off the pixel grid: it fills the MEASURED box,
    /// which at canvas 16 is 12 px against a 12.875 px ideal body, so through `AppIconGrid.idealBodyFill` it
    /// reads **86.87 %** — under the FLOOR, not over the ceiling. Rejected, but as a hollow body rather than
    /// as a square, which is not evidence about the bound it was standing in for.
    ///
    /// It stays exactly as it was for the per-corner read, deliberately: writing 255 into the box's corner
    /// pixel by construction is what makes it a clean subject for THAT half, and it is also why it cannot
    /// exhibit the off-grid gap `AppIconGrid.cornerAlphas` records as **#1320**. Changing that is #1320's
    /// work — it reddens a shipped claim — not a side effect of moving the aggregate.
    func testOnlyTheRoundingPredicateRejectsAHardCorneredSquare() throws {
        var rejected = 0
        for image in try declaredImages() {
            let real = try raster(image.filename)
            let mutant = try XCTUnwrap(AppIconGrid.squaredCorners(real),
                                       "\(image.filename): the squared-corner mutation produced nothing")
            let measurement = try XCTUnwrap(AppIconGrid.measure(mutant),
                                            "\(image.filename): the mutant is not measurable")
            let corners = AppIconGrid.cornerAlphas(mutant, box: measurement.box)
            let fill = AppIconGrid.bodyFill(mutant, box: measurement.box)

            // The mutation genuinely happened — asserted before any verdict below is read as evidence.
            // Read box-relative, because filling the measured box is what this mutation is defined as.
            XCTAssertEqual(fill, AppIconGrid.squareFill, accuracy: 1e-9,
                           "\(image.filename): the squared-corner mutant does not actually fill its body "
                           + "box (\(fill)) — the mutation has become a no-op")

            XCTAssertTrue(
                measurement.onGrid,
                "\(image.filename): the squared-corner canary was rejected by the GRID predicate "
                + "(\(measurement)). It is supposed to be on-grid and wrong only at the corners; if the box "
                + "moved, the mutation is untargeted and says nothing about what the box metric can see"
            )
            XCTAssertFalse(
                corners.allSatisfy { $0 < 255 },
                "\(image.filename): a hard-cornered square passed the per-corner predicate (\(corners)) — "
                + "that half cannot fail, so it is not evidence the baked rx survived"
            )
            print(String(format: "[app-icon-grid] canary hard-cornered %@ — on-grid %@, body-box fill "
                         + "%.4f%%, corner alphas %@ → grid ACCEPTED, per-corner read REJECTED",
                         image.filename, "\(measurement.onGrid)", fill * 100, "\(corners)"))
            rejected += 1
        }
        XCTAssertEqual(rejected, try declaredImages().count, "not every size was canaried")
    }

    /// A hard-cornered square rendered at the IDEAL body edges must pass the grid predicate and be rejected
    /// by the fill CEILING, at every declared size.
    ///
    /// The canary the widened ceiling needs (issue #1160, ADR-0031 § Decision 4 CONSTRAINT-A). A bound that
    /// now runs on ten rasters instead of five has to be shown failing on ten, and on a subject that is a
    /// square at all ten — `AppIconGrid.squaredIdealBody` carries why the measured-box square is not that,
    /// and how it inverts at canvas 16.
    ///
    /// The subject is a render of the model rather than a stand-in: each pixel takes the coverage the ideal
    /// edges give it, which is the same integration the shipped rasters are measured performing. So its
    /// no-op check is that it reads a full 100 % of the ideal body — a number the real artwork misses by
    /// 4.3 pp, which is the whole signal this bound discriminates.
    func testTheFillCeilingRejectsAHardCorneredSquareAtEverySize() throws {
        var rejected = 0
        for image in try declaredImages() {
            let real = try raster(image.filename)
            let mutant = try XCTUnwrap(AppIconGrid.squaredIdealBody(real),
                                       "\(image.filename): the ideal-body square mutation produced nothing")
            let measurement = try XCTUnwrap(AppIconGrid.measure(mutant),
                                            "\(image.filename): the mutant is not measurable")
            let fill = try XCTUnwrap(AppIconGrid.idealBodyFill(mutant),
                                     "\(image.filename): the mutant cannot carry the ideal body region")

            // The mutation genuinely happened: a full ideal body, to within alpha's own 1/255 step.
            XCTAssertEqual(fill, AppIconGrid.squareFill, accuracy: 0.001,
                           "\(image.filename): the ideal-body square does not actually fill the ideal body "
                           + "(\(fill)) — the mutation has become a no-op or the coverage model has drifted")

            XCTAssertTrue(
                measurement.onGrid,
                "\(image.filename): the ideal-body square was rejected by the GRID predicate "
                + "(\(measurement)). It occupies exactly the ideal body, so it is supposed to measure "
                + "on-grid and be wrong only at the corners; if the box moved, the mutation is untargeted"
            )
            XCTAssertGreaterThan(
                fill, AppIconGrid.squareFillThreshold,
                String(format: "%@: a hard-cornered square filling %.4f%% of the ideal body passed the fill "
                       + "ceiling (%.4f%%) — that bound cannot fail here, so its green is not evidence the "
                       + "baked rx survived at this size",
                       image.filename, fill * 100, AppIconGrid.squareFillThreshold * 100)
            )
            print(String(format: "[app-icon-grid] canary ideal-body square %@ — on-grid %@, ideal-body fill "
                         + "%.4f%% (ceiling %.4f%%), corner alphas %@ → grid ACCEPTED, fill ceiling REJECTED",
                         image.filename, "\(measurement.onGrid)", fill * 100,
                         AppIconGrid.squareFillThreshold * 100,
                         "\(AppIconGrid.cornerAlphas(mutant, box: measurement.box))"))
            rejected += 1
        }
        let declaredCount = try declaredImages().count
        XCTAssertEqual(rejected, declaredCount,
                       "the fill ceiling was canaried on \(rejected) of \(declaredCount) declared "
                       + "rasters — since issue #1160 it runs at every size, so anything less means the "
                       + "canary is not covering what the bound now claims")
    }

    // MARK: - CONSTRAINT-A for the two bounds issue #1148 added

    /// A hard-cornered square at a uniform alpha 249 — accepted by EVERY predicate that shipped before
    /// #1148, and rejected only by the opacity floor.
    ///
    /// The targeted canary for that floor, and the asymmetry is the whole evidence: this is the ideal-body
    /// square `testTheFillCeilingRejectsAHardCorneredSquareAtEverySize` uses, dimmed by 2.4 %, and that one
    /// 6-count slide moves it from "rejected by the fill ceiling" to "accepted by every predicate but the
    /// floor" — corners read 140…249 (all < 255, so "rounded") and the ideal body fills to 97.65 %
    /// (< 97.8535 %, so "not square"). The corner read is 249 only where the box corner is fully covered
    /// before dimming; off the pixel grid it trails to 190 at canvas 32 and 140 at canvas 64, which is
    /// still "rounded". A canary the strengthened gate rejects for some OTHER reason would say nothing about
    /// whether the floor is load-bearing, so every prior predicate is asserted to ACCEPT it here.
    ///
    /// The base subject moved from `squaredCorners` to `squaredIdealBody` with issue #1160, and had to: once
    /// the fill bounds run at every size, the measured-box square dimmed to 249 is rejected by a FILL bound
    /// at all five off-grid rasters — by the ceiling at 32/64/128 (99.55 / 98.60 %) and by the floor at 16
    /// (84.83 %) — purely because its box is not the ideal body. The isolation this canary asserts would
    /// have been false at half the ladder, and the fix is a faithful subject rather than a narrower claim:
    /// dimmed, the ideal-body square reads 97.6471–97.6574 % at all ten, inside both bounds everywhere.
    func testOnlyTheOpacityFloorRejectsAUniformlyDimmedSquare() throws {
        let dimmed = 249.0 / 255.0
        var rejected = 0
        for image in try declaredImages() {
            let real = try raster(image.filename)
            let square = try XCTUnwrap(AppIconGrid.squaredIdealBody(real),
                                       "\(image.filename): the ideal-body square mutation produced nothing")
            let mutant = try XCTUnwrap(AppIconGrid.alphaScaled(square, scale: dimmed),
                                       "\(image.filename): the alpha-scale mutation produced nothing")
            let measurement = try XCTUnwrap(AppIconGrid.measure(mutant),
                                            "\(image.filename): the mutant is not measurable")
            let corners = AppIconGrid.cornerAlphas(mutant, box: measurement.box)
            let fill = try XCTUnwrap(AppIconGrid.idealBodyFill(mutant),
                                     "\(image.filename): the mutant cannot carry the ideal body region")
            let peak = AppIconGrid.peakAlpha(mutant, box: measurement.box)

            // Both mutations genuinely happened, before any verdict below is read as evidence: an ideal body
            // filled to 249/255 is a square that has been dimmed, and is neither on its own.
            XCTAssertEqual(fill, dimmed, accuracy: 0.001,
                           "\(image.filename): the mutant is not a uniformly dimmed FULL ideal body (fill "
                           + "\(fill)) — one of the two mutations has become a no-op")

            // Everything that shipped before the floor accepts it. This half is evidence about the DEFECT.
            XCTAssertTrue(measurement.onGrid,
                          "\(image.filename): the dimmed square was rejected by the GRID predicate "
                          + "(\(measurement)) — it is supposed to be on-grid and wrong only in opacity")
            XCTAssertTrue(corners.allSatisfy { $0 < 255 },
                          "\(image.filename): the per-corner read rejected alphas \(corners) — then this "
                          + "canary is not isolating the opacity floor")
            XCTAssertLessThanOrEqual(fill, AppIconGrid.squareFillThreshold,
                                     "\(image.filename): the fill ceiling rejected the dimmed square — then "
                                     + "this canary is not isolating the opacity floor")
            XCTAssertGreaterThanOrEqual(fill, AppIconGrid.circleFillThreshold,
                                        "\(image.filename): the fill floor rejected the dimmed square — "
                                        + "then this canary is not isolating the opacity floor")

            // And the floor rejects it, at every size. This half is evidence about the FIX.
            XCTAssertNotEqual(peak, 255,
                              "\(image.filename): a hard-cornered square at alpha 249 reached full opacity "
                              + "(peak \(peak)) — the floor cannot fail on the defect it exists to catch")
            print(String(format: "[app-icon-grid] canary dimmed-square %@ — on-grid %@, ideal-body fill "
                         + "%.4f%% (band %.4f%%…%.4f%%), corner alphas %@, peak %d → every prior predicate "
                         + "ACCEPTED, opacity floor REJECTED",
                         image.filename, "\(measurement.onGrid)", fill * 100,
                         AppIconGrid.circleFillThreshold * 100, AppIconGrid.squareFillThreshold * 100,
                         "\(corners)", Int(peak)))
            rejected += 1
        }
        XCTAssertEqual(rejected, try declaredImages().count, "not every size was canaried")
    }

    /// A real icon shrunk to 60 % of its box, with the box's four corners held by single alpha-254 pixels —
    /// accepted by every other predicate, including the opacity floor, and rejected only by the fill floor.
    ///
    /// The targeted canary for that floor. The pins are what make it a canary rather than a curiosity: the
    /// body box is measured from the outermost pixels at alpha ≥ 128, so four of them reconstruct a
    /// perfectly on-grid box around a body that occupies 36 % of it. The shrunk body is copied from the real
    /// artwork, so its interior still reaches 255 and the opacity floor accepts it too — leaving exactly one
    /// predicate able to reject it.
    ///
    /// Asserted at every declared size since issue #1160, where it was previously scoped to the five rasters
    /// whose canvas `256` divides. Worth stating what the pins buy the mutant now, because it is less than
    /// it was and NOT nothing — an earlier form of this comment said "nothing", and the measurement below is
    /// what corrects it.
    /// `AppIconGrid.idealBodyFill` never reads the measured box, so the forged box still defeats the GRID
    /// predicate — which is what makes this a canary — and it no longer buys a DENOMINATOR. What the four pins
    /// still buy is their own four pixels of NUMERATOR, which land inside the ideal region: 31.7197 % with them
    /// against 29.3161 % without at canvas 16, +0.6009 pp at 32, +0.0006 pp at 1024. They count for most where
    /// one pixel is a large share of a small body and for nothing where it is not — and nowhere near enough to
    /// carry a 31.72–36.95 % reading up to an 87.1234 % floor, which is why the canary holds either way.
    func testOnlyTheBodyFillFloorRejectsAHollowBody() throws {
        var asserted = 0
        for image in try declaredImages() {
            let real = try raster(image.filename)
            let realBox = try XCTUnwrap(AppIconGrid.bodyBox(of: real), "\(image.filename) carries no body")
            let mutant = try XCTUnwrap(AppIconGrid.hollowBody(real, fraction: 0.6, pinAlpha: 254),
                                       "\(image.filename): the hollow-body mutation produced nothing")
            let measurement = try XCTUnwrap(AppIconGrid.measure(mutant),
                                            "\(image.filename): the mutant is not measurable")
            let corners = AppIconGrid.cornerAlphas(mutant, box: measurement.box)
            let boxFill = AppIconGrid.bodyFill(mutant, box: measurement.box)
            let fill = try XCTUnwrap(AppIconGrid.idealBodyFill(mutant),
                                     "\(image.filename): the mutant cannot carry the ideal body region")
            let peak = AppIconGrid.peakAlpha(mutant, box: measurement.box)

            // The mutation genuinely happened: the box survived intact AND the body inside it collapsed.
            // Read box-relative, because "shrunk to a fraction of its own box" is what the mutation does.
            XCTAssertEqual(measurement.box, realBox,
                           "\(image.filename): the pins did not hold the original box (\(measurement.box) "
                           + "vs \(realBox)) — the mutation is untargeted and the grid predicate, not the "
                           + "fill floor, is what would reject it")
            XCTAssertLessThan(boxFill, 0.5,
                              "\(image.filename): the body did not actually hollow out (fill \(boxFill)) — "
                              + "the mutation has become a no-op")

            // Everything else accepts it, the opacity floor included. Evidence about the DEFECT.
            XCTAssertTrue(measurement.onGrid,
                          "\(image.filename): the hollow body was rejected by the GRID predicate "
                          + "(\(measurement)) — the pins are supposed to make it measure on-grid")
            XCTAssertTrue(corners.allSatisfy { $0 < 255 },
                          "\(image.filename): the per-corner read rejected the alpha-254 pins \(corners)")
            XCTAssertEqual(peak, 255,
                           "\(image.filename): the hollow mutant does not reach full opacity (peak \(peak)) "
                           + "— the opacity floor would reject it and this canary would not isolate the "
                           + "fill floor")
            XCTAssertLessThanOrEqual(fill, AppIconGrid.squareFillThreshold,
                                     "\(image.filename): the fill UPPER bound rejected a body that is "
                                     + "almost empty — the two bounds are the wrong way round")

            print(String(format: "[app-icon-grid] canary hollow-body %@ — on-grid %@, ideal-body fill "
                         + "%.4f%% (floor %.4f%%, box-relative %.4f%%), corner alphas %@, peak %d → every "
                         + "other predicate ACCEPTED, fill floor REJECTED",
                         image.filename, "\(measurement.onGrid)", fill * 100,
                         AppIconGrid.circleFillThreshold * 100, boxFill * 100, "\(corners)", Int(peak)))

            // And the fill floor rejects it. Evidence about the FIX.
            XCTAssertLessThan(fill, AppIconGrid.circleFillThreshold,
                              "\(image.filename): a body filling \(fill) of its ideal body passed the fill "
                              + "floor — that bound cannot fail, so its green is not evidence")
            asserted += 1
        }
        let declaredCount = try declaredImages().count
        XCTAssertEqual(asserted, declaredCount,
                       "the fill floor was canaried on \(asserted) of \(declaredCount) declared rasters "
                       + "— since issue #1160 it runs at every size")
    }

    /// The opacity floor rejects a uniform alpha scale at every declared size — the other two rows of
    /// #1148's table.
    ///
    /// Both are uniform alpha scales that once passed the whole gate: a hard square at alpha **254** was
    /// accepted at 16/32/64/128, where only the per-corner read ran, and a 50 %-transparent icon at
    /// 16/128/256/512/1024. The floor is asserted at every declared size, so it closes both across the whole
    /// ladder.
    ///
    /// **WHAT IT NO LONGER SHOWS** (issue #1160), since a test whose name outlived its premise is worse than no
    /// test. This used to be `…ReachesTheSizesTheFillBoundsCarveOut`, and the reach was its point: the fill
    /// bounds ran on five rasters and the floor on ten, so it measured the gap. There is no gap now — the
    /// bounds run on ten as well, and measured through `AppIconGrid.idealBodyFill` both of these probes are
    /// ALSO rejected by a fill bound at every size — but not everywhere by the same bound, which an earlier
    /// form of this parenthetical got wrong. The 50 % icon is under the FLOOR at all ten (47.9723–48.0439 %).
    /// The 254 square is not: it is the one probe deliberately left on `AppIconGrid.squaredCorners`, the
    /// MEASURED-box square, so read through `idealBodyFill` it is over the ceiling at nine files
    /// (99.6078–101.5514 %) and reads 86.5290 % at canvas 16, under the FLOOR — the same inversion
    /// `AppIconGrid.squaredIdealBody` was added for. So this is no longer evidence that the floor reaches
    /// somewhere else, only that it fires on the subjects #1148 measured, at all ten.
    ///
    /// That the floor is still load-bearing is asserted next door, by a mutant sitting INSIDE both bounds:
    /// `testOnlyTheOpacityFloorRejectsAUniformlyDimmedSquare`, where 249 rather than 254 slides a square
    /// under the ceiling by 0.20 pp and only the floor can see it.
    func testTheOpacityFloorRejectsAUniformAlphaScaleAtEverySize() throws {
        let cases: [(name: String, scale: Double, expectedPeak: UInt8, square: Bool)] = [
            ("hard square at alpha 254", 254.0 / 255.0, 254, true),
            ("uniformly 50 %-transparent icon", 0.5, 128, false),
        ]
        var rejected = 0
        for image in try declaredImages() {
            let real = try raster(image.filename)
            for probe in cases {
                let subject = probe.square
                    ? try XCTUnwrap(AppIconGrid.squaredCorners(real),
                                    "\(image.filename): the squared-corner mutation produced nothing")
                    : real
                let mutant = try XCTUnwrap(AppIconGrid.alphaScaled(subject, scale: probe.scale),
                                           "\(image.filename): the alpha-scale mutation produced nothing")
                let box = try XCTUnwrap(AppIconGrid.bodyBox(of: mutant),
                                        "\(image.filename): the \(probe.name) mutant carries no body")
                let peak = AppIconGrid.peakAlpha(mutant, box: box)

                // The mutation genuinely happened — the scale landed on the alpha the row was measured at.
                XCTAssertEqual(peak, probe.expectedPeak,
                               "\(image.filename): the \(probe.name) mutant peaks at \(peak), not "
                               + "\(probe.expectedPeak) — the mutation has become a no-op or drifted")
                XCTAssertNotEqual(peak, 255,
                                  "\(image.filename): the \(probe.name) reached full opacity — the floor "
                                  + "cannot fail on it")
                rejected += 1
            }
        }
        let declaredCount = try declaredImages().count
        XCTAssertEqual(rejected, declaredCount * cases.count,
                       "the opacity floor was canaried \(rejected) times, not "
                       + "\(declaredCount * cases.count) — a size or a probe was skipped")
        print("[app-icon-grid] canary uniform-alpha reach — the opacity floor rejected both uniform alpha "
              + "scales at all \(declaredCount) sizes")
    }
}
#endif
