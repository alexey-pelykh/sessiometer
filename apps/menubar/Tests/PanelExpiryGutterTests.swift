// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The EXPIRY row's COLUMN gate (issue #951) — the rendered proof that the expiry value shares the
// right-hand value gutter with the reset duration one line above it.
//
// WHAT WENT WRONG, AND WHY NOTHING CAUGHT IT. `ExpiryLine` shared `UsageMeter`'s 52 pt label cell but gave
// its VALUE no `.frame(width:)` at all, so the text sized to its own content and settled into the slot the
// (deliberately absent) bar had vacated. Measured on a 1:1 capture of the shipped app at `83a275d`, across
// six accounts: expiry value ink spanned x = 82…121 while the reset duration one line up ended at
// x = 365…367. Labels (all 19 sub-rows) and percent right edges were already exact — the grid was correct
// everywhere EXCEPT this one cell, which is what made it easy to miss and easy to scan past.
//
// It was missed because the panel's oracles do not cover this row. The design mock
// (`apps/menubar/design/menubar-preview.html`) authors NO expiry surface — the repo CLAUDE.md scopes it as
// the oracle "only for what it authors", so its silence is not authority — and the golden gate
// (`PanelGoldenParityTests`) is a DRIFT gate: it certifies "unchanged since blessed", which means a
// misplacement present at blessing time is exactly what a golden defends. Neither is a bug. Together they
// leave a gap that only a POSITIONAL assertion closes, and this file is that assertion.
//
// WHY THIS MEASURES PIXELS INSTEAD OF ASSERTING CONSTANTS. `PanelTextMetricsTests` already asserts that
// `expiryValueCellWidth` is the derived 40 + 9 + 52 span, and that is worth having — but it is a fact about
// arithmetic, not about layout. A cell of exactly the right width, placed in the wrong order within the
// `HStack`, or left-aligned, or followed by a trailing `Spacer`, satisfies every constant-level assertion
// and still renders the defect. The claim under test is POSITIONAL, so it is measured on a raster, the same
// way the issue itself was measured.
//
// THE PREDICATE, and why it is differential. Locating a text line by scanning for "ink" needs a known
// background, and a panel has several (card, roster row, footer). So instead of segmenting the image, each
// cell is located by CHANGING ONLY THAT CELL and diffing: render twice, vary one field, and the columns
// whose pixels move are that field's cell. The RIGHTMOST such column is the cell's right edge, because both
// variants are `.trailing`-aligned within it. Nothing else in the frame moves, so nothing else can
// contaminate the measurement — and the two variants of each pair are chosen to differ in their FINAL
// glyph, so the rightmost moving column really is at the edge rather than short of it.
//
// THE GLYPH SHADOW, measured — the one thing that makes the differential subtler than it looks, recorded
// because it is why the probes are built the way they are rather than the obvious way. Two right-aligned
// strings whose final glyphs terminate in the same shape at the same offset (`2d4h` / `23h59m` → `h` / `m`)
// are pixel-identical at the edge, so the rightmost MOVING column lands short of the true edge — ~27 pt
// short for that pair, against ~1 pt for `255%` / `7%` and ~0 for `[29d]` / `29d`. The shadow is a property
// of the glyphs and is invisible in the numbers unless you go looking for it, which is exactly how it
// would have been mistaken for a 6.5 pt layout defect had the first draft of this file been believed.
//
// MEASURED, on arm64 / macOS 26.5.2 / Xcode 26.6, printed by the gate on every run so re-deriving these
// needs no code change:
//
//   expiry value right edge ......... 335.50 pt  ← shared-string probe (shadowed, but cancels)
//   weekly reset right edge ......... 335.50 pt  ← same strings, same shadow → EXACT agreement
//   session percent right edge ...... 302.00 pt  ← different shadow; separation reads ~33 of a true 61
//   bracketed `[29d]` right edge .... 363.00 pt  ← shadow-free pair, so ≈ the true 364 pt content edge
//
// NO GATE WITHOUT A PROVEN FALSIFIER (the CONSTRAINT-A discipline this suite's neighbours follow: a gate
// authored against broken output blesses the breakage and then DEFENDS it). The agreement predicate is run
// against the PERCENT cell in the same run, through the same machinery, and REQUIRED to report
// disagreement — the percent cell sits exactly one `rowInterElementSpacing` + `meterResetCellWidth` to the
// left, so a predicate that cannot tell those two apart could not have caught the original defect either.
// That is a real 61 pt discrimination test, not an inspection-only argument.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

