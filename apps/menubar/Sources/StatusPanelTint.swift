// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The panel's ONE SwiftUI-side color seam (issue #388), split out of `StatusPanelView` by #640. It resolves the
// Foundation-only tint / fill / spark tokens — which `StatusPanelFormat` cannot name a `Color` for itself — into
// concrete SwiftUI `Color`s. The role → token tables stay in `StatusPanelFormat` (unit-tested); this file performs
// only the final, untestable SwiftUI conversion, so no panel file ever composes a color by hand.

import SwiftUI

extension Color {
    /// Resolve a Foundation-only `StatusPanelFormat.PanelTint` to a concrete `Color` (#388): an
    /// asset-catalog color set (theme-adaptive Any/Dark + Increased-Contrast) from the app's main bundle,
    /// or a system semantic color. This is the ONE SwiftUI-side seam; the role→token table stays in
    /// `StatusPanelFormat` (Foundation-only, unit-tested), which cannot name a `Color` itself.
    static func panel(_ tint: StatusPanelFormat.PanelTint) -> Color {
        switch tint {
        case .asset(let name): return Color(name, bundle: .main)
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
        Color.accentColor.opacity(StatusPanelFormat.accentOpacity(emphasis, dark: dark))
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
