// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Accessibility-appearance REACHABILITY suite (issue #760) — and the honest answer to what that issue
// asked for.
//
// WHAT #760 ASKED FOR: fixtures rendering the panel under increased contrast, reduce transparency, and
// reduce motion, each asserted to DIFFER from its baseline, on the reasoning that "a variant that renders
// identically to the default means the setting is being ignored". That reasoning is sound. The premise
// underneath it — that a test process can put the panel into those three states at all — is not, and this
// file is the measurement that settles it plus the pins that will redden the day it stops being true.
//
// THE MEASURED VERDICT, per axis. All three are UNREACHABLE from a test process, for two independent
// reasons, and the issue's routing hypothesis (that reduce-transparency / reduce-motion are injectable
// SwiftUI environment values the way issue #749 injected `\.colorScheme`) is FALSE:
//
//   1. ALL FOUR accessibility environment keys are GET-ONLY on `EnvironmentValues` —
//      `colorSchemeContrast`, `accessibilityReduceTransparency`, `accessibilityReduceMotion`,
//      `accessibilityDifferentiateWithoutColor`. `.environment(\.accessibilityReduceTransparency, true)`
//      is a COMPILE ERROR, not a runtime no-op — so there is no injected-variant render to compare
//      against, and no test in this file can even be WRITTEN to attempt one. Both spellings fail, with
//      different diagnostics: direct assignment gives "cannot assign to property: … is a get-only
//      property", while `.environment(\.key, value)` gives "cannot convert value of type
//      'KeyPath<EnvironmentValues, Bool>' to expected argument type 'WritableKeyPath<…>'". `\.colorScheme`
//      is writable, which is precisely why the light/dark axis that #749 unblocked works and these three
//      do not — they are not the same mechanism. And the unreachability is DOUBLE: measured on the CI
//      runner (which has Reduce Transparency and Reduce Motion ON), an `ImageRenderer` render reports
//      `false` for both anyway — so the renderer does not inherit the system setting either, and changing
//      the system would not reach it even if a test could. See
//      `testThePanelRenderReadsTheAccessibilityDefaultsRegardlessOfTheSystem`.
//   2. The one remaining lever, `NSAppearance`, does not reach an `ImageRenderer` render at all — and the
//      precise form of that matters, because the obvious reading of it is wrong.
//      `performAsCurrentDrawingAppearance` IS live in this process (it changes an AppKit colour
//      resolution: `NSColor.textColor` resolves near-black under `.aqua` and near-white under
//      `.darkAqua`), but it does not reach the SwiftUI renderer for ANY appearance — even that maximal
//      `.aqua` / `.darkAqua` pair renders byte-identically at a pinned `\.colorScheme`. So the
//      high-contrast null measured below is NOT a fact about the high-contrast names specifically; it is
//      the general fact, and it would read the same way even if the high-contrast assets resolved
//      perfectly. `testTheAppearanceLeverIsLiveButNeverReachesTheRenderer` pins both halves so that
//      attribution cannot decay — including why these pins do NOT move together with issue #832's, which
//      is the tempting and wrong parallel to draw. See that test's docstring.
//
// So AC-1 and AC-2 of #760 cannot be satisfied as written, and a fixture that appeared to satisfy them
// would be a fabrication. Per the issue's own load-bearing note — "an identical render is a FAIL, not a
// pass" — the correct response to "every attempt renders identically" is to report the failure honestly
// and pin its CAUSE, not to relabel it green. That is what this file does.
//
// WHY PINS RATHER THAN A `XCTSkip` OR A COMMENT. An unreachable axis recorded in prose decays into an
// assumption nobody re-checks; the day a macOS or SwiftUI revision makes `colorSchemeContrast` writable,
// or makes the appearance name reach asset resolution, nothing would notice and the panel would keep
// shipping unguarded. Each pin below therefore drives the SAME predicate a real variant gate would use
// (`PanelRaster.byteDelta(baseline, variant).differing > 0` = "the axis moved the render") and
// asserts it reports NOT-REACHED. When the platform changes, the pin reddens with a message naming the
// follow-up work. This is the shape issue #759 already established for the asset half of the same
// question (`StatusPanelFormatTests.testTheHighContrastVariantsAreNotReachableByAppearance`).
//
// CONSTRAINT-A / AC-4, and why the canary is inverted twice. #760's AC-4 asks for "a fixture that IGNORES
// the setting trips the gate", noting that because the real gate asserts DIFFERENCE, the mutation that
// must redden is one that makes the variant render the SAME. This file's gates assert SAMENESS (the axis
// is unreachable), so the inversion flips back: what must redden a sameness-pin is a genuine DIFFERENCE.
// TWO things must hold for such a pin to be evidence — the comparison can see a difference, AND the lever
// being driven is live — and it is the second that bites here. Each is proven beside the assertion it
// licenses. Verified by MUTATION, never by inspection (issue #437's precedent: three render bugs that a
// golden authored at the time would have DEFENDED).
//
// WHAT IS NOT PINNED HERE, deliberately: reduce MOTION. A still raster encodes no motion by construction,
// so no fixture-shaped gate can ever cover it — this is a property of the artifact, not of the platform,
// and it will not change when the environment keys become writable. A test asserting "a still image does
// not animate" would be tautological ceremony. The panel has FOUR in-flight spinners — the per-row switch
// chip (`StatusPanelRoster`), the next-swap Swap button and the Start-daemon button (`StatusPanelChrome`),
// and the Capture button (`StatusPanelCapture`) — and `PanelRenderHarness` renders none of them: it seeds
// every transport-backed model at `.idle`, so each fixture captures the RESTING surface. Routed to the
// manual checklist in design/README.md and to the product-gap follow-up rather than faked here.
//
// AC-3 — "reduce-transparency output is verified legible" — is the one AC with a substantive answer, and
// it is a MEASUREMENT rather than a verdict. `testThePanelRasterIsBackdropDependent…` below measures that
// the panel is heavily material-dependent (0.93 of the frame changes over an opaque dark backdrop), which
// establishes that removing vibrancy is a LARGE visible change rather than a cosmetic one — so the axis
// genuinely matters and must not be closed as "no effect".
//
// AC-3 is NOT starting from zero, and saying otherwise would understate what already ships.
// `StatusPanelFormatTests` (issue #759) already measures every panel text tint at 4.5:1 and every glyph
// tint at 3:1 against an OPAQUE popover base — `lightBase = RGB(247, 247, 250)` / `darkBase =
// RGB(38, 38, 43)`, described there as the agreed stand-in precisely because "the live panel floats on
// vibrancy, which is not headlessly measurable". That opaque-base sweep IS the token-level half of "does
// not go opaque-on-opaque", and it is already gated. What is missing is the PANEL-level half: whether the
// composed surface still reads once the material stops contributing.
//
// This file deliberately does not close that half with a legibility THRESHOLD. The shipped product
// aesthetic ratifies vibrancy ("CodexBar-class native craft … VIBRANCY", operator-confirmed 2026-07-07),
// but the build reference (`apps/menubar/design/menubar-preview.html`) defines the DEFAULT appearance
// only, so what the panel SHOULD look like once the OS removes vibrancy is an undecided visual question.
// A threshold here would settle it by assertion. Routed as ratification-pending (issue #868).

