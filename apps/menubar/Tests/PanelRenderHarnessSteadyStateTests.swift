// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The gate on `PanelRenderHarness`'s steady state (issue #824) — that a returned raster is one the
// rasterizer has been CONFIRMED to reproduce, not the first one it happened to hand back.
//
// WHAT WENT WRONG, and why it needs a gate of its own. The harness used to discard-render the `healthy`
// fixture until two consecutive rasters agreed byte-for-byte, and call that a process-wide steady state.
// Two things were wrong with it at once, and each hides the other:
//
//   1. It warmed ONE fixture, so the first renders of any other content were still inside the transient.
//   2. Its stopping rule could not see its own transient. The early rasters agree WITH EACH OTHER — the
//      harness's own doc recorded exactly that shape — so "two consecutive agree" returned on the cold
//      pair.
//
// MEASURED, first renders in a fresh process, one fixture at a time: `blind-cornered/dark` agreed with
// itself for two renders and then moved 1075 bytes at worst channel 1; `stats/dark` next agreed with itself
// for two renders and then moved 10; `stale/dark` after those did not move at all. That reads like a
// start-up effect that decays, and the decay is real — it is why the old gap was invisible in a full-suite
// run and reproducible in isolation. It is not the whole story: surveyed by putting every render of the
// whole suite into measurement mode, plateaus of 2 also land at rasterizer passes #48, #648, #656 and
// #1872, the last on a cell already rendered many times. A transient can appear at any point, on content
// already seen, so no amount of warming at start-up covers it.
//
// WHY THE OBVIOUS TEST DOES NOT WORK, stated because it is the trap this suite exists to avoid. "Render
// unseen content and check the first raster is steady" only distinguishes the two implementations while the
// process is still inside its transient. Past it, EVERY raster is steady under the settled implementation
// and the un-settled one alike, so such a test passes on the defect whenever it does not happen to run
// first — measuring luck, not the warm-up, which is the very failure mode issue #824 describes. So the two
// gaps are gated by two order-INDEPENDENT claims instead:
//
//   • the stopping rule looks past a transient that agrees with itself → driven directly, with the measured
//     sequence, by the canaries below;
//   • every render is settled rather than taken on trust → asserted on `rasterPasses`, because after the
//     transient the pass COUNT is the only thing that still separates the two implementations.
//
// `testAFirstRenderOfUnwarmedContentIsAlreadyItsSteadyState` states the end-to-end property anyway. It is a
// companion, not the gate: it is sensitive only when it runs early, and its verdict must not be read as
// covering the case where it does not.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

@MainActor
final class PanelRenderHarnessSteadyStateTests: XCTestCase {

    // MARK: - Scripted rasters

    // Two byte buffers that are simply unequal. The stopping rule only ever asks "are these two rasters
    // identical?", so a one-byte stand-in exercises it exactly as a 2 729 920-byte one would — and keeps the
    // scripted sequences readable as the SHAPES they are copied from.
    private let cold: [UInt8] = [0x2a]
    private let warm: [UInt8] = [0x2b]

    /// Hand `settled` a fixed sequence of rasters, labelled `r0`, `r1`, … so an assertion can name exactly
    /// WHICH raster came back. Returns nil once the script is exhausted, which is also how the
    /// producer-failure path is driven.
    private func scripted(_ frames: [[UInt8]]) -> () -> (raster: String, bytes: [UInt8])? {
        var index = 0
        return {
            guard index < frames.count else { return nil }
            defer { index += 1 }
            return (raster: "r\(index)", bytes: frames[index])
        }
    }

    // MARK: - CANARY: the stopping rule looks PAST a transient that agrees with itself

