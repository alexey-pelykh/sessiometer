// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The panel's INTERACTION-STATE gate (issue #766) — the armed / in-flight / mis-click-guard surfaces the
// one-resting-frame render harness structurally cannot see.
//
// WHAT WAS MISSING. `ImageRenderer` draws ONE resting frame, so `design/README.md` recorded the armed
// brighten, the in-flight `Switching…` spinner and the real-popover round-trip as a silent manual gap
// (#380). Since #448 the switch chip is PERSISTENT, so its resting glyph IS captured — what stayed
// uncovered is every state that needs an INPUT to reach: hover/focus arming, a swap in flight, and the
// row's interactive shape at a width below the affordance budget.
//
// THE THREE PIECES, and why each is where it is:
//
//   1. ARMED vs RESTING is a RENDER question. `switchChipEmphasis(offersSwitch:block:armed:)` is already
//      unit-asserted as a value mapping (`AccountSwapTests`), which is exactly what AC-2 says is not
//      enough — a `.resting`/`.armed` enum pair proves the DECISION differs, never that the PIXELS do. So
//      this suite renders both and measures. The same split applies to #959's blocked-is-`.hidden`
//      routing: the verdict is unit-asserted there, the pixels are asserted here.
//   2. IN-FLIGHT is reachable IN-PROCESS. Issue #761's spike measured it as XCUITest-reachable and priced
//      a UI-test target for it. Issue #758 (`PanelAccessibilityTreeTests`) then proved the accessibility
//      tree is walkable from THIS headless bundle, and `AccountSwapModel.pendingPreview` pins the phase
//      without a socket — so the in-flight sliver needs no XCUITest target, no scheme outside the required
//      `swift` job, no TCC grant, and cannot be blocked by a locked screen (the failure that returned 0 of
//      20 valid local runs for the spike).
//   3. THE MIS-CLICK GUARD is a WIDTH question, and the spike explicitly did NOT measure it. #761's
//      `Button`/`Other` element-role finding is the ACTIVE-vs-SWITCHABLE axis; this is the WIDTH axis.
//      `AccountSwapTests` covers `rowFitsSwitchAffordance` as a predicate over a number. What that cannot
//      say is what the ROW then publishes — and the hazard the guard exists to stop is precisely an
//      interactive surface you cannot see, so "the predicate returned false" is the wrong layer to stop at.
//
// ─────────────────────────────────────────────────────────────────────────────────────────────────────
// THE BUILD REFERENCE, MEASURED RATHER THAN ASSUMED (`design/menubar-preview.html`).
//
// The mock AUTHORS the armed token and even names its SwiftUI mapping (`:242-249`): `.rowact` rests at
// `--text-3`, `.acct:hover .rowact` and `.rowact.armed` brighten to `--text-2`, over the comment
// "Mirrors SwiftUI `StatusPanelFormat.switchChipEmphasis` → `.tertiary` (rest) / `.secondary` (armed)".
//
// But NO element in the mock carries `class="rowact armed"` — all ~20 instantiated chips render at rest,
// and the `.armed` rule exists only to "preview the brighten statically". There is likewise NO in-flight
// frame anywhere in the mock (grep for switching/spinner returns only daemon-starting's static forming
// glyph and the capture card).
//
// So the mock ratifies the armed RELATION (rest quieter → armed brighter) and does NOT author the armed
// or in-flight APPEARANCE. That distinction decides this suite's shape: a committed golden of an armed
// panel would self-baseline against an oracle that does not exist — issue #752's missing-oracle problem —
// so this suite ships NO new golden. It asserts the RELATION the mock does ratify, which needs no
// baseline because it is a comparison between two renders made in the same run.
//
// ─────────────────────────────────────────────────────────────────────────────────────────────────────
// THE METRIC + THRESHOLDS, MEASURED (the issue #525 discipline — calibrated to real separations, never
// guessed). `PanelRaster.diffFraction` counts pixels whose largest channel delta exceeds
// `channelThreshold`, the same primitive the panel golden gate and `BarGlyphRenderer` use.
//
// Measured on arm64 / macOS 26.5.2 / Xcode 26.6, one `Personal` row at the shipped width, 728×188 @2×:
//
//                      T=2       T=4       T=8       T=16      T=32      T=64
//   armed (light)   0.933825  0.930946  0.881298  0.001607  0.001439  0.000833
//   armed (dark)    0.933138  0.930661  0.911503  0.001637  0.001454  0.001030
//   in-flight       0.007482  0.007482  0.007482  0.007482  0.007482  0.007482
//   canary          0.000000  0.000000  0.000000  0.000000  0.000000  0.000000
//   non-target      0.000000  0.000000  0.000000  0.000000  0.000000  0.000000
//
// plus, at the T=4 the suite runs at, the chip step measured in ISOLATION (a row whose `live` guard holds
// the wash out): 0.001666 light / 0.001659 dark.
//
// Issue #959 RE-HOMED that isolation lane without changing what it isolates, and the two numbers above are
// its RE-MEASUREMENT, not the old ones carried forward. The lane used to run on a BLOCKED row; a blocked
// row now has no chip at all, so it runs on a viable row with a SIBLING swap in flight instead — `live` is
// false either way, so the wash is held out identically and both rows are equally `.disabled()`.
//
// The old blocked-row lane read 0.001914 / 0.001929, so the headline is ~13 % lower — but these two
// fractions are NOT like-for-like, and the decomposition matters more than the headline. Measured:
//
//   lane                                  raster     fraction    changed px
//   pre-#959 `nosign` on a blocked row    728×232    0.001914    323.3
//   arrow on a blocked row (mutation)     728×232    0.001350    228.0
//   arrow on a viable row (this lane)     728×188    0.001666    228.0
//
// The changed-pixel COUNT is identical across the last two rows, which decomposes the headline cleanly:
// the glyph swap alone costs −29.5 % (this raster's `nosign` moves 323.3 px against the arrow's 228.0),
// and the move to a shorter raster gives +23.4 % back because the denominator lost #955's cue line
// (232 → 188 rows). Net −13 %. Read the fractions as fractions OF DIFFERENT FRAMES, not as one signal
// weakening.
//
// That −29.5 % is NOT the ink-mass parity #959 rests on, and conflating the two would be easy: this is a
// count of pixels a TINT STEP moves on a 2× raster at T=4, whereas #959's parity is an ink DENSITY on a
// 1× capture (arrow 18.2 over 70 px = 0.260, `nosign` 19.5 over 82 px = 0.238 — the negation marginally
// the quieter). Different quantities over different areas at different scales; neither measurement bears
// on the other, and the fix's rationale rests only on the second.
//
// `chipOnlyFloor` was NOT lowered to suit. Its margin under the measurement narrows from ~1.9× to ~1.67×;
// the floor-to-`unchangedCeiling` ratio is unchanged at 2× (0.001 / 0.0005 — that pair never moved), while
// measurement-to-ceiling eases from 3.83× to 3.33×. Still a floor the signal clears and noise does not.
//
// Read the signals, because they are shaped differently and one threshold cannot serve them all naively:
//
//   • ARMING is a LARGE-AREA, LOW-AMPLITUDE change — the `Color.secondary.opacity(0.08)` wash repaints
//     ~93 % of the row by only ~8–15/255, which is why the number falls off a cliff between T=8 (0.88)
//     and T=16 (0.0016). The golden gate's 64/255 threshold therefore reads this real, ratified design
//     step as 0.0008 — indistinguishable from nothing. That is not a defect in the golden gate: 64/255 is
//     tuned to ignore antialiasing on a DRIFT comparison. It is the wrong instrument for this question.
//   • IN-FLIGHT is the opposite — a SMALL-AREA, HIGH-AMPLITUDE change (the chip's 28 pt slot swaps to a
//     spinner), so it reads a flat 0.007482 at EVERY threshold. Threshold choice is irrelevant to it.
//
// What makes the floors evidence rather than numbers that happen to pass is the pair of controls every
// assertion is bracketed by:
//
//   • the CANARY — resting-vs-resting, i.e. exactly the raster pair a regression that DELETED the arm
//     treatment would produce, pushed through the SAME predicate, which must score BELOW the floor;
//   • the LIVENESS control — arming a row that is NOT a switch target must move nothing, so the delta the
//     gate measures is attributable to the arm treatment and not to ambient render nondeterminism.
//
// A floor with only the first control could still be a gate that always passes; with only the second it
// could be a gate that cannot fail. Both are required (`design/README.md`'s own framing for #760's pins).
//
// ─────────────────────────────────────────────────────────────────────────────────────────────────────
// PROVEN BY MUTATION, NOT BY INSPECTION (issue #748's batch constraint). Each gate below was run against
// a deliberately broken build of the PRODUCTION code and confirmed to redden — the table is the record:
//
//   Mutation applied to Sources/                          | Which test reddens
//   ------------------------------------------------------|--------------------------------------------
//   chip `.armed` case → resting tint (brighten deleted)   | chip-isolation ONLY
//   chip `.resting`/`.armed` tints SWAPPED (arming DIMS)   | chip-isolation ONLY, via its DIRECTION half
//   `RowSwitchButtonStyle.wash` → 0 (wash deleted)         | whole-row arm ONLY
//   `offersSwitch` drops `rowFitsSwitchAffordance`         | narrow-row mis-click guard
//   row `ProgressView()` → `Color.clear` (spinner deleted) | in-flight render lane
//   `isSwitching` / `isSwitchingToTarget` → `false`        | both in-flight tests
//   `switchChipEmphasis` drops its `block == nil` guard    | blocked-row arm lane (#959), plus the two
//     (i.e. the pre-#959 behaviour is restored)            |   verdict tests in `AccountSwapTests`
//   active row's accent fill → `Color.clear`               | active-vs-blocked distinctness (#959)
//
// The `block == nil` row is #959's own falsifier and it was run, not reasoned about: reverting the guard
// restores a chip to the blocked row, arming it moves the chip again (0.001350 / 0.001344, ~2.7× the
// ceiling), and the lane that asserts "nothing moves" fails. A gate for an ABSENCE is worth exactly as
// much as its demonstration that the PRESENCE would trip it — so the check is that the detector FAILS the
// pre-fix code, not merely that it passes the fixed code. Row 1 was re-run after #959 re-homed the
// chip-isolation lane, confirming it still catches a deleted brighten from its new row state and still
// catches it ALONE (the whole-row arm test stays green).
//
// The LAST row is the falsifier for the distinctness gate, and it exists because the first version of that
// test could not fail. It compared two renders of DIFFERENT heights (the blocked row carries #955's cue
// line), and `diffFraction` returns a flat 1 on a size mismatch — so `delta > armFloor` was `1.0 > 0.20`,
// a constant that would have stayed green with every row-level channel deleted. Cropping to the
// overlapping region turned it back into a measurement: with the accent fill mutated away the common
// region falls to 0.0844 / 0.0863, under the floor, and the test reddens. Against the uncropped version
// the same mutation stayed green.
//
// Read the arm rows together, because they are the reason there are two arm tests rather than one, and
// two assertions inside the chip one. Each is blind to the others' mutation:
//
//   • the whole-row measurement is dominated by the wash ~500:1, so DELETING the chip brighten leaves it
//     GREEN — that hole was found by running the mutation, not by reading the code;
//   • both MAGNITUDE assertions are blind to INVERSION. Swapping the two tints so arming dims the chip
//     left all 767 tests that existed before the direction assertion did green, the panel goldens
//     included (at 64/255 they read the whole step as ~0.0008); the 768th is the direction predicate's
//     own canary, which this very hole is what produced. That is the inverse of the ratified
//     `--text-3` → `--text-2` relation shipping
//     under a green suite, which is why the chip lane also asserts direction via `inkMass`. That
//     predicate needs no threshold — the mutation only swaps which raster carries which label, so the
//     two ink masses are the same pair either way and the comparison is a strict `>` over deterministic
//     (`stableRender`-settled) values. Measured margins on the #959 lane: armed 36.1164 vs resting
//     36.0771 light, 27.5343 vs 27.4091 dark (the pre-#959 blocked-row lane read 41.4885 vs 41.4039 and
//     59.2353 vs 58.8083 — a different row state, so a different absolute mass; what the assertion reads
//     is the sign of the difference, and that is unchanged).
//     Note `inkMass` sums ALPHA as well as RGB, and that is load-bearing rather than incidental —
//     see its own doc comment for the measured reason an RGB-only form (which is what
//     `PanelRaster.inkCoverage` is) cannot see this step at all.
//
// Both holes are the shape issue #437 warns about: a gate authored against passing code that then
// DEFENDS the regression it cannot see. Neither was visible by inspection.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

