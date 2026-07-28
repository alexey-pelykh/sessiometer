// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The panel's Dynamic Type scale layer (issue #756) — the ONE place a `DynamicTypeSize` becomes a number
// the panel lays out with.
//
// WHY THIS FILE EXISTS AT ALL, i.e. why not `@ScaledMetric`. Issue #756 prescribes "relative styles or
// `@ScaledMetric`". Both were MEASURED on macOS 26.5.2 / Xcode 26.6 inside the headless `MenubarTests`
// bundle, and BOTH ARE INERT on this platform:
//
//   * `@ScaledMetric(relativeTo: .body)` with a base of 100 returns **100.000 at every one of the twelve
//     `DynamicTypeSize` cases**, `.accessibility5` included. It is not "small on macOS" — it does not move.
//   * A bare `Text("Hello").font(.body)` rasterizes to **31×16 px at every one of the twelve cases**. The
//     relative text styles do not scale either.
//   * The legacy `\.sizeCategory` bridge is inert the same way (it maps onto `\.dynamicTypeSize` correctly
//     — `.accessibilityExtraExtraExtraLarge` → `.accessibility5` — and then nothing consumes the result).
//
// That is not a theory about the platform; it is what the probe printed, and the panel itself is the
// corroborating evidence: it already had ELEVEN relative-style call sites before this change — including
// `.font(.body)` on the roster account label, the most important text on the surface — and raising the
// size class moved none of them. Issue #756's problem statement ("there is no `@ScaledMetric`, no
// `dynamicTypeSize`, no relative text style anywhere") reads those absences as the CAUSE; the measurement
// says adding them would have changed nothing. So the cause is one level down: on macOS nothing CONSUMES
// the size class, and a consumer is what this file supplies.
//
// WHAT DOES WORK, and is the whole mechanism here: `\.dynamicTypeSize` **propagates** correctly. A view
// that reads it sees exactly the injected value. So the panel derives its own factor from that value and
// multiplies every font point size and every layout constant by it.
//
// THE INVARIANT — uniform proportional scale. One factor `k` multiplies EVERYTHING: fonts, cell widths,
// paddings, spacings, and the panel's own width. Therefore
//
//     scaled layout == default layout × k
//
// and every element keeps its exact SHARE of the row at every size class. Two things follow, and both are
// load-bearing:
//
//   1. It honours issue #699. That decision removed the active row's "ACTIVE" text tag because its ~56 pt
//      came out of the ACCOUNT LABEL's width and pushed a fleet of near-identical addresses into
//      truncation; the recorded principle is that protecting the label's width beats adding a cue. Under
//      uniform scale nothing can eat the label: its budget grows by the same `k` as the text inside it, so
//      its character capacity is scale-INVARIANT. A fixed-width panel with scaled chrome would do the
//      opposite — see § Rejected below, where the arithmetic is not marginal but catastrophic.
//   2. `k(.large) == 1.0` EXACTLY, so at the default size class every product is the identity (`11 * 1.0`
//      is `11` in IEEE-754, exactly) and the panel renders byte-for-byte what it rendered before. That is
//      what lets the 34 committed panel goldens (issue #754) stay valid WITHOUT a re-baseline.
//
// REJECTED ALTERNATIVES, with the arithmetic that rejected them:
//
//   * Fixed panel width, scaled text. This is what AC-2 already forbids in words ("a scaled font in a
//      fixed cell is a clipping bug, not a fix"), and the numbers are worse than they sound: a roster row's
//      FIXED columns total 16 + 8 + 30 + 28 + 45 + 6 + 60 = 193 pt of a 364 pt row. Scale those to
//      `.accessibility3` (×2.3529) and they alone want 454 pt — MORE than the whole row — so the label
//      budget does not merely shrink, it goes NEGATIVE. Unshippable, and a direct violation of #699.
//   * Reflow / condense at large sizes. Designs a SECOND panel layout that the ratified build reference
//      (`design/menubar-preview.html`) does not define — it specifies the default text size only. Inventing
//      one silently is precisely the un-ratified design decision this change must not make.
//   * Truncate earlier. Directly contradicts #699 (the label's width is the thing being protected).
//   * Clamp below `.accessibility3`. Under-delivers AC-3, which names `.accessibility3` as the ceiling the
//      panel must render correctly at.
//   * Scaling BELOW the default. Implemented, measured, and reverted — it made the roster label elide at
//      all three sub-default classes, because glyph advance does not shrink linearly while the budget does.
//      The full measurement and the #699 ground for rejecting it are on `PanelTypeScale.floor`.
//
// THE CEILING is `.accessibility3` — AC-3's own ceiling, declared with SwiftUI's first-class limiting
// modifier at the panel root and enforced again inside `factor(for:)` so the two can never disagree.
// `.accessibility4` / `.accessibility5` therefore render at `.accessibility3` sizing rather than growing
// further. Stated as a limit rather than left implicit: at ×2.3529 the healthy panel is already 894 pt
// wide and the Stats tab 1322 pt tall, and the panel has NO `ScrollView` (`StatusPanelStats` records that
// it is "fixed in WIDTH (380pt) but INTRINSIC in height"), so growing past this ceiling trades a readable
// panel for one the screen cannot show. That popover-height ceiling is a PRE-EXISTING, orthogonal limit —
// a long enough roster overflows today at the default text size — and is tracked separately rather than
// absorbed here.
//
// WHAT STILL HAS NO DRIVER, stated plainly because it bounds what a user gets today. macOS's system Text
// Size (System Settings → Accessibility → Display → Text Size) reaches only apps that adopt Apple's
// "preferred reading size" opt-in, and this app has not adopted it — that wiring is a separate item. What
// this file delivers is the CONSUMER: the panel now scales correctly and verifiably at every size class it
// is given, which is the prerequisite for any driver (the system opt-in, an in-app preference, or issue
// #757's gate) to have an effect. Without it, wiring a driver would move nothing at all.