    /// THE mutation gate for issue #824. Restore the old rule and this reddens.
    ///
    /// The script is the measured `blind-cornered/dark` shape: two rasters that agree with each other, then
    /// the steady state. The whole defect is that those first two agree, so any rule satisfied by one
    /// agreement returns a cold raster — and the assertion below names the raster that comes back, which is
    /// what makes "did not return a cold one" a measurement rather than a description.
    ///
    /// What this test does NOT rule out on its own, because reading it as a fixed-count gate would be
    /// reading it wrong: `r4` is also what "always render 5 times and take the last" would return here. What
    /// it pins is that the selection follows the YARDSTICK — the contrast at `pastTransientRun: 1` below
    /// returns `r1` from the same script. Ruling out a tuned count takes the PAIR: this returns `r4` for a
    /// transient of 2 and `testADeeperTransientDemandsALongerConfirmation` returns `r8` for a transient of 4,
    /// and no fixed count satisfies both.
    func testTheStoppingRuleLooksPastATransientThatAgreesWithItself() {
        let frames = [cold, cold, warm, warm, warm, warm]

        // Yardstick 2 — a transient plateau of two rasters, which is what this machine measures. The run of
        // agreeing rasters must exceed it, so the first raster that qualifies is r4 (r2·r3·r4 = a run of 3).
        let settled = PanelRenderHarness.settled(pastTransientRun: 2, budget: 8, scripted(frames))
        XCTAssertTrue(settled.didSettle, "the stopping rule did not fire on a sequence that does settle")
        XCTAssertEqual(settled.raster, "r4",
                       "the settle returned \(settled.raster ?? "nil"): a raster inside the cold group (r0/r1) "
                       + "means the rule stopped at an agreement the transient itself produces — the issue "
                       + "#824 defect — and a later one means the rule is demanding more than it measured")

        // The OLD rule is this same loop with the yardstick pinned to 1 ("two consecutive rasters agree").
        // Asserted, not described: it is what makes the line above a MEASUREMENT of the fix rather than a
        // restatement of it, and it pins the defect to the yardstick rather than to the loop.
        let asTheOldRule = PanelRenderHarness.settled(pastTransientRun: 1, budget: 8, scripted(frames))
        XCTAssertEqual(asTheOldRule.raster, "r1",
                       "the pre-#824 rule is supposed to return a COLD raster here — if it no longer does, "
                       + "this canary has stopped reproducing the defect and proves nothing about the fix")
    }

    /// The yardstick is a MEASUREMENT, so a machine whose transient runs deeper simply gets more demanded of
    /// it. Same loop, same code path, a transient of four instead of two — nothing is tuned to two.
    func testADeeperTransientDemandsALongerConfirmation() {
        let frames = [cold, cold, cold, cold, warm, warm, warm, warm, warm, warm]

        let measured = PanelRenderHarness.settled(pastTransientRun: .max, budget: 10, scripted(frames))
        XCTAssertEqual(measured.longestEndedRun, 4, "the four-raster transient was not measured as four")

        let settled = PanelRenderHarness.settled(pastTransientRun: measured.longestEndedRun, budget: 10,
                                                 scripted(frames))
        XCTAssertTrue(settled.didSettle)
        XCTAssertEqual(settled.raster, "r8",
                       "a transient of 4 must demand a run of 5 — r8 is the first raster that has one. "
                       + "Returning earlier means the requirement did not follow the measurement")
    }