extension PanelRaster {
    /// The first `rows` raster rows, as a raster in its own right.
    ///
    /// Exists for ONE caller — the #959 active-vs-blocked comparison, whose two renders are legitimately
    /// different heights (the blocked row carries #955's extra cue line). `PanelRaster.diffFraction`
    /// returns a flat 1 for mismatched sizes, so comparing them uncropped would score a constant 1.0 and
    /// pass any floor no matter what the pixels did. Cropping to the common region is what turns that
    /// comparison back into a measurement; the size difference is asserted separately at the call site.
    ///
    /// Rows are whole `width * 4`-byte spans, so this is a prefix of `bytes` — no re-packing, and the
    /// result stays in the same normalized RGBA layout every predicate in this file expects.
    func firstRows(_ rows: Int) -> PanelRaster {
        let clamped = max(0, min(rows, height))
        return PanelRaster(width: width, height: clamped,
                           bytes: Array(bytes.prefix(width * clamped * 4)))
    }
}

@MainActor
final class PanelInteractionStateTests: XCTestCase {

    // MARK: - Calibrated threshold (MEASURED — see the header)

    /// The per-channel delta a pixel must move by before it counts as changed. 4/255 — comfortably above
    /// the ±1/255 cold-raster settle issue #824 describes (which `stableRender` removes anyway), and well
    /// below the ~8–15/255 the wash actually moves, so it sits on the flat part of the curve rather than
    /// on the T=8→T=16 cliff. NOT the golden gate's 64/255, which reads the whole arm treatment as 0.0008.
    private let deltaChannelThreshold: UInt8 = 4

