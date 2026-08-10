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
// mutation rather than by inspection. Two mutants, both driven through the SAME `AppIconGrid.measure` and
// `AppIconGrid.cornerAlphas` calls the real assertions above use:
//
//   • a FULL-BLEED raster — the pre-#952 shape, and the exact regression this gate exists to catch. The
//     grid predicate must reject it.
//   • a HARD-CORNERED SQUARE at the correct grid fraction — what dropping `icon.svg`'s `rx` would ship.
//     The grid predicate must ACCEPT it, and only the corner predicate rejects it. That asymmetry is the
//     evidence the corner assertion is load-bearing rather than decorative: the box metric is structurally
//     blind to it, the same way a whole-row ink metric was blind to two deleted elements (ADR-0031's
//     "composite blindness"). A canary that everything rejects would prove nothing about either half.
//
// Both mutants are checked to have genuinely mutated before their verdict is read — a canary that has
// quietly stopped mutating passes forever (ADR-0031's "no-op mutation", which fired for real in #768).

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
    /// Two halves, because neither catches what the other does — and they cover different ladders:
    ///
    ///   • **per-corner, every declared size**: a body-box corner pixel that is FULLY opaque is a square
    ///     corner. Catches ONE squared corner, which moves the aggregate by a quarter of the deficit and
    ///     would stay inside its threshold.
    ///   • **aggregate, the sizes whose canvas `256` divides**: the body box is filled to 95.71 %, not
    ///     100 %. Reads the deficit's MAGNITUDE, so an `rx` shrunk to near-nothing — which leaves every
    ///     corner pixel empty and sails through the per-corner read — is caught. Scoped, because the model
    ///     divides by the measured box and only there is that box the ideal body; `AppIconGrid`'s
    ///     `squareFillThreshold` carries the measurement that bounds it.
    func testEveryEmittedSizeKeptItsBakedCornerRadius() throws {
        var lines: [String] = []
        for image in try declaredImages() {
            let raster = try raster(image.filename)
            let box = try XCTUnwrap(AppIconGrid.bodyBox(of: raster), "\(image.filename) carries no body")
            let corners = AppIconGrid.cornerAlphas(raster, box: box)
            let fill = AppIconGrid.bodyFill(raster, box: box)
            let exact = AppIconGrid.isExactOnPixelGrid(canvas: image.canvas)
            lines.append(String(format: "[app-icon-grid] %@ — body-box fill %.4f%% (rounded %.4f%%, square "
                                + "%.4f%%, threshold %.4f%%%@), corner alphas %@",
                                image.filename, fill * 100, AppIconGrid.roundedFill * 100,
                                AppIconGrid.squareFill * 100, AppIconGrid.squareFillThreshold * 100,
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
}
#endif