    /// The stopping rule's blind spot, pinned so it cannot be mistaken for an oversight and "fixed".
    ///
    /// A settle returns the moment a run EXCEEDS its yardstick, so the deepest plateau it can watch END is
    /// the yardstick itself: it can never discover that its own yardstick is too low. Only measurement mode
    /// can see that, which is why `calibrate` is the sole path allowed to raise `longestTransientRun` and
    /// why `render` deliberately folds nothing back — see that property's "DO NOT ADD A PER-RENDER FOLD".
    ///
    /// The same script also shows the residual limit stated there, which is the honest cost of the design: a
    /// transient DEEPER than the yardstick comes back cold, reported as settled, because the run that
    /// satisfied the rule was still inside it. The floor under the yardstick is what keeps that rare.
    func testASettleCanNeverWatchAPlateauDeeperThanItsOwnYardstick() {
        // A five-raster transient against a yardstick of two — deeper than the harness would ever expect.
        let frames = [cold, cold, cold, cold, cold, warm, warm, warm, warm, warm, warm]

        let stopping = PanelRenderHarness.settled(pastTransientRun: 2, budget: 16, scripted(frames))
        XCTAssertLessThanOrEqual(stopping.longestEndedRun, 2,
                                 "a settle reported watching a plateau of \(stopping.longestEndedRun) end "
                                 + "against a yardstick of 2 — it cannot, and a fold of that value into the "
                                 + "yardstick would be the runaway `longestTransientRun` warns about")

        // The residual limit, asserted rather than described: cold raster, `didSettle == true`.
        XCTAssertTrue(stopping.didSettle)
        XCTAssertEqual(stopping.raster, "r2",
                       "the rule is supposed to fire INSIDE a plateau deeper than its yardstick — if this "
                       + "raster is no longer a cold one, the limit documented on `longestTransientRun` has "
                       + "changed and that doc is now wrong")

        // Measurement mode, same script: the plateau the settle could not see is exactly what this reports.
        let measuring = PanelRenderHarness.settled(pastTransientRun: .max, budget: 16, scripted(frames))
        XCTAssertEqual(measuring.longestEndedRun, 5,
                       "measurement mode is the ONLY thing that can observe a plateau deeper than the "
                       + "yardstick, and it did not observe this one")
    }

    // MARK: - CANARY: the calibration MEASURES rather than assumes

    /// `.max` makes the stopping rule unreachable, which is how `calibrateIfNeeded` turns the loop into a
    /// pure measurement: it spends its budget in full and reports the longest plateau that ENDED. Asserted
    /// because an early exit hidden in that path would put a second stopping rule back into the design —
    /// the very thing that went wrong the first time.
    func testTheCalibrationSpendsItsBudgetAndReportsTheTransientItWatchedEnd() {
        let measuring = PanelRenderHarness.settled(pastTransientRun: .max, budget: 6,
                                                   scripted([cold, cold, warm, warm, warm, warm]))
        XCTAssertEqual(measuring.passes, 6, "measurement mode must spend its whole budget")
        XCTAssertFalse(measuring.didSettle, "the stopping rule must be unreachable at .max")
        XCTAssertEqual(measuring.longestEndedRun, 2, "the two-raster transient was not measured as two")

        // The plateau still running when the budget ends is the STEADY one, and counting it would ratchet
        // the requirement up on every pass until nothing could ever satisfy it.
        let noTransient = PanelRenderHarness.settled(pastTransientRun: .max, budget: 6,
                                                     scripted([warm, warm, warm, warm, warm, warm]))
        XCTAssertEqual(noTransient.longestEndedRun, 0,
                       "a sequence with no transient reported one — the final, un-ended plateau is being "
                       + "counted, which would make the requirement grow without bound")
    }

