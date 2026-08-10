// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Panel geometry ACROSS ROSTER CARDINALITY (issue #755) — how tall the popover gets as the fleet grows,
// and what that costs.
//
// WHAT WAS UNMODELLED. Every `PanelRenderHarness` fixture is a three-account roster (or an empty one), so
// the panel's behaviour at 1 account and at 10-20 was measured nowhere — and the product is explicitly
// multi-account (the whole thing is account rotation). This suite measures it and pins what it found.
//
// THE MEASUREMENT, and why it is not a golden. `PanelRenderHarness.render` returns a `CGImage` sized to
// the panel's own intrinsic layout (`StatusPanelView` pins the width and takes
// `.fixedSize(horizontal: false, vertical: true)`), so `cgImage.height / PanelRenderHarness.scale` IS the
// panel's height in points — a SCALAR read off the same render path the app and the golden gate use, not a
// pixel comparison against a committed baseline. Nothing here needs re-baselining, and nothing here can be
// fixed by re-blessing a file.
//
// The fixtures are built LOCALLY rather than added to `PanelRenderHarness.fixtures`, deliberately. That
// catalog is the DESIGN-ORACLE set: `PanelGoldenParityTests` renders every entry in both themes and diffs
// it against a committed golden, so adding a 20-account probe there would demand two committed PNGs for a
// state the build reference (`design/menubar-preview.html`, which shows only the 3-account case) does not
// define. These are measurement probes, so they go through the harness's RENDER — the shared wiring, so the
// panel is assembled exactly as the app assembles it — without joining its catalog.
//
// ISSUE #824 DID NOT REACH THIS SUITE, and that was asserted rather than assumed. Before #824 the harness
// warmed `healthy`/light/`.large` only, so an un-warmed fixture's first raster could differ from its steady
// state by ±1/255 on a few hundred bytes — the artifact #824 tracked, and the reason
// `PanelAppearanceVariantTests` (#760) needs a `stableRender` before any byte-granular comparison. Every
// predicate HERE reads a DIMENSION, which a colour drift cannot move: a raster's width and height come
// from the layout pass, not from the pixel values a settle converges.
//
// That argument is a-priori and `testTheMeasuredHeightIsStableAcrossRepeatedRenders` CORROBORATES rather
// than proves it: the harness settles every render it serves, so its six agreeing renders are all steady
// ones, and what they demonstrate is REPEATABILITY. Stated precisely because the looser phrasing was worth
// correcting. Do not "fix" a future flake here by loosening a height comparison — a height that moves
// between renders is a bigger finding than a cold raster ever was, and belongs in its own issue.
//
// ── THE DERIVED CEILING IS AN ASSUMPTION, NOT A RATIFIED TARGET ────────────────────────────────────────
//
// Issue #755's AC-1 asks for the height to be "asserted against a DOCUMENTED maximum". No such maximum is
// documented: `design/menubar-preview.html` defines the 3-account case at the default text size and says
// nothing about panel height, scroll affordance, or roster bounds, and the design SSOT
// (`../hq/strategy/design-menubar.md`) carries nothing on any of them either. So this suite DERIVES one and
// labels it as derived — `PanelGeometry.derivedCeiling` below states each input. Whether the product should
// cap, scroll, or condense is a genuine PRODUCT decision with no ratified answer, and this suite does not
// settle it. Nothing here asserts a remedy; everything here measures.
//
// WHERE IT IS ROUTED, stated exactly because the two candidate destinations are not the same place. Issue
// #755's Build Reference clause names `../hq/strategy/design-menubar.md` — the design SSOT, and the right
// home for the eventual RATIFIED answer, which only the product owner can write. What this change can do
// is surface the decision where the evidence for it lives: it is recorded as ratification-pending in a
// comment on issue #818, alongside the measurements below. Issue #818 is the defect this suite evidences
// and already carries the scale-axis half of the same question, so a second issue would be a duplicate and
// a silent decision-in-code would be worse than either.
//
// ── WHAT WAS MEASURED (macOS 26.5.2 / Xcode 26.6, at `d019d1b`) ────────────────────────────────────────
//
// Heights in points, `.connected` roster, no swap callout (see `roster` for why that shape):
//
//   accounts │ `.large` │ `.accessibility3`
//   ─────────┼──────────┼──────────────────
//          1 │   197.00 │            451.00
//          2 │   293.00 │            672.00
//          3 │   389.00 │            893.50
//          7 │   773.00 │           1778.50
//         10 │  1061.00 │           2442.50
//         20 │  2021.00 │           4655.50
//         50 │  4901.00 │          11294.50
//
// Two numbers carry the finding:
//
//   • The marginal cost of one account is EXACTLY 96.00 pt at the default text size (197 → 293 → 389 →
//     485 → 581 → 677 → 773 → 869: eight measurements, seven identical deltas) and ≈221.30 pt at
//     `.accessibility3`. Perfectly linear, with no plateau anywhere — between 20 and 50 accounts the panel
//     is still growing at exactly the same rate it grew between 1 and 2.
//   • Against the 856 pt derived ceiling, the largest roster that fits is SEVEN accounts at the default
//     text size and TWO at `.accessibility3`. At 50 accounts the panel is 4901 pt tall — more than four
//     times the tallest display any Mac laptop presents (the 16-inch MacBook Pro's default scaled mode is
//     1728 × 1117 points) — with no way to reach the 4045 pt below the ceiling.
//
// ── THE ROW COST IS NOW CONDITIONAL (issue #884) ───────────────────────────────────────────────────────
//
// The 96.00 pt marginal cost above is measured on a roster with NO refresh-token expiry line. Issue #884
// added a per-row `EXPIRY` line that materializes for the WHOLE roster once ANY account carries an
// observed deadline (`StatusPanelFormat.rosterShowsExpiry`, mirroring the CLI's column rule), so a real
// fleet on a #882-or-later daemon pays MORE per row than this table records.
//
// Measured through this same render path (macOS 26.5.2 / Xcode 26.6), the harness `healthy` fixture with
// the line on all three rows is 518.00 pt against its 449.00 pt baseline — 69.00 pt for three rows, so:
//
//   • marginal cost per account: 96.00 pt → 119.00 pt (+23.00 pt, the line plus its 9 pt VStack spacing)
//   • largest roster fitting the 856 pt derived ceiling: SEVEN accounts → SIX
//
// Every roster behind the table above carries `expiry: nil` (the `AccountRow` default — only the probe
// named below opts in), so every number in it remains exactly what it claims; the measurement is simply
// narrower than "the panel's row cost" now reads. It tightens issue #818's unbounded-growth finding by one
// account rather than adding a new defect: the panel still has no scroll view at any level, which is what
// makes either number reachable.
//
// So the answer to AC-2's either/or is the second branch: the panel is NOT bounded, it grows without
// limit, and the overflow is unreachable because there is no scroll view at any level. That defect is
// already filed as issue #818 (opened by issue #756 with the SCALE axis: 438 pt → 1031 pt for the
// 3-account panel). This suite adds the CARDINALITY axis it lacked, and a second issue would be a
// duplicate — so the numbers go to #818 rather than to a new report.
//
// One correction owed to that filing, recorded rather than quietly adopted: re-measured here at `d019d1b`
// the harness's own `healthy` fixture is 449 pt at the default text size, not the 438 pt #818's table
// records. Its `.accessibility3` figure (1031 pt) reproduces exactly. The 11 pt is not explained here — it
// is reported so the table can be re-derived rather than trusted.
//
// ── WHY THE MEASURED HEIGHTS ARE A LOWER BOUND ─────────────────────────────────────────────────────────
//
// The rosters below carry NO swap callout, so the cardinality axis is isolated: the callout is
// fixed-height chrome that appears once regardless of roster size, and including it would add a constant
// to every row of the table without changing a single delta. It is not free, though — measured at 60.00 pt
// (`testTheSwapCalloutIsFixedChromeSoTheseHeightsAreALowerBound`) — and the real panel usually shows it. So
// every height here is a LOWER bound on what an operator sees, and every overflow verdict is
// correspondingly conservative: the real panel crosses the ceiling at or before the cardinality named.
//
// ── CONSTRAINT-A ───────────────────────────────────────────────────────────────────────────────────────
//
// `testAnOversizedRosterTripsTheBound` drives the CANARY through `PanelGeometry.exceeds` — the exact
// function every assertion in this file gates on — and requires it to fire on a 20-account roster AND to
// stay silent on a 1-account one. Both halves are load-bearing and neither is true by construction: a
// predicate hardwired to `true` passes the first and fails the second, one hardwired to `false` does the
// reverse, and a `measuredHeight` that returned a constant (a dead lever — the failure mode where a pin
// reads identical whether the feature works or not) cannot satisfy both at once because the two rosters
// would report the same height. Verified by mutation, not by inspection: issue #437's three render bugs
// were read five times as a design failure, and a golden authored then would have defended them.
//
// THE MUTATION MATRIX, run against this file before it was committed. Each row is a defect deliberately
// introduced into the suite's own machinery; the right-hand column is what actually reddened. A gate not
// named by at least one row would be a gate nobody proved could fail.
//
//   mutation                                        │ reddens
//   ────────────────────────────────────────────────┼──────────────────────────────────────────────────
//   `exceeds` hardwired to FALSE                    │ the canary's trip half, the capacity pins, the
//                                                   │ height table's five overflow rows, the unbounded pin
//   `exceeds` hardwired to TRUE                     │ the canary's silent half, the capacity pins, the
//                                                   │ height table's three fitting rows
//   `height` ignores its roster (renders 3 always)  │ the canary's trip AND lever halves, the growth law,
//     — the DEAD LEVER, weak form                   │ capacity, the height table, the AC-3 row cost
//   `height` returns a CONSTANT for every roster    │ the growth law's and the AC-3 row cost's magnitude
//     — the DEAD LEVER, strong form                 │ guards, plus the unbounded pin
//   `width` ignores its size class                  │ the AC-3 column-budget bridge (measured 380.00
//     — the dead lever again, one axis over         │ against the 894.12 pt the class pins)
//   `nextSwap` dropped before the render            │ the lower-bound measurement's own guard
//   the canary's marginal cost halved               │ the canary's attribution half (1824.00 measured
//     — attribution, not magnitude                  │ against the 912.00 the halved cost predicts)
//
// Two things the matrix is deliberately showing. The dead-lever rows are the ones that matter most — a
// stubbed measurement is the failure mode where a pin reads identical whether the feature works or not,
// and it is invisible to inspection. And the two `exceeds` rows redden DISJOINT halves of the canary,
// which is what proves the pair is not satisfiable by any constant.
//
// THE MATRIX IS RE-RUN, NOT READ, and the dead lever has two rows because re-running it is what showed the
// WEAK form is not evidence. Clipping the roster to three rows leaves `height(1)` and `height(2)` genuinely
// different, so the row-cost gates reddened through an EQUALITY — which a measurement returning a CONSTANT
// satisfies as `0 == 0`, leaving them green while the table looked like it had covered them. Hence every
// delta comparison below asserts MAGNITUDE before comparing, and hence the canary's third assertion is an
// ATTRIBUTION check rather than the restatement of its first two it started as. "The canary passed" and
// "the canary can fail" are different claims, and a mutation too weak to distinguish them is the more
// dangerous of the two failures.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

