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
// its canaries through the SAME `measure` / `cornerAlphas` / `idealBodyFill` / `peakAlpha` calls the real
// assertions use, so a canary that reddens is evidence about the real gate rather than about a parallel
// simplified check.

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
    ///
    /// 128 is HALF of 255, and that is the whole content of "half-covered" — so this contour means what it
    /// says only while 255 means fully covered. That anchor is asserted rather than assumed; `peakAlpha`
    /// below carries it and states the regime.
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
    /// Derived in `edgeTolerance` above, and this is the whole of what it says: where the ideal edges are
    /// integers the half-pixel tolerance COLLAPSES TO EQUALITY — two integers within 0.5 of each other are
    /// the same integer — so the measured body box IS the ideal body rather than a rounding of it.
    ///
    /// It is a named predicate rather than a remark in that comment because the grid assertion tests it
    /// directly: `testEveryEmittedSizeSitsOnTheMacOSAppIconGrid` asks for EQUALITY at these canvases, not
    /// merely for tolerance. Measured on the committed set, all five land on the ideal edges to the pixel —
    /// `[25,230]` at 256, `[50,461]` at 512, `[100,923]` at 1024 — so the stronger claim is the true one and
    /// asserting only the tolerance there would be leaving a derived consequence unread.
    ///
    /// **IT NO LONGER GATES THE FILL BOUNDS** (issue #1160). It used to, and the reason was structural
    /// rather than incidental: `bodyFill` divides by the MEASURED box, so it needs that box to BE the ideal
    /// body, which restricted both bounds to 5 of the 10 shipped rasters. `idealBodyFill` divides by the
    /// ideal body AREA, which is exact at every canvas — so both bounds now run on all ten and the carve-out
    /// is retired rather than widened. Two bodies of reasoning went with it, and neither is lost: the
    /// weighed-and-kept argument for the carve-out (issue #1148 R-3) was an argument about the measured-box
    /// estimator and does not survive its replacement, and the measured list of what the carve-out left
    /// uncovered (issue #1149) is closed by the same change — a hollow body and a shrunken radius are now
    /// read at every size. One item on that list was never about the carve-out at all: the per-corner read's
    /// own reach off the grid, which is a property of `cornerAlphas` and is stated there, still open as
    /// **#1320**.
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
    ///
    /// **NOT the subject of the fill bounds** — `idealBodyFill` is (issue #1160). What remains here is a
    /// BOX-RELATIVE read, and that is exactly what the mutation no-op checks want: `squaredCorners` fills
    /// the measured box by construction, so "its box is filled to 1.0" is the statement that the mutation
    /// happened, in the terms the mutation is defined in. Reading those through the ideal-body estimator
    /// instead would fold the box-vs-ideal offset into a check whose whole job is to be trivially true.
    static func bodyFill(_ raster: PanelRaster, box: AppIconBodyBox) -> Double {
        var total = 0
        for y in box.minY...box.maxY {
            for x in box.minX...box.maxX { total += Int(alpha(raster, x: x, y: y)) }
        }
        return Double(total) / (255.0 * Double(box.width) * Double(box.height))
    }

    /// The side of the ideal body on `canvas`, in continuous pixels: `canvas × 824/1024`. A real number,
    /// exact at every canvas — no quantisation, and nothing read off the raster.
    static func idealBodySide(canvas: Int) -> Double {
        Double(canvas) * Double(bodySpan) / Double(templateCanvas)
    }

    /// The pixel rows and columns the ideal body can reach: the ideal edges rounded OUTWARD.
    ///
    /// `nil` for a degenerate canvas, on the same rule `bodyBox` follows — a subject that cannot carry the
    /// measurement is refused rather than silently given a number.
    ///
    /// Outward, not nearest, because the region has one job: **provably contain all of the body's coverage**.
    /// The body occupies `[near, far]` in continuous coordinates, so the pixels it can touch are
    /// `⌊near⌋ … ⌈far⌉ − 1` — every pixel the interval intersects, including the two whose coverage is
    /// partial. Rounding to nearest would clip exactly the antialiased fringe that carries the difference.
    /// At canvas 16 that is pixels 1…14 against a 12-px measured box of 2…13: the outward region keeps the
    /// two edge columns the box's inward rounding throws away.
    ///
    /// Where `isExactOnPixelGrid` the rounding is a no-op and this region IS the measured box — which is why
    /// the five exact rasters read identically under both estimators, rather than nearly so.
    static func idealBodyRegion(canvas: Int) -> ClosedRange<Int>? {
        guard canvas > 0 else { return nil }
        let ideal = idealEdges(canvas: canvas)
        let lo = max(0, Int(ideal.near.rounded(.down)))
        let hi = min(canvas - 1, Int(ideal.far.rounded(.up)) - 1)
        guard lo <= hi else { return nil }
        return lo...hi
    }

    /// Alpha-weighted coverage of the IDEAL body: `Σα` over `idealBodyRegion`, divided by the ideal body
    /// area `(canvas × 824/1024)²`. `nil` when the canvas is not square or cannot carry the region.
    ///
    /// **WHY THE DENOMINATOR IS NOT THE MEASURED BOX** (issue #1160). `bodyFill` divides by the box, which
    /// is the ideal body only where `256 | canvas`; off the grid the box is the ideal body ROUNDED, and the
    /// rounding error swamps the signal the bounds discriminate. Measured, that error is not small and its
    /// SIGN is not derivable — it depends on where `canvas × 100/1024` falls relative to the half-pixel:
    /// canvas 16 rounds inward and `bodyFill` reads 98.8617 % (+3.15 pp against a 95.7070 % model), while
    /// 32, 64 and 128 round outward and read 93.7493 / 93.7726 / 94.5127 (−1.96 / −1.93 / −1.19 pp). A bound
    /// asserted on that is asserted on quantisation, which is why it was restricted to five rasters.
    ///
    /// This denominator is a real number computed from the canvas, so it has no quantisation at all, and the
    /// outward-rounded numerator collects the fringe an inward-rounded box clips. Measured across the ten
    /// shipped rasters the readings collapse onto the model: **95.5661 … 95.7143 %**, a spread of 0.1483 pp
    /// against a model of 95.7070 % — worst deviation 0.1409 pp, at canvas 16, where the body is 12.875 px
    /// across and a single edge pixel is 7.8 % of the side. Under `bodyFill` the same ten spread 5.11 pp.
    ///
    /// Two consequences beyond the coverage, both worth having:
    ///
    /// 1. **It does not read the measured box at all.** The region and the denominator come from the canvas,
    ///    so a body that fails to reach its own ideal edges reads low no matter what its bounding box says. The
    ///    hollow-body canary makes that concrete: it reconstructs a perfect box out of four pinned pixels, and those
    ///    pins no longer buy it a DENOMINATOR. They are not inert, though, and an earlier form of this comment said
    ///    they were: the pins land INSIDE the ideal region, so they still contribute their own four pixels to the
    ///    numerator — 2.4036 pp of the canvas-16 reading, 0.6009 pp at 32, 0.0006 pp at 1024.
    /// 2. **It is exact for the hypotheses the bounds discriminate.** A hard square rendered at the ideal
    ///    edges reads 100 % here at every canvas (`squaredIdealBody` measures 99.99–100.05 %, the residual
    ///    being alpha's own 1/255 quantisation), where the same square measured against its box reads 100 %
    ///    only on the grid and 86.87 % at canvas 16 — the wrong side of the FLOOR.
    ///
    /// What it does NOT do is read WHERE the area sits: it is an area estimator, so a body missing area from
    /// its middle rather than its corners reads the same as one missing it from the corners. Measured — a
    /// disc of radius 0.165 × the body box's width, punched about that box's centre, passes BOTH bounds at
    /// canvas 16, 32, 512 and 1024 (88.3269 / 87.7361 / 87.1493 / 87.1513 %), which is six of the ten
    /// files, and is caught by the floor only at 64, 128 and 256 (86.8550 / 87.0236 / 87.1151 %). An
    /// earlier form of this line omitted canvas 32; the set above is re-derived rather than carried. That
    /// residual is real and unchanged by this estimator; it is tracked as **#1331** rather than implied
    /// here.
    static func idealBodyFill(_ raster: PanelRaster) -> Double? {
        guard raster.width == raster.height,
              let region = idealBodyRegion(canvas: raster.width) else { return nil }
        var total = 0
        for y in region {
            for x in region { total += Int(alpha(raster, x: x, y: y)) }
        }
        let side = idealBodySide(canvas: raster.width)
        return Double(total) / (255.0 * side * side)
    }

    /// What `idealBodyFill` reads for a rounded rectangle at `cornerRadiusRatio` — the shipped artwork.
    ///
    /// Four quarter-discs of radius `r = k·W` replace four `r × W` corners, removing `(4 − π)r²` of a `W²`
    /// body, so the fill is `1 − (4 − π)k²` = 95.71 %.
    static let roundedFill = 1 - (4 - Double.pi) * cornerRadiusRatio * cornerRadiusRatio

    /// What `idealBodyFill` reads for a hard-cornered square: the whole body.
    static let squareFill = 1.0

    /// The most the ideal body may be filled before it is called square.
    ///
    /// DERIVED AS THE MAXIMUM-MARGIN BOUNDARY between the two hypotheses this predicate discriminates —
    /// rounded at the artwork's own `rx` (95.71 %) and hard-cornered (100 %) — so it is the midpoint,
    /// 97.85 %, and neither hypothesis is favoured by the placement. A threshold fitted to the current
    /// output would instead pin whatever the rasterizer happens to do today.
    ///
    /// **ASSERTED AT EVERY SHIPPED SIZE**, and the margin is measured rather than assumed (issue #1160).
    /// Read through `idealBodyFill`, the ten rasters span 95.5661 … 95.7143 %, so the binding control is the
    /// HIGHEST reading — 95.7143 % at canvas 128 — and this bound sits **2.1392 pp** above it. Against what
    /// it must reject the separation is the other **2.1389 pp**: a hard square rendered at the ideal body
    /// edges measures 99.9923 … 100.0515 % across the ten, and the margin is taken from the NEAREST of
    /// those, so the two hypotheses stay a full band apart everywhere rather than only on the five rasters
    /// the old measured-box estimator could speak about.
    ///
    /// It was previously scoped to `isExactOnPixelGrid`, which restricted it to 5 of the 10 rasters; that
    /// carve-out was a property of `bodyFill`'s denominator and retired with it. The predicate itself was
    /// not the problem and is still correct about what it says — see its own comment.
    ///
    /// **THE RADIUS SET THIS ACCEPTS, so no reader takes it for a radius pin** (issue #1149). Because the
    /// threshold is the MIDPOINT of the same one-parameter family the constants above use,
    /// `fill(k) = 1 − (4 − π)k²`, solving `fill(k) = (fill(k_declared) + 1)/2` gives `k = k_declared/√2`
    /// exactly — the `(4 − π)` cancels, so the admitted shrink is **29.29 %** of whatever radius the
    /// artwork declares, not a figure fitted to the current one. Against today's `cornerRadiusRatio` that
    /// is `k ≥ 0.1581`, an `rx` down to **162** on the 1024 master against the declared 229. Paired with
    /// `circleFillThreshold` at the other end (`k ≤ 0.3873`, `rx ≤ 396`), the band actually asserted is
    /// `k ∈ [0.1581, 0.3873]` around a declared 0.2236.
    ///
    /// That band is the two-hypothesis discrimination these constants were derived for — rounded against
    /// hard-cornered here, rounded against inscribed-circle there — and it is NOT a statement about the
    /// radius's value. Both endpoints are functions of `cornerRadiusRatio`, so an `rx` legitimately
    /// changed in `icon.svg` moves them with it and the discrimination survives; what does not exist, at
    /// either endpoint, is any assertion that the emitted radius is 22.36 %. Pinning that would mean
    /// fitting a threshold to what the rasterizer happens to do today, which is the trade `edgeTolerance`
    /// and this constant both refuse — so the pin belongs upstream, in `icon.svg` and the
    /// `cornerRadiusRatio` read from it, not here.
    static let squareFillThreshold = (roundedFill + squareFill) / 2

    /// The largest corner radius the rounded-rect model admits: half the body span, where the four
    /// quarter-discs meet and the tile IS a circle inscribed in its own box.
    ///
    /// The far endpoint of the SAME one-parameter family the two constants above already use —
    /// `fill(k) = 1 − (4 − π)k²`, evaluated at the artwork's own `k = 229/1024` for `roundedFill` and at
    /// `k = 0` for `squareFill`. Past `k = ½` the shape is not a rounder rectangle; it is not a rounded
    /// rectangle at all, so this is a bound on the model rather than a value read off anything.
    static let maxCornerRadiusRatio = 0.5

    /// What `idealBodyFill` reads at that limit: `1 − (4 − π)/4 = π/4`, 78.5398 % of the body.
    static let circleFill = 1 - (4 - Double.pi) * maxCornerRadiusRatio * maxCornerRadiusRatio

    /// The LEAST the ideal body may be filled before it is called hollow (issue #1148, R-1).
    ///
    /// DERIVED THE SAME WAY `squareFillThreshold` IS, at the other end. That one is the maximum-margin
    /// boundary between the artwork's declared rounding (95.7070 %) and the near hypothesis, a hard-cornered
    /// square (100 %); this is the maximum-margin boundary between the same null and the FAR hypothesis, an
    /// inscribed circle (78.5398 %). So it is the midpoint, **87.1234 %**, and neither hypothesis is
    /// favoured. Both brackets are functions of `cornerRadiusRatio`, so an `rx` legitimately changed in
    /// `icon.svg` moves them with it: what is asserted is that the EMITTED fill matches the DECLARED radius,
    /// never that the radius is any particular value.
    ///
    /// Why a floor was needed at all: without one, `bodyFill` is bounded only from above, so a body box can
    /// be arbitrarily EMPTY and still pass. Measured (#1148) — a real icon shrunk to 60 % of its box with
    /// the box's four corners held by single pixels at alpha 254 measures a perfectly on-grid body filled to
    /// **34.41 %**, and every shipped predicate accepted it at all ten rasters.
    ///
    /// **The margin, re-derived at every shipped size** (issue #1160). The binding control is the LOWEST
    /// reading across the ten, which through `idealBodyFill` is 95.5661 % at canvas 16 — the number a floor
    /// must clear. This floor sits **8.4427 pp** under it, 60× the largest deviation any raster shows from
    /// the model (0.1409 pp, the same canvas). Against what it must reject it is clearer still: the
    /// 47.97 % half-transparent case is 39.15 pp below it, and the 31.72 % hollow case 55.40 pp below.
    ///
    /// The previous statement of this margin used the LOWEST EXACT-canvas reading (95.6927 % at 256, 8.5693
    /// pp of margin, 601× a 0.0142 pp deviation) because the bound ran on five rasters. Both the control and
    /// the ratio move once it runs on ten: the spread it must sit under is now 0.1483 pp rather than 0.015,
    /// which is the honest cost of the coverage and still two orders of magnitude of headroom.
    ///
    /// What it therefore rejects, stated as thresholds rather than as the three rasters that motivated it:
    /// any uniform alpha scale below **91.02 %** of full (91.03 % against the model; 91.02 % is the measured
    /// value that bites at every canvas, binding on the highest reading); any corner radius past **0.3873**
    /// of the body span (`rx` beyond 396 on the 1024 master, against the declared 229); and any body more
    /// than **4.59 %** linearly short of its IDEAL extent.
    ///
    /// That last one is where the estimator change buys the most. It used to read "short of its own bounding
    /// box", which a mutant can forge: four pinned pixels reconstruct a correct-looking box around almost
    /// nothing, which is exactly how the hollow-body canary defeats every other predicate. `idealBodyFill`
    /// never reads the box, so the extent it measures short against is fixed by the canvas and cannot be
    /// pinned into agreement.
    static let circleFillThreshold = (roundedFill + circleFill) / 2

    /// Alpha at the four corners of `box` — top-left, top-right, bottom-left, bottom-right.
    ///
    /// The per-corner half of the rounding check, and it catches what the aggregate above cannot: ONE
    /// squared corner moves the fill by a quarter of the deficit, which stays inside the threshold. What
    /// it asserts is that the corner pixel is not FULLY covered — not that it is empty. At canvas 16 the
    /// radius is 2.88 px and the arc cuts the corner pixel at ~59 % coverage, so it measures ~150; that is
    /// the rounding being present at a coarse pixel grid, not absent.
    ///
    /// **WHAT IT REACHES, MEASURED** (issue #1320, restated here from `isExactOnPixelGrid` now that the fill
    /// bounds no longer carve anything out — this was always a property of THIS predicate rather than of
    /// that one). Off the pixel grid the ideal body edge lands mid-pixel, so the box's own corner pixel is
    /// only partly covered by the body BEFORE any rounding is applied. Read at mid-height on the shipped
    /// rasters, where the edge is straight, the left-edge pixel measures 112 / 223 / 192 / 130 at canvas
    /// 16 / 32 / 64 / 128 — the 43.75 / 87.5 / 75 / 50 % coverage the ideal edges predict, so the rasterizer
    /// integrates coverage exactly as the area model assumes. A hard square therefore puts that corner pixel
    /// at the product of its two edge coverages: at canvas 16 the 43.75 % pixel falls BELOW the
    /// alpha ≥ `opaqueAlpha` contour, so the box starts one pixel in at a column wholly inside the body and
    /// the corner reads a full 255 — caught. At 32 and 64 the edge pixel is inside the box and the corner
    /// reads 0.875² → 195 and 0.75² → 143; at 128 both ideal edges land ON the half-pixel and the contour
    /// settles the axes differently — column 12 peaks at 130 and is kept, row 12 at 121 and is dropped — so
    /// the box is 104×103 at (12,13) and its corner is half-covered in x and whole in y, the 130 that column
    /// measures. All three are under 255, so at those canvases this read accepts an outright dropped `rx` —
    /// it is not a weakened rounding check there, it is not a rounding check at all. (`squaredIdealBody`
    /// prints 64 at that corner: it rounds a 0.5 coverage to alpha 128 and keeps the row the producer drops.)
    ///
    /// What holds those sizes is no longer an argument about the producer: `idealBodyFill`'s ceiling now
    /// rejects a hard square at every canvas, which is the fix #1320 named (its option 2) and this file
    /// landed. #1320 stays open for its other half — `squaredCorners` fills the MEASURED box, writing 255
    /// into that corner pixel by construction, so it cannot exhibit the gap above and its green over this
    /// predicate is not evidence off the grid. Making that canary faithful reddens this read at 32/64/128,
    /// which is a change to what the corner half claims and belongs with the issue that measured it.
    static func cornerAlphas(_ raster: PanelRaster, box: AppIconBodyBox) -> [UInt8] {
        [alpha(raster, x: box.minX, y: box.minY),
         alpha(raster, x: box.maxX, y: box.minY),
         alpha(raster, x: box.minX, y: box.maxY),
         alpha(raster, x: box.maxX, y: box.maxY)]
    }

    /// The highest alpha anywhere in `box` — the raster's own evidence that full coverage OCCURS in it.
    ///
    /// **THE OPACITY REGIME, STATED AND ASSERTED AT ITS POINT OF USE** (issue #1148, R-2). Three of the
    /// reads above are ABSOLUTE and all three key on opacity magnitude: `opaqueAlpha = 128` is the
    /// half-covered contour, `cornerAlphas < 255` means "not fully covered", and both fill estimators divide
    /// by 255 to turn alpha into area. Every one of them interprets the byte 255 as full coverage — so ONE
    /// global alpha scale moves all of them together, and none can then tell a rounded tile from a
    /// hard-cornered square that has merely been made slightly transparent.
    ///
    /// That is measured, not feared: a hard-cornered square at a uniform alpha **249** is accepted by all
    /// three at all ten shipped rasters, its corners reading 140…249 (all < 255, so "rounded") and its
    /// ideal body filled to **97.65 %** (< 97.8535 %, so "not square"). The corner read trails the dimming
    /// target off the pixel grid, where the box's own corner pixel is only PARTLY covered before it is
    /// dimmed — 190 at canvas 32, 140 at canvas 64, 249 at the other seven files — and under 255 is all the
    /// corner half needs. A square is precisely what that half exists to catch. Widening the fill bounds
    /// to the whole ladder (issue #1160) does not close this and was not expected to — the dimming slides
    /// the square UNDER the ceiling by 0.20 pp at every canvas, which is the point of the mutant.
    ///
    /// So the anchor is asserted rather than assumed, and this is what makes it sound: `brand/src/icon.svg`
    /// fills its body opaque, and `brand/generate.sh` puts that through a SINGLE `rsvg-convert` pass per
    /// size with no compositing stage that could introduce a global alpha. Full opacity is therefore the
    /// artwork's declared state and not an accident of the current output — and all ten shipped rasters
    /// measure exactly 255. Once it holds, the three absolute reads are coverage reads again rather than
    /// arbitrary byte comparisons.
    ///
    /// The bound on what it buys, so nobody reads more into a green than it carries: this anchors the
    /// SCALE, it does not certify every pixel. A raster holding one pixel at 255 while scaling the rest by
    /// 0.976 satisfies it and lands inside both fill bounds. No `rsvg-convert` pass emits that shape — it is
    /// a crafted subject rather than a regression the producer can reach — and closing it would need a
    /// fitted quantile in place of a derived anchor, which is the trade `edgeTolerance` and
    /// `squareFillThreshold` both refuse.
    static func peakAlpha(_ raster: PanelRaster, box: AppIconBodyBox) -> UInt8 {
        var peak: UInt8 = 0
        for y in box.minY...box.maxY {
            for x in box.minX...box.maxX { peak = max(peak, alpha(raster, x: x, y: y)) }
        }
        return peak
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
    /// (ADR-0031 § Decision 4, "composite blindness"). The corner read is what rejects it, and since issue
    /// #1160 a fill bound does too at every size: through `idealBodyFill` it reads 86.8696 % at canvas 16 —
    /// under the FLOOR, so rejected as a hollow body rather than as a square — and 100.0000–101.9512 % over
    /// the CEILING at the other nine. That inversion is why the ceiling canary moved to `squaredIdealBody`.
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

    /// A hard-cornered square occupying the IDEAL body, rendered with the edge coverage a rasterizer would
    /// integrate — what `brand/generate.sh` would actually emit if `icon.svg`'s `rx` were dropped.
    ///
    /// **WHY THIS EXISTS ALONGSIDE `squaredCorners`** (issue #1160). That one fills the MEASURED box, which
    /// is the ideal body only where `isExactOnPixelGrid`; off the grid the two subjects differ, and once the
    /// fill bounds run on the whole ladder the difference stops being academic and changes the verdict's
    /// SIGN. Measured through `idealBodyFill`, the measured-box square reads 101.95 % at canvas 32/64 and
    /// 100.97 % at 128 — rejected by the ceiling, correctly, if for a slightly inflated reason — but at
    /// canvas 16 it reads **86.87 %**, which is under the FLOOR. Filling a box that is smaller than the
    /// ideal body does not look square there, it looks hollow. A canary rejected by the wrong bound is not
    /// a canary for the right one, so the ceiling gets a subject that is a square at every canvas.
    ///
    /// Coverage is the overlap of each pixel with `[near, far]` per axis, multiplied — the same integration
    /// `cornerAlphas` measures the shipped rasters performing (112 / 223 / 192 / 130 at the four off-grid
    /// canvases, against 43.75 / 87.5 / 75 / 50 % predicted). So this is a render of the model rather than a
    /// synthetic stand-in, and it reads 99.9923–100.0515 % at the ten — the residual is alpha's own 1/255
    /// quantisation, not a modelling error.
    ///
    /// Its BOX parts from the producer's at canvas 128 and nowhere else — measured, box for box, at all ten.
    /// Both ideal edges land on the half-pixel there, and rounding a 0.5 coverage to alpha 128 keeps the edge
    /// pixel on BOTH axes, so this box is 104×104 where the shipped raster keeps only the column (130) and
    /// drops the row (121) — 104×103. Its own fill does not move with that, being summed over the ideal
    /// region rather than the box, but the corner pixel does, which is why `cornerAlphas` derives the
    /// producer's corner at that canvas rather than quoting this mutant's.
    ///
    /// It is deliberately NOT a fix for #1320. That issue is about what `cornerAlphas` can reach off the
    /// grid and about `squaredCorners` being unable to exhibit it; making THAT canary faithful would redden
    /// the per-corner assertion at 32/64/128, which is a change to a shipped claim. This adds a second
    /// subject for a second bound and leaves the first one exactly as it was.
    static func squaredIdealBody(_ raster: PanelRaster) -> PanelRaster? {
        guard raster.width == raster.height, let region = idealBodyRegion(canvas: raster.width) else {
            return nil
        }
        let canvas = raster.width
        let ideal = idealEdges(canvas: canvas)
        // A real body colour, so the mutant is a plausible icon rather than an obviously-broken one.
        let centre = ((canvas / 2) * canvas + canvas / 2) * 4
        let body = (Double(raster.bytes[centre]), Double(raster.bytes[centre + 1]),
                    Double(raster.bytes[centre + 2]))
        /// How much of pixel `i` the ideal body covers along one axis, in [0, 1].
        func coverage(_ i: Int) -> Double {
            max(0, min(Double(i) + 1, ideal.far) - max(Double(i), ideal.near))
        }

        var bytes = [UInt8](repeating: 0, count: canvas * canvas * 4)
        for y in region {
            for x in region {
                let covered = coverage(x) * coverage(y)
                guard covered > 0 else { continue }
                let dst = (y * canvas + x) * 4
                // Premultiplied-last, so the colour channels carry the same coverage the alpha does.
                bytes[dst] = UInt8((body.0 * covered).rounded())
                bytes[dst + 1] = UInt8((body.1 * covered).rounded())
                bytes[dst + 2] = UInt8((body.2 * covered).rounded())
                bytes[dst + 3] = UInt8((255.0 * covered).rounded())
            }
        }
        return PanelRaster(width: canvas, height: canvas, bytes: bytes)
    }

    /// The same artwork with every channel scaled by `scale` — a UNIFORM opacity change.
    ///
    /// RGB is scaled alongside alpha because `PanelRaster` is premultiplied-last: scaling alpha alone
    /// would leave `RGB > A`, a state no decoder emits, and the mutant is meant to be a raster the
    /// pipeline could plausibly produce rather than an obviously-broken one.
    ///
    /// This is the shape issue #1148 measured: one global alpha scale defeats the per-corner read, the
    /// body-fill read and the `opaqueAlpha` contour together, because all three key on absolute opacity.
    static func alphaScaled(_ raster: PanelRaster, scale: Double) -> PanelRaster? {
        guard raster.width > 0, raster.height > 0 else { return nil }
        var bytes = raster.bytes
        for i in stride(from: 0, to: bytes.count, by: 4) {
            for channel in 0..<4 {
                bytes[i + channel] = UInt8(max(0, min(255, (Double(bytes[i + channel]) * scale).rounded())))
            }
        }
        return PanelRaster(width: raster.width, height: raster.height, bytes: bytes)
    }

    /// The body shrunk to `fraction` of its own box and re-centred, with the box's four corners held by
    /// single pixels at `pinAlpha` — a real icon rendered too small, whose bounding box still measures ideal.
    ///
    /// The targeted canary for the body-fill LOWER bound, and the reason that bound is needed: the box
    /// metric reads the pins, so `measure` sees a perfectly on-grid body while the visible artwork occupies
    /// `fraction²` of it. The pins are deliberately below 255 so the per-corner read accepts them, and the
    /// shrunk body is copied from the real artwork so its interior still reaches full opacity — leaving the
    /// fill floor as the only predicate that can reject it.
    static func hollowBody(_ raster: PanelRaster, fraction: Double, pinAlpha: UInt8) -> PanelRaster? {
        guard let box = bodyBox(of: raster), fraction > 0, fraction < 1 else { return nil }
        let innerW = max(1, Int((Double(box.width) * fraction).rounded()))
        let innerH = max(1, Int((Double(box.height) * fraction).rounded()))
        // The pins must land OUTSIDE the shrunk body, or the mutation silently becomes a smaller no-op.
        guard box.width - innerW >= 2, box.height - innerH >= 2 else { return nil }
        let originX = box.minX + (box.width - innerW) / 2
        let originY = box.minY + (box.height - innerH) / 2

        var bytes = [UInt8](repeating: 0, count: raster.width * raster.height * 4)
        for y in 0..<innerH {
            let sy = box.minY + y * box.height / innerH
            for x in 0..<innerW {
                let sx = box.minX + x * box.width / innerW
                let src = (sy * raster.width + sx) * 4
                let dst = ((originY + y) * raster.width + originX + x) * 4
                for channel in 0..<4 { bytes[dst + channel] = raster.bytes[src + channel] }
            }
        }
        // A real body colour, premultiplied to `pinAlpha` so the pin is a valid pixel and not a bare alpha.
        let centre = ((raster.height / 2) * raster.width + raster.width / 2) * 4
        let scale = Double(pinAlpha) / 255.0
        for (x, y) in [(box.minX, box.minY), (box.maxX, box.minY),
                       (box.minX, box.maxY), (box.maxX, box.maxY)] {
            let dst = (y * raster.width + x) * 4
            for channel in 0..<3 {
                bytes[dst + channel] = UInt8((Double(raster.bytes[centre + channel]) * scale).rounded())
            }
            bytes[dst + 3] = pinAlpha
        }
        return PanelRaster(width: raster.width, height: raster.height, bytes: bytes)
    }
}
#endif