    /// The measurement is only worth taking if it reaches the yardstick — this gates the WIRING between them.
    ///
    /// The test above proves measurement mode reports the right number; this proves the number lands. They
    /// are separate because the calibration is a once-per-process side effect on a `private(set)` static, and
    /// a fold that silently stopped folding would leave every other assertion in this suite green: the floor
    /// alone still satisfies `testTheYardstickIsFlooredAtTheDeepestTransientEverMeasured`, and every scripted
    /// canary passes its yardstick in explicitly.
    ///
    /// Driven through `calibrate`'s injected producer against LOCAL storage, so what is asserted is the
    /// scripted number rather than anything this machine measured — `longestTransientRun` itself belongs to
    /// the machine and no test may pin it.
    func testTheCalibrationRaisesTheYardstickItIsGiven() {
        // A four-raster transient, measured against the harness's own starting floor.
        let frames = [cold, cold, cold, cold, warm, warm, warm, warm]

        var yardstick = 2
        let measured = PanelRenderHarness.calibrate(into: &yardstick, budget: 12, scripted(frames))
        XCTAssertEqual(measured.longestEndedRun, 4)
        XCTAssertEqual(yardstick, 4,
                       "the calibration measured a transient of 4 and the yardstick stayed at \(yardstick) — "
                       + "the measurement is not reaching the value the stopping rule reads, so a machine "
                       + "with a deeper transient would keep the floor's estimate (issue #824)")

        // It may only ever RAISE. A calibration that lands on a partially-warm process measures shallower
        // than the floor, and assigning that would restore the pre-#824 rule wholesale.
        var alreadyDeeper = 6
        PanelRenderHarness.calibrate(into: &alreadyDeeper, budget: 12, scripted(frames))
        XCTAssertEqual(alreadyDeeper, 6, "a shallower measurement LOWERED the yardstick")

        // And a process that shows no transient at all leaves it exactly where it started.
        var untouched = 2
        PanelRenderHarness.calibrate(into: &untouched, budget: 6,
                                     scripted([warm, warm, warm, warm, warm, warm]))
        XCTAssertEqual(untouched, 2)
    }

    // MARK: - CANARY: the budget is a valve, and says so

    /// Exhausting the budget hands back the last raster with `didSettle == false` rather than trapping — the
    /// harness compiles into the shipping app, so a render path that traps is not an option. What must NOT
    /// happen is the pretence: an unsettled raster reported as settled.
    func testTheBudgetValveReportsRatherThanPretending() {
        let neverAgrees = PanelRenderHarness.settled(pastTransientRun: 2, budget: 4,
                                                     scripted([[0], [1], [2], [3]]))
        XCTAssertFalse(neverAgrees.didSettle, "a sequence that never repeats was reported as settled")
        XCTAssertEqual(neverAgrees.passes, 4, "the budget valve did not bound the loop")
        XCTAssertEqual(neverAgrees.raster, "r3", "the valve must hand back the last raster it pulled")

        // A producer that fails on its first call has nothing to hand back, and must not claim otherwise.
        let failsImmediately = PanelRenderHarness.settled(pastTransientRun: 2, budget: 4, scripted([]))
        XCTAssertNil(failsImmediately.raster)
        XCTAssertFalse(failsImmediately.didSettle)
        XCTAssertEqual(failsImmediately.passes, 0)
    }

    // MARK: - CANARY: every render is SETTLED, not taken on trust

    /// The order-independent gate on the FIRST gap (the old warm-up warmed one fixture and left every other
    /// render un-settled).
    ///
    /// It counts `ImageRenderer` passes rather than comparing rasters, because once the process is past its
    /// transient the two implementations return the SAME raster and only the pass count still tells them
    /// apart. The old harness rasterized exactly once per `render` after its one-off warm-up; a settled one
    /// cannot, because confirming a run longer than the measured transient takes at least that many passes.
    /// This is why the assertion is `>= longestTransientRun + 1` and not a literal: the floor is whatever
    /// this machine measured.
    func testEveryRenderIsSettledRatherThanTakenOnTrust() throws {
        let fixture = try XCTUnwrap(PanelRenderHarness.fixtures(now: Int64(Date().timeIntervalSince1970))
            .first(where: { $0.name == "healthy" }))
        // Discharge the once-per-process calibration first, so what is counted below is one render's own
        // passes rather than the calibration's budget riding along with it.
        _ = PanelRenderHarness.render(fixture, scheme: .light)

        let before = PanelRenderHarness.rasterPasses
        _ = PanelRenderHarness.render(fixture, scheme: .light)
        let passes = PanelRenderHarness.rasterPasses - before

        let yardstick = PanelRenderHarness.longestTransientRun
        print("[panel-steady] this machine measured a transient run of \(yardstick); "
              + "one render took \(passes) rasterizer passes")
        XCTAssertGreaterThan(passes, 1,
                             "`render` rasterized \(passes) time(s) and returned the result — that is the "
                             + "pre-#824 behaviour, in which nothing confirms the raster is reproducible")
        XCTAssertGreaterThanOrEqual(passes, yardstick + 1,
                                    "a run longer than the measured transient (\(yardstick)) needs at least "
                                    + "\(yardstick + 1) passes to observe, and \(passes) were run — the "
                                    + "stopping rule is not being applied to this render")
        XCTAssertLessThanOrEqual(passes, PanelRenderHarness.settleBudget,
                                 "a render exceeded the budget valve, which the loop must bound it by")
        XCTAssertEqual(PanelRenderHarness.unsettledRenders, 0,
                       "\(PanelRenderHarness.unsettledRenders) render(s) in this process came back through "
                       + "the budget valve rather than the stopping rule, so their rasters were never "
                       + "confirmed reproducible — on the `--render-panel` path those become committed PNGs")
    }

