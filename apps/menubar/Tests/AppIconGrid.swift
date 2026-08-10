// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The macOS app-icon grid measurement instrument (issue #991) — the opaque-content bounding box of an
// emitted raster, and the verdict on whether that box sits on Apple's published grid.
//
// WHY IT EXISTS AT ALL. Issue #952 moved the app icon onto the grid (824 of a 1024 canvas) and the
// instrument that verified it lived in `.tmp/` — uncommitted, and now gone. It was the load-bearing
// evidence for the fix, so its loss left the invariant asserted by nothing: no test in `src/` or
// `apps/menubar/Tests/` measured icon geometry, and no CI job references `brand/**` at all. A future edit
// to `brand/generate.sh` could re-point `AppIcon.appiconset` at the full-bleed raster set and reintroduce
// the exact defect, with every gate still green.
//
// WHY IT MEASURES THE RASTER, NOT THE SVG. `actool` reinterprets SVG in ways only visible on the emitted
// artwork — the `.symbolset` precedent (issue #532) is the local one, where a `stroke-width` silently
// became a fill and only an on-device read caught it. The shipped artifact is the PNG, so the PNG is the
// subject. `brand/generate.sh` is the producer and a producer's source is not evidence about its output.
//
// WHY IT LIVES IN THIS BUNDLE. The rasters are committed under `apps/menubar/Sources/Assets.xcassets/`,
// which is inside the `swift` CI job's `apps/menubar/**` path filter — so a change to the very artifact
// this guards runs this gate, in a job that already exists. That is the coupling the design doc reached
// for (`docs/design/panel-presentation-reference-coverage-solution-design.md` § 18.3, "Cap-4.1 upgrade"):
// no new CI job, no `ci-ok.needs` edit, and no `Gate-Change-Acknowledged:` trailer. It is also headless by
// construction — decoding a PNG and walking its alpha channel needs no window, no screen and no TCC grant
// — so it runs inside the standalone `TEST_HOST: ""` bundle the required `swift` job drives.
//
// The stated limit, so nobody reads more into a green than it carries: a `brand/**`-only edit matches NO
// path filter in `ci.yml` and runs zero jobs (same design doc, § 18.3, Cap-4.2). This gate is coupled to
// the SHIPPED artifact, not to its producer — which is the right coupling for an invariant about what
// ships, but it means a `generate.sh` regression is caught at the moment the rasters are regenerated, not
// at the moment the script is edited.
//
// PREDICATES, NOT ASSERTIONS. Everything here returns a measurement or a verdict; nothing calls
// `XCTAssert`. That is deliberate and is what makes CONSTRAINT-A satisfiable: `AppIconGridTests` drives
// its canaries through the SAME `measure` / `cornerAlphas` calls the real assertions use, so a canary
// that reddens is evidence about the real gate rather than about a parallel simplified check.

#if DEBUG
import Foundation

/// The opaque-content bounding box of a raster, in pixel coordinates on its canvas.
///
/// The edges are exposed as CONTINUOUS pixel-boundary coordinates rather than indices: a body occupying
/// columns 2…13 spans `[2, 14)`. That is the frame the grid is stated in — an inset is a distance, not an
/// index — and it is what lets the tolerance below be derived per edge instead of guessed per span.
struct AppIconBodyBox: Equatable {
    let minX: Int
    let minY: Int
    let maxX: Int
    let maxY: Int

    var width: Int { maxX - minX + 1 }
    var height: Int { maxY - minY + 1 }

    var left: Double { Double(minX) }
    var right: Double { Double(maxX + 1) }
    var top: Double { Double(minY) }
    var bottom: Double { Double(maxY + 1) }
}

/// One raster measured against the grid.
struct AppIconGridMeasurement: Equatable, CustomStringConvertible {
    let canvas: Int
    let box: AppIconBodyBox
    /// The ideal body edges on this canvas, in continuous pixel coordinates.
    let idealNear: Double
    let idealFar: Double
    /// `measured − ideal`, in pixels, for left, right, top and bottom in that order.
    let edgeErrors: [Double]

    var worstEdgeError: Double { edgeErrors.map { abs($0) }.max() ?? .infinity }
    var onGrid: Bool { worstEdgeError <= AppIconGrid.edgeTolerance }
    /// Body span as a fraction of canvas — the number the PRD's `IconGridConformance` scale is stated in.
    var bodyFraction: Double { Double(box.width) / Double(canvas) }