    /// The share of the row's pixels the ARM treatment must move. Measured 0.9309 (light) / 0.9307 (dark);
    /// this floor keeps a ~4.6× margin under the measurement and 400× over `unchangedCeiling` (the canary
    /// itself measures 0.000000, against which a ratio is undefined), so it tolerates cross-machine
    /// antialiasing without ever tolerating a deleted arm treatment.
    private let armFloor = 0.20

    /// The share the CHIP BRIGHTEN alone must move, measured on a row whose wash is held out by its `live`
    /// guard (since #959, a viable row with a SIBLING swap in flight). Measured 0.001666 (light) /
    /// 0.001659 (dark) — small because the chip is an 11 pt glyph in a 728×188 raster (its whole slot is only
    /// ~0.005 of the frame), so read it as "about a third of the slot's ink moved", not as a weak signal.
    /// ~1.67× margin under the measurement; the floor still sits 2× over `unchangedCeiling`, as it always
    /// did (that pair did not move — what narrowed is measurement-to-ceiling, 3.83× → 3.33×).
    ///
    /// The floor is UNCHANGED across #959 even though the headline fraction fell ~13 % — a drop that is
    /// mostly a DENOMINATOR change, not a weaker signal (see the header's decomposition: the changed-pixel
    /// count is identical to arrow-on-a-blocked-row; the new lane's raster is simply 188 rows rather than
    /// 232). Lowering a floor to preserve a margin would buy back the appearance of headroom while
    /// weakening the gate — the honest record is a narrower margin over the same floor, which it clears.
    private let chipOnlyFloor = 0.001

    /// The share the IN-FLIGHT change must move. A separate, much smaller floor because the spinner is a
    /// SMALL-AREA change (a 28 pt slot in a 728×188 raster) where arming is a whole-row wash — reusing
    /// `armFloor` here would demand the spinner repaint a fifth of the row, which is not what it does or
    /// should do. Measured a flat 0.007482 at every threshold; this floor keeps a ~1.9× margin under it.
    private let inFlightFloor = 0.004

    /// The share that must separate the ACTIVE row from a BLOCKED one over their overlapping region
    /// (issue #959) — the risk that arrives once neither carries a chip. Measured 0.9608 (light) /
    /// 0.9653 (dark); mutating the accent row fill to `Color.clear` drops it to 0.0844 / 0.0863, which is
    /// what proves the floor sits between a healthy row-identity encoding and a collapsed one.
    ///
    /// Numerically equal to `armFloor` and deliberately NOT the same constant — the two gates are
    /// calibrated to different measurements (0.93 vs 0.96) and answer different questions, so re-tuning
    /// the arm treatment's floor must not silently move this one. Same reasoning `unchangedCeiling` gives
    /// below for staying separate from the floors.
    private let rowIdentityFloor = 0.20

    /// The ceiling the CANARY (a resting-vs-resting pair — what a deleted arm treatment renders) and the
    /// LIVENESS control (arming a non-target row) must both stay under. Both measure exactly 0.000000, so
    /// this is pure headroom. Deliberately a separate constant from the floors above: the gap between them
    /// is the evidence margin, and collapsing them into one number would hide it.
    private let unchangedCeiling = 0.0005

    // MARK: - Fixtures

    /// A wall-clock seed, exactly as `PanelRenderHarness` does — at epoch 0 every countdown formats as
    /// decades in the past, which is not a state the panel is ever asked to render.
    private static let now = Int64(Date().timeIntervalSince1970)

    /// `isActive` is the only axis these tests vary: `switchState` is injected directly rather than derived
    /// from the row, so the fields `StatusPanelFormat.rowSwitchState` would read (`weeklyExhausted` and
    /// friends) cannot influence anything here and are pinned to one shape.
    private func account(isActive: Bool = false) -> AccountRow {
        AccountRow(label: "Personal", isActive: isActive, isEnabled: true, isQuarantined: false,
                   isRecovering: false, auth: .healthy, sessionPct: 31, weeklyPct: 71,
                   sessionResetsAt: Self.now + 3_600, weeklyResetsAt: Self.now + 3 * 86_400,
                   weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil)
    }

    /// A swap in flight targeting a DIFFERENT account than the row under test.
    ///
    /// This is the wash-held-out seam the chip-isolation lane runs on since issue #959 (see that test for
    /// why it is no longer a blocked row). The target must not be `"Personal"` — the row `account()`
    /// builds — or `isSwitching` becomes true and the slot renders the `Switching…` spinner instead of the
    /// chip, which is a different lane entirely (`testTheInFlightRowRendersDifferentlyFromTheRestingRow`).
    private static func siblingSwapInFlight() -> AccountSwapModel {
        .pendingPreview(target: "Temp")
    }

    /// One roster row, wired through the same environment seam the panel uses. `switchState` and
    /// `rowWidth` are the two axes under test; `armed` is the #766 render seam.
    private func rowView(armed: Bool = false,
                         switchState: StatusPanelFormat.RowSwitchState = .available,
                         rowWidth: Double = StatusPanelFormat.defaultRowWidth,
                         isActive: Bool = false,
                         swap: AccountSwapModel? = nil,
                         scheme: ColorScheme = .light) -> some View {
        AccountRowView(row: account(isActive: isActive), monogram: "PE", now: Self.now,
                       switchState: switchState, nextSwap: nil, rowWidth: rowWidth, armed: armed)
            .frame(width: rowWidth)
            .environmentObject(swap ?? AccountSwapModel(client: nil))
            .environment(\.colorScheme, scheme)
            // Pin the accent for the same reason the golden gate does (#754): rendered from this bundle
            // the app target's `ASSETCATALOG_COMPILER_GLOBAL_ACCENT_COLOR_NAME` is absent, so an unpinned
            // `Color.accentColor` would resolve to the OPERATOR'S system accent.
            .tint(Color.panelAccent)
    }

