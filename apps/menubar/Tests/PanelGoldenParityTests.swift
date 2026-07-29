// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Panel golden-render DRIFT gate (issue #754) — the automated visual guard the panel had none of.
//
// WHAT WAS MISSING. `design/build-comparison.py` slices the mock's `.pop` blocks LIVE at comparison time,
// so editing `menubar-preview.html` silently RE-BASELINED the only comparison that existed — no committed
// artifact whose change would show in a diff — and nothing invoked it from CI. The panel's visual state was
// defended by nothing automated. The bar GLYPH had `BarGlyphParityTests` (issue #525); the panel did not,
// because a false belief said its `ImageRenderer` needed a windowserver. Issue #749 (PR #771) measured that
// belief and retired it: SwiftUI rasterizes inside THIS `TEST_HOST: ""` bundle on the `macos-26` runner.
// This suite is what that unblocked.
//
// SCOPE — a DRIFT gate, NOT a design-FIDELITY oracle. A golden certifies "unchanged since the last blessed
// render". It cannot certify "matches the mock": the mock is the ratified build reference, and the built
// panel intentionally differs from it on documented axes (no provider line, #173; the #448 switch chip; the
// `Temp` fixture, #709 — see design/README.md § Expected reconciliations). Those reconciliations are baked
// INTO the goldens on purpose. Reading a green here as "the panel matches the design" is the exact
// misreading `BarGlyphParityTests` warns about for artwork; `build-comparison.py` + a human eye remain the
// fidelity path.
//
// THE BASELINE TRAP (issue #437, at cost of a near brand re-ratification). A golden gate blesses whatever
// the renderer produced on first run — if the renderer is broken, the broken output becomes the reference
// and the gate then DEFENDS the bug, reporting green. #437's three render bugs drew all four bar glyphs as
// one identical white blob and were misread FIVE times as "the DESIGN fails distinctness"; a golden
// authored then would have defended them. Two consequences, both load-bearing here:
//   1. The rig is built to PROVE it can fail — every gate predicate has a MUTATION-driven canary below,
//      never an inspection-only argument.
//   2. Two renderer defects that would silently poison the references were hunted BEFORE blessing anything:
//      • asset colours (`HealthOK` / `UtilGreen` / `UtilAmber` / `UtilOrange` / `UtilRed`) resolved through
//        `Color(name, bundle: .main)`, and in this bundle `Bundle.main` is the `xctest` runner, which
//        carries no `Assets.car` — so every health tint would have failed to resolve;
//      • `Color.accentColor` resolves via `ASSETCATALOG_COMPILER_GLOBAL_ACCENT_COLOR_NAME`, set on the APP
//        target only, so here it would have fallen back to the OPERATOR'S macOS system accent — a
//        machine-dependent hue.
//      Both are fixed at the source (`Color.panelAssets` / `Color.panelAccent`, plus the harness's explicit
//      `.tint`), and `testTheHealthTintAssetsResolveInThisBundle` is the loud guard that the first one
//      actually happened — the direct analogue of `BarGlyphParityTests`'
//      `testTheRealBespokeSymbolsResolveInThisBundle`.
//
// WHAT IS REQUIRED VS SOFT, and why the split is exactly there. AC "gate lands non-required, promoted after
// N green runs" targets RISK-2: a flaky required check trains merging past red, degrading the gates that
// currently work. Only ONE class of assertion here is cross-machine sensitive — the comparison against
// COMMITTED goldens, which were rasterized on one machine and are re-rendered on an unpinned
// `macos-latest`. Everything else is a SAME-RUN comparison and therefore cross-machine immune by
// construction (the same reasoning `BarGlyphParityTests` uses for its distinctness floor). So:
//   • ALWAYS ON (inside the required `swift` job) — renders succeed, carry ink, are deterministic, ignore
//     the host process's appearance, stay pairwise distinct, and every canary trips its predicate. These
//     prove the RIG.
//   • SOFT, env-gated on `SESSIOMETER_PANEL_GOLDEN_GATE=1` (run only by the non-required `panel-goldens`
//     CI job) — `testEveryRenderMatchesItsCommittedGolden` and
//     `testEachFreshRenderIsNearestToItsOwnGolden`. These are the committed-reference comparisons.
// Promotion criterion (N = 10 consecutive green `panel-goldens` runs on `main`) is mirrored in
// design/README.md § Panel golden drift gate; the promotion DECISION — the tally, the re-calibration
// question it must answer about `driftCeiling` first, and the two-line mechanics — is recorded in issue
// #790.
//
// THE METRIC + THRESHOLDS. `PanelRaster.diffFraction` counts pixels whose largest RGBA channel delta
// exceeds 64/255 — deliberately the same shape and the same 0.25-of-full-scale threshold as
// `BarGlyphRenderer.diffFraction`, so the two gates' numbers are comparable, but over a raw normalized
// buffer rather than `NSBitmapImageRep.colorAt` (a panel raster is ~600×740 = 440 k px and the
// nearest-golden sweep is quadratic; `colorAt` per pixel made that minutes instead of seconds).
//
// Thresholds are calibrated to MEASURED separations on THIS content, not guessed. Re-derive every number
// with the commands recorded in design/README.md § Panel golden drift gate; these are what those runs
// printed on arm64 / macOS 26.5.2 / Xcode 26.6, over the full 34-cell catalog (17 fixtures × 2 themes).
// The provenance column matters — two rows do NOT come from `testMeasureSeparations`, so re-deriving the
// whole table means running the default suite as well as the measurement test:
//
//   identical re-render .............................................. 0.000000  ← testMeasureSeparations
//   re-render seeded 1 / 7 / 29 s earlier (clock-drift window) ....... 0.000000  ← testMeasureSeparations
//   golden PNG round-trip (write → read → compare) .................... 0.000000  ← testMeasureSeparations
//   same fixture under aqua vs darkAqua host appearance .............. 0.000000  ← testRendersDoNotDepend…
//                                                                                 (no host Dark-Mode dep)
//   app `--render-panel` output vs in-bundle goldens, all 34 cells .... 0.000000  ← out-of-band, recipe in
//                                                                                 design/README.md; also
//                                                                                 BYTE-identical, 34/34
//   closest distinct same-size pair .................................. 0.002513  healthy/dark vs stale/dark
//   … #2 .............................................................. 0.002621  healthy/light vs stale/light
//   … #3 .............................................................. 0.003827  fault-scrub-exhausted/dark
//                                                                                vs fault-scrub-recovering/dark
//   farthest same-size pair .......................................... 0.992180  connecting/light vs …/dark
//   median same-size pair ............................................ 0.971210  ← quoted to be dismissed
//   perturbation canary, 0.5 / 1.0 / 1.5 / 3.0 % of frame ............ 0.004454 / 0.010022 / 0.014477 /
//                                                                      0.030067
//
// Every 0.000000 above is also 0 at the BYTE level, which is a stronger claim than the metric can make and
// is asserted separately (the metric ignores channel deltas under 64/255, so it would report 0.000000 for a
// raster off by ±1 everywhere). That byte-exactness is not free: the first renders in a process disagree
// with the steady state by ±1/255 on ~0.03 % of bytes — a rasterization warm-up artifact, measured by
// rendering one fixture six times (renders 0–1 agree, renders 2–5 agree, the groups differ) and ruled
// clock-independent by the seed-lag rows above. `PanelRenderHarness` discards renders until two consecutive
// ones agree, so both the app tool and this gate rasterize from the steady state; the byte assertions here
// are what keep that honest. HONEST LIMIT, measured for issue #821: that warm-up renders the `healthy`
// fixture only, and its "two consecutive agree" rule cannot detect a cold group that agrees with ITSELF —
// rendering `blind-cornered/dark` ten times as a process's first renders gives frames 0–1 identical and
// frames 2+ differing from them by 1/255 on 728 bytes, at an IDENTICAL seed. So the steady state is reached
// in practice by the volume of renders this suite does, not guaranteed by the warm-up, and a byte assertion
// can still redden on a run that rasterizes a cell for the first time. Tracked as issue #824; do NOT
// absorb it into a tolerance.
//
// The median is quoted only to be dismissed: of the 57 same-size pairs, 37 are cross-theme (light vs dark,
// ~0.97), which drags the median to 0.971210 and so says nothing whatever about the gate's real margin. The
// 20 same-theme pairs are the informative ones, and the CLOSEST of those is what the thresholds must
// respect. They are narrow by nature — `healthy` vs `stale` differ only by a banner and a footer string.
//
// From those:
//   • `distinctnessFloor` = 0.0005 — 5× under the 0.002513 closest REAL pair, so no genuinely-distinct
//     pair is ever flagged identical, yet a collapse (which is EXACTLY 0.000000 — the SAME image, a
//     pipeline smell, never a design finding) still reddens. Same-run → cross-machine immune.
//   • `driftCeiling` = 0.002 — the COARSE tripwire for the absolute committed-golden comparison, NOT the
//     primary gate. Set to the tightest value that is still MEANINGFUL: just under the 0.002513 closest
//     real pair, because a ceiling ABOVE that could not tell two real panel states apart. The
//     same-machine noise floor is exactly 0.000000, so there is no lower pressure on it at all.
//     HONEST LIMIT: cross-machine antialiasing drift is NOT measurable from one machine, so this ceiling
//     is UNVALIDATED against the unpinned `macos-latest` runner — which is precisely what the
//     non-required landing exists to measure. The soft gate prints the MAX observed drift on every run
//     (see `testEveryRenderMatchesItsCommittedGolden`), so the promotion decision has data rather than a
//     guess. If runner AA turns out to exceed 0.002, the right response is recorded with the promotion
//     criterion in design/README.md — NOT a silent bump here.
//   • The PRIMARY gate is relative: `testEachFreshRenderIsNearestToItsOwnGolden`. A fresh render's nearest
//     same-size golden must be ITSELF, which is immune to any uniform cross-machine shift (it nudges every
//     comparison equally and cannot flip the winner) and catches the honesty-critical drift — one state's
//     panel morphing into another state's appearance. This is why AC3 asks for a relative primary check:
//     it needs no cross-machine calibration at all, where the absolute ceiling above does.
//
// WHY THE CLOCK-DRIFT ROW IS EXACT, not merely small. The panel's clock is `TimelineView`'s `context.date`
// (issue #326), which no seam can pin, so a render lands a moment AFTER its fixture was seeded and every
// countdown / age it prints is computed from the later instant. `PanelRenderHarness.boundaryGuardSecs`
// offsets every clock-relative fixture instant 30 s PAST a `humanizeUntil` unit boundary, so a render that
// lands anywhere in the next 30 s formats to the identical string — and identical strings rasterize to
// identical bytes, hence 0.000000 rather than "close". `testRendersSurviveTheClockDriftWindow` drives that
// by MUTATION (re-seeding as if the fixtures were built 1 / 7 / 29 s ago, which is the real-world drift
// direction) instead of asserting it.