#if DEBUG
import AppKit
import SwiftUI
import XCTest

@MainActor
final class PanelAppearanceVariantTests: XCTestCase {

    // MARK: - Calibrated thresholds (MEASURED — see the header and each call site)

    /// The ceiling two renders over the SAME backdrop must stay under, so the 0.93 the dependence
    /// measurement reports is attributable to the backdrop rather than to rig noise. Deliberately FAR
    /// tighter than `backdropDependenceFloor` itself: the claim here is "no difference at all", not "a
    /// small difference". Reuses `PanelGoldenParityTests`' `distinctnessFloor` value, which sits 5x under
    /// the measured closest distinct same-size fixture pair (0.002513) — so if this ever fires, two
    /// renders that should be identical are further apart than two genuinely different panel states.
    private let backdropNoiseCeiling = 0.0005

    /// The floor the backdrop-dependence measurement must clear to establish that removing vibrancy is a
    /// LARGE change. MEASURED at 0.9337 (vs black) / 0.9093 (vs 0.1 white) / 0.9292 (vs a saturated
    /// backdrop) — see `testThePanelRasterIsBackdropDependentSoRemovingVibrancyIsAVisibleChange`. Set to
    /// 0.5 rather than just under the measurement on purpose: the claim being pinned is the QUALITATIVE
    /// one ("most of the frame moves"), and the exact figure carries the panel's current content, which
    /// legitimately shifts whenever a fixture string or a layout does. A drop below half the frame means
    /// the material stopped compositing the backdrop, which is the finding worth reddening on.
    private let backdropDependenceFloor = 0.5

    // MARK: - Fixture plumbing

    private static func wallClock() -> Int64 { Int64(Date().timeIntervalSince1970) }

    private func healthyFixture() throws -> PanelRenderFixture {
        try XCTUnwrap(PanelRenderHarness.fixtures(now: Self.wallClock()).first { $0.name == "healthy" },
                      "the `healthy` fixture is gone from the harness catalog — this suite's subject")
    }