    // MARK: - CANARY: the yardstick has a measured floor under it

    /// The yardstick is measured at runtime, but it may not start from nothing.
    ///
    /// A calibration that lands on a PARTIALLY-warm process measures a shallower transient than one that
    /// lands on a cold one — in a whole-suite run it reports 1, because other suites rasterize before this
    /// one is first asked for a panel. A yardstick of 1 is the pre-#824 rule exactly, and the transient does
    /// not politely confine itself to start-up: surveyed across a full suite in measurement mode, plateaus
    /// of 2 appear at rasterizer passes #48, #648, #656 and #1872, the last on a cell rendered many times
    /// already. So the harness floors its estimate at the deepest plateau ever measured for this issue
    /// rather than at "no transient at all".
    ///
    /// Asserted as a FLOOR and never as a value: the measurement on top of it belongs to the machine, and
    /// pinning it would make this suite fail on a slower one for being right.
    func testTheYardstickIsFlooredAtTheDeepestTransientEverMeasured() {
        XCTAssertGreaterThanOrEqual(PanelRenderHarness.longestTransientRun, 2,
                                    "the yardstick is \(PanelRenderHarness.longestTransientRun) — at 1 the "
                                    + "stopping rule is 'two consecutive rasters agree', which is the rule "
                                    + "issue #824 replaced, and a plateau of 2 walks straight through it")
    }

    // MARK: - End-to-end (companion, NOT the gate — see the header)

    /// The property issue #824 asks for, stated directly: the raster `render` hands back for content the
    /// harness never warmed is already the one every later render reproduces.
    ///
    /// Sensitive only while the process is inside its transient, so it is deliberately NOT relied on as the
    /// mutation gate — see the header. `blind-cornered/dark` and `stats/dark` are the two cells whose
    /// transients were measured for this issue (1075 and 10 bytes at worst channel 1); neither is the
    /// `healthy`/`.light` cell the calibration renders.
    func testAFirstRenderOfUnwarmedContentIsAlreadyItsSteadyState() throws {
        for name in ["blind-cornered", "stats"] {
            let fixture = try XCTUnwrap(PanelRenderHarness.fixtures(now: Int64(Date().timeIntervalSince1970))
                .first(where: { $0.name == name }))
            let first = try XCTUnwrap(PanelRenderHarness.render(fixture, scheme: .dark)
                .flatMap(PanelRaster.normalize))
            for pass in 1...4 {
                let again = try XCTUnwrap(PanelRenderHarness.render(fixture, scheme: .dark)
                    .flatMap(PanelRaster.normalize))
                let (differing, worst) = PanelRaster.byteDelta(first, again)
                XCTAssertEqual(worst, 0,
                               "\(name)/dark: the raster `render` returned differs from render \(pass) after "
                               + "it by \(worst)/255 on \(differing) bytes — the first one was not the steady "
                               + "state, so a golden blessed from it would bake cold pixels (issue #824)")
            }
        }
    }
}
#endif