#if DEBUG
import AppKit
import SwiftUI
import XCTest

@MainActor
final class PanelGoldenParityTests: XCTestCase {

    // MARK: - Calibrated thresholds (MEASURED — see the header)

    /// Catches a collapse to the SAME image (which scores EXACTLY 0.000000), set 5× below the MEASURED
    /// closest distinct same-size pair (0.002513, healthy/dark vs stale/dark) so no genuinely-different
    /// pair is ever flagged identical. A same-run comparison → cross-machine immune.
    private let distinctnessFloor = 0.0005

    /// The COARSE tripwire for the ABSOLUTE committed-golden comparison — NOT the primary drift gate (that
    /// is the relative `testEachFreshRenderIsNearestToItsOwnGolden`). Set just under the MEASURED closest
    /// real pair (0.002513): a ceiling above that could not tell two real panel states apart, and the
    /// same-machine noise floor is exactly 0.000000 so nothing pushes it up. UNVALIDATED against
    /// cross-machine antialiasing — unmeasurable from one machine, and the reason this comparison lands
    /// non-required. The 0.015-area canary measures 0.014477, ~7× this ceiling, so it stays reachable even
    /// if a promotion re-calibration raises the ceiling substantially. See the header.
    private let driftCeiling = 0.002

    /// The fraction of the frame the perturbation canary flips. MEASURED: 0.5 % → 0.004454, 1.0 % →
    /// 0.010022, 1.5 % → 0.014477, 3.0 % → 0.030067 — so the metric tracks the perturbed area almost
    /// linearly, which is itself evidence the metric is behaving. 1.5 % is kept: small enough that the
    /// canary is a real test of sensitivity, large enough to stay above the ceiling if a promotion
    /// re-calibration raises it. Raise the ceiling past 0.0144 and the canary assertion reddens — which is
    /// the intended loud failure, not an accident.
    private let canaryAreaFraction = 0.015

    // MARK: - Environment switches

    /// Regenerate the committed goldens (`SESSIOMETER_PANEL_GOLDENS=update`). Deliberately an explicit,
    /// named opt-in rather than an auto-bless-on-missing: the whole defect this issue fixes is a baseline
    /// that could move as a SIDE EFFECT. Re-baselining is a decision, so it needs a command — and a
    /// `Panel-Goldens-Rebaselined:` commit trailer, which `scripts/check-panel-golden-rebaseline.sh`
    /// enforces in CI.
    private var isUpdatingGoldens: Bool {
        ProcessInfo.processInfo.environment["SESSIOMETER_PANEL_GOLDENS"] == "update"
    }

    /// Run the committed-golden comparisons (`SESSIOMETER_PANEL_GOLDEN_GATE=1`). Off by default so the
    /// cross-machine-sensitive half of this suite lands NON-REQUIRED — see the header.
    private var isGoldenGateEnabled: Bool {
        ProcessInfo.processInfo.environment["SESSIOMETER_PANEL_GOLDEN_GATE"] == "1"
    }

    /// Print the calibration matrix (`SESSIOMETER_PANEL_MEASURE=1`) — the command that re-derives every
    /// number in this file's header.
    private var isMeasuring: Bool {
        ProcessInfo.processInfo.environment["SESSIOMETER_PANEL_MEASURE"] == "1"
    }

    // MARK: - Fixture / render plumbing

    /// One rendered cell: a fixture in a theme.
    private struct Cell {
        let fixture: String
        let scheme: ColorScheme
        var name: String { PanelRenderHarness.fileName(fixture: fixture, scheme: scheme) }
    }

    /// Every (fixture × theme) cell this suite covers, in catalog order. Fixture NAMES do not depend on the
    /// clock, so the seed used to enumerate them is immaterial.
    private func cells() -> [Cell] {
        PanelRenderHarness.fixtures(now: Self.wallClock()).flatMap { fixture in
            PanelRenderHarness.themes.map { Cell(fixture: fixture.name, scheme: $0) }
        }
    }

    private static func wallClock() -> Int64 { Int64(Date().timeIntervalSince1970) }

    /// Render cache — 34 `ImageRenderer` passes are the expensive part of this suite and several tests need
    /// the same set. Keyed by filename; only ever holds un-lagged renders (`seedLag == 0`).
    private static var cache: [String: PanelRaster] = [:]

    /// Rasterize one cell.
    ///
    /// `seedLag` is the ONLY clock lever, and it is why this does NOT freeze one seed for the whole class.
    /// The fixtures are seeded from the wall clock read HERE, immediately before rasterizing, so the gap
    /// between seeding and `TimelineView`'s own `context.date` is always sub-second — far inside
    /// `PanelRenderHarness.boundaryGuardSecs`. A single class-wide seed would instead let that gap grow with
    /// the suite's own runtime (34 renders take seconds), so a later test would rasterize different
    /// clock-relative text than an earlier one and the whole comparison basis would rot mid-run. Re-reading
    /// the clock per render makes every render in the suite — and every render on any future day, against
    /// the committed goldens — carry identical clock-relative strings.
    ///
    /// `seedLag > 0` deliberately re-creates that gap (as if the fixtures had been built `seedLag` seconds
    /// ago), which is what `testRendersSurviveTheClockDriftWindow` mutates.
    private func render(_ cell: Cell, seedLag: Int64 = 0, cached: Bool = true,
                        file: StaticString = #filePath, line: UInt = #line) -> PanelRaster? {
        if cached, seedLag == 0, let hit = Self.cache[cell.name] { return hit }
        let now = Self.wallClock() - seedLag
        guard let fixture = PanelRenderHarness.fixtures(now: now).first(where: { $0.name == cell.fixture }) else {
            XCTFail("no fixture named \(cell.fixture) in the harness catalog", file: file, line: line)
            return nil
        }
        guard let cg = PanelRenderHarness.render(fixture, scheme: cell.scheme) else {
            XCTFail("""
                ImageRenderer returned nil for \(cell.name). If ImageRendererHeadlessProbeTests is ALSO red, \
                a platform revision withdrew headless SwiftUI rasterization and this whole gate has lost its \
                foundation (issue #749); if the probes are green, it is this panel's wiring.
                """, file: file, line: line)
            return nil
        }
        guard let raster = PanelRaster.normalize(cg) else {
            XCTFail("could not normalize \(cell.name) into an sRGB RGBA8 buffer", file: file, line: line)
            return nil
        }
        if cached, seedLag == 0 { Self.cache[cell.name] = raster }
        return raster
    }