    // MARK: - Rendering

    private func render(_ view: some View) throws -> PanelRaster {
        let renderer = ImageRenderer(content: view)
        renderer.scale = 2
        let cg = try XCTUnwrap(renderer.cgImage, "ImageRenderer produced no raster for the row")
        return try XCTUnwrap(PanelRaster.normalize(cg), "raster did not normalize")
    }

    /// A row render settled to the BYTE before it is compared — the #760 control for the ±1/255 cold-raster
    /// drift issue #824 describes.
    ///
    /// The loop, its bound, its predicate and the reasoning for NOT absorbing that drift into a looser
    /// threshold moved onto `PanelRaster.settled` at issue #911, taking this file's copy of that prose with
    /// them: it was one of three near-verbatim copies, and three homes is three places for it to drift
    /// silently, since a copy that quietly relaxes its bound still passes. Read it there;
    /// `PanelGoldenParityTests.testSettledLoopsUntilTwoConsecutiveRastersAgree` and
    /// `PanelGoldenParityTests.testSettledThrowsAndNamesItsCallerRatherThanReturningAnUnsettledRaster` prove
    /// the shared loop genuinely iterates and genuinely throws. `PanelRenderHarness`'s
    /// `warmUpIfNeeded()` deliberately keeps its own: it ships in the app target, where `PanelRaster` does
    /// not exist. What stays here is the SUBJECT label a failure names; `file`/`line` default at the caller,
    /// so a red points at the test that drifted rather than at this wrapper.
    private func stableRender(_ view: some View,
                              file: StaticString = #filePath, line: UInt = #line) throws -> PanelRaster {
        try PanelRaster.settled("a row", file: file, line: line) { try render(view) }
    }

    /// The ONE magnitude predicate every assertion, every canary and every control in this file routes
    /// through — the arm lanes, the in-flight lane and the liveness control alike. Single-sourced
    /// deliberately, because a canary that exercises a parallel copy of the comparison proves nothing
    /// about the comparison the real assertion uses.
    ///
    /// Callers must hand it SAME-SIZED rasters. `diffFraction` returns a flat 1 on a size mismatch, which
    /// silently satisfies any `> floor` assertion — see `firstRows` for the one comparison here that comes
    /// from differently-sized renders and has to crop before it can measure anything.
    private func pixelDelta(_ a: PanelRaster, _ b: PanelRaster) -> Double {
        PanelRaster.diffFraction(a, b, channelThreshold: deltaChannelThreshold)
    }

    /// The DIRECTION predicate — mean per-pixel deviation from the raster's own background, in summed-RGBA
    /// units (0…1020; see the note on why alpha is one of the four).
    ///
    /// `pixelDelta` above is a magnitude: it answers "did anything move?", which is necessary and NOT
    /// sufficient. The mock ratifies a *directed* relation — `--text-3` at rest → `--text-2` armed, i.e.
    /// armed carries MORE contrast against the row than resting does — and a gate that only measures
    /// movement passes just as happily on the inverse. That is not hypothetical: swapping the view's two
    /// `foregroundStyle` cases so arming DIMS the chip left all 767 tests that predate this assertion
    /// green, the panel goldens included (their 64/255 threshold reads the whole step as ~0.0008). This
    /// closes that.
    ///
    /// Background-referenced like `PanelRaster.inkCoverage`, so it needs no hardcoded chip rect to rot on —
    /// but it sums MAGNITUDE where `inkCoverage` counts pixels over a threshold, and that is precisely what
    /// makes it directional. It is also theme-agnostic without a luminance sign: a more opaque glyph departs
    /// further from its backdrop whether the ink is darkening on light or lightening on dark.
    ///
    /// THE RASTER IS COMPOSITED ONTO AN OPAQUE BACKDROP FIRST, and that step is load-bearing rather than
    /// cosmetic. A bare row renders on a TRANSPARENT backdrop (a non-active row's own background is
    /// `Color.clear`) into a PREMULTIPLIED buffer, and measuring that buffer directly cannot work in either
    /// naive form:
    ///
    ///   * RGB-only, uncomposited, is structurally BLIND to the step. In the light scheme the chip's ink is
    ///     near-black, and premultiplied black is `(0,0,0)` at *every* opacity — so a translucent
    ///     `.tertiary` and a translucent `.secondary` produce byte-identical RGB and differ ONLY in alpha.
    ///     Measured: that form returned 21.168576104746318 for both, to the last digit.
    ///   * Summing ALPHA alongside RGB fixes the blindness but measures COVERAGE, not contrast — and the two
    ///     part company the moment one state's token is opaque and the other's is not. Issue #956 made
    ///     exactly that change: `.resting` moved to the opaque `SwapChipResting` asset while `.armed` kept
    ///     the translucent `.secondary`, so the resting chip contributed ~255 of alpha per glyph pixel
    ///     against armed's ~128 and this predicate reported resting as the STRONGER of the two (light 42.21
    ///     vs 41.49, dark 59.35 vs 59.24). That verdict was an artifact: on the surface the panel actually
    ///     draws on, resting measures 3.34:1 and armed 4.73:1 — armed is higher-contrast by 1.42×, exactly
    ///     the ratified relation. A metric that inverts on a correct change is not a guard, it is a trap.
    ///
    /// Compositing resolves both at once, because it is what the eye does: source-over onto the row backdrop
    /// turns alpha back into the colour difference alpha was always standing in for, so a translucent glyph
    /// and an opaque one are finally measured on one scale. The alpha term then drops out (every composited
    /// pixel is opaque) without reopening the blindness above.
    ///
    /// `PanelRaster.inkCoverage` keeps the uncomposited RGB-only shape — correct for the opaque full-panel
    /// rasters it was written for, wrong here; its doc comment carries that warning.
    ///
    /// The `4` stride is `PanelRaster`'s RGBA pixel width, restated because `PanelRaster.bytesPerPixel` is
    /// private to that type. It is asserted rather than assumed — `normalize` pins the layout, and the
    /// unit-consistency check below would break loudly if the stride were ever wrong.
    private func inkMass(_ raster: PanelRaster, on scheme: ColorScheme) -> Double {
        guard raster.width > 0, raster.height > 0, raster.bytes.count >= 4 else { return 0 }
        XCTAssertEqual(raster.bytes.count, raster.width * raster.height * 4,
                       "inkMass assumes 4-byte RGBA pixels; PanelRaster.normalize changed its layout")
        let base = Self.rowBackdrop(scheme)
        var total = 0
        raster.bytes.withUnsafeBufferPointer { p in
            var i = 0
            while i < p.count {
                // Source-over. The buffer is PREMULTIPLIED, so the source term is already scaled by alpha
                // and only the backdrop needs the (1 - alpha) weight.
                let clear = 255 - Int(p[i + 3])
                total += abs(Int(p[i]) + base.red * clear / 255 - base.red)
                       + abs(Int(p[i + 1]) + base.green * clear / 255 - base.green)
                       + abs(Int(p[i + 2]) + base.blue * clear / 255 - base.blue)
                i += 4
            }
        }
        return Double(total) / Double(raster.width * raster.height)
    }