    var description: String {
        let errs = edgeErrors.map { String(format: "%+.4f", $0) }.joined(separator: " ")
        return String(format: "%d×%d canvas: body %d×%d at (%d,%d) = %.4f%% of canvas "
                      + "(ideal %.4f…%.4f, edge errors L R T B = %@, worst %.4f px)",
                      canvas, canvas, box.width, box.height, box.minX, box.minY, bodyFraction * 100,
                      idealNear, idealFar, errs, worstEdgeError)
    }
}

enum AppIconGrid {

    // MARK: - The grid, and the one tolerance

    /// Apple's published macOS app-icon template: an 824 body on a 1024 canvas — 80.46875 %.
    ///
    /// Corroborated by measurement rather than taken on trust: five shipping macOS apps land on 412 of a
    /// 512 canvas with zero variance, and a separate pass over six system apps reproduced the same fraction
    /// at 256. `brand/README.md` § "The app icon sits on the macOS grid" carries both lines and is the
    /// grounding this constant is read from — NOT the current output of `brand/generate.sh`, which is the
    /// subject under test and cannot also be its own oracle.
    static let bodySpan = 824
    static let templateCanvas = 1024

    /// A pixel counts as body at alpha ≥ 128 — the HALF-COVERED contour, which is the real geometric edge.
    ///
    /// Not any-alpha: an any-alpha box also catches the antialiased fringe and reads several points high in
    /// a way that looks like a plausible answer (83.0–83.6 % for the five peers, against their true 80.47).
    /// The threshold is load-bearing, not incidental — `brand/README.md` records it as part of the grid.
    static let opaqueAlpha: UInt8 = 128

    /// How far a measured edge may sit from its ideal, in pixels: **half a pixel, per edge.**
    ///
    /// DERIVED, NOT CALIBRATED — this is the whole reason the number is 0.5 and not something read off the
    /// current output. A rasterizer resolves a continuous edge onto the pixel grid by coverage: a boundary
    /// pixel is body iff it is at least half covered, which for an axis-aligned edge is exactly "its centre
    /// lies inside". So the emitted edge is the ideal edge ROUNDED TO THE NEAREST PIXEL BOUNDARY, and the
    /// distance from any real number to the nearest integer is at most 0.5 by definition. Closed rather
    /// than strict, because an edge landing exactly on a pixel centre is exactly half covered — alpha
    /// 127.5, which the encoder may round either side of the threshold above.
    ///
    /// Three consequences worth stating, because they are what make ONE rule enough:
    ///
    /// 1. **Where 256 divides the canvas it collapses to equality.** The ideal edges are `canvas × 100/1024`
    ///    and `canvas × 924/1024`; both are integers iff `256 | canvas` (100/1024 = 25/256 in lowest terms,
    ///    and gcd(25,256) = 1). On this ladder that is exactly {256, 512, 1024}. When the ideal is an
    ///    integer, `|measured − ideal| ≤ 0.5` with both integers forces `measured == ideal`. So the issue's
    ///    "equality at ≥ 256, tolerance below" is not two rules — it is this one rule, and the 256 cut is
    ///    where the grid stops being exactly representable on the pixel grid rather than a size chosen.
    /// 2. **Below 256 it predicts the quantisation rather than absorbing it.** At canvas 16 the ideal body
    ///    is 12.875 px and the box quantises to 12; at 32, 25.75 quantises to 26. Both are edge errors of
    ///    0.4375 and 0.125 — comfortably inside the bound, and both match what Calculator, Notes and Mail's
    ///    own native rasters measure at those sizes.
    /// 3. **It stays a gate.** A full-bleed revert misses by 100 px at canvas 1024, not by one.
    static let edgeTolerance = 0.5

    /// True when the ideal body edges land exactly on pixel boundaries — that is, when `256 | canvas`.
    ///
    /// Derived in `edgeTolerance` above. Two things follow, and the second is why this is a named predicate
    /// rather than a remark in that comment: the tolerance collapses to equality, AND the measured body box
    /// IS the ideal body rather than a rounding of it — the precondition for any measurement whose
    /// denominator is the box.
    static func isExactOnPixelGrid(canvas: Int) -> Bool { canvas % 256 == 0 }

    /// The ideal body edges on `canvas`, in continuous pixel coordinates.
    static func idealEdges(canvas: Int) -> (near: Double, far: Double) {
        let inset = Double(templateCanvas - bodySpan) / 2 / Double(templateCanvas)
        return (Double(canvas) * inset, Double(canvas) * (1 - inset))
    }

    // MARK: - Measurement

    /// Alpha at `(x, y)`. `PanelRaster` is 8-bit RGBA premultiplied-last, so alpha is byte 3 and is
    /// unaffected by the premultiplication — a body pixel reads its true coverage whatever its colour is.
    static func alpha(_ raster: PanelRaster, x: Int, y: Int) -> UInt8 {
        raster.bytes[(y * raster.width + x) * 4 + 3]
    }