import AppKit
import SwiftUI

/// The panel's Dynamic Type scale: a pure `DynamicTypeSize` → multiplier map, plus the `floor ... ceiling`
/// range the panel declares support over. Pure and Foundation/SwiftUI-only so `PanelTextMetricsTests` can
/// read the exact factor the views lay out with (the issue #750 discipline: one value, never a second copy).
enum PanelTypeScale {

    /// The largest size class the panel lays out for. AC-3's own ceiling; see the file header for why the
    /// panel declares a limit rather than scaling without bound.
    static let ceiling: DynamicTypeSize = .accessibility3

    /// The smallest size class the panel lays out for — the DEFAULT. The panel GROWS with the text size
    /// and never shrinks below its designed density.
    ///
    /// MEASURED, not assumed. Scaling below 1.0 was implemented first and then measured, and it pushed the
    /// realistic fleet label `oleksii@company-one.com` OVER its own budget at all three sub-default
    /// classes — by 2.45 pt at `.xSmall`, 1.47 at `.small`, 0.46 at `.medium` — so the roster label began
    /// eliding where it previously fit. The cause is that glyph advance does not shrink LINEARLY with
    /// point size (hinting and device rounding keep stems and sidebearings from following it down), while
    /// the budget does; at the default size that label already clears its 171 pt budget by only ~0.97 pt
    /// (issue #750 measured 170.03 pt), so there is nothing to absorb the divergence.
    ///
    /// Shrinking is therefore rejected on the ratified ground that decided issue #699: the active row's
    /// "ACTIVE" tag was REMOVED because its width came out of the account label and pushed near-identical
    /// fleet addresses into truncation — protecting the label's width beats the competing consideration.
    /// A sub-default class buys no ACCESSIBILITY (issue #756 is a defect about RAISING the text size) and
    /// costs exactly the truncation #699 spent a decision removing, so the trade is one-sided.
    ///
    /// The consequence is stated rather than buried: the panel does not respond to a text size SMALLER
    /// than the default — those classes render identically to it. That is a deliberate asymmetry.
    static let floor: DynamicTypeSize = .large

    /// The multiplier applied to every font point size and every layout constant.
    ///
    /// The curve is Apple's own published Dynamic Type progression for the body text style — 14, 15, 16,
    /// **17**, 19, 21, 23, 28, 33, 40, 47, 53 pt — expressed as a ratio against the `.large` default of 17.
    /// Using the platform's published progression rather than a hand-picked one keeps the panel's growth
    /// matched to what the rest of the system does at each step, including the deliberately large jump into
    /// the accessibility sizes.
    ///
    /// `.large` maps to EXACTLY `1.0`, which is what makes this change a no-op at the default size class
    /// (see the file header's invariant 2). Sizes are clamped into `floor ... ceiling`, so everything at or
    /// below the default returns exactly 1.0 and everything above the ceiling returns the ceiling's factor.
    static func factor(for size: DynamicTypeSize) -> Double {
        switch min(max(size, floor), ceiling) {
        // UNREACHABLE below `floor` — `max(size, floor)` already raised them to `.large` — but they are
        // known cases the compiler still requires. They return the default's factor, which is what the
        // floor MEANS; writing their smaller published ratios here would contradict the clamp.
        case .xSmall, .small, .medium: return 1.0
        case .large:   return 1.0
        case .xLarge:  return 19.0 / 17.0
        case .xxLarge: return 21.0 / 17.0
        case .xxxLarge: return 23.0 / 17.0
        case .accessibility1: return 28.0 / 17.0
        case .accessibility2: return 33.0 / 17.0
        case .accessibility3: return 40.0 / 17.0
        // `.accessibility4` / `.accessibility5` are UNREACHABLE — `min(size, ceiling)` already capped them
        // to `.accessibility3` — but they are known cases, so the compiler still requires them listed.
        // They return the ceiling's factor, which is what the clamp means; writing their own larger ratios
        // here would create a second, contradictory answer for a value the clamp says cannot arrive.
        case .accessibility4, .accessibility5: return 40.0 / 17.0
        // `DynamicTypeSize` is non-frozen: a future case behaves like the largest SUPPORTED one rather
        // than falling through to unscaled (which would look like the very defect this file fixes).
        @unknown default: return 40.0 / 17.0
        }
    }