    /// The opaque surface a bare row is composited onto before its ink is measured — the panel's OWN row
    /// backdrop, read off a built panel in issue #949 rather than assumed. Only the direction of the
    /// comparison depends on it, and both sides are composited onto the same value, so this is a reference
    /// surface in the sense `BarGlyphRenderer.swift:67` establishes: the live panel floats on translucent
    /// vibrancy over arbitrary wallpaper, which has no single adjacent colour to measure against.
    private static func rowBackdrop(_ scheme: ColorScheme) -> (red: Int, green: Int, blue: Int) {
        scheme == .light ? (239, 239, 239) : (48, 48, 48)
    }

    // MARK: - AC-2: the armed row is VISIBLY different, not merely a different enum value

    func testTheArmedRowIsVisiblyDifferentFromTheRestingRow() throws {
        var checked = 0
        for scheme in [ColorScheme.light, .dark] {
            let resting = try stableRender(rowView(armed: false, scheme: scheme))
            let armed = try stableRender(rowView(armed: true, scheme: scheme))

            XCTAssertEqual(resting.width, armed.width, "arming must not resize the row")
            XCTAssertEqual(resting.height, armed.height,
                           "arming must not reflow the row — the slot width is identical hidden/resting/armed")

            let delta = pixelDelta(resting, armed)
            XCTAssertGreaterThan(delta, armFloor, """
                the ARMED row is visually indistinguishable from the resting one in \(scheme) \
                (\(delta) changed at ≥\(deltaChannelThreshold)/255, floor \(armFloor)). \
                `switchChipEmphasis` may still return .armed — that is the enum, not the pixels, and \
                issue #766 AC-2 is explicitly about the pixels. The mock ratifies the relation \
                (--text-3 at rest → --text-2 armed); this says the built panel does not honour it.
                """)
            checked += 1
        }
        // Degenerate-subject guard: the pass is evidence only if both themes were actually compared.
        XCTAssertEqual(checked, 2, "expected 2 (light + dark) arm comparisons, ran \(checked)")
    }

    /// The arm treatment has TWO halves, and the whole-row measurement above is dominated by one of them.
    ///
    /// Measured: the wash alone repaints ~93 % of the row, so a regression that deleted ONLY the chip's
    /// `.tertiary` → `.secondary` brighten would leave the number above essentially unchanged and sail
    /// through. That matters because the chip step is the half the MOCK actually ratifies
    /// (`--text-3` → `--text-2`, naming those exact SwiftUI roles); the wash is a native interaction
    /// treatment the static mock has no state for at all.
    ///
    /// Isolated without cropping to a hardcoded rect (which would rot on any layout change): a row whose
    /// `live` guard is off has `RowSwitchButtonStyle.wash == 0` whatever the hover state, while
    /// `offersSwitch` stays true so the chip still resolves through `switchChipEmphasis`. Arming one
    /// therefore moves the chip and nothing else — the same production code path, a different row state.
    ///
    /// WHICH non-live row, and why it changed (issue #959). This lane used to use a BLOCKED row. That row no
    /// longer has a chip at all — #959 removed it, because the chip and its own negation were at ink-mass
    /// parity in the same slot and could not be told apart at rest — so arming one now moves nothing and
    /// this lane would measure 0 against its own floor. The substitute is a VIABLE row with a SIBLING swap
    /// in flight: `isLiveSwitch` is false (`swap.phase.isPending`), so the wash is held out identically,
    /// while `blockReason == nil` and `isSwitching == false` keep the chip present and armable. Both rows
    /// are equally `.disabled()`, so the only thing that differs between the two frames is still the chip.
    /// The isolation property is unchanged; only the row state that produces it moved.
    func testTheChipBrightenIsMeasurableWithTheRowWashHeldOut() throws {
        var checked = 0
        for scheme in [ColorScheme.light, .dark] {
            let resting = try stableRender(rowView(armed: false, swap: Self.siblingSwapInFlight(),
                                                   scheme: scheme))
            let armed = try stableRender(rowView(armed: true, swap: Self.siblingSwapInFlight(),
                                                 scheme: scheme))

            // MAGNITUDE — something moved.
            let delta = pixelDelta(resting, armed)
            XCTAssertGreaterThan(delta, chipOnlyFloor, """
                the switch chip does not visibly brighten when armed in \(scheme) (\(delta), floor \
                \(chipOnlyFloor)). Measured on a row whose wash is held out by its own `live` guard (a \
                SIBLING swap is in flight) — so this is the chip step alone, and it is the half \
                `menubar-preview.html` ratifies (--text-3 at rest → --text-2 armed). The whole-row test \
                cannot see this: the wash dominates it ~500:1
                """)

            // DIRECTION — it moved the RIGHT WAY. Magnitude alone passes identically on the inverse, and
            // the inverse is a real, shippable regression: swap the view's two `foregroundStyle` cases and
            // arming DIMS the chip, which contradicts the ratified `--text-3` → `--text-2` relation while
            // every other test in this repo — panel goldens included — stays green.
            let restingInk = inkMass(resting, on: scheme)
            let armedInk = inkMass(armed, on: scheme)
            XCTAssertGreaterThan(armedInk, restingInk, """
                the armed chip carries NO MORE ink than the resting one in \(scheme) \
                (armed \(armedInk) vs resting \(restingInk)) — so arming either DIMS the chip or, if the \
                two are equal, does not tint it at all. The mock ratifies a \
                directed relation (rest --text-3 → armed --text-2: armed is the higher-contrast of the \
                two), so this is the inverse of the design, not merely a different value. The magnitude \
                assertion above cannot see it — it passes on either direction
                """)
            checked += 1
        }
        XCTAssertEqual(checked, 2, "expected 2 (light + dark) chip comparisons, ran \(checked)")
    }