// MARK: - The measurement primitives and THE predicate

/// Panel-height measurement for issue #755's gate, and the one predicate every assertion below — real and
/// canary alike — is driven through.
///
/// Scoped to this file on purpose. `TextMetrics` lives in its own shared home because a SECOND suite needed
/// it (issue #762) and two copies of a predicate can disagree at a boundary; there is one consumer here, so
/// hoisting now would be abstraction ahead of need. If a second suite ever measures panel height, move this
/// enum out exactly as issues #762/#765 moved `TextMetrics` — and move it whole, so the predicate does not
/// fork.
@MainActor
enum PanelGeometry {

    // The ceiling arithmetic and the predicate are `nonisolated`: they are pure value work, and the
    // predicate is used as a default argument (which Swift evaluates outside the actor). Only the RENDER
    // needs the main actor — the same split `PanelRenderHarness` makes for its naming/scale surface.

    // MARK: The derived ceiling (AN ASSUMPTION — see the file header)

    /// The smallest logical display height a Mac meeting this app's deployment target (macOS 13.0, see
    /// `project.yml`) plausibly presents: the 13-inch Retina MacBook Air / Pro default scaled mode is
    /// 1440 × 900 points. Logical points, not pixels — the panel is laid out in points.
    nonisolated static let smallestPlausibleDisplayHeight: Double = 900