    /// The bounding box of every pixel at alpha ≥ `opaqueAlpha`, or `nil` when the raster carries none.
    ///
    /// `nil` is a real answer, not a failure to compute: a raster with no body is the degenerate subject a
    /// gate must refuse rather than silently pass, so the caller is forced to handle it.
    static func bodyBox(of raster: PanelRaster, opaqueAlpha: UInt8 = AppIconGrid.opaqueAlpha) -> AppIconBodyBox? {
        guard raster.width > 0, raster.height > 0 else { return nil }
        var minX = raster.width, minY = raster.height, maxX = -1, maxY = -1
        for y in 0..<raster.height {
            for x in 0..<raster.width where alpha(raster, x: x, y: y) >= opaqueAlpha {
                if x < minX { minX = x }
                if x > maxX { maxX = x }
                if y < minY { minY = y }
                if y > maxY { maxY = y }
            }
        }
        guard maxX >= 0, maxY >= 0 else { return nil }
        return AppIconBodyBox(minX: minX, minY: minY, maxX: maxX, maxY: maxY)
    }

    /// Measure `raster` against the grid. `nil` when it is not a square canvas or carries no body.
    static func measure(_ raster: PanelRaster) -> AppIconGridMeasurement? {
        guard raster.width == raster.height, let box = bodyBox(of: raster) else { return nil }
        let canvas = raster.width
        let ideal = idealEdges(canvas: canvas)
        return AppIconGridMeasurement(
            canvas: canvas,
            box: box,
            idealNear: ideal.near,
            idealFar: ideal.far,
            edgeErrors: [box.left - ideal.near, box.right - ideal.far,
                         box.top - ideal.near, box.bottom - ideal.far]
        )
    }

    // MARK: - The corner radius

    // WHY THIS IS MEASURED AT ALL. macOS is NOT iOS and applies no mask of its own — every peer measured
    // reads its rounding in its own artwork, so dropping `icon.svg`'s `rx="229"` would ship a hard-cornered
    // SQUARE. `docs/specs/app-icon-grid.feature.md` asks for exactly that drop and is wrong; `generate.sh`
    // and `brand/README.md` § "The baked `rx` stays" both record the correction. A square has the SAME
    // bounding box as the shipped tile, so `measure` above is structurally blind to it: this is the second
    // probe that blindness needs.

    /// The corner radius as a fraction of the body span — `rx="229"` on `icon.svg`'s 1024 viewBox.
    ///
    /// The ratio is scale-invariant, so it is the same 22.36 % after `inset_app_icon` shrinks the master to
    /// 824: a radius of 229/1024 on the master becomes 184.3/824 on the body. Read from the artwork's own
    /// source, not from the emitted rasters — the rasters are the subject and cannot be their own oracle.
    static let cornerRadiusRatio = 229.0 / 1024.0

    /// Alpha-weighted coverage of the body box: `Σα / (255 · width · height)`.
    ///
    /// Alpha-weighted rather than thresholded because the quantity of interest is an AREA, and a rasterizer
    /// already integrates partial coverage into alpha. Counting pixels over a threshold would throw that
    /// integration away and leave the 12×12 body at canvas 16 measuring in steps of 0.7 %.
    static func bodyFill(_ raster: PanelRaster, box: AppIconBodyBox) -> Double {
        var total = 0
        for y in box.minY...box.maxY {
            for x in box.minX...box.maxX { total += Int(alpha(raster, x: x, y: y)) }
        }
        return Double(total) / (255.0 * Double(box.width) * Double(box.height))
    }

    /// What `bodyFill` reads for a rounded rectangle at `cornerRadiusRatio` — the shipped artwork.
    ///
    /// Four quarter-discs of radius `r = k·W` replace four `r × W` corners, removing `(4 − π)r²` of a `W²`
    /// box, so the fill is `1 − (4 − π)k²` = 95.71 %.
    static let roundedFill = 1 - (4 - Double.pi) * cornerRadiusRatio * cornerRadiusRatio

    /// What `bodyFill` reads for a hard-cornered square: the whole box.
    static let squareFill = 1.0