    /// The direction predicate's own canary: it must be able to report "no increase".
    ///
    /// `inkMass` is a mean, so an assertion built on it could pass on noise if the two renders were nearly
    /// equal. Two RESTING renders must therefore come out EXACTLY equal — same bytes, same mass — which
    /// both proves the measure is deterministic and pins the floor the directional claim stands on.
    func testTheDirectionPredicateReportsNoIncreaseBetweenTwoIdenticalRenders() throws {
        // The SAME row state the direction assertion above runs on (#959 moved it off a blocked row) — a
        // canary that exercised a different lane would prove nothing about the lane that carries the claim.
        let resting = try stableRender(rowView(armed: false, swap: Self.siblingSwapInFlight()))
        let restingAgain = try stableRender(rowView(armed: false, swap: Self.siblingSwapInFlight()))

        XCTAssertEqual(inkMass(resting, on: .light), inkMass(restingAgain, on: .light), accuracy: 0.0, """
            two identical resting renders report different ink mass — the direction predicate is \
            nondeterministic, so `armed > resting` could be reporting noise rather than the tint step
            """)
    }

    // MARK: - Baseline trap: the gate PROVES it can fail (canary), by MUTATION through the same predicate

    /// The mutation is the real regression: DELETE the arm treatment and an armed row renders exactly like
    /// a resting one. That is the raster pair fed here — and the floor must reject it.
    ///
    /// This is what makes `testTheArmedRowIsVisiblyDifferentFromTheRestingRow` evidence rather than a gate
    /// that cannot fail. The local precedent is expensive: issue #437's three render bugs were misread five
    /// times as "the DESIGN fails distinctness", and a golden blessed then would have DEFENDED them.
    func testTheArmCanaryScoresBelowTheFloorWhenTheArmTreatmentIsAbsent() throws {
        let resting = try stableRender(rowView(armed: false))
        let restingAgain = try stableRender(rowView(armed: false))

        let canary = pixelDelta(resting, restingAgain)
        XCTAssertLessThan(canary, unchangedCeiling, """
            two RESTING renders differ by \(canary) — above the \(unchangedCeiling) ceiling, so the \
            predicate reports change where there is none and the arm floor above proves nothing
            """)
        XCTAssertLessThan(canary, armFloor, """
            the canary (\(canary)) reaches the arm floor (\(armFloor)) — a deleted arm treatment would \
            still pass, so the gate cannot fail and is not evidence
            """)
    }

    /// The LIVENESS control the canary above cannot supply: is the `armed` lever attributable at all?
    ///
    /// On a row that is NOT a switch target (`.notATarget` — the active row, or any row on a dropped
    /// connection) there is no chip and no live switch, so arming must change nothing. If this moved,
    /// the delta the gate measures would not be attributable to the arm treatment.
    func testArmingARowThatIsNotASwitchTargetChangesNothing() throws {
        let resting = try stableRender(rowView(armed: false, switchState: .notATarget, isActive: true))
        let armed = try stableRender(rowView(armed: true, switchState: .notATarget, isActive: true))

        let delta = pixelDelta(resting, armed)
        XCTAssertLessThan(delta, unchangedCeiling, """
            arming a NON-target row moved \(delta) of its pixels — the active row has no chip and no live \
            switch, so nothing should arm. Either the wash escaped its `live` guard (a row that cannot be \
            clicked now reads as pressable) or the measurement is picking up render nondeterminism, which \
            would make the arm floor unattributable
            """)
    }

    // MARK: - #959: the blocked row's EMPTY chip slot

    /// Issue #959: a wire-BLOCKED row carries NO chip, so arming one must move nothing.
    ///
    /// This is the exact INVERSION of what this suite asserted before #959 — the chip-isolation lane above
    /// used to run on a blocked row precisely because arming it moved the chip and only the chip, and it
    /// measured 0.0019 (~4× the ceiling asserted here). So this is not a vacuous "nothing happened" claim:
    /// the same predicate, on the same row state, scored decisively ABOVE this ceiling until the chip was
    /// removed, which is what makes the null result evidence.
    ///
    /// What it does and does not prove. It proves the blocked row has no ARMABLE chip; it does not by
    /// itself prove the slot is empty of ink — a chip pinned at one FIXED tint would also not brighten.
    /// What closes that is the pure verdict: `.hidden` maps to `Color.clear` and reaches no tint case at
    /// all, and the verdict is unit-asserted (`AccountSwapTests.testAWireBlockedTargetRendersNoChipAtAll`),
    /// so given the verdict the absence follows by construction rather than by measurement. Nor is a
    /// globally-dead chip an escape: the isolation lane above proves the chip still brightens where it
    /// does exist, so both cannot be green at once.
    ///
    /// The committed `panel-blind-cornered-{light,dark}` goldens RECORD the empty slot but do not GATE it,
    /// and the difference is worth stating rather than blurring: removing both blocked rows' chips scores
    /// 0.000502 against the drift gate's 0.002 ceiling — 4× UNDER it — because two 23×23 px regions in a
    /// 760×922 frame is a smaller change than that gate is tuned to see. The comparison is also env-gated
    /// off in the required `swift` job and runs only in the non-required, `continue-on-error`
    /// `panel-goldens` job. Treat those PNGs as the human-readable record of what shipped, never as the
    /// thing that would catch a regression here.
    func testArmingABlockedRowMovesNothingBecauseItHasNoChip() throws {
        let blocked = StatusPanelFormat.RowSwitchState.blocked(.weeklyExhausted)
        var checked = 0
        for scheme in [ColorScheme.light, .dark] {
            let resting = try stableRender(rowView(armed: false, switchState: blocked, scheme: scheme))
            let armed = try stableRender(rowView(armed: true, switchState: blocked, scheme: scheme))

            let delta = pixelDelta(resting, armed)
            XCTAssertLessThan(delta, unchangedCeiling, """
                arming a BLOCKED row moved \(delta) of its pixels in \(scheme) (ceiling \
                \(unchangedCeiling)) — since #959 a blocked row has no chip to brighten and its wash is \
                held out by the `live` guard, so nothing should arm. Either the chip came back (the \
                affordance and its own negation are interchangeable at rest again) or the wash escaped its \
                guard
                """)
            checked += 1
        }
        XCTAssertEqual(checked, 2, "expected 2 (light + dark) blocked-row arm checks, ran \(checked)")
    }