    /// The committed-goldens directory, located from this source file (like `BarGlyphParityTests` locates
    /// `design/renders/bar-glyphs`) — CI checks the tree out at the same path it compiled from.
    private var goldensDirectory: URL {
        URL(fileURLWithPath: #filePath)          // .../apps/menubar/Tests/PanelGoldenParityTests.swift
            .deletingLastPathComponent()         // .../apps/menubar/Tests
            .deletingLastPathComponent()         // .../apps/menubar
            .appendingPathComponent("design/renders/panel-goldens")
    }

    private func loadGolden(_ cell: Cell,
                            file: StaticString = #filePath, line: UInt = #line) -> PanelRaster? {
        let url = goldensDirectory.appendingPathComponent(cell.name)
        guard let raster = PanelRaster.normalize(pngAt: url) else {
            XCTFail("""
                missing or undecodable golden \(cell.name) — regenerate with \
                TEST_RUNNER_SESSIOMETER_PANEL_GOLDENS=update (see design/README.md § Panel golden drift gate)
                """, file: file, line: line)
            return nil
        }
        return raster
    }

    // MARK: - Guard: the health-tint assets resolved in THIS bundle (not a failed lookup)

    // The whole suite is meaningless if the panel's asset-catalog colours did not resolve: every health
    // tint would fall back to a lookup failure, the goldens would bake a wrong-coloured panel, and the gate
    // would then DEFEND that (the baseline trap). `Bundle.main` here is the `xctest` runner, so this is a
    // REAL risk and not a theoretical one — `Color.panelAssets` exists precisely to avoid it. The direct
    // analogue of `BarGlyphParityTests.testTheRealBespokeSymbolsResolveInThisBundle`.
    func testTheHealthTintAssetsResolveInThisBundle() throws {
        let bundle = Color.panelAssets
        XCTAssertNotEqual(bundle.bundleURL, Bundle.main.bundleURL,
                          "panelAssets resolved to Bundle.main — in this logic-test bundle that is the xctest "
                          + "runner, which carries no Assets.car")
        for name in ["HealthOK", "UtilGreen", "UtilAmber", "UtilOrange", "UtilRed", "AccentColor"] {
            XCTAssertNotNil(NSColor(named: name, bundle: bundle),
                            "colour set \(name) did not resolve from \(bundle.bundleURL.lastPathComponent) — "
                            + "the compiled Assets.xcassets is missing from the MenubarTests bundle (project.yml)")
        }
        // The tints must also be DISTINCT: five names all resolving to ONE fallback colour would satisfy
        // the non-nil checks above while collapsing the whole health axis into a single hue.
        let names = ["HealthOK", "UtilGreen", "UtilAmber", "UtilOrange", "UtilRed"]
        let tints = names.compactMap { NSColor(named: $0, bundle: bundle) }
            .map { $0.usingColorSpace(.sRGB) ?? $0 }
        XCTAssertEqual(tints.count, names.count, "not every health tint resolved")
        for i in 0..<tints.count {
            for j in (i + 1)..<tints.count {
                XCTAssertNotEqual(tints[i], tints[j],
                                  "health tints \(names[i]) and \(names[j]) resolved to the same colour — "
                                  + "the lookup fell back to a default instead of reading the catalog")
            }
        }
    }

    // MARK: - Rig: every fixture rasterizes, at a stable size, carrying ink

    func testEveryFixtureRendersNonBlank() {
        let all = cells()
        var checked = 0
        for cell in all {
            guard let raster = render(cell) else { continue }
            XCTAssertEqual(raster.width, Int(PanelRenderHarness.scale) * Int(PanelMetrics.width),
                           "\(cell.name) is \(raster.width) px wide — the panel is fixed-width by construction")
            XCTAssertGreaterThan(raster.height, 0, "\(cell.name) has no height")
            // `inkCoverage` scores departure from the CORNER pixel, so a blank raster AND a uniform fill
            // both collapse to 0 — the LOWER bound is the load-bearing one and it catches both (the
            // measured behaviour `ImageRendererHeadlessProbeTests`' header records; do not import the
            // "strictly inside (0,1) catches blank and blob" rationale, which does not hold for this
            // primitive).
            let coverage = PanelRaster.inkCoverage(raster)
            XCTAssertGreaterThan(coverage, 0.02,
                                 "\(cell.name) has almost no ink (\(coverage)) — the panel did not draw")
            checked += 1
        }
        // Degenerate-subject guard: the pass is evidence only if it evaluated the full planned set.
        XCTAssertEqual(checked, all.count,
                       "expected \(all.count) (fixture × theme) renders, ran \(checked)")
        XCTAssertGreaterThan(all.count, 20, "the harness catalog collapsed to \(all.count) cells")
    }

    // MARK: - Rig: the renderer is deterministic (0.000 means the SAME image)

    // Both halves are asserted on purpose, because they are DIFFERENT claims and only one of them is what
    // the gate metric can see. `diffFraction` counts pixels whose largest channel delta exceeds 64/255, so
    // drift 0.000000 means "no pixel differs VISIBLY" — a raster off by ±1 on every channel also scores
    // 0.000000. Determinism of the rig is the stronger claim (identical BYTES), and it is the one that has
    // to hold for a committed golden to mean anything, so it is asserted directly rather than inferred from
    // the metric. Elsewhere in this file "drift 0" always means the metric, never byte equality.
    func testAnIdenticalRerenderScoresExactlyZero() throws {
        let cell = Cell(fixture: "healthy", scheme: .light)
        // BOTH uncached, deliberately: re-run `ImageRenderer` twice back to back rather than compare against
        // a buffer some earlier test left in the cache. A cache hit would silently turn this into a
        // renders-SECONDS-APART comparison, which is a different (and weaker) claim — see the byte-jitter
        // measurement in `testRendersSurviveTheClockDriftWindow`, where that comparison actually belongs.
        let a = try XCTUnwrap(render(cell, cached: false))
        let b = try XCTUnwrap(render(cell, cached: false))
        XCTAssertEqual(PanelRaster.diffFraction(a, b), 0.0, accuracy: 0.0,
                       "two renders of the same fixture at the same seed must score drift 0 under the gate "
                       + "metric — else the rig is nondeterministic and no golden can hold")
        let (differing, worst) = PanelRaster.byteDelta(a, b)
        print(String(format: "[panel-goldens] identical re-render: %d of %d bytes differ, worst delta %d",
                     differing, a.bytes.count, worst))
        // BYTE-exact, not merely under the metric threshold. `ImageRenderer` is deterministic for a given
        // seed once the rasterizer is warm, and asserting the strong form here is what lets the seed-lag
        // result next door (`testRendersSurviveTheClockDriftWindow`, also byte-exact) be attributed to the
        // clock guard rather than to luck.
        XCTAssertEqual(worst, 0,
                       "two back-to-back renders of the same fixture differ by \(worst)/255 on some channel "
                       + "(\(differing) of \(a.bytes.count) bytes) — `ImageRenderer` is nondeterministic at "
                       + "this seed, and no committed golden can hold")

        // WHICH LAYER jitters — asserted, not guessed, because the two answers have opposite consequences.
        // `normalize` re-draws the renderer's `CGImage` into a fresh sRGB RGBA8 context, and that conversion
        // is a plausible source of ±1 rounding all by itself. If normalization is the jittery layer, the
        // committed PNGs are still byte-reproducible and only the in-memory comparison buffers wobble; if
        // `ImageRenderer` is, then no golden PNG can ever be byte-reproducible. Normalizing ONE CGImage
        // twice separates them.
        let fixture = try XCTUnwrap(PanelRenderHarness.fixtures(now: Self.wallClock())
            .first(where: { $0.name == cell.fixture }))
        let cg = try XCTUnwrap(PanelRenderHarness.render(fixture, scheme: cell.scheme))
        let n1 = try XCTUnwrap(PanelRaster.normalize(cg))
        let n2 = try XCTUnwrap(PanelRaster.normalize(cg))
        let (normDiffering, normWorst) = PanelRaster.byteDelta(n1, n2)
        print(String(format: "[panel-goldens] same CGImage normalized twice: %d bytes differ, worst delta %d",
                     normDiffering, normWorst))
        XCTAssertEqual(normWorst, 0,
                       "normalizing the SAME CGImage twice produced different bytes (\(normDiffering) bytes, "
                       + "worst \(normWorst)/255) — the comparison buffer itself is nondeterministic, which "
                       + "makes every drift number in this suite noise rather than measurement")
    }

    // MARK: - Rig: the clock-drift window (MUTATION-driven, not asserted)

    // A render lands a moment AFTER its fixture was seeded, and the panel computes every countdown / age
    // against `TimelineView`'s own clock (issue #326) — so the rendered TEXT, and therefore the pixels,
    // depend on that gap. `PanelRenderHarness.boundaryGuardSecs` places every clock-relative instant 30 s
    // past a `humanizeUntil` unit boundary so the whole window formats identically. This drives it by
    // MUTATION: re-seed as if the fixtures had been built 1 / 7 / 29 s ago (the real-world direction — a
    // render is always later than its seed) and require an unchanged render (drift 0, the gate metric).
    //
    // The BYTE delta is measured here too, and it is ALSO zero — the seed lag moves no bytes at all, which
    // is the strong form of the claim above (identical strings rasterize to identical bytes). An earlier
    // revision of this comment asserted the opposite — that this was "the one place in this suite where it
    // is NOT zero", ±1/255 tracking the seed→raster latency — while the assertion below demanded exactly 0.
    // That prose was stale (issue #821): it predated `PanelRenderHarness.warmUpIfNeeded()`, whose own doc
    // records the measurement that refutes it — "renders seeded seconds apart are byte-identical, which
    // rules the clock out directly". Re-measured for #821: 11 consecutive runs (5 of this class alone, 6 of
    // the whole suite under 14-core saturation, load average to 38) reported `worst delta 0 over up to 0
    // bytes` every time. The assertion was right and the comment was wrong; do NOT re-loosen it to a
    // tolerance on the strength of the deleted prose.
    //
    // WHAT DOES move bytes by ±1 is a COLD raster, not the clock — the header's HONEST LIMIT carries that
    // measurement, and this test is where it surfaces, because `atSeed` lands in the cold group while the
    // lagged renders land outside it. Worth knowing it is NOT a flake when it does: running this test ALONE
    // (`-only-testing:…/testRendersSurviveTheClockDriftWindow`) is red 6 times out of 6, at exactly 882
    // bytes and worst delta 1, on this commit and on the one before it alike — the observed #756 signature
    // exactly, and a warm-up signature rather than a clock one. In the FULL suite the earlier tests have
    // already rasterized the catalog, so the rasterizer is warm by the time this runs and the measurement is
    // 0 — which is why the required job is green, and why a lone re-run is not a diagnosis. Closing the
    // exposure is a change to the harness's warm-up, tracked as issue #824.
    //
    // `atSeed` is rendered UNCACHED below for the same reason `testAnIdenticalRerenderScoresExactlyZero`
    // renders both of its sides uncached: the lagged renders already bypass the cache, so caching only this
    // side would compare an arbitrarily-old entry — whatever the FIRST test to touch the cell rasterized —
    // against a fresh render. Symmetry, not a fix; measured NOT to change the outcome in any of the three
    // scenarios tried (isolated, full-class, cache-populated-while-cold).
    func testRendersSurviveTheClockDriftWindow() throws {
        var worstByteDelta = 0
        var worstByteCount = 0
        for cell in [Cell(fixture: "healthy", scheme: .light),
                     Cell(fixture: "stale", scheme: .dark),
                     Cell(fixture: "disconnected", scheme: .light),
                     Cell(fixture: "blind-cornered", scheme: .dark)] {
            let atSeed = try XCTUnwrap(render(cell, cached: false))
            for lag: Int64 in [1, 7, 29] {
                let drifted = try XCTUnwrap(render(cell, seedLag: lag))
                XCTAssertEqual(PanelRaster.diffFraction(atSeed, drifted), 0.0, accuracy: 0.0,
                               "\(cell.name) rendered differently when its fixture was seeded \(lag)s earlier "
                               + "— a clock-relative string crossed a humanizeUntil boundary inside the "
                               + "\(PanelRenderHarness.boundaryGuardSecs)s guard window, so this gate would "
                               + "redden at random. Move the offending fixture offset off the boundary.")
                let (differing, worst) = PanelRaster.byteDelta(atSeed, drifted)
                worstByteDelta = max(worstByteDelta, worst)
                worstByteCount = max(worstByteCount, differing)
            }
        }
        print(String(format: "[panel-goldens] seed-latency byte jitter: worst delta %d over up to %d bytes",
                     worstByteDelta, worstByteCount))
        XCTAssertEqual(worstByteDelta, 0,
                       "renders seeded seconds apart differ by \(worstByteDelta)/255 on some channel "
                       + "(up to \(worstByteCount) bytes) while scoring 0.000000 under the gate metric. "
                       + "Sub-threshold drift the metric is too coarse to see — the committed goldens stop "
                       + "being byte-reproducible, so a re-bless churns files that did not change and the "
                       + "`Panel-Goldens-Rebaselined:` audit trail stops being readable. "
                       + "DIAGNOSE, do not re-run (issue #821): a worst delta of exactly 1 over a few "
                       + "hundred bytes is the COLD-RASTER signature, not a clock failure — the warm-up "
                       + "covers the `healthy` fixture only (issue #824). A worst delta above 1, or a byte "
                       + "count in the thousands, is the real subject of this test: a clock-relative string "
                       + "crossed a `humanizeUntil` boundary inside the "
                       + "\(PanelRenderHarness.boundaryGuardSecs)s guard window — move the offending "
                       + "fixture offset off the boundary rather than relaxing this assertion")
    }

    // MARK: - Cross-machine robustness: the render ignores the HOST process's appearance

    // The goldens are rasterized on one machine and re-rendered on another, and those machines can sit in
    // DIFFERENT system appearances (Dark Mode on or off). Several panel colours are AppKit-semantic
    // (`Color(nsColor: .tertiaryLabelColor)`) and the health tints are asset colour sets with Any/Dark
    // variants — so if ANY of them resolved against the host process's `NSAppearance` instead of the
    // SwiftUI `\.colorScheme` the harness pins per fixture, every golden would silently be a function of
    // the operator's Dark Mode setting. That is a cross-machine dependency no single-machine run can see:
    // this machine and the CI runner are both in light mode, so it would stay invisible until the day
    // someone regenerated the goldens with Dark Mode on. Measured rather than assumed — render the same
    // fixture with the current drawing appearance forced to aqua and to darkAqua and require an unchanged
    // render (drift 0, the gate metric).
    func testRendersDoNotDependOnTheHostProcessAppearance() throws {
        for cell in [Cell(fixture: "healthy", scheme: .light),
                     Cell(fixture: "healthy", scheme: .dark),
                     Cell(fixture: "empty-roster", scheme: .light)] {
            var underAqua: PanelRaster?
            var underDarkAqua: PanelRaster?
            // `cached: false` on both — the shared cache would hand back one raster for both passes and the
            // comparison would be vacuously 0 (a degenerate pass).
            try XCTUnwrap(NSAppearance(named: .aqua)).performAsCurrentDrawingAppearance {
                underAqua = render(cell, cached: false)
            }
            try XCTUnwrap(NSAppearance(named: .darkAqua)).performAsCurrentDrawingAppearance {
                underDarkAqua = render(cell, cached: false)
            }
            let aqua = try XCTUnwrap(underAqua, "\(cell.name) did not render under aqua")
            let darkAqua = try XCTUnwrap(underDarkAqua, "\(cell.name) did not render under darkAqua")
            XCTAssertEqual(PanelRaster.diffFraction(aqua, darkAqua), 0.0, accuracy: 0.0,
                           "\(cell.name) rendered differently under aqua vs darkAqua — some colour resolves "
                           + "against the HOST process appearance rather than the fixture's pinned "
                           + "colorScheme, so the committed goldens depend on the operator's Dark Mode "
                           + "setting and are not portable between machines")
        }
    }

    // MARK: - Baseline trap: distinct states must not render as the same image

    // Same-size fixture pairs must differ. A pair scoring 0.000 is the SAME image — a pipeline smell (the
    // renderer ignored the fixture), never a design finding. Different-size pairs are excluded because a
    // size difference IS the distinction, and `diffFraction` is undefined across sizes.
    func testDistinctFixturesRenderDistinctly() {
        var compared = 0
        for group in sameSizeGroups() {
            for i in 0..<group.count {
                for j in (i + 1)..<group.count {
                    let diff = PanelRaster.diffFraction(group[i].raster, group[j].raster)
                    XCTAssertGreaterThan(diff, distinctnessFloor,
                        "\(group[i].cell.name) and \(group[j].cell.name) are near-identical (\(diff)) at the "
                        + "same pixel size — either the renderer ignored the fixture (a 0.000 means the SAME "
                        + "image) or two panel states collapsed into one appearance")
                    compared += 1
                }
            }
        }
        // Degenerate-subject guard, exact rather than non-zero: `> 0` would pass having compared 1 of the 37
        // planned pairs, which is the partial-subject hole its four siblings in this file are closed against.
        // 37 = the measured pair count over the 34-cell catalog's same-size groups
        // (C(4,2) + 4·C(4,2) + 7·C(2,2) = 6 + 24 + 7). It moves only when the catalog or a panel height
        // does, which is a deliberate act — re-measure with SESSIOMETER_PANEL_MEASURE=1, do not tune.
        //
        // It moved from 57 at issue #776, and the move is the point: `View log` made `starting` and
        // `crash-looping` TALLER, and by different amounts (the mock styles the action `.btn.link` in one and
        // `.btn` in the other), so the 8-cell group those two shared with `connecting` / `unsupported` split
        // into a 4-cell group plus two singleton pairs — 28 → 6 + 1 + 1. Losing 20 comparisons is a real
        // reduction in this check's subject; the coverage consequence is recorded against
        // `testEachFreshRenderIsNearestToItsOwnGolden`'s `withoutCrossStateRival` tripwire, which is the
        // number the issue #790 promotion decision reads.
        XCTAssertEqual(compared, 37,
                       "expected 37 same-size fixture pairs, compared \(compared) — the distinctness check's "
                       + "subject changed; re-measure rather than relaxing this count")
    }

    // MARK: - CANARY: the coarse drift ceiling can FAIL (mutation, same predicate)

    // Proof the absolute committed-golden comparison is reachable: perturb a fresh render and push it
    // through the SAME `diffFraction(...) < driftCeiling` predicate `testEveryRenderMatchesItsCommittedGolden`
    // uses. No committed file is involved, so this canary runs in the required job even while the
    // committed-golden comparison is soft-landed.
    func testAPerturbedRenderTripsTheDriftCeiling() throws {
        let clean = try XCTUnwrap(render(Cell(fixture: "healthy", scheme: .light)))
        let perturbed = PanelRaster.perturbed(clean, areaFraction: canaryAreaFraction)
        let canary = PanelRaster.diffFraction(clean, perturbed)
        XCTAssertGreaterThan(canary, driftCeiling,
                             "canary \(canary) did NOT exceed the drift ceiling \(driftCeiling) — the gate "
                             + "cannot fail, so a green is not evidence")
        // And the perturbation must be the only difference: an unperturbed copy still scores exactly 0, so
        // the canary above measures the blot rather than incidental rig noise.
        XCTAssertEqual(PanelRaster.diffFraction(clean, PanelRaster.perturbed(clean, areaFraction: 0)), 0.0,
                       accuracy: 0.0, "a zero-area perturbation changed the raster — the mutator is not exact")
    }

    // MARK: - CANARY: the relative nearest-golden gate can FAIL (mutation, same predicate)

    // The perturbation canary above cannot exercise the RELATIVE gate: a blot moves a render away from
    // every reference equally, so its nearest is still itself. The mutation that DOES exercise it is
    // substitution — hand the resolver one state's render while claiming another's — which is exactly the
    // real-world failure it exists to catch (a panel state drifting into another state's appearance). This
    // drives the same `nearest(...)` predicate the primary gate uses, over a SAME-RUN reference set, so it
    // too runs in the required job.
    func testASubstitutedRenderTripsTheNearestReferenceGate() throws {
        let group = try XCTUnwrap(sameSizeGroups().max(by: { $0.count < $1.count }),
                                 "no same-size group to run the substitution canary over")
        // A hard failure, deliberately NOT an `XCTSkipIf`: a skipped canary reads as green while proving
        // nothing, which is the degenerate pass every other guard in this file is closed against.
        // Unreachable today — `sameSizeGroups()` already filters to `count > 1` — so it fires only if that
        // filter changes underneath the canary.
        guard group.count >= 2 else {
            return XCTFail("the substitution canary needs ≥2 same-size fixtures, found \(group.count) — "
                           + "`sameSizeGroups()` no longer guarantees that, so this canary is not "
                           + "exercising the nearest-reference gate at all")
        }
        let references = group.map { (name: $0.cell.name, raster: $0.raster) }

        // Control: each render's nearest reference IS itself — the predicate is live.
        for entry in group {
            XCTAssertEqual(nearest(to: entry.raster, in: references), entry.cell.name,
                           "control failed: \(entry.cell.name)'s nearest same-run reference is not itself, so "
                           + "the resolver is broken and the mutation below would prove nothing")
        }

        // Mutation: substitute a sibling's render. The resolver must NOT return the claimed name.
        let claimed = group[0].cell.name
        let substituted = group[1].raster
        XCTAssertNotEqual(nearest(to: substituted, in: references), claimed,
                          "a substituted render still resolved to \(claimed) — the nearest-reference gate "
                          + "cannot distinguish two different panel states, so it cannot catch drift")
    }

    // MARK: - CANARY: the byte-equality assertions can FAIL (mutation, same predicate)

    // Proof that the `byteDelta(...).worst == 0` assertions are reachable — the ones in
    // `testAnIdenticalRerenderScoresExactlyZero` and `testRendersSurviveTheClockDriftWindow`, which issue
    // #821 questioned. They are equality-to-zero assertions over a same-run pair, so a renderer that is
    // merely CONSISTENT rather than correct satisfies them for free, and an inspection-only argument for
    // them is exactly the argument issue #437 taught this repo not to accept: a golden authored over a
    // broken renderer DEFENDS the break and reports green.
    //
    // Both halves below are load-bearing, and they say opposite things about the same ±1 mutation:
    //   • `byteDelta` MUST see it — the gate can fail; and
    //   • `diffFraction` MUST NOT (0.000000) — which is why the byte assertions are not redundant with the
    //     metric gates next door. If this half ever reddens, the metric grew sensitive enough to subsume
    //     them and the byte assertions can be reconsidered; until then they are the only thing standing
    //     between a ±1-everywhere raster and a green suite.
    // Same-run comparison, no committed file → runs in the required job, like its sibling canaries.
    func testANudgedRenderTripsTheByteEqualityGate() throws {
        let clean = try XCTUnwrap(render(Cell(fixture: "healthy", scheme: .light)))

        // Control: the mutator is exact — a zero-count nudge changes nothing, so the canary below measures
        // the nudge rather than incidental rig noise.
        let (unchanged, unchangedWorst) = PanelRaster.byteDelta(clean,
                                                               PanelRaster.byteNudged(clean, count: 0))
        XCTAssertEqual(unchangedWorst, 0,
                       "a zero-count nudge changed \(unchanged) bytes — the mutator is not exact")

        // 1 byte is the MINIMAL drift the predicate has to see; 728 is the measured cold-raster signature
        // (see the header) — the failure this canary is shaped after.
        for count in [1, 728] {
            let nudged = PanelRaster.byteNudged(clean, count: count)
            let (differing, worst) = PanelRaster.byteDelta(clean, nudged)

            // The gate can FAIL: this is the same `byteDelta(...).worst` predicate the real assertions use.
            XCTAssertGreaterThan(worst, 0,
                                 "nudging \(count) byte(s) by 1 did NOT move `byteDelta`'s worst delta — the "
                                 + "byte-equality assertions cannot fail, so their green is not evidence")
            XCTAssertEqual(worst, 1, "expected a worst delta of exactly 1, got \(worst) — the mutator is not "
                           + "producing the ±1/255 signature this canary claims to model")
            XCTAssertEqual(differing, count,
                           "expected exactly \(count) differing byte(s), got \(differing) — the nudge is not "
                           + "landing on \(count) distinct byte(s), so the canary overstates itself")

            // …and the METRIC cannot see it. This is the whole reason the byte assertions exist.
            XCTAssertEqual(PanelRaster.diffFraction(clean, nudged), 0.0, accuracy: 0.0,
                           "a \(count)-byte ±1 nudge was visible to the gate metric — `diffFraction`'s "
                           + "64/255 threshold is meant to be blind to this, and the byte assertions exist "
                           + "precisely to cover what it cannot see")
        }
    }

    // MARK: - Pipeline integrity: a golden round-trips exactly

    // The gate compares a fresh in-memory raster against a raster decoded from a committed PNG. If that
    // encode/decode round-trip were lossy — a colour-profile conversion, a premultiplication difference —
    // every comparison would carry a constant offset that the thresholds would then have to absorb, and the
    // measured numbers in the header would be meaningless. This asserts the round-trip is EXACT.
    func testAGoldenRoundTripsToTheSameBytes() throws {
        let raster = try XCTUnwrap(render(Cell(fixture: "healthy", scheme: .dark)))
        let png = try XCTUnwrap(PanelRaster.png(raster), "could not PNG-encode a normalized raster")
        let decoded = try XCTUnwrap(PanelRaster.normalize(png: png), "could not decode the PNG just written")
        XCTAssertEqual(decoded.width, raster.width)
        XCTAssertEqual(decoded.height, raster.height)
        XCTAssertEqual(PanelRaster.diffFraction(raster, decoded), 0.0, accuracy: 0.0,
                       "PNG round-trip is lossy — the committed goldens would not decode to the bytes they "
                       + "were written from, so every drift measurement carries a hidden offset")
        XCTAssertEqual(decoded.bytes, raster.bytes, "PNG round-trip changed the raw buffer")
    }

    // MARK: - Re-baseline: write the committed goldens (explicit opt-in only)

    // NOT an auto-bless. The defect this issue fixes is precisely a baseline that moved as a SIDE EFFECT
    // (editing the mock silently re-baselined `build-comparison.py`), so blessing a render is a decision
    // that needs a command — and a `Panel-Goldens-Rebaselined:` commit trailer, which
    // `scripts/check-panel-golden-rebaseline.sh` requires in CI whenever the goldens directory is touched.
    func testRegenerateGoldensWhenExplicitlyRequested() throws {
        try XCTSkipUnless(isUpdatingGoldens,
                          "re-baselining is opt-in: TEST_RUNNER_SESSIOMETER_PANEL_GOLDENS=update — the "
                          + "bare, un-prefixed name reaches xcodebuild and not the test, which then lands "
                          + "on this very skip. Full command in design/README.md § Panel golden drift gate")
        let directory = goldensDirectory
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var written = 0
        let all = cells()
        for cell in all {
            let raster = try XCTUnwrap(render(cell), "cannot bless \(cell.name): it did not render")
            let png = try XCTUnwrap(PanelRaster.png(raster), "cannot PNG-encode \(cell.name)")
            try png.write(to: directory.appendingPathComponent(cell.name))
            written += 1
        }
        XCTAssertEqual(written, all.count, "wrote \(written) goldens, expected \(all.count)")
        print("[panel-goldens] wrote \(written) goldens to \(directory.path)")
    }

    // MARK: - SOFT GATE: every render matches its committed golden (coarse absolute tripwire)

    // The case the relative gate below is BLIND to: every frame drifting together, which keeps each nearest
    // to its own shifted golden. Loose ceiling on purpose — see the header. Soft-landed (non-required)
    // because this is the one cross-machine-sensitive comparison in the suite.
    func testEveryRenderMatchesItsCommittedGolden() throws {
        try XCTSkipUnless(isGoldenGateEnabled,
                          "committed-golden comparison is the non-required half: SESSIOMETER_PANEL_GOLDEN_GATE=1")
        let all = cells()
        var checked = 0
        var worst = (name: "", drift: 0.0)
        for cell in all {
            guard let golden = loadGolden(cell), let fresh = render(cell) else { continue }
            XCTAssertEqual(fresh.width, golden.width, "\(cell.name) width drifted from its golden")
            XCTAssertEqual(fresh.height, golden.height,
                           "\(cell.name) height drifted from its golden (\(fresh.height) vs \(golden.height)) "
                           + "— the panel's layout changed")
            guard fresh.width == golden.width, fresh.height == golden.height else { continue }
            let drift = PanelRaster.diffFraction(fresh, golden)
            if drift >= worst.drift { worst = (cell.name, drift) }
            XCTAssertLessThan(drift, driftCeiling,
                              "\(cell.name) drifted \(drift) from its golden (> \(driftCeiling)) — the panel's "
                              + "rendered appearance changed. If intentional, re-bless with "
                              + "TEST_RUNNER_SESSIOMETER_PANEL_GOLDENS=update (the bare, un-prefixed name "
                              + "reaches xcodebuild and not the test, which then silently skips — full "
                              + "command in design/README.md § Panel golden drift gate), LOOK at the new "
                              + "renders, and record why in the commit (Panel-Goldens-Rebaselined: <reason>)")
            checked += 1
        }
        // Report the MAX observed drift whether or not the gate passed. This is the measurement the
        // promotion decision needs: `driftCeiling` is calibrated on ONE machine, and only a run on the
        // actual CI runner can say what cross-machine antialiasing costs. Without this line a green soft
        // run would carry no information about how much margin is left.
        print(String(format: "[panel-goldens] max drift %.6f (%@) over %d cells, ceiling %.6f",
                     worst.drift, worst.name as NSString, checked, driftCeiling))
        // Degenerate-subject guard: a pass over a partial set is not evidence.
        XCTAssertEqual(checked, all.count,
                       "expected \(all.count) golden comparisons, ran \(checked)")
    }

    // MARK: - SOFT GATE: nearest-golden identity (the PRIMARY, cross-machine-immune drift gate)

    // A fresh render's CLOSEST committed golden (among the same-size goldens) must be ITSELF. Relative, so
    // a uniform cross-machine antialiasing shift nudges every comparison equally and cannot flip the
    // winner; it catches the honesty-critical failure — one state's panel morphing toward ANOTHER state's
    // appearance — with the full measured 0.002513 margin (the closest cross-state pair; self-distance is
    // 0.000000, so the whole separation is headroom).
    //
    // MEASURED LIMIT, and it is not small: this check only has power where a same-size golden of a
    // DIFFERENT fixture exists to lose to. Goldens are sized by content, and 7 of the 17 fixtures own a
    // unique pixel height (`stats`, `disconnected`, `not-running`, `empty-roster`, `blind-cornered`, and —
    // since issue #776 gave each of them a differently-styled `View log` action — `starting` and
    // `crash-looping`), so their size group holds only their own two themes. Light-vs-dark of the same
    // fixture sits ~0.97 apart, so "nearest is itself" is trivially true for those 14 of 34 cells and
    // detects nothing. That is 4 cells WORSE than before #776, and deliberately recorded as such. Those cells
    // rest on `testEveryRenderMatchesItsCommittedGolden`'s absolute ceiling alone — i.e. on the
    // cross-machine-UNVALIDATED half, which is a real input to the promotion decision (issue #790) and is
    // why the count below is asserted rather than merely mentioned. Both numbers print on every run.
    func testEachFreshRenderIsNearestToItsOwnGolden() throws {
        try XCTSkipUnless(isGoldenGateEnabled,
                          "committed-golden comparison is the non-required half: SESSIOMETER_PANEL_GOLDEN_GATE=1")
        // Group the GOLDENS by pixel size, then resolve each fresh render against its own size group.
        var groups: [String: [(name: String, fixture: String, raster: PanelRaster)]] = [:]
        let all = cells()
        for cell in all {
            guard let golden = loadGolden(cell) else { continue }
            groups["\(golden.width)x\(golden.height)", default: []].append((cell.name, cell.fixture, golden))
        }
        var resolved = 0
        // Cells whose size group contains no OTHER fixture: the relative check cannot detect state
        // morphing there, because there is no other state to morph into.
        var withoutCrossStateRival = 0
        var weakestMargin = (name: "", margin: Double.greatestFiniteMagnitude)
        for cell in all {
            guard let fresh = render(cell) else { continue }
            let group = groups["\(fresh.width)x\(fresh.height)"] ?? []
            let rivals = group.filter { $0.fixture != cell.fixture }
            guard !rivals.isEmpty else {
                withoutCrossStateRival += 1
                resolved += 1
                continue
            }
            let winner = nearest(to: fresh, in: group.map { (name: $0.name, raster: $0.raster) })
            XCTAssertEqual(winner, cell.name,
                           "\(cell.name) is closest to \(winner ?? "nothing")'s golden, "
                           + "not its own — this panel state drifted toward another state's appearance")
            // Headroom = how far this render is from the nearest OTHER state, minus its own drift. This is
            // the quantity `driftCeiling` is calibrated against, measured per run rather than assumed.
            // The cell's own golden is already decoded in `group` (same size, same name); fall back to a
            // re-read only when it is NOT there, which happens exactly when the fresh render's size drifted
            // from the golden's — the case `testEveryRenderMatchesItsCommittedGolden` reports.
            let ownGolden = group.first { $0.name == cell.name }?.raster ?? loadGolden(cell)
            let ownDrift = ownGolden.map { PanelRaster.diffFraction(fresh, $0) } ?? 0
            let nearestRival = rivals
                .map { PanelRaster.diffFraction(fresh, $0.raster) }
                .min() ?? 0
            let margin = nearestRival - ownDrift
            if margin < weakestMargin.margin { weakestMargin = (cell.name, margin) }
            resolved += 1
        }
        print(String(format: "[panel-goldens] weakest cross-state margin %.6f (%@); %d of %d cells have no "
                             + "cross-state rival and rest on the absolute ceiling alone",
                     weakestMargin.margin, weakestMargin.name as NSString,
                     withoutCrossStateRival, all.count))
        // Tripwire, not a preference: this is the measured coverage of the PRIMARY gate. If a fixture is
        // added or a layout changes a panel's height, this number moves and the promotion decision in issue
        // #790 needs to know. Re-measure and update deliberately — never widen it to make a run green.
        XCTAssertEqual(withoutCrossStateRival, 14,
                       "measured 14 of 34 cells have no same-size cross-state rival; got "
                       + "\(withoutCrossStateRival). The relative gate's coverage changed — re-measure and "
                       + "record it against the promotion criterion (issue #790) rather than adjusting this "
                       + "number to fit")
        XCTAssertEqual(resolved, all.count, "expected \(all.count) nearest-golden resolutions, ran \(resolved)")
    }

    // MARK: - Calibration: re-derive every number in this file's header

    func testMeasureSeparations() throws {
        try XCTSkipUnless(isMeasuring, "calibration run only: SESSIOMETER_PANEL_MEASURE=1")
        let all = cells()
        var lines: [String] = ["", "=== panel golden calibration (issue #754) ==="]

        // Sizes + ink, per cell.
        for cell in all {
            guard let raster = render(cell) else { continue }
            let padded = cell.name.padding(toLength: 40, withPad: " ", startingAt: 0)
            lines.append(padded + String(format: " %4dx%-4d ink %.6f",
                                         raster.width, raster.height, PanelRaster.inkCoverage(raster)))
        }

        // Identical re-render. BOTH uncached, matching `testAnIdenticalRerenderScoresExactlyZero`: the
        // per-cell loop above already cached this probe, so taking the cache hit would silently make this
        // row a renders-SECONDS-APART measurement — which is the seed-lag row below, a different and weaker
        // claim. `cached: false` is the seam for that; reaching into the shared cache is not.
        let probe = Cell(fixture: "healthy", scheme: .light)
        let a = try XCTUnwrap(render(probe, cached: false))
        let b = try XCTUnwrap(render(probe, cached: false))
        lines.append(String(format: "  identical re-render ................ %.6f", PanelRaster.diffFraction(a, b)))

        // Clock-drift window.
        for lag: Int64 in [1, 7, 29] {
            let drifted = try XCTUnwrap(render(probe, seedLag: lag))
            lines.append(String(format: "  re-seeded %2ds earlier ............. %.6f",
                                lag, PanelRaster.diffFraction(a, drifted)))
        }

        // Round-trip.
        let png = try XCTUnwrap(PanelRaster.png(a))
        let decoded = try XCTUnwrap(PanelRaster.normalize(png: png))
        lines.append(String(format: "  golden round-trip ................. %.6f", PanelRaster.diffFraction(a, decoded)))

        // Pairwise separations within each size group.
        var pairs: [(Double, String)] = []
        for group in sameSizeGroups() {
            for i in 0..<group.count {
                for j in (i + 1)..<group.count {
                    pairs.append((PanelRaster.diffFraction(group[i].raster, group[j].raster),
                                  "\(group[i].cell.fixture)/\(PanelRenderHarness.themeToken(group[i].cell.scheme))"
                                  + " vs \(group[j].cell.fixture)/\(PanelRenderHarness.themeToken(group[j].cell.scheme))"))
                }
            }
        }
        pairs.sort { $0.0 < $1.0 }
        lines.append("  same-size pairs compared .......... \(pairs.count)")
        // The five closest pairs are what the distinctness floor and the drift ceiling are calibrated
        // against — a median over these pairs is near-useless, because most same-size pairs are
        // light-vs-dark and therefore almost wholly different.
        for (index, pair) in pairs.prefix(5).enumerated() {
            lines.append(String(format: "  closest pair #%d .................... %.6f  ", index + 1, pair.0)
                         + pair.1)
        }
        if let farthest = pairs.last {
            lines.append(String(format: "  FARTHEST pair ..................... %.6f  ", farthest.0) + farthest.1)
        }

        // Canary.
        for fraction in [0.005, 0.010, 0.015, 0.030] {
            let canary = PanelRaster.diffFraction(a, PanelRaster.perturbed(a, areaFraction: fraction))
            lines.append(String(format: "  canary @ %.3f of frame ........... %.6f", fraction, canary))
        }
        lines.append("=== end calibration ===")
        print(lines.joined(separator: "\n"))
    }

    // MARK: - Helpers

    private struct SizedCell {
        let cell: Cell
        let raster: PanelRaster
    }

    /// Fresh renders grouped by identical pixel size — the only groups `diffFraction` is defined over.
    /// Groups of one are dropped (nothing to compare within them).
    private func sameSizeGroups() -> [[SizedCell]] {
        var groups: [String: [SizedCell]] = [:]
        for cell in cells() {
            guard let raster = render(cell) else { continue }
            groups["\(raster.width)x\(raster.height)", default: []].append(SizedCell(cell: cell, raster: raster))
        }
        return groups.values.filter { $0.count > 1 }.sorted { $0.count > $1.count }
    }

    /// The name of the reference `candidate` is closest to. The ONE predicate both the relative gate and its
    /// substitution canary drive, so the canary cannot prove something the gate does not use.
    private func nearest(to candidate: PanelRaster,
                         in references: [(name: String, raster: PanelRaster)]) -> String? {
        references
            .map { (name: $0.name, drift: PanelRaster.diffFraction(candidate, $0.raster)) }
            .min { $0.drift < $1.drift }?
            .name
    }
}

// MARK: - Raster primitives

/// A normalized raster: 8-bit RGBA, premultiplied-last, sRGB, tightly packed.
///
/// Normalization is not cosmetic. The gate compares an `ImageRenderer` `CGImage` (whose pixel format and
/// colour space are the renderer's choice) against a raster decoded from a committed PNG. Comparing raw
/// bytes across two different representations would be meaningless, so BOTH sides are redrawn into an
/// identical `CGContext` first — which is also what makes `testAGoldenRoundTripsToTheSameBytes` able to
/// assert an EXACT round-trip.
struct PanelRaster {
    let width: Int
    let height: Int
    /// `width * height * 4` bytes, RGBA order.
    let bytes: [UInt8]