    /// The most a body box may be filled before it is called square.
    ///
    /// DERIVED AS THE MAXIMUM-MARGIN BOUNDARY between the two hypotheses this predicate discriminates —
    /// rounded at the artwork's own `rx` (95.71 %) and hard-cornered (100 %) — so it is the midpoint,
    /// 97.85 %, and neither hypothesis is favoured by the placement. A threshold fitted to the current
    /// output would instead pin whatever the rasterizer happens to do today.
    ///
    /// **Only meaningful where `isExactOnPixelGrid`**, and that bound is measured, not assumed. The model
    /// divides by the measured box, so it needs the box to BE the ideal body; where it is not, the
    /// denominator error swamps the 4.29 pp signal. Measured across the ladder: {256, 512, 1024} read
    /// 95.6927 / 95.7081 / 95.7065 % against the model's 95.7070 — agreement to 0.015 pp. The other four
    /// read 93.7493 (32), 93.7726 (64), 94.5127 (128) — LOW, because the box rounds outward and carries
    /// margin — and 98.8617 (16), HIGH, because there the box rounds INWARD and clips the body's own outer
    /// edge, at a 12-pixel body where half a pixel per edge is 7 % of the denominator. So this half of the
    /// rounding check is asserted only where `isExactOnPixelGrid`; `cornerAlphas` covers every size.
    static let squareFillThreshold = (roundedFill + squareFill) / 2

    /// Alpha at the four corners of `box` — top-left, top-right, bottom-left, bottom-right.
    ///
    /// The per-corner half of the rounding check, and it catches what the aggregate above cannot: ONE
    /// squared corner moves `bodyFill` by a quarter of the deficit, which stays inside the threshold. What
    /// it asserts is that the corner pixel is not FULLY covered — not that it is empty. At canvas 16 the
    /// radius is 2.88 px and the arc cuts the corner pixel at ~59 % coverage, so it measures ~150; that is
    /// the rounding being present at a coarse pixel grid, not absent.
    static func cornerAlphas(_ raster: PanelRaster, box: AppIconBodyBox) -> [UInt8] {
        [alpha(raster, x: box.minX, y: box.minY),
         alpha(raster, x: box.maxX, y: box.minY),
         alpha(raster, x: box.minX, y: box.maxY),
         alpha(raster, x: box.maxX, y: box.maxY)]
    }

    // MARK: - Mutations, for the CONSTRAINT-A canaries

    /// The subject as it was BEFORE issue #952: the same artwork with its body filling the whole canvas.
    ///
    /// Built by cropping to the measured body box and resampling nearest-neighbour up to the canvas, so it
    /// is a real full-bleed render of this icon rather than a synthetic stand-in, and so the mutation
    /// cannot silently become a no-op — `nil` if there is nothing to crop, and the caller asserts the
    /// result genuinely measures 100 % before drawing any conclusion from the gate rejecting it.
    static func fullBleed(_ raster: PanelRaster) -> PanelRaster? {
        guard let box = bodyBox(of: raster), raster.width == raster.height else { return nil }
        let canvas = raster.width
        var bytes = [UInt8](repeating: 0, count: canvas * canvas * 4)
        for y in 0..<canvas {
            let sy = box.minY + y * box.height / canvas
            for x in 0..<canvas {
                let sx = box.minX + x * box.width / canvas
                let src = (sy * raster.width + sx) * 4
                let dst = (y * canvas + x) * 4
                bytes[dst] = raster.bytes[src]
                bytes[dst + 1] = raster.bytes[src + 1]
                bytes[dst + 2] = raster.bytes[src + 2]
                bytes[dst + 3] = raster.bytes[src + 3]
            }
        }
        return PanelRaster(width: canvas, height: canvas, bytes: bytes)
    }

    /// The subject as it would be if the baked `rx` were dropped: an opaque square filling the SAME body
    /// box, transparent outside it.
    ///
    /// The targeted half of the canary pair. Its bounding box is identical to the real raster's, so it
    /// PASSES the grid measurement — which is exactly the point: it isolates what the box metric is blind
    /// to, the way removing two elements from a panel row once *raised* its ink coverage
    /// (ADR-0031 § Decision 4, "composite blindness"). Only the corner read can reject it.
    static func squaredCorners(_ raster: PanelRaster) -> PanelRaster? {
        guard let box = bodyBox(of: raster) else { return nil }
        // A real body colour, so the mutant is a plausible icon rather than an obviously-broken one: the
        // centre pixel is inside the body at every size on this ladder.
        let centre = ((raster.height / 2) * raster.width + raster.width / 2) * 4
        let body = (raster.bytes[centre], raster.bytes[centre + 1], raster.bytes[centre + 2])
        var bytes = [UInt8](repeating: 0, count: raster.width * raster.height * 4)
        for y in box.minY...box.maxY {
            for x in box.minX...box.maxX {
                let dst = (y * raster.width + x) * 4
                bytes[dst] = body.0
                bytes[dst + 1] = body.1
                bytes[dst + 2] = body.2
                bytes[dst + 3] = 255
            }
        }
        return PanelRaster(width: raster.width, height: raster.height, bytes: bytes)
    }
}
#endif