    /// Rasterize a fixture through the SHARED harness (the same call both the app tool and the golden gate
    /// make), under a forced drawing appearance. The appearance is required rather than optional because
    /// every measurement in this suite is ABOUT the appearance — an unforced render would silently inherit
    /// whatever the host process is in, which is the machine-dependence `PanelGoldenParityTests` exists to
    /// keep out of the goldens.
    private func render(_ fixture: PanelRenderFixture, scheme: ColorScheme,
                        appearance: NSAppearance.Name) throws -> PanelRaster {
        var out: PanelRaster?
        try XCTUnwrap(NSAppearance(named: appearance),
                      "NSAppearance(named: .\(appearance.rawValue)) did not resolve — the appearance "
                      + "name itself is gone, so this measurement is not testing what it claims")
            .performAsCurrentDrawingAppearance {
                out = PanelRenderHarness.render(fixture, scheme: scheme).flatMap(PanelRaster.normalize)
            }
        return try XCTUnwrap(out, "the healthy fixture did not rasterize")
    }

    /// A render proven REPRODUCIBLE in this process before it is compared at byte granularity.
    ///
    /// The loop, its `0..<8` bound, its `byteDelta(...).differing == 0` predicate and — above all — the whole
    /// reasoning for NOT absorbing issue #824's ±1/255 cold-raster drift into a looser threshold moved onto
    /// `PanelRaster.settled` at issue #911. This was one of three near-verbatim copies of that reasoning, and
    /// three copies is three places for it to drift silently, since a copy that quietly relaxes its bound
    /// still passes. Read `PanelRaster.settled`'s doc comment for it;
    /// `PanelGoldenParityTests.testSettledLoopsUntilTwoConsecutiveRastersAgree` and
    /// `PanelGoldenParityTests.testSettledThrowsAndNamesItsCallerRatherThanReturningAnUnsettledRaster` prove
    /// the shared loop genuinely iterates and genuinely throws.
    ///
    /// What stays here is the only part that was ever local: WHICH render is being settled, and the SUBJECT
    /// label a failure names. `file`/`line` default at the caller, so a red points at the test that drifted
    /// rather than at this wrapper.
    private func stableRender(_ fixture: PanelRenderFixture, scheme: ColorScheme,
                              appearance: NSAppearance.Name,
                              file: StaticString = #filePath, line: UInt = #line) throws -> PanelRaster {
        let subject = "\(fixture.name)/\(PanelRenderHarness.themeToken(scheme)) "
            + "under .\(appearance.rawValue)"
        return try PanelRaster.settled(subject, file: file, line: line) {
            try render(fixture, scheme: scheme, appearance: appearance)
        }
    }

    // MARK: - AXIS 1 (increased contrast) — PIN: `NSAppearance` does not reach an `ImageRenderer` render

    /// FIRST, the control that makes the pin below mean anything: is the `NSAppearance` lever LIVE AT ALL
    /// in this process?
    ///
    /// WHY THIS EXISTS — it is the guard against the exact mis-attribution this suite nearly shipped. The
    /// pin below drives `performAsCurrentDrawingAppearance` and observes no change. Read alone, that reads
    /// as "the high-contrast appearance specifically does not arrive". It is not: measured,
    /// `performAsCurrentDrawingAppearance` does not reach `ImageRenderer` for ANY appearance — even
    /// `.aqua` vs `.darkAqua`, a maximal difference, renders byte-identically. So a zero from that pin is
    /// explained entirely by the RENDER PATH ignoring appearance, and would still be zero if the
    /// high-contrast assets resolved perfectly. Without this control the pin is a degenerate-subject gate:
    /// a lever that does nothing produces a passing measurement about nothing.
    ///
    /// This test therefore pins BOTH halves, which together make the attribution honest:
    ///   • the lever IS live in this process — the SAME `performAsCurrentDrawingAppearance` block changes
    ///     an AppKit colour resolution (`NSColor.textColor` resolves near-black under `.aqua` and
    ///     near-white under `.darkAqua`), so a null result below is not a broken test harness; and
    ///   • the lever does NOT reach `ImageRenderer` — the SwiftUI path ignores it.
    ///
    /// This is also the correction to a claim worth not repeating: these pins and issue #832's asset-level
    /// pin do NOT "move together". #832 resolves colours through the AppKit path, where the lever is live,
    /// so its zero is a genuine finding about asset lookup honouring the high-contrast NAME. This suite
    /// uses a path where the lever is dead, so its zero is a different and more general platform fact. If
    /// AppKit ever starts honouring the name, #832's pin reddens and this one stays green.
    func testTheAppearanceLeverIsLiveButNeverReachesTheRenderer() throws {
        // Half 1 — the lever moves an AppKit resolution. If this stops holding, every appearance-based
        // measurement in this file (and #832's) is measuring a no-op and must be re-derived.
        func textColorComponents(_ name: NSAppearance.Name) throws -> [CGFloat] {
            var out: [CGFloat]?
            try XCTUnwrap(NSAppearance(named: name)).performAsCurrentDrawingAppearance {
                out = NSColor.textColor.usingColorSpace(.sRGB).map {
                    [$0.redComponent, $0.greenComponent, $0.blueComponent]
                }
            }
            return try XCTUnwrap(out, "NSColor.textColor did not resolve under .\(name.rawValue)")
        }
        let aquaText = try textColorComponents(.aqua)
        let darkAquaText = try textColorComponents(.darkAqua)
        XCTAssertNotEqual(aquaText, darkAquaText,
                          "`performAsCurrentDrawingAppearance` did not change even an AppKit colour "
                          + "resolution (.aqua and .darkAqua both gave \(aquaText)) — the appearance lever "
                          + "is INERT in this process, so every appearance-based null result in this file "
                          + "is a degenerate pass rather than a platform finding. Re-derive them before "
                          + "trusting any of them, and re-check issue #832's pin, which drives this path.")

        // Half 2 — that same live lever does not reach `ImageRenderer`, for ANY appearance. Deliberately
        // measured on the maximal pair rather than the high-contrast one: if even light-vs-dark does not
        // arrive, no appearance does, and the high-contrast null below needs no separate explanation.
        let fixture = try healthyFixture()
        let underAqua = try stableRender(fixture, scheme: .light, appearance: .aqua)
        let underDarkAqua = try stableRender(fixture, scheme: .light, appearance: .darkAqua)
        XCTAssertEqual(PanelRaster.byteDelta(underAqua, underDarkAqua).differing, 0,
                       "`ImageRenderer` now honours `NSAppearance` — the panel rendered differently under "
                       + ".aqua vs .darkAqua at a pinned `\\.colorScheme`. That reopens the appearance "
                       + "route as a possible seam for the increased-contrast axis (issue #760), and it "
                       + "also means `PanelGoldenParityTests.testRendersDoNotDependOnTheHostProcessAppearance` "
                       + "is now a live cross-machine risk rather than a structural certainty — the "
                       + "committed goldens would start depending on the operator's Dark Mode setting.")
    }