    private static let bytesPerPixel = 4

    // MARK: Construction

    /// Redraw `cg` into the canonical sRGB / RGBA8 / premultiplied-last representation.
    static func normalize(_ cg: CGImage) -> PanelRaster? {
        let width = cg.width
        let height = cg.height
        guard width > 0, height > 0, let space = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        var bytes = [UInt8](repeating: 0, count: width * height * bytesPerPixel)
        let drew = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let context = CGContext(data: buffer.baseAddress,
                                          width: width, height: height,
                                          bitsPerComponent: 8,
                                          bytesPerRow: width * bytesPerPixel,
                                          space: space,
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            // `.copy`, not the default `.normal`: the source's own alpha must land in the buffer verbatim
            // rather than compositing over the zeroed backing, so a translucent panel edge round-trips.
            context.setBlendMode(.copy)
            context.draw(cg, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        return drew ? PanelRaster(width: width, height: height, bytes: bytes) : nil
    }

    static func normalize(png data: Data) -> PanelRaster? {
        guard let rep = NSBitmapImageRep(data: data), let cg = rep.cgImage else { return nil }
        return normalize(cg)
    }

    static func normalize(pngAt url: URL) -> PanelRaster? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return normalize(png: data)
    }

    /// PNG-encode from the NORMALIZED buffer (not from the original `CGImage`), so what gets committed is
    /// byte-for-byte what the comparison reads back.
    static func png(_ raster: PanelRaster) -> Data? {
        guard let provider = CGDataProvider(data: Data(raster.bytes) as CFData),
              let space = CGColorSpace(name: CGColorSpace.sRGB),
              let cg = CGImage(width: raster.width, height: raster.height,
                               bitsPerComponent: 8, bitsPerPixel: 8 * bytesPerPixel,
                               bytesPerRow: raster.width * bytesPerPixel,
                               space: space,
                               bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                               provider: provider, decode: nil, shouldInterpolate: false,
                               intent: .defaultIntent)
        else { return nil }
        return NSBitmapImageRep(cgImage: cg).representation(using: .png, properties: [:])
    }

    // MARK: Metrics

    /// The fraction of pixels whose largest RGBA channel delta exceeds `channelThreshold`.
    ///
    /// Deliberately the same shape and the same 0.25-of-full-scale threshold as
    /// `BarGlyphRenderer.diffFraction` (64/255 ≈ 0.251), so the two gates' numbers are directly comparable:
    /// localized-change sensitive, antialiasing-tolerant. Mismatched sizes score 1 (maximally different) —
    /// the size itself is asserted separately.
    static func diffFraction(_ a: PanelRaster, _ b: PanelRaster, channelThreshold: UInt8 = 64) -> Double {
        guard a.width == b.width, a.height == b.height, a.width > 0, a.height > 0 else { return 1 }
        let threshold = Int(channelThreshold)
        var differing = 0
        a.bytes.withUnsafeBufferPointer { pa in
            b.bytes.withUnsafeBufferPointer { pb in
                var i = 0
                while i < pa.count {
                    let dr = abs(Int(pa[i]) - Int(pb[i]))
                    let dg = abs(Int(pa[i + 1]) - Int(pb[i + 1]))
                    let db = abs(Int(pa[i + 2]) - Int(pb[i + 2]))
                    let da = abs(Int(pa[i + 3]) - Int(pb[i + 3]))
                    if max(max(dr, dg), max(db, da)) > threshold { differing += 1 }
                    i += bytesPerPixel
                }
            }
        }
        return Double(differing) / Double(a.width * a.height)
    }

    /// Raw byte agreement, BELOW the gate metric's threshold: `(differing bytes, worst single-byte delta)`.
    ///
    /// `diffFraction` deliberately ignores channel deltas under 64/255, which is right for a drift gate and
    /// wrong for asking "is this renderer reproducible at all". That second question needs its own primitive
    /// — see `testAnIdenticalRerenderScoresExactlyZero`, which measures that the answer is YES once the
    /// rasterizer is warm: consecutive renders of one fixture agree to the BYTE, not merely to ±1/255. What
    /// still moves bytes by ±1 is a COLD raster (issue #824), not the seed or the clock.
    static func byteDelta(_ a: PanelRaster, _ b: PanelRaster) -> (differing: Int, worst: Int) {
        guard a.bytes.count == b.bytes.count else { return (max(a.bytes.count, b.bytes.count), 255) }
        var differing = 0
        var worst = 0
        // Unsafe buffers for the same reason `diffFraction` and `inkCoverage` use them: this walks every
        // byte of a ~2.7 MB raster, a dozen times per run, in a Debug build.
        a.bytes.withUnsafeBufferPointer { pa in
            b.bytes.withUnsafeBufferPointer { pb in
                for i in 0..<pa.count where pa[i] != pb[i] {
                    differing += 1
                    worst = max(worst, abs(Int(pa[i]) - Int(pb[i])))
                }
            }
        }
        return (differing, worst)
    }

    /// The fraction of pixels departing from the CORNER pixel — the same primitive
    /// `BarGlyphRenderer.inkCoverage` uses, and it behaves the same way here: a blank raster AND a uniform
    /// fill both collapse to 0, so the LOWER bound is the load-bearing one (see
    /// `ImageRendererHeadlessProbeTests`' header, which measured this).
    ///
    /// RGB-ONLY, which is correct for the OPAQUE full-panel rasters it was written for and a trap on a
    /// TRANSPARENT-backed one: the normalized raster is premultiplied, so near-black ink over a clear
    /// backdrop is `(0,0,0)` at EVERY opacity and an opacity-only change is invisible here while remaining
    /// plainly visible to `diffFraction` (which does compare alpha). Measured in issue #766 — see
    /// `PanelInteractionStateTests.inkMass`, the alpha-inclusive variant written for exactly that case.
    static func inkCoverage(_ raster: PanelRaster) -> Double {
        guard raster.width > 0, raster.height > 0, raster.bytes.count >= bytesPerPixel else { return 0 }
        let br = Int(raster.bytes[0]), bg = Int(raster.bytes[1]), bb = Int(raster.bytes[2])
        // 0.15 of full scale, matching BarGlyphRenderer's summed-RGB-delta threshold.
        let threshold = Int(0.15 * 255)
        var ink = 0
        raster.bytes.withUnsafeBufferPointer { p in
            var i = 0
            while i < p.count {
                let d = abs(Int(p[i]) - br) + abs(Int(p[i + 1]) - bg) + abs(Int(p[i + 2]) - bb)
                if d > threshold { ink += 1 }
                i += bytesPerPixel
            }
        }
        return Double(ink) / Double(raster.width * raster.height)
    }

    // MARK: Mutation (canary)

    /// A copy with `areaFraction` of the frame flipped to opaque red — the deliberate perturbation that
    /// proves the drift ceiling is reachable. The blot is a centred band, so it lands on real panel content
    /// rather than a transparent margin (which alpha-only deltas would still register, but less honestly).
    /// `areaFraction == 0` returns an exact copy, which the canary asserts scores 0.
    static func perturbed(_ raster: PanelRaster, areaFraction: Double) -> PanelRaster {
        guard areaFraction > 0 else { return raster }
        var bytes = raster.bytes
        let rows = max(1, Int((Double(raster.height) * areaFraction).rounded()))
        let firstRow = max(0, (raster.height - rows) / 2)
        for y in firstRow..<min(raster.height, firstRow + rows) {
            for x in 0..<raster.width {
                let i = (y * raster.width + x) * bytesPerPixel
                bytes[i] = 255      // R
                bytes[i + 1] = 0    // G
                bytes[i + 2] = 0    // B
                bytes[i + 3] = 255  // A
            }
        }
        return PanelRaster(width: raster.width, height: raster.height, bytes: bytes)
    }

    /// A copy with `count` bytes nudged by exactly +1 (saturating) — the MINIMAL drift the byte assertions
    /// exist to catch, and deliberately the one the gate metric is blind to.
    ///
    /// `perturbed` above is the canary for the METRIC (a red band, deltas of 255, whole pixels). It cannot
    /// prove the byte predicate is reachable, because anything that trips `diffFraction` trips `byteDelta`
    /// trivially. The interesting claim is the other direction: that `byteDelta` catches drift `diffFraction`
    /// reports as 0.000000. So this mutates by exactly the ±1/255 signature the real cold-raster defect
    /// produces (issue #824) — the canary is the same shape as the failure it guards against.
    ///
    /// Bytes are spread evenly across the buffer rather than taken from the front, so the mutation cannot be
    /// satisfied by a transparent margin alone; the step is derived from `count`, so exactly `count` bytes
    /// move (clamped to the buffer). `count == 0` returns an exact copy, which the canary asserts moves
    /// nothing.
    static func byteNudged(_ raster: PanelRaster, count: Int) -> PanelRaster {
        guard count > 0, !raster.bytes.isEmpty else { return raster }
        var bytes = raster.bytes
        let touched = min(count, bytes.count)
        // ≥ 1 because `touched` ≤ `bytes.count`, so the indices below are distinct and the last one,
        // `(touched - 1) * step`, is inside the buffer.
        let step = bytes.count / touched
        for k in 0..<touched {
            let i = k * step
            bytes[i] = bytes[i] == 255 ? 254 : bytes[i] + 1
        }
        return PanelRaster(width: raster.width, height: raster.height, bytes: bytes)
    }
}
#endif
