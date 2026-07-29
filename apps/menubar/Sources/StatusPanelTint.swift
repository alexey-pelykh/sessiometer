// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The panel's ONE SwiftUI-side color seam (issue #388), split out of `StatusPanelView` by #640. It resolves the
// Foundation-only tint / fill / spark tokens — which `StatusPanelFormat` cannot name a `Color` for itself — into
// concrete SwiftUI `Color`s. The role → token tables stay in `StatusPanelFormat` (unit-tested); this file performs
// only the final, untestable SwiftUI conversion, so no panel file ever composes a color by hand.

import SwiftUI

/// Anchors `Color.panelAssets` to whichever bundle the panel's code was compiled into — see that property.
private final class PanelAssetAnchor {}

extension Color {
    /// The bundle the panel's asset-catalog colour sets resolve from (issue #754).
    ///
    /// `Bundle(for:)` rather than `.main`, for one reason: the panel is compiled into TWO bundles. In the
    /// app this resolves to the app bundle, which IS `.main` — no behaviour change. In `MenubarTests`
    /// (`TEST_HOST: ""`) `Bundle.main` is the `xctest` runner, which carries no `Assets.car`, so
    /// `Color("HealthOK", bundle: .main)` would silently fail to resolve and the panel golden gate would
    /// bake a wrong-coloured reference — the BASELINE TRAP (#437), where the gate then defends the defect.
    /// The test bundle compiles `Sources/Assets.xcassets` (project.yml), so `Bundle(for:)` finds the real
    /// colour sets there. Same idiom, same reason as `StatusGauge.image(for:in:)`'s explicit bundle
    /// parameter, which `BarGlyphParityTests` needs for exactly this.
    static var panelAssets: Bundle { Bundle(for: PanelAssetAnchor.self) }

    /// The brand accent (#391), resolved EXPLICITLY from the `AccentColor` asset rather than through
    /// `Color.accentColor` (issue #754).
    ///
    /// In the app the two name the same colour — `ASSETCATALOG_COMPILER_GLOBAL_ACCENT_COLOR_NAME` pins
    /// `Color.accentColor` to this very asset so a non-blue macOS system accent cannot drift the panel hue
    /// off the design mock. But that is a build setting on the APP target: anywhere else the panel is
    /// compiled, `Color.accentColor` falls back to the OPERATOR'S system accent. Naming the asset directly
    /// makes the #391 pin a property of the code rather than of one target's build settings, which is what
    /// lets the panel golden gate produce machine-independent references.
    ///
    /// SAME COLOUR IS NOT SAME PIXELS, and the difference was measured rather than assumed. Switching the
    /// three use sites to this property changed the app's own render on 22 of the 34 golden cells the
    /// catalog held when it was measured (#391; it has since grown), by up to
    /// 21/255 on a single channel over ~30 % of pixels (A/B against a build-level control that confirmed the
    /// render is otherwise byte-deterministic across independent builds). `AccentColor.colorset` is sRGB
    /// #007AFF / #0A84FF — nominally the macOS default accent — so this is a dynamic-vs-static colour
    /// RESOLUTION shift, not a hue change: visually imperceptible, and invisible to the golden gate, whose
    /// metric ignores channel deltas under 64/255. Recorded because "changes nothing" would have been the
    /// convenient claim and it is false; what this actually does is pin the panel accent to the brand asset
    /// instead of to a dynamic system colour that happens to match it.
    static var panelAccent: Color { Color("AccentColor", bundle: panelAssets) }

    /// Resolve a Foundation-only `StatusPanelFormat.PanelTint` to a concrete `Color` (#388): an
    /// asset-catalog color set (theme-adaptive Any/Dark + Increased-Contrast) from the bundle the panel
    /// was compiled into, or a system semantic color. This is the ONE SwiftUI-side seam; the role→token
    /// table stays in `StatusPanelFormat` (Foundation-only, unit-tested), which cannot name a `Color`
    /// itself.
    static func panel(_ tint: StatusPanelFormat.PanelTint) -> Color {
        switch tint {
        case .asset(let name): return Color(name, bundle: panelAssets)
        case .secondary:       return .secondary
        case .primary:         return .primary
        }
    }