    /// The high-contrast names specifically, driven and measured — subordinate to the control above, which
    /// is what licenses reading this null as a fact about the platform rather than about the rig.
    ///
    /// `\.colorSchemeContrast` is get-only (see the header), so `NSAppearance` is the only way a test
    /// process could ask for increased contrast at all. It does not arrive: MEASURED byte-identical, 0
    /// bytes differing, worst channel 0, in BOTH schemes. Per the control, the CAUSE is that the SwiftUI
    /// renderer ignores appearance wholesale — not something specific to the high-contrast names.
    ///
    /// Asserted at BYTE equality rather than through `diffFraction`, deliberately: `diffFraction` ignores
    /// channel deltas under 64/255, so it would report a comfortable 0.000000 for a render that shifted by
    /// ±1 everywhere — and a faint, real high-contrast effect is exactly the thing that would hide under
    /// that threshold. The stronger claim is the one worth pinning, and it is the one that was measured.
    ///
    /// WHEN THIS REDDENS: the appearance name started reaching the render, so the axis became gateable.
    /// Build the real variant fixtures #760 asked for. Check the control above at the same time — if it
    /// reddened too, the renderer began honouring appearance generally; if only this one reddened, the
    /// high-contrast names acquired a SwiftUI-specific path, which is the more surprising outcome.
    func testTheIncreasedContrastAppearanceDoesNotReachThePanelRender() throws {
        let fixture = try healthyFixture()
        let pairs: [(ColorScheme, NSAppearance.Name, NSAppearance.Name)] = [
            (.light, .aqua, .accessibilityHighContrastAqua),
            (.dark, .darkAqua, .accessibilityHighContrastDarkAqua),
        ]
        for (scheme, plain, highContrast) in pairs {
            let baseline = try stableRender(fixture, scheme: scheme, appearance: plain)
            let variant = try stableRender(fixture, scheme: scheme, appearance: highContrast)
            let delta = PanelRaster.byteDelta(baseline, variant)
            XCTAssertEqual(delta.differing, 0,
                           "the panel rendered DIFFERENTLY under .\(highContrast.rawValue) than under "
                           + ".\(plain.rawValue) (\(delta.differing) bytes, worst channel \(delta.worst)) — "
                           + "the increased-contrast axis has become REACHABLE from a test process. That is "
                           + "good news and this pin has done its job: build the appearance-variant fixtures "
                           + "issue #760 asked for. Note this does NOT automatically move issue #832's "
                           + "asset-level pin, which drives the AppKit path — check it separately.")
        }
    }