    /// The macOS menu bar the popover hangs below. Measured on this machine `NSStatusBar.system.thickness`
    /// reports 22.0; 24 is used instead because a ceiling should err toward being too GENEROUS to the panel
    /// (a tighter allowance would manufacture overflow verdicts).
    nonisolated static let menuBarHeight: Double = 24

    /// `NSPopover`'s own chrome — the arrow (~11 pt) plus the margin macOS keeps from the screen edge.
    /// Deliberately excludes the Dock, which on a default configuration takes considerably more (measured
    /// on this machine: a 1080 pt display reports a 960 pt `visibleFrame`, i.e. 120 pt of chrome against
    /// the 44 pt allowed here). Same direction as the menu-bar figure: generous to the panel.
    nonisolated static let popoverChromeAllowance: Double = 20

    /// The height a popover may reach before its content is off-screen and unreachable — **856 pt**.
    ///
    /// DERIVED, NOT RATIFIED. Every input above is an assumption about the operator's hardware, not a
    /// product decision, and no design source states a maximum (file header). It is used here as a
    /// measuring stick that makes "does this fit?" answerable, NOT as a target the panel is required to
    /// meet — the cap-vs-scroll-vs-condense question is routed to issue #818 undecided.
    nonisolated static var derivedCeiling: Double {
        smallestPlausibleDisplayHeight - menuBarHeight - popoverChromeAllowance
    }

    // MARK: THE predicate

    /// `true` when a panel of `height` points does not fit `ceiling`.
    ///
    /// Every real assertion and the CONSTRAINT-A canary in this file run through this one function, so the
    /// thing proven falsifiable is the thing actually gating.
    nonisolated static func exceeds(_ height: Double, ceiling: Double = derivedCeiling) -> Bool {
        height > ceiling
    }

    // MARK: Rendering

    /// The panel's rendered size in POINTS for `rows` at `size` — the one render both readers below share.
    ///
    /// Goes through `PanelRenderHarness.render` — the shared path the app's `--render-panel` tool and
    /// `PanelGoldenParityTests` both use — so the panel is wired exactly as it ships. Returns `nil` only if
    /// the rasterizer itself failed, which callers surface loudly rather than treating as a zero. The
    /// fixture `name` is inert here (`PanelRenderHarness` renders from the state, never from the name) and
    /// is set only so a paused render is identifiable in a debugger.
    private static func pointSize(rows: [AccountRow], size: DynamicTypeSize, nextSwap: NextSwap?,
                                  now: Int64) -> (width: Double, height: Double)? {
        let fixture = PanelRenderFixture(name: "roster-\(rows.count)",
                                         state: rows.isEmpty ? .emptyRoster : .connected,
                                         rows: rows, nextSwap: nextSwap, generatedAt: now - 12)
        guard let cg = PanelRenderHarness.render(fixture, scheme: .light, dynamicTypeSize: size) else {
            return nil
        }
        let scale = Double(PanelRenderHarness.scale)
        return (width: Double(cg.width) / scale, height: Double(cg.height) / scale)
    }

    /// The panel's rendered HEIGHT in points — what every measurement in this suite reads.
    static func height(rows: [AccountRow], size: DynamicTypeSize,
                       nextSwap: NextSwap? = nil, now: Int64) -> Double? {
        pointSize(rows: rows, size: size, nextSwap: nextSwap, now: now)?.height
    }

    /// The panel's rendered WIDTH in points — read off the same render, for the AC-3 column-budget bridge.
    static func width(rows: [AccountRow], size: DynamicTypeSize, now: Int64) -> Double? {
        pointSize(rows: rows, size: size, nextSwap: nil, now: now)?.width
    }
}

@MainActor
final class PanelRosterGeometryTests: XCTestCase {

    // MARK: - Fixture plumbing

    /// Distinct account labels — `roster` below extends them by suffix past the end of this list, so
    /// distinctness holds at EVERY cardinality this suite renders (the growth law probes 50).
    ///
    /// Distinctness matters because `StatusPanelFormat.accountMonograms` resolves monograms over the WHOLE
    /// roster, assigning each label its most-distinguishing FREE candidate in roster order — so a repeated
    /// label would make a row's badge a function of who else is on the roster, precisely the confound this
    /// suite is measuring around. The monogram is always two characters, so a collision would change which
    /// letters render and never the geometry; the isolation is kept anyway, because it costs nothing and
    /// this suite's whole claim is that cardinality is the only thing varying.
    private static let labels = ["Work", "Personal", "Temp", "Scratch", "Spare", "Backup", "Alpha",
                                 "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
                                 "India", "Juliet", "Kilo", "Lima", "Mike", "November", "Oscar",
                                 "Papa", "Quebec", "Romeo", "Sierra"]

    /// The four cardinalities issue #755's AC-1 names. The growth law and the capacity probe below carry
    /// their own denser ranges — this one is the acceptance table's axis and nothing else.
    private static let acceptanceCardinalities = [1, 3, 10, 20]

    /// Both ends of the `PanelTypeScale` range the panel supports — the 2-D worst corner issue #755 needs is
    /// a large roster at the ceiling class, not a large roster at the default.
    private static let sizeClasses: [(DynamicTypeSize, String)] = [(.large, "large"),
                                                                   (.accessibility3, "accessibility3")]

