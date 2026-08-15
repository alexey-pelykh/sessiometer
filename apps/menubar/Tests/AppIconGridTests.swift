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
// mutation rather than by inspection. Four mutants, every one driven through the SAME `AppIconGrid.measure`,
// `cornerAlphas`, `bodyFill` and `peakAlpha` calls the real assertions above use:
//
//   • a FULL-BLEED raster — the pre-#952 shape, and the exact regression this gate exists to catch. The
//     grid predicate must reject it.
//   • a HARD-CORNERED SQUARE at the correct grid fraction — what dropping `icon.svg`'s `rx` would ship.
//     The grid predicate must ACCEPT it, and only the corner predicate rejects it. That asymmetry is the
//     evidence the corner assertion is load-bearing rather than decorative: the box metric is structurally
//     blind to it, the same way a whole-row ink metric was blind to two deleted elements (ADR-0031's
//     "composite blindness"). A canary that everything rejects would prove nothing about either half.
//   • that SAME square DIMMED to alpha 249 — which every predicate above accepted, at all ten rasters,
//     until issue #1148 added the opacity floor. Only the floor rejects it.
//   • a HOLLOW BODY at 60 % of its box, held on-grid by four alpha-254 pixels. Everything accepts it, the
//     floor included, and only `bodyFill`'s lower bound rejects it.
//
// The last two are the same asymmetry one layer down: each isolates a bound #1148 added by asserting that
// every OTHER predicate accepts the mutant, so a green cannot be coming from somewhere else. Every mutant
// is checked to have genuinely mutated before its verdict is read — a canary that has quietly stopped
// mutating passes forever (ADR-0031's "no-op mutation", which fired for real in #768).

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
    /// `cornerAlphas < 255` and `bodyFill`'s /255 all read the byte 255 as full coverage, so a uniform alpha
    /// scale slides every one of them at once and nothing else here can see it: a hard-cornered square at
    /// alpha 249 was accepted by all three at all ten rasters. `AppIconGrid.peakAlpha` carries why this is
    /// the right anchor and what it does not close.
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

    /// Every emitted size sits on the 824/1024 grid, within the half-pixel each edge quantises by.
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
    ///   • **aggregate upper bound, the sizes whose canvas `256` divides**: the body box is filled to
    ///     95.71 %, not 100 %. Reads the deficit's MAGNITUDE, so it catches a radius that shrank without
    ///     vanishing — still wide enough to keep the corner pixel off a full 255, which is the whole of
    ///     what the per-corner read above asks. Note the direction, because this prose had it backwards
    ///     until issue #1149: shrinking `rx` drives the corner pixel TOWARD fully covered, not toward
    ///     empty. At the declared radius it measures 0 from canvas 32 up and ~150 at canvas 16 — the arc
    ///     has taken the pixel — and it climbs as the radius falls, reaching 255 at 16/256/512/1024 but
    ///     capping under it at 32/64/128, where the box's corner pixel is only partly covered by the body
    ///     to begin with (`AppIconGrid.isExactOnPixelGrid` measures that, and what it costs). Scoped,
    ///     because the model divides by the measured box and only there is that box the ideal body;
    ///     `AppIconGrid`'s `squareFillThreshold` carries the measurement that bounds it, and the radius
    ///     set it therefore admits.
    ///   • **aggregate LOWER bound, the same sizes**: the box is filled to 95.71 %, not to *anything less*.
    ///     Without it the fill is bounded from above only, so a body box can be arbitrarily empty and still
    ///     pass — a real icon shrunk to 60 % of its box, with the box held by four alpha-254 pixels,
    ///     measured 34.41 % and was accepted everywhere (issue #1148). `circleFillThreshold` carries the
    ///     derivation and the margin.
    ///
    /// The complementarity above is a claim about SHAPE, and it holds only inside the opacity regime
    /// `testEveryEmittedSizeIsFullyOpaqueSomewhereInItsBody` asserts — which is exactly the bound the prose
    /// used to state it without. All three reads key on opacity magnitude, so outside that regime one
    /// global alpha scale slides them together and none of them is evidence about the corner radius at all.
    func testEveryEmittedSizeKeptItsBakedCornerRadius() throws {
        var lines: [String] = []
        for image in try declaredImages() {
            let raster = try raster(image.filename)
            let box = try XCTUnwrap(AppIconGrid.bodyBox(of: raster), "\(image.filename) carries no body")
            let corners = AppIconGrid.cornerAlphas(raster, box: box)
            let fill = AppIconGrid.bodyFill(raster, box: box)
            let exact = AppIconGrid.isExactOnPixelGrid(canvas: image.canvas)
            lines.append(String(format: "[app-icon-grid] %@ — body-box fill %.4f%% (rounded %.4f%%, "
                                + "admitted band %.4f%%…%.4f%% between an inscribed circle %.4f%% and a "
                                + "square %.4f%%%@), corner alphas %@",
                                image.filename, fill * 100, AppIconGrid.roundedFill * 100,
                                AppIconGrid.circleFillThreshold * 100, AppIconGrid.squareFillThreshold * 100,
                                AppIconGrid.circleFill * 100, AppIconGrid.squareFill * 100,
                                exact ? "" : " — not asserted, canvas is off the pixel grid", "\(corners)"))

            XCTAssertTrue(
                corners.allSatisfy { $0 < 255 },
                "\(image.filename) is FULLY opaque at a corner of its own body box (alphas \(corners)) — "
                + "that corner is square. macOS does NOT mask app icons, so a square ships as a square. Do "
                + "not 'fix' it by dropping icon.svg's rx=\"229\": that IS this defect — see brand/README.md "
                + "§ The baked `rx` stays. Partial coverage is expected and fine: at canvas 16 the radius is "
                + "2.88 px and the arc cuts the corner pixel to ~150"
            )
            guard exact else { continue }
            XCTAssertLessThanOrEqual(
                fill, AppIconGrid.squareFillThreshold,
                String(format: "%@ fills %.4f%% of its own body box — at or past the %.4f%% boundary "
                       + "between a rounded tile (%.4f%%, the rx=\"229\" the artwork declares) and a hard "
                       + "SQUARE (100%%). The corner radius has been dropped or shrunk in the artwork",
                       image.filename, fill * 100, AppIconGrid.squareFillThreshold * 100,
                       AppIconGrid.roundedFill * 100)
            )
            XCTAssertGreaterThanOrEqual(
                fill, AppIconGrid.circleFillThreshold,
                String(format: "%@ fills only %.4f%% of its own body box — at or under the %.4f%% boundary "
                       + "between a rounded tile (%.4f%%, the rx=\"229\" the artwork declares) and an "
                       + "inscribed CIRCLE (%.4f%%). The body is hollow, over-rounded, or partly "
                       + "transparent: the box is measured from the outermost pixels at alpha ≥ %d, so a "
                       + "handful of stray opaque pixels can hold a correct-looking box around almost "
                       + "nothing. Check that brand/generate.sh still renders the whole body rather than "
                       + "an inset of it, and that no step introduced a global alpha",
                       image.filename, fill * 100, AppIconGrid.circleFillThreshold * 100,
                       AppIconGrid.roundedFill * 100, AppIconGrid.circleFill * 100,
                       Int(AppIconGrid.opaqueAlpha))
            )
        }
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

    /// A hard-cornered square at the correct grid fraction must PASS the grid predicate and FAIL the
    /// rounding one — which is what makes the rounding assertion evidence rather than decoration.
    func testOnlyTheRoundingPredicateRejectsAHardCorneredSquare() throws {
        var asserted = 0
        for image in try declaredImages() {
            let real = try raster(image.filename)
            let mutant = try XCTUnwrap(AppIconGrid.squaredCorners(real),
                                       "\(image.filename): the squared-corner mutation produced nothing")
            let measurement = try XCTUnwrap(AppIconGrid.measure(mutant),
                                            "\(image.filename): the mutant is not measurable")
            let corners = AppIconGrid.cornerAlphas(mutant, box: measurement.box)
            let fill = AppIconGrid.bodyFill(mutant, box: measurement.box)

            // The mutation genuinely happened — asserted before any verdict below is read as evidence.
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
            if AppIconGrid.isExactOnPixelGrid(canvas: image.canvas) {
                XCTAssertGreaterThan(
                    fill, AppIconGrid.squareFillThreshold,
                    "\(image.filename): a hard-cornered square passed the body-fill predicate — that half "
                    + "cannot fail either"
                )
                asserted += 1
            }
            print(String(format: "[app-icon-grid] canary hard-cornered %@ — on-grid %@, body-box fill "
                         + "%.4f%% (threshold %.4f%%), corner alphas %@ → grid ACCEPTED, rounding REJECTED",
                         image.filename, "\(measurement.onGrid)", fill * 100,
                         AppIconGrid.squareFillThreshold * 100, "\(corners)"))
        }
        // The scoped half runs on a non-empty ladder — a carve-out that carved out everything would leave
        // `squareFillThreshold` unexercised while this canary still reported green. Derived from the
        // manifest rather than written as a count, so it cannot go stale against the shipped ladder.
        let eligible = try declaredImages().filter { AppIconGrid.isExactOnPixelGrid(canvas: $0.canvas) }
        XCTAssertFalse(eligible.isEmpty, "no shipped canvas is on the pixel grid, so the body-fill half of "
                       + "the rounding check is never asserted anywhere")
        XCTAssertEqual(asserted, eligible.count,
                       "the body-fill half was canaried on \(asserted) of \(eligible.count) eligible images")
    }

    // MARK: - CONSTRAINT-A for the two bounds issue #1148 added

    /// A hard-cornered square at a uniform alpha 249 — accepted by EVERY predicate that shipped before
    /// #1148, and rejected only by the opacity floor.
    ///
    /// The targeted canary for that floor, and the asymmetry is the whole evidence: this is the same
    /// squared-corner mutant `testOnlyTheRoundingPredicateRejectsAHardCorneredSquare` uses, dimmed by 2.4 %,
    /// and that one 6-count slide moves it from "rejected by the rounding predicate" to "accepted by all of
    /// it" — corners read 249 (< 255, so "rounded") and the box fills to 97.6471 % (< 97.8535 %, so "not
    /// square"). A canary the strengthened gate rejects for some OTHER reason would say nothing about
    /// whether the floor is load-bearing, so every prior predicate is asserted to ACCEPT it here.
    func testOnlyTheOpacityFloorRejectsAUniformlyDimmedSquare() throws {
        let dimmed = 249.0 / 255.0
        var rejected = 0
        for image in try declaredImages() {
            let real = try raster(image.filename)
            let square = try XCTUnwrap(AppIconGrid.squaredCorners(real),
                                       "\(image.filename): the squared-corner mutation produced nothing")
            let mutant = try XCTUnwrap(AppIconGrid.alphaScaled(square, scale: dimmed),
                                       "\(image.filename): the alpha-scale mutation produced nothing")
            let measurement = try XCTUnwrap(AppIconGrid.measure(mutant),
                                            "\(image.filename): the mutant is not measurable")
            let corners = AppIconGrid.cornerAlphas(mutant, box: measurement.box)
            let fill = AppIconGrid.bodyFill(mutant, box: measurement.box)
            let peak = AppIconGrid.peakAlpha(mutant, box: measurement.box)

            // Both mutations genuinely happened, before any verdict below is read as evidence: a box filled
            // to exactly 249/255 is a square that has been dimmed, and is neither on its own.
            XCTAssertEqual(fill, dimmed, accuracy: 1e-9,
                           "\(image.filename): the mutant is not a uniformly dimmed FULL box (fill \(fill)) "
                           + "— one of the two mutations has become a no-op")

            // Everything that shipped before the floor accepts it. This half is evidence about the DEFECT.
            XCTAssertTrue(measurement.onGrid,
                          "\(image.filename): the dimmed square was rejected by the GRID predicate "
                          + "(\(measurement)) — it is supposed to be on-grid and wrong only in opacity")
            XCTAssertTrue(corners.allSatisfy { $0 < 255 },
                          "\(image.filename): the per-corner read rejected alphas \(corners) — then this "
                          + "canary is not isolating the opacity floor")
            if AppIconGrid.isExactOnPixelGrid(canvas: image.canvas) {
                XCTAssertLessThanOrEqual(fill, AppIconGrid.squareFillThreshold,
                                         "\(image.filename): the body-fill upper bound rejected the dimmed "
                                         + "square — then this canary is not isolating the opacity floor")
                XCTAssertGreaterThanOrEqual(fill, AppIconGrid.circleFillThreshold,
                                            "\(image.filename): the body-fill lower bound rejected the "
                                            + "dimmed square — then this canary is not isolating the floor")
            }

            // And the floor rejects it, at every size. This half is evidence about the FIX.
            XCTAssertNotEqual(peak, 255,
                              "\(image.filename): a hard-cornered square at alpha 249 reached full opacity "
                              + "(peak \(peak)) — the floor cannot fail on the defect it exists to catch")
            print(String(format: "[app-icon-grid] canary dimmed-square %@ — on-grid %@, fill %.4f%% (band "
                         + "%.4f%%…%.4f%%), corner alphas %@, peak %d → every prior predicate ACCEPTED, "
                         + "opacity floor REJECTED",
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
            let fill = AppIconGrid.bodyFill(mutant, box: measurement.box)
            let peak = AppIconGrid.peakAlpha(mutant, box: measurement.box)

            // The mutation genuinely happened: the box survived intact AND the body inside it collapsed.
            XCTAssertEqual(measurement.box, realBox,
                           "\(image.filename): the pins did not hold the original box (\(measurement.box) "
                           + "vs \(realBox)) — the mutation is untargeted and the grid predicate, not the "
                           + "fill floor, is what would reject it")
            XCTAssertLessThan(fill, 0.5,
                              "\(image.filename): the body did not actually hollow out (fill \(fill)) — the "
                              + "mutation has become a no-op")

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
                                     "\(image.filename): the body-fill UPPER bound rejected a body that is "
                                     + "almost empty — the two bounds are the wrong way round")

            print(String(format: "[app-icon-grid] canary hollow-body %@ — on-grid %@, fill %.4f%% (floor "
                         + "%.4f%%), corner alphas %@, peak %d → every other predicate ACCEPTED, body-fill "
                         + "floor %@",
                         image.filename, "\(measurement.onGrid)", fill * 100,
                         AppIconGrid.circleFillThreshold * 100, "\(corners)", Int(peak),
                         AppIconGrid.isExactOnPixelGrid(canvas: image.canvas)
                             ? "REJECTED" : "not asserted, canvas is off the pixel grid"))

            // And the fill floor rejects it, where it runs. Evidence about the FIX.
            guard AppIconGrid.isExactOnPixelGrid(canvas: image.canvas) else { continue }
            XCTAssertLessThan(fill, AppIconGrid.circleFillThreshold,
                              "\(image.filename): a body filling \(fill) of its own box passed the fill "
                              + "floor — that bound cannot fail, so its green is not evidence")
            asserted += 1
        }
        let eligible = try declaredImages().filter { AppIconGrid.isExactOnPixelGrid(canvas: $0.canvas) }
        XCTAssertFalse(eligible.isEmpty, "no shipped canvas is on the pixel grid, so the body-fill floor is "
                       + "never asserted anywhere")
        XCTAssertEqual(asserted, eligible.count,
                       "the body-fill floor was canaried on \(asserted) of \(eligible.count) eligible images")
    }

    /// The opacity floor reaches the sizes the fill bounds carve out — the other two rows of #1148's table.
    ///
    /// Both are uniform alpha scales, and both were accepted at canvases where `isExactOnPixelGrid` is
    /// false: a hard square at alpha **254** passed everything at 16/32/64/128 because only the per-corner
    /// read runs there, and a 50 %-transparent icon passed at 16/128/256/512/1024. The floor is asserted at
    /// every declared size, so it closes both across the whole ladder rather than on the exact half — which
    /// is a real part of why the carve-out was kept as derived rather than widened (`isExactOnPixelGrid`).
    func testTheOpacityFloorReachesTheSizesTheFillBoundsCarveOut() throws {
        let cases: [(name: String, scale: Double, expectedPeak: UInt8, square: Bool)] = [
            ("hard square at alpha 254", 254.0 / 255.0, 254, true),
            ("uniformly 50 %-transparent icon", 0.5, 128, false),
        ]
        var carvedOut = 0
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
            }
            if !AppIconGrid.isExactOnPixelGrid(canvas: image.canvas) { carvedOut += 1 }
        }
        XCTAssertGreaterThan(carvedOut, 0,
                             "every shipped canvas is on the pixel grid, so this test proves nothing about "
                             + "the floor reaching past the fill bounds' carve-out")
        print("[app-icon-grid] canary carved-out reach — the opacity floor rejected both uniform alpha "
              + "scales at all \(try declaredImages().count) sizes, \(carvedOut) of them off the pixel grid "
              + "where neither fill bound runs")
    }
}
#endif