    /// CANARY for the pin above — the mutation that must redden it, driven through its EXACT predicate
    /// (`PanelRaster.byteDelta(...).differing == 0`).
    ///
    /// The pin's passing condition is "nothing changed", which is the passing condition a completely broken
    /// rig would also report — a renderer that returned the same cached raster for every call, or a
    /// comparison that never looked at the pixels, would sail through it. So the predicate is fed two
    /// rasters KNOWN to differ (the same fixture in the two colour schemes, which the golden suite measures
    /// at ~0.97 apart) and required to say so. A green here means the pin above is discriminating; without
    /// it, that pin is not evidence.
    func testADifferingPairTripsTheReachabilityPin() throws {
        let fixture = try healthyFixture()
        let light = try stableRender(fixture, scheme: .light, appearance: .aqua)
        let dark = try stableRender(fixture, scheme: .dark, appearance: .aqua)
        let delta = PanelRaster.byteDelta(light, dark)
        XCTAssertGreaterThan(delta.differing, 0,
                             "the reachability predicate reported ZERO differing bytes between the light and "
                             + "dark renders of the same fixture, which are ~0.97 apart under the gate "
                             + "metric. The predicate cannot detect a difference, so "
                             + "`testTheIncreasedContrastAppearanceDoesNotReachThePanelRender` passing "
                             + "proves nothing about the platform")

        // The other half of the same question: a predicate that fired on EVERYTHING would satisfy the
        // assertion above while making the pin equally uninformative. Two independently-stabilized renders
        // of the SAME cell must agree exactly. Not vacuous despite the stabilization — `stableRender` only
        // proves a raster matched its immediate predecessor WITHIN one call, so this is the check that the
        // steady state is the same steady state ACROSS calls, which is what the pin relies on when it
        // compares two separately-obtained rasters.
        let againLight = try stableRender(fixture, scheme: .light, appearance: .aqua)
        XCTAssertEqual(PanelRaster.byteDelta(light, againLight).differing, 0,
                       "two independently-stabilized renders of the SAME fixture in the SAME appearance "
                       + "differ — the reachability predicate fires on rig noise, so it cannot attribute any "
                       + "difference to an appearance. If the worst channel delta is 1 over a few hundred "
                       + "bytes this is issue #824's cold-raster artifact escaping `stableRender`; diagnose "
                       + "it there rather than widening this predicate")
    }

    // MARK: - AXIS 2 + 3 (reduce transparency / reduce motion) — PIN: the render is pinned to the DEFAULTS