    private static func wallClock() -> Int64 { Int64(Date().timeIntervalSince1970) }

    /// A healthy `n`-account roster whose only varying property is CARDINALITY.
    ///
    /// Every row carries identical percents, identical reset instants, and healthy auth, so a height
    /// difference between two rosters is attributable to the row COUNT and to nothing else. The first row is
    /// the active account (as a real roster always has exactly one). Reset offsets sit a
    /// `boundaryGuardSecs`-style 30 s clear of a `humanizeUntil` rounding boundary, so a sub-second delay
    /// between seeding and rasterizing cannot reflow a cell.
    private func roster(_ n: Int, now: Int64, expiry: AccountExpiry? = nil) -> [AccountRow] {
        let sessionReset = now + 2 * 3600 + 14 * 60 + PanelRenderHarness.boundaryGuardSecs
        let weeklyReset = now + 3 * 86_400 + PanelRenderHarness.boundaryGuardSecs
        return (0..<n).map { i -> AccountRow in
            // Past the end of the list a wrap gets a disambiguating suffix rather than an exact duplicate,
            // so the distinctness `labels` documents survives the growth law's 50-account probe. Every
            // cardinality inside the list is labelled exactly as it was before.
            let base = Self.labels[i % Self.labels.count]
            let label = i < Self.labels.count ? base : "\(base)-\(i / Self.labels.count)"
            return AccountRow(label: label, isActive: i == 0, isEnabled: true,
                              isQuarantined: false, isRecovering: false, auth: .healthy,
                              sessionPct: 42, weeklyPct: 88,
                              sessionResetsAt: sessionReset, weeklyResetsAt: weeklyReset,
                              weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil,
                              expiry: expiry)
        }
    }

    private func measuredHeight(_ n: Int, _ size: DynamicTypeSize, now: Int64,
                                nextSwap: NextSwap? = nil) throws -> Double {
        try XCTUnwrap(PanelGeometry.height(rows: roster(n, now: now), size: size,
                                           nextSwap: nextSwap, now: now),
                      "the \(n)-account panel did not rasterize at \(size), so nothing was measured")
    }

    /// The same roster with an OBSERVED refresh-token deadline on every account (issue #884), so the
    /// `EXPIRY` line materializes (`StatusPanelFormat.rosterShowsExpiry`) and its cost is measurable. The
    /// deadline carries the same `boundaryGuardSecs` clearance the reset instants do — the line renders a
    /// `humanizeUntil` duration, so it is subject to the identical rounding-reflow hazard.
    ///
    /// Goes through `roster` rather than rebuilding its rows, so the pair this suite subtracts differs in
    /// the expiry modifier and NOTHING else — a field added to `AccountRow` later cannot land on one side
    /// of the subtraction only.
    private func rosterWithExpiry(_ n: Int, now: Int64) -> [AccountRow] {
        let deadline = now + 6 * 86_400 + 21 * 3600 + PanelRenderHarness.boundaryGuardSecs
        return roster(n, now: now,
                      expiry: AccountExpiry(expiresAt: deadline, horizonState: .within))
    }

    // MARK: - The #884 EXPIRY line's row cost (the conditional-cost note above, made executable)

    /// The `EXPIRY` line (issue #884) costs a MEASURED, CONSTANT amount per row, and that constant is what
    /// moves this file's headline numbers when a real fleet's daemon reports deadlines.
    ///
    /// This exists because the line is otherwise UNRENDERED by every test in the suite: each roster here —
    /// and every `PanelRenderHarness` fixture — carries the `expiry: nil` default, so `rosterShowsExpiry`
    /// is false and `ExpiryLine` never rasterizes. Without this probe the comment block's 119 pt / SIX
    /// figures would be a prose claim with no executable backing, and the view itself would have zero
    /// render coverage. Measured through the SAME `PanelGeometry.height` path as the table above.
    func testTheExpiryLineCostsAConstantPerRowAndTightensTheCeilingByOneAccount() throws {
        let now = Self.wallClock()
        var deltas: [Double] = []

        for n in 1...4 {
            let without = try XCTUnwrap(
                PanelGeometry.height(rows: roster(n, now: now), size: .large, nextSwap: nil, now: now),
                "the \(n)-account panel did not rasterize")
            let with = try XCTUnwrap(
                PanelGeometry.height(rows: rosterWithExpiry(n, now: now), size: .large, nextSwap: nil, now: now),
                "the \(n)-account panel with expiry did not rasterize")

            // The LEVER half: the line must actually cost something, or a `showsExpiry` wired to `false`
            // (or an `ExpiryLine` that renders empty) would pass every assertion below by reporting a
            // zero delta at every cardinality.
            XCTAssertGreaterThan(with, without,
                                 "the expiry line must add height at \(n) accounts — a zero delta means it never rendered")
            deltas.append((with - without) / Double(n))
        }

        // CONSTANT per row: the same cost at 1, 2, 3 and 4 accounts. A per-row line must scale with the
        // roster; a one-off chrome cost would show a shrinking per-account delta.
        for (i, delta) in deltas.enumerated() {
            XCTAssertEqual(delta, Self.expiryRowCost, accuracy: 0.01,
                           "the per-row expiry cost drifted at \(i + 1) accounts: \(delta) pt")
        }

        // And the consequence the comment block records: the marginal account cost rises by exactly this
        // constant, and the largest roster fitting the derived ceiling drops from SEVEN to SIX.
        let baseFits = (1...20).filter { !PanelGeometry.exceeds(101 + 96.0 * Double($0)) }.max()
        let expiryFits = (1...20).filter {
            !PanelGeometry.exceeds(101 + (96.0 + Self.expiryRowCost) * Double($0))
        }.max()
        XCTAssertEqual(baseFits, 7)
        XCTAssertEqual(expiryFits, 6, "the expiry line must cost exactly one account of headroom")
    }

    /// The measured per-row cost of the `EXPIRY` line: an 11 pt text line plus the row `VStack`'s 9 pt
    /// spacing. Re-derive with the test above rather than trusting this number.
    private static let expiryRowCost = 23.00