@MainActor
final class PanelExpiryGutterTests: XCTestCase {

    // MARK: - Tolerance

    /// How far apart two right edges may sit and still count as the SAME column, in POINTS.
    ///
    /// Not slack for layout error — layout error is what this gate exists to catch. It absorbs exactly one
    /// thing: the right side bearing differs between the glyphs each cell happens to end on (`]` / `d` for
    /// expiry, `h` / `m` for reset), so two perfectly-aligned cells whose last glyphs differ report right
    /// edges a fraction of a point apart.
    ///
    /// Calibrate the size of this against the defect it must still catch: the original misplacement put the
    /// two edges ~250 pt apart, so this tolerance is ~170x smaller than the signal. Widening it to hide a
    /// real regression would mean widening it past the point of measuring anything.
    private let edgeToleranceInPoints = 1.5

    /// Points-per-pixel for the harness's fixed 2x raster.
    private var pointsPerPixel: Double { 1.0 / Double(PanelRenderHarness.scale) }

    // MARK: - The gate

    /// The `EXPIRY` value's right edge and the `WEEKLY` reset duration's right edge are the SAME column.
    ///
    /// This is issue #951's AC-1, measured rather than eyeballed. Before the fix the two differ by ~250 pt;
    /// after it they coincide, because both cells are their `HStack`'s last child, `.trailing`-aligned,
    /// inside rows of equal width.
    func testTheExpiryValueSharesTheResetCellsRightEdge() throws {
        let baseline = try rightEdges()

        XCTAssertEqual(baseline.expiry, baseline.reset, accuracy: edgeToleranceInPoints, """
            the EXPIRY value's right edge (\(fmt(baseline.expiry)) pt) does not agree with the WEEKLY reset \
            cell's (\(fmt(baseline.reset)) pt) — they are \(fmt(abs(baseline.expiry - baseline.reset))) pt \
            apart. This is issue #951: the expiry value is sitting in a different column from the \
            structurally identical duration one line above it, so an operator scanning the right-hand value \
            gutter skips it. Check that `ExpiryLine`'s value `Text` still carries \
            `.frame(width: StatusPanelFormat.expiryValueCellWidth * scale, alignment: .trailing)` AND is \
            still the LAST child of its `HStack` — a trailing `Spacer` after it re-creates the defect with \
            the frame still in place.
            """)
    }

    /// CONSTRAINT-A: the same predicate, on the PERCENT cell, must FAIL.
    ///
    /// The percent cell's right edge is one gap + one reset cell (9 + 52 = 61 pt) left of the reset cell's.
    /// If the agreement predicate cannot separate those, it is not measuring column position at the
    /// granularity #951 lives at, and its green above would be worthless.
    func testTheAgreementPredicateSeparatesTheAdjacentPercentColumn() throws {
        let baseline = try rightEdges()
        let separation = abs(baseline.percent - baseline.reset)

        XCTAssertGreaterThan(separation, edgeToleranceInPoints, """
            the percent cell's right edge (\(fmt(baseline.percent)) pt) reads as the SAME column as the \
            reset cell's (\(fmt(baseline.reset)) pt) under this gate's own tolerance — so the gate cannot \
            distinguish adjacent columns and its pass above proves nothing.
            """)

        // …and it separates them SUBSTANTIALLY — the same order as the 61 pt the grid puts between those
        // two columns, not a sliver that happens to clear the tolerance.
        //
        // A FLOOR, and deliberately not a band around the constant. Measured, this separation reads
        // ~33 pt against a true 61 pt, and the gap is not layout error — it is the glyph shadow described
        // in the header, which each probe carries in a DIFFERENT amount (the reset pair's `h`/`m` stems
        // hide ~27 pt; the percent pair's shared `%` hides ~1 pt). The expiry↔reset comparison above is
        // exact precisely because identical strings make those shadows cancel; across two different cells
        // in two different fonts they cannot, so asserting `61 ± small` here would be asserting a
        // coincidence of glyph shapes and would redden on an unrelated font revision. What the canary
        // actually needs to establish is that the predicate resolves adjacent columns at all, and a floor
        // an order of magnitude above the tolerance establishes exactly that.
        let predicted = StatusPanelFormat.rowInterElementSpacing + StatusPanelFormat.meterResetCellWidth
        XCTAssertGreaterThan(separation, 20, """
            the percent→reset separation measured \(fmt(separation)) pt — far below the \(fmt(predicted)) \
            pt the constants put between those columns, even allowing for the glyph shadow. Either the \
            meter grid moved or this file's differential is picking up the wrong cell, and in both cases \
            the agreement claim above is measuring something other than what it says.
            """)
    }