    /// The risk #959 introduces, measured rather than argued: the ACTIVE row and a BLOCKED row now BOTH
    /// render an empty trailing slot, so the chip axis can no longer distinguish them. They must still be
    /// distinguishable. The row-level encoding has two halves — a FILLED leading dot against a ring (a
    /// SHAPE cue, which is what makes the active row legible under monochrome and colour-vision
    /// deficiency) plus the accent row fill — while the blocked row carries #955's persistent reason line
    /// and `.disabled()` dimming.
    ///
    /// WHAT THIS TEST CAN AND CANNOT SEE, because the failure message has to send the next reader
    /// somewhere true. It measures AGGREGATE separation, and at that scale the accent fill is essentially
    /// the whole signal: `StatusDot` is 8 pt wide inside a 14 pt halo, so at `scale = 2` the dot occupies
    /// at most 28×28 px of a 728×188 frame — ≤ 0.0057, against a 0.20 floor ~35× coarser. The mutation
    /// confirms it from the other side: deleting the accent fill ALONE drops the measurement from 0.9608
    /// to 0.0844. So a dot-only regression cannot move this number, and the message must not send anyone
    /// hunting for one.
    ///
    /// The dot's fill-vs-ring cue has no dedicated pixel assertion anywhere — it is a structural property
    /// of `StatusDot` (`fill(isActive ? .panelAccent : .clear)` over `strokeBorder(lineWidth: isActive ?
    /// 0 : 1.5)`) and is captured, not gated, by the panel goldens. Stated rather than papered over: #959
    /// leans on that cue in its rationale, so the honest record is that this test does not carry it.
    ///
    /// THE SIZE SHORT-CIRCUIT, and why this test asserts two things rather than one. The two rows do not
    /// rasterize to the same height — the blocked one is taller because #955's cue line is a whole extra
    /// line (728×232 against the active row's 728×188). `PanelRaster.diffFraction` returns a flat 1 for
    /// mismatched sizes ("the size itself is asserted separately", per its own doc comment), so a single
    /// `pixelDelta(active, blocked) > armFloor` here would reduce to `1.0 > 0.20` — TRUE for any two
    /// differently-sized rasters, and true even if every other channel had collapsed. That gate would be a
    /// constant wearing a measurement's clothes, so:
    ///
    ///   1. the HEIGHT difference is asserted DIRECTLY, as the real signal it is (the cue line, #959's own
    ///      precondition — if it vanished, the rows would become the same height and this would catch it);
    ///   2. the overlapping region is then compared on its own, so distinctness does not rest on height
    ///      alone. Measured 0.9608 light / 0.9653 dark — the active row's tint fill repaints nearly all of
    ///      it — so the ARM floor is the honest bar: a separation failing to clear it would mean the
    ///      row-level encoding had collapsed, not merely dimmed.
    ///
    /// `AccountSwapTests` carries the enum-level half; this is the pixel half.
    func testTheActiveAndBlockedRowsStayVisiblyDistinctThoughNeitherCarriesAChip() throws {
        var checked = 0
        for scheme in [ColorScheme.light, .dark] {
            let active = try stableRender(rowView(switchState: .notATarget, isActive: true, scheme: scheme))
            let blocked = try stableRender(rowView(switchState: .blocked(.weeklyExhausted), scheme: scheme))

            // 1. The size channel, asserted rather than relied on implicitly.
            XCTAssertEqual(active.width, blocked.width, "the two rows are laid out at the same width")
            XCTAssertGreaterThan(blocked.height, active.height, """
                the BLOCKED row (\(blocked.height)px) is no taller than the ACTIVE one (\(active.height)px) \
                in \(scheme) — #955's persistent reason line is what makes #959 safe to ship, so if it \
                stopped occupying its own line the blocked row lost the explanation that replaced its chip
                """)

            // 2. The overlapping region, so the verdict does not rest on the height difference alone.
            let common = min(active.height, blocked.height)
            let delta = pixelDelta(active.firstRows(common), blocked.firstRows(common))
            XCTAssertGreaterThan(delta, rowIdentityFloor, """
                over their overlapping \(common) rows the ACTIVE and BLOCKED rows differ by only \(delta) \
                in \(scheme) (floor \(rowIdentityFloor)). Neither carries a chip since #959, so if the \
                row-level encoding has collapsed too, the two states are no longer tellable apart. At this \
                scale the channel to check is the ACCENT ROW FILL — the leading dot is ≤0.0057 of the \
                frame and cannot move this number either way. The fix is NOT a new blocked-row marker \
                (#959 rejected that: five of six rows carry a mark, so an added element pays its cost five \
                times)
                """)
            checked += 1
        }
        XCTAssertEqual(checked, 2, "expected 2 (light + dark) active-vs-blocked comparisons, ran \(checked)")
    }

    // MARK: - AC-3: the mis-click guard, at the INTERACTION layer, at NARROW widths specifically

    /// Below the affordance budget the row must publish NO activatable control at all.
    ///
    /// This is the hazard `AccountSwapTests:305` names — a too-narrow row must go non-interactive rather
    /// than degrade into an invisible whole-row hot zone, because the whole row IS the hit rect
    /// (`.contentShape` + Fitts's law) and the chip + wash are what make that rect visible. A row that
    /// kept its `Button` while dropping the chip would be exactly the accidental-credential-swap surface
    /// the arm-on-hover guard exists to prevent — and `rowFitsSwitchAffordance` returning `false` cannot
    /// tell you whether that happened. The tree can.
    ///
    /// Issue #761 explicitly warned against reading its `Button`/`Other` element-role finding as covering
    /// this: that split is the ACTIVE-vs-SWITCHABLE axis, and the spike never measured at narrow widths.
    func testARowNarrowerThanTheBudgetPublishesNoInteractiveControl() {
        let narrow = StatusPanelFormat.switchAffordanceMinRowWidth - 1
        let nodes = PanelA11y.tree(for: rowView(rowWidth: narrow),
                                   size: CGSize(width: narrow, height: 140))

        // The absence trap: "no button in the tree" is evidence ONLY if the tree is populated at all. An
        // activation failure yields an empty tree, which satisfies every absence claim perfectly.
        assertKnownPresent(nodes, "Personal", "the narrow-row mis-click guard")

        // `interactiveNodes` is role-only (`AXButton`), deliberately NOT filtered on `enabled` — the guard
        // is about ABSENCE, not dimming. A present-but-`.disabled()` button would still occupy the whole-row
        // hit rect and still be reachable by keyboard; disabling is the treatment for a non-viable TARGET,
        // which stays visibly a control. A sub-budget row has no room to show it is a control at all, so it
        // must not be one.
        XCTAssertEqual(nodes.interactiveNodes.count, 0, """
            a \(narrow)pt row (budget \(StatusPanelFormat.switchAffordanceMinRowWidth)pt) still publishes \
            \(nodes.interactiveNodes.count) control(s), enabled or not: \
            \(nodes.interactiveNodes.map { "\($0.role)/enabled=\($0.enabled)" }). Below the budget the \
            affordance is not merely hidden — the row must stop being interactive, or the whole-row hit \
            rect survives with nothing visible marking it, which is the accidental-swap hazard the guard \
            exists to prevent
            """)
    }