    // MARK: - Control: the measurement is a DIMENSION, so issue #824 cannot reach it

    /// Six renders of an UN-WARMED fixture must report the same height to the pixel.
    ///
    /// WHY THIS COMES FIRST. It is what turns the file header's a-priori argument — a DIMENSION cannot
    /// carry the ±1/255 cold-raster artifact issue #824 tracked, because width and height come from the
    /// layout pass rather than from the pixel values a settle converges — into a measurement (observed:
    /// 1546 px, six times out of six, on a fixture outside the harness's catalog).
    ///
    /// It doubles as the degenerate-subject guard for everything below: a rasterizer returning nil, or a
    /// zero-height image, would make every height comparison in this file a comparison of nothing.
    ///
    /// WHEN THIS REDDENS: heights have become non-deterministic across renders. Diagnose it as a rasterizer
    /// finding — do NOT absorb it by widening a height comparison here, which would silently retire the
    /// bound this suite exists to measure.
    func testTheMeasuredHeightIsStableAcrossRepeatedRenders() throws {
        let now = Self.wallClock()
        let rows = roster(7, now: now)
        var heights: [Double] = []
        for _ in 0..<6 {
            heights.append(try XCTUnwrap(PanelGeometry.height(rows: rows, size: .large, now: now),
                                         "the 7-account panel did not rasterize"))
        }
        let first = try XCTUnwrap(heights.first)
        XCTAssertGreaterThan(first, 0,
                             "the panel rasterized to ZERO height — every height comparison in this suite "
                             + "would be comparing nothing")
        XCTAssertEqual(Set(heights).count, 1,
                       "six renders of the same un-warmed fixture reported different heights (\(heights)). "
                       + "This suite's whole premise is that a DIMENSION is immune to the ±1/255 cold-raster "
                       + "artifact issue #824 tracks — a moving height is a larger finding than #824 and "
                       + "must be diagnosed there, NOT absorbed by relaxing a comparison here")
    }

    // MARK: - AC-1: the height at each named cardinality, against the derived ceiling

    /// The AC-1 table: {1, 3, 10, 20} accounts × both ends of the supported type-scale range, each measured
    /// and each judged by `PanelGeometry.exceeds` against the derived 856 pt ceiling.
    ///
    /// The expectations below are PINS ON MEASURED REALITY, not targets. The panel is unbounded today
    /// (`testTheRosterGrowsLinearlyAndWithoutBound`), so most of this table is expected to OVERFLOW, and
    /// asserting otherwise would be fabricating a pass. Pinning the verdicts is what makes the eventual fix
    /// visible: when issue #818 lands a scroll boundary, the overflow rows redden and this table has to be
    /// re-derived against whatever bound the fix establishes — which is the point.
    ///
    /// The 2-D corner matters and is why the size class is an axis rather than a footnote: panel height is a
    /// function of roster cardinality AND type scale since issue #756 (`d179da9`), so 20 accounts at
    /// `.accessibility3` is the real worst case and it is far worse than 20 at the default. (Nothing drives
    /// the size class in the shipped app yet — issue #817 — so operators see the `.large` column today; the
    /// mechanism is in the code and the `.accessibility3` column is what lands with it.)
    func testThePanelHeightAtEachRosterCardinalityIsPinnedAgainstTheDerivedCeiling() throws {
        let now = Self.wallClock()
        // MEASURED at `d019d1b`; see the file header's table. `true` = exceeds the derived ceiling.
        let expected: [String: Bool] = [
            "1@large": false, "3@large": false, "10@large": true, "20@large": true,
            "1@accessibility3": false, "3@accessibility3": true,
            "10@accessibility3": true, "20@accessibility3": true,
        ]
        var report: [String] = []
        for (size, sizeName) in Self.sizeClasses {
            for n in Self.acceptanceCardinalities {
                let key = "\(n)@\(sizeName)"
                let height = try measuredHeight(n, size, now: now)
                let over = PanelGeometry.exceeds(height)
                let margin = abs(height - PanelGeometry.derivedCeiling)
                report.append("\(key) = " + String(format: "%.2f", height) + " pt ("
                              + (over ? "over" : "under") + " by " + String(format: "%.2f", margin) + ")")
                XCTAssertEqual(over, try XCTUnwrap(expected[key], "no expectation recorded for \(key)"),
                               "the \(n)-account panel at \(sizeName) measured "
                               + String(format: "%.2f pt", height)
                               + " against the \(PanelGeometry.derivedCeiling) pt derived ceiling, which is "
                               + "NOT the recorded verdict. If the panel gained a bound (issue #818), this "
                               + "is the expected reddening: re-derive this whole table against the new "
                               + "bound rather than flipping one entry. If it did not, the panel's height "
                               + "moved for another reason and the growth law below should be re-read. "
                               + "Measured so far: \(report.joined(separator: "; "))")
            }
        }
        // Degenerate-subject guard, exact rather than non-zero (the idiom `PanelTextMetricsTests`'
        // AC-3 sweep uses): a table that silently measured one corner would otherwise pass green.
        // Literals, not `sizeClasses.count * acceptanceCardinalities.count` — a count derived from the
        // same arrays that drive the loop would agree with an emptied array.
        XCTAssertEqual(report.count, 2 * 4,
                       "expected \(2 * 4) (size class × cardinality) measurements, took \(report.count)")
    }

    // MARK: - AC-2: the panel is NOT bounded — it grows linearly, forever