    /// The environment keys cannot be OVERRIDDEN (get-only — a compile error, so no test can attempt it).
    /// What a test CAN establish is the consequence, and the consequence is stronger than "you cannot
    /// change it": what SwiftUI hands a view during an `ImageRenderer` render is the DEFAULT value for all
    /// three, regardless of what the system is actually set to.
    ///
    /// MEASURED, and this pin's first draft got it wrong in a way only CI could catch. The draft asserted
    /// equality against `NSWorkspace`, on the model "the render tracks the system, so a process that cannot
    /// change the system cannot change the render" — which passes on any machine where the settings are
    /// OFF, including this one. The GitHub `macos-latest` runner has Reduce Transparency and Reduce Motion
    /// **ON**, and there the two diverge:
    ///
    ///   NSWorkspace.accessibilityDisplayShouldReduceTransparency = true   SwiftUI = false
    ///   NSWorkspace.accessibilityDisplayShouldReduceMotion       = true   SwiftUI = false
    ///
    /// So the renderer does not inherit the system setting either. That is the decisive observation, and it
    /// is only available on a machine where a setting is on — which is exactly why the equality form looked
    /// correct locally and was not. Recorded because the lesson generalises: a pin whose two sides agree by
    /// coincidence on the authoring machine is not evidence, and the cheapest falsifier for it is a machine
    /// configured differently.
    ///
    /// The consequence for #760 is a DOUBLE unreachability, and it makes the verdict firmer rather than
    /// weaker: the axis cannot be driven by injection (get-only), and it could not be driven by changing
    /// the system setting either, because the render does not read it. Nothing a test process can do
    /// reaches it.
    ///
    /// A side effect worth knowing, for the golden gate: this is *why* the committed panel goldens are
    /// portable across machines with different accessibility settings. `PanelGoldenParityTests` never had
    /// to think about it; this is the measurement that says it did not need to.
    ///
    /// WHEN THIS REDDENS: the renderer started sourcing these from somewhere — the system, or a new
    /// override seam. Check whether the keys became writable on `EnvironmentValues`; if so, the
    /// reduce-transparency axis is gateable and #760's fixtures should be built for it. Either way the
    /// panel goldens need re-examining for a new machine dependence.
    func testThePanelRenderReadsTheAccessibilityDefaultsRegardlessOfTheSystem() throws {
        let sink = AccessibilityEnvironmentSink()
        let renderer = ImageRenderer(content: AccessibilityEnvironmentProbe(sink: sink)
            .frame(width: 24, height: 24))
        renderer.scale = 1
        XCTAssertNotNil(renderer.cgImage, "the probe view did not rasterize, so it never read the environment")

        // Degenerate-subject guard: an unrendered probe leaves every field nil, so the assertions below
        // would be comparing against nothing. `testAnInjectedEnvironmentValueReachesTheProbe` is the
        // companion proof that a value which CAN be injected does arrive — together they rule out "the
        // probe reports a constant" as the explanation for the defaults below.
        let reduceTransparency = try XCTUnwrap(sink.reduceTransparency,
                                               "the probe never evaluated its body — nothing was measured")
        let reduceMotion = try XCTUnwrap(sink.reduceMotion, "the probe never evaluated its body")
        let contrast = try XCTUnwrap(sink.contrast, "the probe never evaluated its body")

        // The system's values are NOT the expectation — they are context for the failure message, so a
        // reddening reads as "the render started following the system" rather than as a bare mismatch.
        let workspace = NSWorkspace.shared
        let systemState = "system reports reduceTransparency="
            + "\(workspace.accessibilityDisplayShouldReduceTransparency), reduceMotion="
            + "\(workspace.accessibilityDisplayShouldReduceMotion), increaseContrast="
            + "\(workspace.accessibilityDisplayShouldIncreaseContrast)"

        XCTAssertFalse(reduceTransparency,
                       "SwiftUI reported `accessibilityReduceTransparency` TRUE during an ImageRenderer "
                       + "render (\(systemState)). The renderer now sources this from somewhere — the "
                       + "system, or a new override seam. If the key also became writable, the "
                       + "reduce-transparency axis is gateable: build issue #760's fixtures. Either way, "
                       + "the committed panel goldens have acquired a dependence on the operator's "
                       + "accessibility settings and are no longer machine-portable.")
        XCTAssertFalse(reduceMotion,
                       "SwiftUI reported `accessibilityReduceMotion` TRUE during an ImageRenderer render "
                       + "(\(systemState)) — see the note on the transparency assertion above. Reduce "
                       + "MOTION stays outside a still-render gate regardless (a raster encodes no motion); "
                       + "it would need an animation-aware harness, not a fixture.")
        XCTAssertEqual(contrast, .standard,
                       "SwiftUI reported `colorSchemeContrast` .increased during an ImageRenderer render "
                       + "(\(systemState)). That is the seam issue #832 looked for and did not find — "
                       + "re-open it, and build issue #760's increased-contrast fixtures.")
    }

    /// CANARY for the pin above: is the environment -> sink path the pin depends on actually LIVE?
    ///
    /// The pin asserts that what SwiftUI hands the panel equals what the system reports. The way that
    /// assertion goes wrong is not a false equality — it is a probe whose captured reading never reflected
    /// the render at all, in which case the pin compares a stale or defaulted value against the system and
    /// happens to match. So the canary drives the SAME sink through the one accessibility-adjacent key that
    /// IS writable — `\.colorScheme` — and requires the captured value to follow the injection. If the
    /// environment reaches the sink for a key we CAN move, the pin's readings for the keys we cannot move
    /// are trustworthy readings rather than defaults.
    ///
    /// Deliberately NOT the earlier form of this canary, which asserted each captured `Bool` differed from
    /// its own negation. `XCTAssertNotEqual(b, !b)` is true by the type's definition: it cannot fail under
    /// any mutation, so it certified nothing while reading as a canary — the precise failure mode this
    /// file's header warns about, found by an adversarial review of this suite rather than by inspection.
    func testAnInjectedEnvironmentValueReachesTheProbe() throws {
        func capturedScheme(_ scheme: ColorScheme) throws -> ColorScheme {
            let sink = AccessibilityEnvironmentSink()
            let renderer = ImageRenderer(content: AccessibilityEnvironmentProbe(sink: sink)
                .environment(\.colorScheme, scheme)
                .frame(width: 24, height: 24))
            renderer.scale = 1
            XCTAssertNotNil(renderer.cgImage, "the probe view did not rasterize")
            return try XCTUnwrap(sink.colorScheme, "the probe never evaluated its body — nothing was captured")
        }
        XCTAssertEqual(try capturedScheme(.light), .light,
                       "an injected `\\.colorScheme` did not reach the probe's captured reading — the "
                       + "environment→sink path is dead, so the accessibility values "
                       + "`testTheAccessibilitySettingsReachThePanelRenderOnlyFromTheSystem` compares "
                       + "against the system are defaults rather than live readings, and that pin is not "
                       + "evidence")
        XCTAssertEqual(try capturedScheme(.dark), .dark,
                       "the probe captured the same scheme for both injections — it is reporting a constant, "
                       + "so its accessibility readings are equally constant and prove nothing")
    }