    /// The other half of the same predicate — WITHOUT this, the test above would pass on a row that never
    /// publishes a button at any width, which is a broken affordance rather than a working guard.
    func testAtAndAboveTheBudgetTheSameRowDoesPublishAButton() {
        var checked = 0
        for width in [StatusPanelFormat.switchAffordanceMinRowWidth, StatusPanelFormat.defaultRowWidth] {
            let nodes = PanelA11y.tree(for: rowView(rowWidth: width),
                                       size: CGSize(width: width, height: 140))
            assertKnownPresent(nodes, "Personal", "the at-budget control at \(width)pt")
            XCTAssertGreaterThan(nodes.interactiveNodes.count, 0, """
                a \(width)pt row (budget \(StatusPanelFormat.switchAffordanceMinRowWidth)pt) publishes NO \
                activatable control — the switch affordance is gone at a width that clears the budget, so \
                the narrow-row guard above is measuring a row that is never interactive anyway
                """)
            checked += 1
        }
        XCTAssertEqual(checked, 2, "expected 2 (at-budget + shipped) width checks, ran \(checked)")
    }

    // MARK: - AC-1 (automated branch): the in-flight window, in-process — no XCUITest target

    /// The sliver issue #761 measured as reachable, reached WITHOUT the UI-test target it priced.
    ///
    /// Three in-flight surfaces are asserted together because they are one guarantee: while a swap is in
    /// flight the operator must be told what is happening AND must not be able to start a second one.
    func testTheInFlightSwapIsAnnouncedAndDisablesEverySwitchTarget() throws {
        let fixture = try XCTUnwrap(PanelA11y.allFixtures.first { $0.name == "healthy" },
                                    "no 'healthy' render fixture")
        // Derive the target from the fixture rather than naming one. The footer announces only its OWN
        // target (`swap.phase.pendingTarget == target`), so a hardcoded label that drifts off the fixture
        // silently degrades this into a test of the resting panel — which is exactly what happened while
        // writing it, with "Personal" against a fixture whose next swap is "Temp".
        guard case .target(let target, _)? = fixture.nextSwap else {
            return XCTFail("the 'healthy' fixture has no next-swap target to drive an in-flight swap with")
        }

        let resting = PanelA11y.panelTree(fixture: fixture)
        assertKnownPresent(resting, "Sessiometer.", "the in-flight control (resting)")

        let inFlight = PanelA11y.panelTree(fixture: fixture,
                                           swapOverride: .pendingPreview(target: target))
        assertKnownPresent(inFlight, "Sessiometer.", "the in-flight control (pending)")

        // 1. The panel SAYS a swap is in flight, and names its target.
        XCTAssertNotNil(inFlight.firstContaining("Switching to \(target)"), """
            no element announces 'Switching to \(target)' while the swap is pending. The in-flight window \
            is the one interaction state issue #761 measured as automatable; if it stops being published \
            the operator is left watching a panel that looks idle mid-credential-swap
            """)

        // 2. No second swap can be started — `.disabled(swap.phase.isPending)` on every sibling row and on
        //    the footer, because the daemon holds a single-writer lock behind the one `swap` verb.
        //
        //    The RESTING count is asserted first and is not decoration: "0 activatable rows mid-swap" is
        //    satisfied just as well by a panel whose rows were never activatable, so without this the
        //    disable claim is vacuous.
        let restingRows = resting.interactiveNodes.filter { $0.enabled && $0.text.contains("session") }
        XCTAssertEqual(restingRows.count, 2, """
            the resting panel publishes \(restingRows.count) activatable roster row(s), expected 2 \
            (Personal + Temp; the active Work row is `.notATarget`). The mid-swap assertion below is \
            vacuous unless these rows are activatable to begin with
            """)
        let inFlightRows = inFlight.interactiveNodes.filter { $0.enabled && $0.text.contains("session") }
        XCTAssertEqual(inFlightRows.count, 0, """
            \(inFlightRows.count) roster row(s) are still activatable mid-swap: \
            \(inFlightRows.map(\.text)). A second swap must not be reachable while one is in flight
            """)

        // 3. The tree genuinely CHANGED SHAPE — the footer's `Button` becomes an `AXBusyIndicator`. This is
        //    the discriminator that caught the wrong-target bug above (a phase that never reaches the panel
        //    leaves the histogram identical), so it stays even though 1 and 2 now pass.
        XCTAssertNotEqual(resting.roleHistogram, inFlight.roleHistogram, """
            the in-flight tree has the same shape as the resting one (\(inFlight.roleHistogram)) — the \
            pending phase did not reach the panel, so both assertions above are about the resting panel
            """)
    }

    /// The in-flight state is also a VISUAL change (the chip is replaced by a spinner), so the same
    /// render lane that covers arming covers it — no golden, same relational comparison.
    func testTheInFlightRowRendersDifferentlyFromTheRestingRow() throws {
        let resting = try stableRender(rowView(armed: false))
        let pending = try stableRender(rowView(armed: false, swap: .pendingPreview(target: "Personal")))

        let delta = pixelDelta(resting, pending)
        XCTAssertGreaterThan(delta, inFlightFloor, """
            a row with its own swap in flight renders identically to a resting one (\(delta), floor \
            \(inFlightFloor)) — the `Switching…` spinner never replaced the chip, so the panel shows no \
            sign a credential swap is under way
            """)

        // The same canary discipline the arm floor carries: two RESTING renders must score under the
        // floor, or the floor is reachable by noise and proves nothing about the spinner.
        let restingAgain = try stableRender(rowView(armed: false))
        let canary = pixelDelta(resting, restingAgain)
        XCTAssertLessThan(canary, inFlightFloor, """
            two RESTING renders differ by \(canary), reaching the in-flight floor \(inFlightFloor) — a \
            removed spinner would still pass, so the gate cannot fail and is not evidence
            """)
    }
}
#endif