    /// Degenerate-subject guard: the three edges must have actually been MEASURED.
    ///
    /// Every assertion above compares numbers this file derives from pixel diffs. If a render returned nil,
    /// or a diff found no moving pixels (a fixture whose two variants coincidentally draw the same string),
    /// the comparisons would be over a default rather than an observation — and `0 == 0` passes. So the
    /// probes are required to have found ink, at plausible positions, before any agreement claim counts.
    func testTheDifferentialProbesActuallyMovedPixels() throws {
        let baseline = try rightEdges()
        let panelWidth = Double(PanelMetrics.width)

        for (name, edge) in [("expiry", baseline.expiry), ("reset", baseline.reset),
                             ("percent", baseline.percent)] {
            XCTAssertGreaterThan(edge, 0, """
                the \(name) probe found NO moving pixels — its two render variants drew the same thing, so \
                every measurement in this file is comparing defaults rather than columns.
                """)
            XCTAssertLessThan(edge, panelWidth, """
                the \(name) probe's right edge (\(fmt(edge)) pt) lies outside the \(fmt(panelWidth)) pt \
                panel — the differential picked up something other than the intended cell.
                """)
        }

        // The expiry value must land in the RIGHT half of the panel. This is the crude, direction-carrying
        // form of the whole fix, stated independently of the reset cell: the defect put it at ~82…121 pt on
        // a 380 pt panel, i.e. hard left of centre.
        XCTAssertGreaterThan(baseline.expiry, panelWidth / 2, """
            the EXPIRY value's right edge (\(fmt(baseline.expiry)) pt) sits in the LEFT half of the \
            \(fmt(panelWidth)) pt panel — the #951 signature, independent of where the reset cell is.
            """)
    }

    // MARK: - Differential measurement

    private struct RightEdges {
        let expiry: Double
        let reset: Double
        let percent: Double
    }