    // MARK: - AC-3 evidence: removing vibrancy is a LARGE change, so the axis is not cosmetic

    /// What Reduce Transparency actually DOES is replace the popover's vibrancy with an opaque fill, so the
    /// question AC-3 asks — does the vibrancy-dependent chrome survive that — first needs an answer to
    /// "how much does the panel depend on what is behind it at all?". This measures that, and the answer is
    /// a lot.
    ///
    /// MEASURED (healthy/light, 760x898 at scale 2). The panel's `.regularMaterial` scrim
    /// (`StatusPanelView`) composites the backdrop WITHIN the SwiftUI pass, so the same panel over
    /// different opaque backdrops rasterizes differently:
    ///
    ///   over white ......... 0.000000 vs the bare render   (indistinguishable)
    ///   over 0.9 white ..... 0.000000                       (indistinguishable)
    ///   over 0.5 gray ...... 0.000000                       (indistinguishable)
    ///   over 0.1 white ..... 0.909323
    ///   over black ......... 0.933727   worst channel 90
    ///   over rgb(.9,.2,.6) . 0.929223   (a saturated non-grey, to rule out a grey-axis-only effect)
    ///
    /// The light-backdrop rows reading 0.000000 is not a null result — `.regularMaterial` in the LIGHT
    /// scheme resolves near-white, so a light backdrop lands within the metric's 64/255 channel threshold
    /// of the material's own unbacked fallback. The dark and saturated rows are the informative ones: most
    /// of the frame moves. Worth stating because the bare render is 100% OPAQUE at the alpha level
    /// (measured: 682480/682480 pixels at alpha 255, zero partial), so "the panel is opaque" and "the panel
    /// is backdrop-dependent" are BOTH true and are not in tension — the blending happens inside the
    /// SwiftUI compositing pass, before the raster gets its alpha.
    ///
    /// WHAT THIS DOES AND DOES NOT ESTABLISH. It establishes that the reduce-transparency axis is
    /// consequential — a large fraction of the panel changes when the material's backdrop does — so nobody
    /// can close the axis as "no visible effect". It does NOT establish that the panel remains LEGIBLE
    /// under the OS's actual opaque substitution, and no threshold here attempts to: the mock defines the
    /// default appearance only, so the target appearance under Reduce Transparency is an unratified design
    /// question. Asserting a legibility floor would decide it by assertion. Routed instead.
    func testThePanelRasterIsBackdropDependentSoRemovingVibrancyIsAVisibleChange() throws {
        let fixture = try healthyFixture()
        // Both sides go through the SAME composited path (a ZStack with an opaque backdrop) so the only
        // variable is the backdrop colour — comparing a composited render against the harness's own
        // uncomposited one would confound the backdrop with the wrapper.
        let overLight = try compositedRender(fixture, backdrop: .white)
        let overDark = try compositedRender(fixture, backdrop: .black)
        let dependence = PanelRaster.diffFraction(overLight, overDark)
        XCTAssertGreaterThan(dependence, backdropDependenceFloor,
                             "the panel rendered nearly the same (\(dependence)) over a white and a black "
                             + "backdrop, where 0.9337 was measured. The `.regularMaterial` scrim has "
                             + "stopped compositing what is behind it — which would mean the panel no "
                             + "longer depends on vibrancy at all. If that is intentional, this measurement "
                             + "and the AC-3 note in design/README.md are both stale; if it is not, the "
                             + "vibrancy the product aesthetic ratifies has been lost.")
    }

    /// CANARY for the measurement above, driven through its EXACT predicate
    /// (`PanelRaster.diffFraction`), at a far tighter bound than the measurement's own floor.
    ///
    /// The measurement's passing condition is "these two differ a lot", which a predicate that fired on
    /// EVERYTHING would also satisfy — including on rig noise, which would make the 0.93 meaningless. So
    /// the same predicate is fed two renders over the SAME backdrop and required NOT to fire.
    func testASameBackdropPairDoesNotTripTheDependenceMeasurement() throws {
        let fixture = try healthyFixture()
        let first = try compositedRender(fixture, backdrop: .white)
        let second = try compositedRender(fixture, backdrop: .white)
        let noise = PanelRaster.diffFraction(first, second)
        XCTAssertLessThan(noise, backdropNoiseCeiling,
                          "two renders over the SAME backdrop differ by \(noise) — the backdrop-dependence "
                          + "predicate fires on rig noise, so the 0.93 it reports between light and dark "
                          + "backdrops cannot be attributed to the backdrop")
    }

    // MARK: - Helpers