    /// Build a neutral panel FILL (#388) from the testable `StatusPanelFormat.neutralFill` spec as a PLAIN
    /// sRGB translucent color — deliberately NOT routed through the panel material, so the source-over
    /// composite matches the mock's rgba math. This REPLACES `Color.secondary.opacity(k)` for chrome fills:
    /// `.secondary` is a label-family tint (already ~0.5 alpha over base ~(60,60,67)), so opacity-ing it for
    /// a fill washed out at ≈half the mock's alpha over the wrong hue (the #388 washout). The theme value is
    /// chosen by the caller from `@Environment(\.colorScheme)`.
    static func panelFill(_ role: StatusPanelFormat.NeutralFillRole, dark: Bool) -> Color {
        let c = StatusPanelFormat.neutralFill(role, dark: dark)
        return Color(.sRGB, red: c.red, green: c.green, blue: c.blue, opacity: c.alpha)
    }

    /// Build an accent-emphasis fill (#388) — `Color.accentColor` at the role's theme-aware `accentOpacity`.
    /// The accent counterpart to `panelFill`: it centralises the `accentColor.opacity(accentOpacity(…))`
    /// composition so each call site names the emphasis SURFACE, not the mechanism. The accent HUE stays the
    /// brand-blue `AccentColor` asset (#391); only the theme-variant alpha comes from the token.
    static func accentEmphasis(_ emphasis: StatusPanelFormat.AccentEmphasis, dark: Bool) -> Color {
        Color.panelAccent.opacity(StatusPanelFormat.accentOpacity(emphasis, dark: dark))
    }

    /// The Stats sparkline stroke / area / dot color (#446) — mock `--spark`, from the testable
    /// `StatusPanelFormat.sparkColor` spec (a plain sRGB translucent color, like `panelFill`). The area is
    /// this at a fraction of the alpha (drawn by the view: mock `.sp-area { fill-opacity:.2 }`).
    static func spark(dark: Bool) -> Color {
        let c = StatusPanelFormat.sparkColor(dark: dark)
        return Color(.sRGB, red: c.red, green: c.green, blue: c.blue, opacity: c.alpha)
    }

    /// The Stats signal pill's background fill (#446) — mock `--sig-*-bg`, from `StatusPanelFormat.statsSignalFill`.
    static func statsSignalFill(_ signal: StatusPanelFormat.StatSignal, dark: Bool) -> Color {
        let c = StatusPanelFormat.statsSignalFill(signal, dark: dark)
        return Color(.sRGB, red: c.red, green: c.green, blue: c.blue, opacity: c.alpha)
    }

    /// The Stats signal pill's foreground (label + dot) color (#446) — mock `--sig-*-fg`, from
    /// `StatusPanelFormat.statsSignalText`.
    static func statsSignalText(_ signal: StatusPanelFormat.StatSignal, dark: Bool) -> Color {
        let c = StatusPanelFormat.statsSignalText(signal, dark: dark)
        return Color(.sRGB, red: c.red, green: c.green, blue: c.blue, opacity: c.alpha)
    }

    /// The per-account badge FILL (#445) — the `label`-seeded palette hue, as a plain sRGB color (like
    /// `panelFill`). A LOW-CHROMA muted identity tone, never provider branding (#173); the accent hue is
    /// excluded. Resolved by the testable `StatusPanelFormat.accountBadgeFill`.
    static func accountBadge(_ label: String, dark: Bool) -> Color {
        let c = StatusPanelFormat.accountBadgeFill(for: label, dark: dark)
        return Color(.sRGB, red: c.red, green: c.green, blue: c.blue, opacity: c.alpha)
    }

    /// The account MONOGRAM glyph color (#445) — the high-contrast neutral that reads on the badge fill in
    /// both themes (asserted ≥ 4.5:1 against every slot). From `StatusPanelFormat.accountMonogramColor`.
    static func accountMonogram(dark: Bool) -> Color {
        let c = StatusPanelFormat.accountMonogramColor(dark: dark)
        return Color(.sRGB, red: c.red, green: c.green, blue: c.blue, opacity: c.alpha)
    }
}