    /// The defect evidence for issue #818, stated as the two properties that make it a defect rather than a
    /// large number: growth is LINEAR in roster size, and it never PLATEAUS.
    ///
    /// A panel that grew and then stopped would be capped (AC-2's first branch, and no defect). A panel
    /// whose marginal cost merely varied would still be bounded if it converged. Measured, neither holds:
    /// the marginal cost of one account is exactly 96.00 pt at the default text size across every step from
    /// 1 to 8 accounts, and at 50 accounts the panel is still growing at that same rate, having reached
    /// 4901 pt — roughly five and a half times the derived ceiling, and more than four times the tallest
    /// display a Mac laptop presents.
    ///
    /// Asserted as a RELATION (equal deltas, strict growth) rather than against pinned point heights,
    /// deliberately: the absolute numbers carry font metrics that legitimately shift with a macOS revision,
    /// while "every account costs the same and it never stops" is the claim that is actually about the
    /// panel. The tolerance is 1 pt — twice the 0.5 pt raster quantum at `PanelRenderHarness.scale` = 2, so
    /// it admits whole-pixel rounding in either direction and nothing else.
    ///
    /// WHEN THIS REDDENS: the panel acquired a bound, a cap, or a scroll boundary. That is issue #818 being
    /// fixed — re-derive this suite against it; do not relax the tolerance.
    func testTheRosterGrowsLinearlyAndWithoutBound() throws {
        let now = Self.wallClock()
        let steps = [1, 2, 3, 4, 5, 6, 7, 8]
        var heights: [Double] = []
        for n in steps { heights.append(try measuredHeight(n, .large, now: now)) }

        let deltas = zip(heights.dropFirst(), heights).map { $0 - $1 }
        let marginal = try XCTUnwrap(deltas.first, "not enough measurements to derive a marginal cost")
        // The equal-deltas loop below compares deltas to EACH OTHER, and a measurement that ignored its
        // roster would satisfy it as `0 == 0` — the dead-lever hole an adversarial re-run of this file's
        // mutation matrix found. So the magnitude is asserted first: an account must COST something.
        XCTAssertGreaterThan(marginal, 0,
                             "adding a second account changed the panel height by "
                             + String(format: "%.2f pt", marginal) + ". Either the roster is not reaching "
                             + "the measurement — in which case the equal-deltas check below is comparing "
                             + "zeroes and proves nothing — or a row genuinely costs no height, which is a "
                             + "layout bug")
        for (index, delta) in deltas.enumerated() {
            XCTAssertEqual(delta, marginal, accuracy: 1.0,
                           "the marginal cost of account \(steps[index + 1]) was "
                           + String(format: "%.2f pt", delta) + " against "
                           + String(format: "%.2f pt", marginal) + " for the first — growth is no longer "
                           + "linear in roster size. If the panel gained a cap or a scroll boundary (issue "
                           + "#818) that is the good outcome and this whole suite needs re-deriving; if not, "
                           + "a row's height became a function of its POSITION, which is a layout bug. "
                           + "Measured: \(heights.map { String(format: "%.2f", $0) })")
        }

        // No plateau: far past every plausible fleet size, the panel is still growing at the same rate.
        let twenty = try measuredHeight(20, .large, now: now)
        let fifty = try measuredHeight(50, .large, now: now)
        XCTAssertEqual((fifty - twenty) / 30.0, marginal, accuracy: 1.0,
                       "between 20 and 50 accounts the panel grew by "
                       + String(format: "%.2f pt", (fifty - twenty) / 30.0)
                       + " per account against " + String(format: "%.2f pt", marginal)
                       + " at the low end — growth has changed rate, so re-derive whether the panel is now "
                       + "bounded (issue #818) or merely non-linear")
        XCTAssertTrue(PanelGeometry.exceeds(fifty),
                      "a 50-account panel measured " + String(format: "%.2f pt", fifty)
                      + ", which fits the \(PanelGeometry.derivedCeiling) pt derived ceiling. The panel has "
                      + "acquired a bound — issue #818's premise no longer holds and every measurement in "
                      + "this file must be re-derived against the new one")
    }

    /// How many accounts actually fit — the figure issue #818 needs and the one an operator experiences.
    ///
    /// MEASURED: **7** at the default text size, **2** at `.accessibility3`. Both are lower-bound-generous
    /// (the swap callout would take another 60 pt, see below; the Dock is not charged at all), and the
    /// second is the one that stings — a two-account fleet is not a fleet, and account rotation is the
    /// product.
    ///
    /// Pinned as RANGES rather than at the exact figures, because the exact figure carries font metrics a
    /// macOS revision can move by a row while the finding — "a fleet-sized roster does not fit, and at the
    /// accessibility ceiling almost nothing does" — stays true. The ranges are still sharply falsifiable: a
    /// scroll boundary (issue #818) makes every cardinality fit and reddens both.
    func testTheLargestRosterThatFitsIsSmallerThanAPlausibleFleet() throws {
        let now = Self.wallClock()
        var capacities: [String: Int] = [:]
        for (size, sizeName) in Self.sizeClasses {
            var capacity = 0
            // Probed well past the band pinned below, so SATURATION cannot masquerade as a passing
            // capacity: if every cardinality fitted (a scroll boundary landing — issue #818) this reports
            // 24 and the band reddens. A cap landing anywhere in 11...24 reddens for the same reason.
            for n in 1...24 {
                let height = try measuredHeight(n, size, now: now)
                if PanelGeometry.exceeds(height) { break }
                capacity = n
            }
            capacities[sizeName] = capacity
        }
        let atDefault = try XCTUnwrap(capacities["large"])
        let atCeiling = try XCTUnwrap(capacities["accessibility3"])
        let measured = "measured \(atDefault) at the default text size and \(atCeiling) at "
            + ".accessibility3, against \(PanelGeometry.derivedCeiling) pt"

        XCTAssertTrue((3...10).contains(atDefault),
                      "the default-text-size roster capacity is \(atDefault) (7 when this was written) — "
                      + "outside the single-digit band this pin records. Above it, the panel has gained a "
                      + "bound (issue #818) or the rows got much shorter; below it, the panel got "
                      + "dramatically taller. Either way re-derive rather than widening the band. \(measured)")
        // Banded at BOTH ends, like the default-size pin above. A bare `<= 5` would also be satisfied by a
        // capacity of ZERO — the signature of a measurement that stopped reaching the panel, which is
        // exactly the reading this suite must never accept as a pass.
        XCTAssertTrue((1...5).contains(atCeiling),
                      "the `.accessibility3` roster capacity is \(atCeiling) (2 when this was written). "
                      + "Above the band the panel stopped scaling with the type size, or gained a bound "
                      + "(issue #818); at zero not even a one-account panel fits, which is likelier to be a "
                      + "broken measurement than a panel that got that much taller. Both are "
                      + "re-derivations, not tuning. \(measured)")
        XCTAssertLessThan(atCeiling, atDefault,
                          "the panel fits at least as many accounts at .accessibility3 as at the default "
                          + "text size, which would mean the type scale (issue #756) stopped reaching the "
                          + "roster. \(measured)")
    }