    /// Rasterize the panel composited over an opaque `backdrop`, wiring the environment exactly as
    /// `PanelRenderHarness.render` does. Deliberately a local re-wiring rather than a new harness seam:
    /// this is a MEASUREMENT of how the material behaves, not a rendering mode the app or the golden gate
    /// should acquire, and adding a backdrop parameter to the shared harness would put an unused axis into
    /// the path both committed-golden consumers run through.
    ///
    /// Warms the rasterizer through the shared harness first: the first renders in a process disagree with
    /// the steady state by ±1/255 (`PanelRenderHarness.warmUpIfNeeded`, and the honest limit issue #824
    /// records about it). Nothing here asserts at byte granularity, so that drift cannot change a verdict —
    /// but a cold first render would still make the two composited rasters incomparable for no reason.
    private func compositedRender(_ fixture: PanelRenderFixture, backdrop: Color,
                                  scheme: ColorScheme = .light) throws -> PanelRaster {
        _ = PanelRenderHarness.render(fixture, scheme: scheme)
        let store = WatchStatusStore.preview(state: fixture.state, rows: fixture.rows,
                                             nextSwap: fixture.nextSwap, generatedAt: fixture.generatedAt,
                                             canonicalScrub: fixture.canonicalScrub,
                                             keychainLocked: fixture.keychainLocked,
                                             systemicRefreshFailure: fixture.systemicRefreshFailure,
                                             systemicRefreshSource: fixture.systemicRefreshSource)
        let panel = StatusPanelView()
            .statusPanelEnvironment(store: store,
                                    capture: AccountCaptureModel(client: nil),
                                    swap: AccountSwapModel(client: nil),
                                    stats: fixture.statsWire.map { PanelStatsModel.loadedPreview($0) }
                                        ?? PanelStatsModel(client: nil),
                                    loginItem: LoginItemModel(service: AppearanceVariantLoginItemService()),
                                    // Seed the `View log` probe from the FIXTURE, exactly as
                                    // `PanelRenderHarness.render` does (issue #776). This composite is warmed by
                                    // a harness render of the same fixture above, so anything but the same
                                    // seeding would make the two disagree for `starting` / `crash-looping` —
                                    // the only two fixtures that carry a path — for no reason connected to the
                                    // appearance axes under test.
                                    daemonLog: .fixed(fixture.daemonLogPath))
            .environment(\.colorScheme, scheme)
            .tint(Color.panelAccent)
            .dynamicTypeSize(.large)
        let composited = ZStack {
            backdrop
            panel
        }
        let renderer = ImageRenderer(content: composited)
        renderer.scale = PanelRenderHarness.scale
        let cg = try XCTUnwrap(renderer.cgImage, "the composited panel did not rasterize")
        return try XCTUnwrap(PanelRaster.normalize(cg), "the composited raster did not normalize")
    }
}

// MARK: - Environment probe

/// Captures what SwiftUI hands a view for the three accessibility keys during an actual render. A class
/// rather than a return value because a `View`'s body cannot hand anything back to the test.
@MainActor
private final class AccessibilityEnvironmentSink {
    var reduceTransparency: Bool?
    var reduceMotion: Bool?
    var contrast: ColorSchemeContrast?
    /// The one WRITABLE key on the probe, captured so the canary can prove the environment→sink path is
    /// live before the pin trusts the three read-only readings above.
    var colorScheme: ColorScheme?
}

/// Reads the three keys through `@Environment` — the same path `StatusPanelView` would read them through
/// if it consulted them (it does not; see design/README.md).
private struct AccessibilityEnvironmentProbe: View {
    let sink: AccessibilityEnvironmentSink
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        sink.reduceTransparency = reduceTransparency
        sink.reduceMotion = reduceMotion
        sink.contrast = contrast
        sink.colorScheme = colorScheme
        // A body must return a View; the colour is arbitrary — callers assert on `cgImage` and on the
        // sink, never on ink (a uniform fill scores 0 under this repo's corner-relative `inkCoverage`).
        return Color.gray
    }
}

/// Hermetic `LoginItemService` for the composited measurement. Re-declared rather than shared because
/// `PanelRenderHarness`'s own is `private` to the harness, and widening that for a measurement's
/// convenience would couple them. The seed is INERT here: the composited measurement renders `healthy`,
/// which shows neither the Start-daemon card nor the capture card, so no login-item surface is drawn.
private final class AppearanceVariantLoginItemService: LoginItemService {
    let appStatus: LoginItemStatus = .enabled
    let daemonAgentStatus: LoginItemStatus = .notRegistered
    let cliManagedAgentPresent: Bool = false
    let daemonLockHeld: Bool = false
    let daemonAgentRunState: DaemonAgentRunState = .notRunning
    func registerApp() throws {}
    func unregisterApp() throws {}
    func registerDaemonAgent() throws {}
    func unregisterDaemonAgent() throws {}
    func openLoginItemsSettings() {}
}
#endif