    /// Measure all three right edges from one seeded clock, so the three probes describe the SAME frame.
    private func rightEdges() throws -> RightEdges {
        // A fixed instant, not the wall clock: every rendered duration here must be a stable string rather
        // than one that can tick between the two renders of a pair and register as a spurious diff. The
        // fixtures below place every instant on a `humanizeUntil` plateau for the same reason
        // `PanelRenderHarness` does, but a frozen `now` is what makes it airtight — nothing in this file
        // needs a live clock.
        let now: Int64 = 1_800_000_000
        let day: Int64 = 86_400

        // ONE account, because the claim is per-row and a single row is the smallest frame that can carry
        // it. The percents are the widest the WIRE can send (255 — `WireModel` decodes them as a bare
        // `UInt8` with no clamp), and the expiry is WITHIN the horizon so `expiryLineCell` brackets it:
        // this is the issue #951 / RR-6 worst case — the bracketed form standing beside a full-width
        // percent — which is exactly the row the merged cell exists to hold. Measuring on the widest row
        // means a pass here is not resting on slack a narrower row would have had.
        func row(sessionPct: UInt8, weeklyResetIn: Int64, expiry: AccountExpiry?) -> AccountRow {
            AccountRow(label: "work", isActive: true, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy,
                       sessionPct: sessionPct, weeklyPct: 255,
                       sessionResetsAt: now + 2 * 3600 + PanelRenderHarness.boundaryGuardSecs,
                       weeklyResetsAt: now + weeklyResetIn + PanelRenderHarness.boundaryGuardSecs,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil,
                       expiry: expiry)
        }

        // THE TWO DURATIONS BOTH PROBES SHARE, and why sharing them is what makes the comparison exact.
        //
        // A differential probe finds the rightmost column whose pixels MOVE, which is the cell's right edge
        // only if the two variants' final glyphs actually differ THERE. Measured, several do not: `2d4h`
        // and `23h59m`, right-aligned, terminate in `h` and `m` — whose right-hand stems are the same shape
        // at the same offset — so the last ~7 pt of that pair is pixel-identical and the probe reads ~7 pt
        // short of the true edge. That is a property of the GLYPHS, not of the layout.
        //
        // Rather than hunt for a glyph pair with no such shadow, both probes are driven by the SAME pair of
        // strings. Whatever the shadow costs, it costs both cells identically and cancels in the
        // comparison — so the difference between the two measured edges is pure LAYOUT offset, which is the
        // only thing this file is entitled to claim. (The absolute numbers are therefore lower bounds on
        // the true edges; nothing here asserts an absolute position beyond a half-panel sanity check.)
        let shortIn: Int64 = 29 * day               // → `29d`
        let longIn: Int64 = 23 * 3600 + 59 * 60     // → `23h59m`

        // UNBRACKETED on purpose: `.beyond` makes the expiry cell draw the very same string the reset cell
        // draws, so the two probes are measuring identical ink in different columns.
        func beyond(_ seconds: Int64) -> AccountExpiry {
            AccountExpiry(expiresAt: now + seconds + PanelRenderHarness.boundaryGuardSecs,
                          horizonState: .beyond)
        }

        let base = row(sessionPct: 255, weeklyResetIn: shortIn, expiry: beyond(shortIn))

        // Each variant moves EXACTLY one cell, and the expiry/reset pair moves it BY THE SAME STRINGS.
        let expiryVaried = row(sessionPct: 255, weeklyResetIn: shortIn, expiry: beyond(longIn))
        let resetVaried = row(sessionPct: 255, weeklyResetIn: longIn, expiry: beyond(shortIn))
        // `255%` vs `7%`. The canary only needs a column far enough left to be distinguishable, and it
        // carries its own glyph shadow (a shared trailing `%`), so its separation is asserted as a bounded
        // RANGE rather than at the exact constant — see the canary.
        let percentVaried = row(sessionPct: 7, weeklyResetIn: shortIn, expiry: beyond(shortIn))

        let edges = RightEdges(
            expiry:  try rightmostMovingColumn(base, expiryVaried, now: now, probe: "expiry"),
            reset:   try rightmostMovingColumn(base, resetVaried, now: now, probe: "reset"),
            percent: try rightmostMovingColumn(base, percentVaried, now: now, probe: "percent"))

        // Printed on every run: these are the measurements the assertions are calibrated on, and the
        // neighbouring gates in this bundle report their numbers the same way so a re-derivation needs no
        // code change.
        print(String(format: "[expiry-gutter] right edges — expiry %.2f pt, reset %.2f pt, percent %.2f pt",
                     edges.expiry, edges.reset, edges.percent))
        return edges
    }