    /// The `ControlSize` a bordered control takes at `scale` — the ONE lever that moves AppKit-backed
    /// controls, because `.buttonStyle(.borderedProminent)` **substitutes its own font** and silently
    /// ignores an outer `.font()`. That is measurable, not theoretical: with the `.font(.panel(…))`
    /// modifier alone the Swap / Capture / Start-daemon buttons rendered 97×40, 332×42 and 218×40 px at
    /// BOTH `.large` and `.accessibility3` while the panel around them went 760 → 1789 px wide. A font
    /// modifier that looks like it scales and does not is exactly the defect this file exists to fix, so
    /// the three prominent buttons drive this instead.
    ///
    /// The trade-off, stated plainly: `ControlSize` is a 4-value ENUM, not a multiplier, so these controls
    /// track the text size in STEPS while everything else scales continuously — and macOS 13 (this app's
    /// deployment target) tops out at `.large`; `.extraLarge` is 14+. Above `.xxxLarge` the buttons
    /// therefore saturate: at `.accessibility3` the panel is at ×2.35 and its buttons are not. The
    /// alternative — a hand-rolled `ButtonStyle` that scales continuously — was rejected: it would replace
    /// the panel's ONE native accent action (issue #169's Von Restorff primary) with a look the design
    /// reference never ratified, and would churn all 34 committed goldens to do it. A stepped native
    /// control beats a continuous unratified one; issue #757's gate is where the saturation gets measured.
    ///
    /// `.mini` is unreachable by construction — the scale floor is `.large`, so `scale` is never below 1.0.
    static func controlSize(for scale: Double) -> ControlSize {
        // Thresholds sit BETWEEN the published factors, never on one, so no size class lands on a boundary:
        // `.large` 1.0 → small; `.xLarge` 1.118 / `.xxLarge` 1.235 → regular; `.xxxLarge` 1.353 and every
        // accessibility class → large.
        switch scale {
        case ..<1.09: return .small
        case ..<1.30: return .regular
        default: return .large
        }
    }
}

// MARK: - Environment plumbing

/// The panel's scale factor, injected ONCE at the panel root (`StatusPanelView`) from the clamped
/// `\.dynamicTypeSize` and read by every subview.
///
/// Injected rather than re-derived per view so there is a single computation of the factor, and so a test
/// (or issue #757's gate) can drive the whole panel through one seam. The root is the ONLY writer;
/// `PanelTypeScaleTests` asserts the injected value equals `PanelTypeScale.factor(for:)` of the size class
/// the environment actually carries, so the two representations cannot drift apart.
private struct PanelScaleKey: EnvironmentKey {
    /// `1.0` — the default size class's factor, so a view rendered outside the panel root (a preview, an
    /// isolated unit test) lays out exactly as it did before this change rather than collapsing to zero.
    static let defaultValue: Double = 1.0
}

extension EnvironmentValues {
    /// The panel's uniform Dynamic Type multiplier. See `PanelTypeScale`.
    var panelScale: Double {
        get { self[PanelScaleKey.self] }
        set { self[PanelScaleKey.self] = newValue }
    }
}

// MARK: - Scaled fonts

extension Font {

    /// A panel font at `points` **before** scaling, multiplied by the panel's Dynamic Type factor.
    ///
    /// The one call shape every fixed-size panel font goes through, so "did this site get scaled?" is a
    /// grep rather than a reading exercise.
    static func panel(_ points: Double, _ weight: Font.Weight = .regular, scale: Double) -> Font {
        .system(size: points * scale, weight: weight)
    }

    /// A panel font tracking an AppKit TEXT STYLE's point size, multiplied by the panel's factor.
    ///
    /// Used where the panel previously wrote a relative style (`.font(.body)`, `.font(.caption)`, …). Those
    /// styles do not scale on macOS (file header), but their POINT SIZES are still the platform's own
    /// metric, so reading them at runtime keeps the panel tracking the system rather than freezing today's
    /// numbers as literals — the same reasoning `PanelTextMetricsTests` already applies to the roster
    /// label's font ("READ from the text style rather than hardcoded to 13").
    ///
    /// The WEIGHT must be passed explicitly: a text style carries a weight that `Font.system(size:)` does
    /// not inherit. The equivalences were established by rasterizing both forms and comparing bytes, so
    /// each call site's default-size render is unchanged — `.body` is regular, `.headline` is **bold**,
    /// `.subheadline` regular, `.caption` regular, and `.caption2` **medium**.
    static func panel(style: NSFont.TextStyle, _ weight: Font.Weight = .regular, scale: Double) -> Font {
        .system(size: NSFont.preferredFont(forTextStyle: style).pointSize * scale, weight: weight)
    }
}
