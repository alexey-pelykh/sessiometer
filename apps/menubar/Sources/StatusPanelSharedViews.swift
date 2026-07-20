// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The status panel's SHARED subviews (issue #640) — the three the file split kept in one place rather than
// filing under a single per-area file. `MonogramBadge` and `StatusDot` are read by BOTH the Status roster
// (`StatusPanelRoster`) and the Stats tab (`StatusPanelStats`), so neither area owns them; `BannerView` is the
// always-present honest-state header that the ROOT view (`StatusPanelView`) renders above whichever area is
// showing, so it belongs to no area either. A subview read by only one area stays `private` in that area's
// file — these three are internal purely so their callers across file boundaries can reach them.

import SwiftUI

// MARK: - Honest-state banner

/// The always-present honest-state header. A shape-and-color status dot plus a plain title/detail,
/// tinted by the banner's kind — the panel's promise that it never shows healthy on a degraded daemon.
struct BannerView: View {
    let banner: StatusPanelFormat.Banner

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Circle()
                .fill(tint)
                .frame(width: 8, height: 8)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(banner.title)
                    .font(.headline)
                Text(banner.detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(banner.title). \(banner.detail)")
    }

    private var tint: Color {
        switch banner.kind {
        case .healthy: return .green
        case .info:    return .secondary
        case .warning: return .orange
        case .error:   return .red
        }
    }
}

// MARK: - Row building blocks (per the design reference)

/// The account's monogram badge — a smart 2-char MONOGRAM over a per-account identity COLOR (issue #445),
/// both seeded from the operator `label` (never a provider brand mark or logo — #15/#173: the color is a
/// LOW-CHROMA generic identity hue with the accent EXCLUDED, and it is only ever a REDUNDANT cue beside the
/// monogram glyph + the row's label text, never color-alone — WCAG 1.4.1). The monogram is PRE-RESOLVED by
/// the parent (`RosterView` / `StatsContent`) so its collision-escalation sees every sibling label.
/// Accessibility-hidden; the row's VoiceOver label already speaks the identity.
struct MonogramBadge: View {
    let label: String
    /// The roster-resolved 2-char monogram (issue #445) — derived from the label's distinguishing token, so
    /// a same-local-part roster does not collapse to one letter. Computed once per roster by the parent.
    let monogram: String
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let dark = colorScheme == .dark
        RoundedRectangle(cornerRadius: 8)
            // Per-account identity color (issue #445), seeded from `label` — the deliberate deviation from the
            // mock's neutral `--badge-bg` monochrome badge. A low-chroma muted hue, never provider branding.
            .fill(Color.accountBadge(label, dark: dark))
            .frame(width: 30, height: 30)
            .overlay(
                Text(monogram)
                    .font(.system(size: 13, weight: .bold))
                    .tracking(0.4)
                    // High-contrast neutral glyph ON the fill (asserted ≥ 4.5:1 per slot, both themes).
                    .foregroundStyle(Color.accountMonogram(dark: dark))
            )
            .accessibilityHidden(true)
    }
}

/// The leading status dot — the design reference's per-row marker: a filled accent disc for the active
/// (being-consumed) account, a hollow ring otherwise. FILL-vs-RING is a SHAPE difference, so active is
/// legible without color (WCAG 1.4.1); the accent is a redundant cue and the row's "ACTIVE" tag +
/// VoiceOver label state it in words.
struct StatusDot: View {
    let isActive: Bool
    /// The active halo opacity is theme-aware (#388, mock `--accent-halo`); the inactive ring takes the
    /// mock's `--text-3` (a tertiary-label neutral), not a washed `secondary.opacity`.
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        Circle()
            .fill(isActive ? Color.accentColor : Color.clear)
            .overlay(
                // Inactive ring = mock `--text-3` (`.acct:not(.active) .dot { inset 0 0 0 1.5px var(--text-3) }`).
                // `.tertiaryLabelColor` is the label-family neutral the footer freshness line also uses for
                // `--text-3` (one token, one impl); it REPLACES `Color.secondary.opacity(0.55)`, which
                // rendered ≈half the mock's neutral (the #388 washout).
                Circle().strokeBorder(Color(nsColor: .tertiaryLabelColor), lineWidth: isActive ? 0 : 1.5)
            )
            .frame(width: 8, height: 8)
            // The design reference rings the active disc with a soft accent halo (`box-shadow 0 0 0 3px`) —
            // a redundant emphasis behind the fill-vs-ring shape difference, never the sole active cue. Its
            // opacity is theme-aware (#388, mock `--accent-halo`): .20 light / .30 dark.
            .background {
                if isActive {
                    Circle()
                        .fill(Color.accentEmphasis(.activeDotHalo, dark: colorScheme == .dark))
                        .frame(width: 14, height: 14)
                }
            }
            .accessibilityHidden(true)
    }
}