    /// The BRACKETED within-horizon cell lands in the same column as the unbracketed one (issue #951).
    ///
    /// Split out because the bracket cannot be measured by the shared-string trick above: `[29d]` and
    /// `[23h59m]` both end in `]` at the same x, so that pair's rightmost MOVING column sits a glyph short
    /// of the edge. Diffing the bracketed form against the UNBRACKETED one instead (`[29d]` vs `29d`) puts
    /// two genuinely different glyphs at the edge — and the resulting edge must not sit left of where the
    /// unbracketed probe already found the cell, which is what "the two extra characters are absorbed, not
    /// clipped" means in rendered terms.
    func testTheWithinHorizonBracketRendersInTheSameColumnUnclipped() throws {
        let now: Int64 = 1_800_000_000
        let day: Int64 = 86_400

        func row(expiry: AccountExpiry) -> AccountRow {
            AccountRow(label: "work", isActive: true, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 255, weeklyPct: 255,
                       sessionResetsAt: now + 2 * 3600 + PanelRenderHarness.boundaryGuardSecs,
                       weeklyResetsAt: now + 29 * day + PanelRenderHarness.boundaryGuardSecs,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil, expiry: expiry)
        }
        let deadline = now + 29 * day + PanelRenderHarness.boundaryGuardSecs
        let bracketed = row(expiry: AccountExpiry(expiresAt: deadline, horizonState: .within))
        let plain = row(expiry: AccountExpiry(expiresAt: deadline, horizonState: .beyond))

        // Same deadline, so the DURATION is identical and the only difference is the horizon mark.
        XCTAssertEqual(StatusPanelFormat.expiryLineCell(bracketed.expiry, now: now), "[29d]")
        XCTAssertEqual(StatusPanelFormat.expiryLineCell(plain.expiry, now: now), "29d")

        let edge = try rightmostMovingColumn(bracketed, plain, now: now, probe: "bracket")
        print(String(format: "[expiry-gutter] bracketed cell right edge %.2f pt", edge))

        let baseline = try rightEdges()
        XCTAssertGreaterThanOrEqual(edge, baseline.reset - edgeToleranceInPoints, """
            the WITHIN-horizon bracketed cell's right edge (\(fmt(edge)) pt) sits left of the reset cell's \
            (\(fmt(baseline.reset)) pt). The bracket is being clipped or the cell is re-aligning when the \
            two extra characters arrive — which is exactly the failure mode a 52 pt reset-width cell would \
            have had, and the reason `expiryValueCellWidth` merges the percent cell in.
            """)
    }

    /// Render both single-row fixtures and return the RIGHTMOST column (in points) whose pixels differ.
    private func rightmostMovingColumn(_ a: AccountRow, _ b: AccountRow,
                                       now: Int64, probe: String) throws -> Double {
        let left = try raster(a, now: now, probe: probe)
        let right = try raster(b, now: now, probe: probe)

        XCTAssertEqual(left.width, right.width, "\(probe): the two variants rasterized to different widths")
        XCTAssertEqual(left.height, right.height,
                       "\(probe): the two variants rasterized to different heights — varying this field "
                       + "reflowed the row, so the diff is no longer confined to one cell")
        guard left.width == right.width, left.height == right.height else { return 0 }

        // Same channel threshold the neighbouring gates use (64/255): antialiasing-tolerant, but sensitive
        // to a glyph actually being somewhere else.
        let threshold = 64
        var rightmost = -1
        for y in 0..<left.height {
            let rowStart = y * left.width * 4
            for x in stride(from: left.width - 1, through: rightmost + 1, by: -1) {
                let i = rowStart + x * 4
                let dr = abs(Int(left.bytes[i]) - Int(right.bytes[i]))
                let dg = abs(Int(left.bytes[i + 1]) - Int(right.bytes[i + 1]))
                let db = abs(Int(left.bytes[i + 2]) - Int(right.bytes[i + 2]))
                let da = abs(Int(left.bytes[i + 3]) - Int(right.bytes[i + 3]))
                if max(max(dr, dg), max(db, da)) > threshold {
                    rightmost = x
                    break
                }
            }
        }
        guard rightmost >= 0 else { return 0 }
        // Pixel INDEX → the right edge in points: the column's far side, converted at the harness's scale.
        return Double(rightmost + 1) * pointsPerPixel
    }

    /// Rasterize a one-row panel through the SAME harness path the app's `--render-panel` tool and the
    /// golden gate use, so this measures the shipped wiring rather than a bespoke view host.
    private func raster(_ row: AccountRow, now: Int64, probe: String) throws -> PanelRaster {
        let fixture = PanelRenderFixture(name: "expiry-gutter-\(probe)", state: .connected,
                                         rows: [row], nextSwap: nil, generatedAt: now - 12)
        let cg = try XCTUnwrap(PanelRenderHarness.render(fixture, scheme: .light),
                               "\(probe): the panel did not rasterize")
        return try XCTUnwrap(PanelRaster.normalize(cg), "\(probe): the raster did not normalize")
    }

    private func fmt(_ value: Double) -> String { String(format: "%.2f", value) }
}
#endif