    /// Evidences the file header's "these heights are a LOWER bound" claim rather than asserting it.
    ///
    /// The rosters above carry no swap callout so the cardinality axis is isolated. That is only a fair
    /// simplification if the callout is fixed chrome — a constant added once, not a per-row cost — and this
    /// measures exactly that: the same 60.00 pt whether the roster is 2 accounts or 10. So the real panel
    /// (which usually shows it) is uniformly taller than this suite's table, every overflow verdict here is
    /// conservative, and no delta in the growth law is affected.
    func testTheSwapCalloutIsFixedChromeSoTheseHeightsAreALowerBound() throws {
        let now = Self.wallClock()
        let target = NextSwap.target(to: Self.labels[1],
                                     reason: .soonestReset(resetsAt: now + 3 * 86_400
                                                           + PanelRenderHarness.boundaryGuardSecs))
        var costs: [Double] = []
        for n in [2, 5, 10] {
            let bare = try measuredHeight(n, .large, now: now)
            let withCallout = try measuredHeight(n, .large, now: now, nextSwap: target)
            costs.append(withCallout - bare)
        }
        let first = try XCTUnwrap(costs.first)
        XCTAssertGreaterThan(first, 0,
                             "adding a next-swap target changed the panel height by "
                             + String(format: "%.2f pt", first) + " — the swap callout did not render, so "
                             + "this measurement says nothing about the lower-bound claim it exists to "
                             + "support")
        for (index, cost) in costs.enumerated() {
            XCTAssertEqual(cost, first, accuracy: 1.0,
                           "the swap callout cost " + String(format: "%.2f pt", cost)
                           + " on a \([2, 5, 10][index])-account roster against "
                           + String(format: "%.2f pt", first) + " on a 2-account one — it is no longer "
                           + "cardinality-independent chrome, so omitting it from this suite's rosters "
                           + "biases the growth law rather than merely offsetting it")
        }
    }

    // MARK: - AC-3: the single-account panel is not degenerate

    /// The one-row roster is where spacers, minimum lengths and `GeometryReader` proportions misbehave, and
    /// it is the case nobody looks at — every committed fixture is a three-account roster.
    ///
    /// The assertion is that a single account costs EXACTLY what an account in a crowd costs. That is the
    /// direct vertical-degeneracy test: a `Spacer` stretching to fill a short panel, or a row taking a
    /// different proportion when it is the only one, would put the 1-account measurement off the line every
    /// larger roster sits on. Measured, it is on the line to the pixel — the 1 → 2 delta equals the 7 → 8
    /// delta.
    ///
    /// Deliberately NOT a raster comparison of the single row against the same row inside a larger roster.
    /// That was tried and it is confounded: the header sub-line states the account COUNT
    /// (`StatusPanelFormat.headerSubtitle`), so the two panels legitimately differ above the roster
    /// (measured: 7 704 differing bytes in the top 100 pt, worst channel 239). Cropping around that would
    /// hardcode the header's height, which is a brittler thing to depend on than the growth law itself.
    func testASingleAccountRowCostsExactlyWhatEveryOtherRowCosts() throws {
        let now = Self.wallClock()
        for (size, sizeName) in Self.sizeClasses {
            let one = try measuredHeight(1, size, now: now)
            let two = try measuredHeight(2, size, now: now)
            let seven = try measuredHeight(7, size, now: now)
            let eight = try measuredHeight(8, size, now: now)
            let firstRowCost = two - one
            let interiorRowCost = eight - seven
            // Magnitude BEFORE the comparison, for the reason the growth-law test states: two costs that
            // are both zero are trivially equal, so without this a stubbed measurement would leave this
            // gate green. Re-running the mutation matrix adversarially is what surfaced that.
            XCTAssertGreaterThan(firstRowCost, 0,
                                 "at \(sizeName) the second account added "
                                 + String(format: "%.2f pt", firstRowCost) + " — an account costs no "
                                 + "height, so the equality below would hold between two zeroes and this "
                                 + "gate would pass on a measurement that ignores its roster")
            XCTAssertEqual(firstRowCost, interiorRowCost, accuracy: 1.0,
                           "at \(sizeName) the SECOND account added "
                           + String(format: "%.2f pt", firstRowCost) + " while the EIGHTH added "
                           + String(format: "%.2f pt", interiorRowCost) + ". A single-account roster lays "
                           + "its row out differently from a crowded one — the degenerate case issue #755's "
                           + "AC-3 asks about. Look for a `Spacer` without a `minLength`, or a "
                           + "`GeometryReader` taking a proportion of a panel whose height now depends on "
                           + "the row count")
        }
    }

    /// The other half of AC-3 — nothing collapses or stretches HORIZONTALLY on a one-row roster, so the
    /// meter / label / reset column budgets still hold.
    ///
    /// This is a BRIDGE, not a re-measurement, and the distinction is why it is short. Those budgets are
    /// `StatusPanelFormat` constants with no roster-size term, and `PanelTextMetricsTests` (issue #750)
    /// already gates every one of them against its widest reachable content at every size class
    /// (`testEveryShippedMeterCellFitsItsWidestReachableContent`,
    /// `testEveryGatedCellFitsAtEveryDynamicTypeSizeClass` — the latter sweeping all twelve
    /// `DynamicTypeSize` cases × five gated cells). Re-deriving that here would be a second copy of a
    /// predicate — the exact thing `TextMetrics`' shared home exists to prevent.
    ///
    /// One limit on how far the bridge carries, inherited from issue #750 rather than introduced here: the
    /// three `UsageMeter` cells are real `.frame(width:)` pins (`StatusPanelRoster`), so for them "the
    /// budget holds" is DIRECTLY gated; `rosterLabelBudget` has no frame anywhere in `Sources/` and is a
    /// derived allowance, so for the label it is MODELLED. Carrying that distinction forward rather than
    /// letting AC-3's phrasing flatten it.
    ///
    /// What is NOT covered by that gate is whether a single-account panel still lays out at the width those
    /// budgets are derived FROM. `rosterLabelBudget` and the three meter cells all descend from
    /// `panelContentWidth`, so if the panel became content-sized — narrowing around one row — #750's whole
    /// gate would be measuring a width the single-account panel does not use, and would stay green while
    /// the row truncated. This measures the rendered width at every cardinality and pins it to the shipped
    /// constant, which is what makes #750's coverage transfer to the n = 1 case.
    func testASingleAccountPanelKeepsTheShippedWidthSoTheColumnBudgetsStillApply() throws {
        let now = Self.wallClock()
        for (size, sizeName) in Self.sizeClasses {
            let scale = PanelTypeScale.factor(for: size)
            let expected = Double(PanelMetrics.scaledWidth(scale))
            for n in Self.acceptanceCardinalities {
                let width = try XCTUnwrap(PanelGeometry.width(rows: roster(n, now: now), size: size,
                                                              now: now),
                                          "the \(n)-account panel did not rasterize at \(sizeName)")
                // 1 pt = twice the 0.5 pt raster quantum at `PanelRenderHarness.scale` = 2, so this
                // admits whole-pixel rounding in either direction and nothing else. It is not a soft
                // tolerance: the failure it exists to catch — a panel that sizes to its content — misses
                // by tens of points at `.large` and by hundreds at `.accessibility3` (measured: 380.00
                // against 894.12 when the size class is dropped), so the margin here is ~500× the signal.
                XCTAssertEqual(width, expected, accuracy: 1.0,
                               "the \(n)-account panel rendered "
                               + String(format: "%.2f pt", width) + " wide at \(sizeName) against the "
                               + String(format: "%.2f pt", expected) + " `PanelMetrics.scaledWidth` pins. "
                               + "The panel is no longer fixed-width, so every budget derived from "
                               + "`StatusPanelFormat.panelContentWidth` — `rosterLabelBudget` and the three "
                               + "`UsageMeter` cells — is now a function of roster size, and issue #750's "
                               + "text-metrics gate is measuring against a width this panel does not use")
            }
        }
    }

    // MARK: - CONSTRAINT-A: prove the bound can actually fire

    /// The canary for every assertion above, driven through their EXACT predicate
    /// (`PanelGeometry.exceeds`) against their exact ceiling.
    ///
    /// Two halves, and neither is true by construction. An intentionally oversized roster must TRIP the
    /// bound, and a one-account roster must NOT — together they rule out a predicate hardwired either way,
    /// AND they rule out the subtler failure this file's header names: a dead lever. If `measuredHeight`
    /// ignored its roster and returned a constant (a stubbed render, a cached raster, a fixture builder that
    /// dropped its argument), both rosters would report the same height and no ceiling could separate them
    /// — so the pair cannot both pass. That is what makes the real assertions evidence rather than
    /// decoration.
    ///
    /// The third half is the lever itself, asserted rather than inferred: the oversized roster must measure
    /// STRICTLY taller than the small one, by more than the ceiling's own margin. Growth reaching the
    /// measurement is what every table above rests on.
    func testAnOversizedRosterTripsTheBound() throws {
        let now = Self.wallClock()
        let small = try measuredHeight(1, .large, now: now)
        let oversized = try measuredHeight(20, .large, now: now)

        XCTAssertTrue(PanelGeometry.exceeds(oversized),
                      "a deliberately oversized 20-account roster measured "
                      + String(format: "%.2f pt", oversized) + " and did NOT trip the "
                      + "\(PanelGeometry.derivedCeiling) pt bound. Either the panel is now bounded (issue "
                      + "#818 — good news, re-derive this suite) or the bound cannot fire at all, in which "
                      + "case every 'fits' verdict above is vacuous")
        XCTAssertFalse(PanelGeometry.exceeds(small),
                       "a 1-account roster measured " + String(format: "%.2f pt", small)
                       + " and TRIPPED the \(PanelGeometry.derivedCeiling) pt bound. The predicate fires on "
                       + "everything, so the overflow verdicts above prove nothing about roster size")
        // The lever, asserted as a claim the two above do NOT already imply. A bare `oversized > small`
        // would be redundant — assertions 1 and 2 together give `oversized > ceiling >= small` for free,
        // so it could never fail on its own. (An earlier draft asserted
        // `oversized - small > derivedCeiling - small`, which cancels `small` and is exactly that
        // restatement wearing a relative-comparison message; an adversarial re-read caught it.) The
        // independent claim is ATTRIBUTION: the gap between a 1- and a 20-account panel must be the
        // nineteen added rows and not something else, so it is checked against a marginal cost measured
        // separately — plus that the cost is non-zero, which is what a stubbed measurement fails.
        let marginal = try measuredHeight(2, .large, now: now) - small
        XCTAssertGreaterThan(marginal, 0,
                             "one account costs " + String(format: "%.2f pt", marginal)
                             + " — the roster is not reaching the measurement, so the two assertions above "
                             + "are separating something other than cardinality")
        XCTAssertEqual(oversized - small, 19 * marginal, accuracy: 1.0,
                       "the 20-account panel is " + String(format: "%.2f pt", oversized - small)
                       + " taller than the 1-account one, against the "
                       + String(format: "%.2f pt", 19 * marginal) + " that nineteen accounts cost at "
                       + String(format: "%.2f pt", marginal) + " each. The height gap is not attributable "
                       + "to the added rows, so the bound above is separating the two rosters on something "
                       + "other than cardinality")
    }
}
#endif
