// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The status panel's chrome (issue #326) — header, callouts, and footer — split out of `StatusPanelView` by #640:
// the app-identity header with its Status|Stats switcher, the dropped-connection honest strip, the swap-callout
// hero with the settled swap's shared outcome line, and the snapshot-age footer. The hero is the panel's ONE accent
// action — the daemon's own `next_swap` recommendation, sent WYSIWYG (issue #169, Von Restorff) — and the outcome
// line is shared by BOTH swap paths, since the daemon's single-writer lock allows at most one swap in flight.
// Everything else here is read-only; every string comes from `StatusPanelFormat`.

import SwiftUI

// MARK: - Header + callouts (per the design reference)

/// The app-identity header — a neutral gauge glyph, the product name, and the honest identity sub-line
/// (`StatusPanelFormat.headerSubtitle`). Always present; the SUB-LINE — never the glyph — carries the
/// connection state, so a degraded daemon reads "last-known" / "· stale", never a false "active".
/// Provider-neutral (issue #173): a generic gauge, no brand mark or color.
struct PanelHeader: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let subtitle: String
    /// Whether to show the Status|Stats seg control (issue #446). Only where the Stats tab can deliver (a
    /// live roster, not the capture surface; gated in `content`). Defaults off, so every degraded-state
    /// header is byte-unchanged from before #446.
    var showsSwitcher: Bool = false
    @EnvironmentObject private var stats: PanelStatsModel
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 10 * scale) {
            // The identity block (glyph + name + sub-line) combines into ONE accessibility element; the seg
            // control keeps its own button traits alongside it, so VoiceOver reads "Sessiometer, …" then the
            // two tab buttons rather than one merged blob.
            HStack(spacing: 10 * scale) {
                RoundedRectangle(cornerRadius: 7 * scale)
                    // Mock `--badge-bg` neutral fill (#388) — replaces a washed `Color.secondary.opacity(0.16)`.
                    .fill(Color.panelFill(.badge, dark: colorScheme == .dark))
                    .frame(width: 27 * scale, height: 27 * scale)
                    .overlay(
                        Image(systemName: "gauge.medium")
                            .font(.panel(14, .semibold, scale: scale))
                            .foregroundStyle(.primary)
                    )
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 1 * scale) {
                    Text("Sessiometer")
                        .font(.panel(13.5, .semibold, scale: scale))
                    Text(subtitle)
                        .font(.panel(11, scale: scale))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Sessiometer. \(subtitle)")

            Spacer(minLength: 0)

            if showsSwitcher {
                // Mock `.seg` — right-aligned Status|Stats switcher (issue #446). Drives the stats model's
                // tab selection; switching TO Stats triggers the one-shot socket query.
                PanelTabSwitcher(tab: stats.tab) { stats.select($0) }
            }
        }
        .padding(.horizontal, 14 * scale).padding(.top, 12 * scale).padding(.bottom, 11 * scale)
    }
}

/// The mock's `.seg` Status|Stats control (issue #446): a rounded two-button switcher, right-aligned in the
/// header. The active tab carries the raised `--seg-on` chip; the inactive is a quiet transparent button.
/// Provider-neutral, read-only chrome — selecting Stats only QUERIES (UI never acts). The seg colors are the
/// mock's exact `--seg-*` chrome values inline (decorative control chrome, not a data-bearing tint — the
/// data colors, `--spark` / `--sig-*`, live in the testable `StatusPanelFormat` layer).
private struct PanelTabSwitcher: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let tab: PanelStatsModel.Tab
    let select: (PanelStatsModel.Tab) -> Void
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 2 * scale) {
            segButton("Status", target: .status)
            segButton("Stats", target: .stats)
        }
        .padding(2 * scale)
        .background(
            RoundedRectangle(cornerRadius: 7 * scale)
                .fill(segBackground)
                .overlay(RoundedRectangle(cornerRadius: 7 * scale).strokeBorder(segBorder, lineWidth: 0.5 * scale))
        )
    }

    private func segButton(_ title: String, target: PanelStatsModel.Tab) -> some View {
        let on = tab == target
        return Button { select(target) } label: {
            Text(title)
                .font(.panel(11, on ? .semibold : .medium, scale: scale))
                .foregroundStyle(on ? Color.primary : Color.secondary)
                .padding(.horizontal, 9 * scale)
                .padding(.vertical, 2.5 * scale)
                .background(
                    RoundedRectangle(cornerRadius: 5 * scale)
                        .fill(on ? segOnFill : Color.clear)
                        .shadow(color: on ? Color.black.opacity(0.18) : .clear,
                                radius: 0.75 * scale, y: 0.5 * scale)
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .accessibilityAddTraits(on ? [.isButton, .isSelected] : .isButton)
    }

    // Mock `--seg-bg` / `--seg-border` / `--seg-on` (light / dark), inline as exact sRGB chrome values.
    private var dark: Bool { colorScheme == .dark }
    private var segBackground: Color {
        dark ? Color(.sRGB, white: 1, opacity: 0.08)
             : Color(.sRGB, red: 120.0 / 255, green: 120.0 / 255, blue: 128.0 / 255, opacity: 0.12)
    }
    private var segBorder: Color {
        dark ? Color(.sRGB, white: 1, opacity: 0.06) : Color(.sRGB, white: 0, opacity: 0.05)
    }
    private var segOnFill: Color {
        dark ? Color(.sRGB, white: 1, opacity: 0.18) : Color(.sRGB, white: 1, opacity: 1)
    }
}

/// The honest strip shown over a dimmed last-known roster on a DROPPED connection — the design
/// reference's disconnected bar. States the degradation loudly (tinted, titled) so the retained numbers
/// below are never mistaken for live (#137). Richer per-state strips (keychain-locked "paused", a
/// Reconnect action) are #169.
struct HonestStrip: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let banner: StatusPanelFormat.Banner

    var body: some View {
        HStack(spacing: 8 * scale) {
            Image(systemName: "bolt.horizontal.circle")
                .font(.panel(style: .caption1, scale: scale))
                .accessibilityHidden(true)
            Text(banner.title)
                .font(.panel(11.5, .semibold, scale: scale))
            Text(banner.detail)
                .font(.panel(11.5, scale: scale))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 0)
        }
        .foregroundStyle(stripTint)
        .padding(.horizontal, 14 * scale).padding(.vertical, 9 * scale)
        .background(stripTint.opacity(0.12))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(banner.title). \(banner.detail)")
    }

    private var stripTint: Color {
        switch banner.kind {
        case .healthy: return .green
        case .info:    return .secondary
        case .warning: return .orange
        case .error:   return .red
        }
    }
}

/// The swap-callout hero — the design reference's primary action: the daemon's `next_swap` target, the
/// daemon's OWN "why" line (issue #393 — carried on the wire, no longer client-derived), and the live
/// Swap button. Accent-tinted: this is the panel's ONE accent action, the daemon's RECOMMENDATION
/// (Von Restorff — the quiet per-row switches are the operator choosing instead).
///
/// WYSIWYG (issue #169): the button sends the `target` this card DISPLAYS — never a client re-pick, and
/// never a targetless "swap to whatever you'd choose" verb. It is the same `swap` command a per-row
/// switch sends; the daemon re-validates it either way.
struct SwapCalloutCard: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let target: String
    /// The daemon's selection rationale for `target`, already rendered from the wire
    /// `NextSwap.target` reason (issue #393); `nil` for a pre-#393 daemon that sent no reason, in
    /// which case the card shows just the target label.
    let reason: String?

    @EnvironmentObject private var swap: AccountSwapModel
    /// The callout's accent-tint fill + border opacities are theme-aware (#388): the mock raises them in dark.
    @Environment(\.colorScheme) private var colorScheme

    /// The in-flight swap is this card's own target (as opposed to a per-row switch elsewhere).
    private var isSwitchingToTarget: Bool { swap.phase.pendingTarget == target }

    var body: some View {
        HStack(spacing: 10 * scale) {
            // The card's TEXT is one combined VoiceOver element; the button below is a SEPARATE one.
            // (Combining the whole card, as this did while the button was dead, would now swallow a live
            // control and leave it unreachable.)
            VStack(alignment: .leading, spacing: 1 * scale) {
                // MIDDLE-truncate the TARGET label (issue #445), keeping the "→" prefix whole, so a
                // same-local-part target's distinguishing suffix survives the elision (the earlier "clunky"
                // read was a tail-truncated target). The prefix is `.fixedSize`d; the target absorbs the
                // squeeze. That prefix is a BARE arrow (issue #698) — no leading icon, no "Next swap"
                // words: the adjacent Swap button already names the verb, and the width they cost is width
                // the target needs. `accessibilityText` deliberately does NOT match — VoiceOver reads this
                // text element on its own, so it still speaks the whole "Next swap to …" sentence.
                HStack(spacing: 0) {
                    Text("→ ").fixedSize()
                    Text(target).fontWeight(.semibold)
                        .lineLimit(1)
                        .truncationMode(StatusPanelFormat.identityElision.truncationMode)
                }
                .font(.panel(12, scale: scale))
                if let reason {
                    Text(reason)
                        .font(.panel(10.5, scale: scale))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(accessibilityText)

            Spacer(minLength: 6 * scale)

            Button(action: { Task { await swap.swap(to: target) } }) {
                if isSwitchingToTarget {
                    HStack(spacing: 5 * scale) {
                        ProgressView().controlSize(PanelTypeScale.controlSize(for: scale))
                        Text(StatusPanelFormat.swapPendingText)
                    }
                } else {
                    Text("Swap")
                }
            }
            .font(.panel(12, .semibold, scale: scale))
            .buttonStyle(.borderedProminent)
            .controlSize(PanelTypeScale.controlSize(for: scale))
            // Any in-flight swap disables this button too — the footer and the rows are siblings on the
            // one `swap` verb, and the daemon holds a single-writer lock behind it.
            .disabled(swap.phase.isPending)
            .help(StatusPanelFormat.switchHelpText(label: target))
            .accessibilityLabel(isSwitchingToTarget
                                ? "Switching to \(target)"
                                : StatusPanelFormat.switchHelpText(label: target))
        }
        .padding(.leading, 11 * scale).padding(.trailing, 8 * scale).padding(.vertical, 9 * scale)
        // Fill + border opacities are theme-aware (#388, mock `--accent-tint` / `--accent-tint-border`):
        // .10/.20 light, .16/.30 dark — the dark callout was too faint hardcoded to the light values.
        .background(
            RoundedRectangle(cornerRadius: 9 * scale)
                .fill(Color.accentEmphasis(.swapCalloutFill, dark: colorScheme == .dark))
                .overlay(RoundedRectangle(cornerRadius: 9 * scale)
                    .strokeBorder(Color.accentEmphasis(.swapCalloutBorder, dark: colorScheme == .dark), lineWidth: 0.5 * scale))
        )
        .padding(.horizontal, 8 * scale).padding(.top, 9 * scale).padding(.bottom, 4 * scale)
    }

    /// The spoken label for the card's text — delegated to `StatusPanelFormat.swapCalloutAccessibilityLabel`
    /// (#702) so the #698 spoken-label invariant (keep the "Next swap to " prefix; no dangling ". ." when the
    /// reason is absent) is guarded by a direct unit test rather than resting on code review. The Swap button
    /// speaks for itself.
    private var accessibilityText: String {
        StatusPanelFormat.swapCalloutAccessibilityLabel(target: target, reason: reason)
    }
}

/// The not-running state's Start-daemon affordance (issue #170) — the design mock's not-running card. It
/// reuses the honest-state `BannerView` for the "Daemon not running" message (consistent with its sibling
/// cold states — connecting / starting / crash-looping all render `BannerView`), then offers a primary
/// **Start daemon** button that registers (and, via the plist's `RunAtLoad`, starts) the embedded daemon
/// LaunchAgent through `SMAppService`.
///
/// Honest degradation (the crown-jewel rule, StatusPanelView's honest-affordance discipline): the button
/// appears ONLY where it can act — `LoginItemModel.canStartDaemon`, i.e. the bundled agent is present (#171
/// ships it) AND no CLI-managed agent already owns the label. In the #170 shipped state no plist is bundled
/// yet, so `canStartDaemon` is false and the card is exactly the inert banner it was before — never a dead
/// button over a daemon it can't start. On success the daemon comes up and the panel leaves `.notRunning`
/// on the next `watch` snapshot (like a swap's new active row); a failure surfaces its reason inline.
struct StartDaemonCard: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    @EnvironmentObject private var loginItem: LoginItemModel

    /// The Start registration is in flight (the transient spinner beat).
    private var isRegistering: Bool {
        if case .registering = loginItem.startPhase { return true }
        return false
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8 * scale) {
            BannerView(banner: StatusPanelFormat.banner(for: .notRunning, accountCount: 0))
            if loginItem.canStartDaemon {
                startButton
                if case .failed(let reason) = loginItem.startPhase {
                    Label(reason, systemImage: "exclamationmark.triangle.fill")
                        .font(.panel(11, scale: scale))
                        .foregroundStyle(.red)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Text(StatusPanelFormat.startDaemonHint)
                    .font(.panel(10.5, scale: scale))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var startButton: some View {
        Button(action: { Task { await loginItem.startDaemon() } }) {
            if isRegistering {
                HStack(spacing: 5 * scale) {
                    ProgressView().controlSize(PanelTypeScale.controlSize(for: scale))
                    Text(StatusPanelFormat.startDaemonPendingText)
                }
            } else {
                Label(StatusPanelFormat.startDaemonButtonTitle, systemImage: "play.fill")
            }
        }
        .font(.panel(12, .semibold, scale: scale))
        .buttonStyle(.borderedProminent)
        .controlSize(PanelTypeScale.controlSize(for: scale))
        .disabled(isRegistering)
        .accessibilityLabel(isRegistering
                            ? StatusPanelFormat.startDaemonPendingText
                            : StatusPanelFormat.startDaemonButtonTitle)
    }
}

/// The settled swap's inline outcome (issue #169) — one line beneath the swap-callout card, shared by
/// BOTH swap paths (the footer recommendation and a per-row manual switch), because the daemon holds a
/// single-writer swap lock: at most one swap is ever in flight, so at most one outcome needs a home.
///
/// PENDING renders nothing here — it is shown ON the clicked row / the footer button, where the operator
/// is already looking; a second spinner would be noise. `done` clears itself after a short beat; a
/// `failed` persists until the next swap attempt, so an error the operator has not read cannot vanish.
struct SwapStatusLine: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    @EnvironmentObject private var swap: AccountSwapModel

    var body: some View {
        switch swap.phase {
        case .idle, .pending:
            EmptyView()
        case .done(let success):
            line(StatusPanelFormat.swapDoneText(success),
                 symbol: "checkmark.circle.fill", tint: .green)
                .lineLimit(1)
                .truncationMode(StatusPanelFormat.identityElision.truncationMode)
        case .failed(let failure):
            line(StatusPanelFormat.swapErrorText(failure),
                 symbol: "exclamationmark.triangle.fill", tint: .red)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func line(_ text: String, symbol: String, tint: Color) -> some View {
        Label(text, systemImage: symbol)
            .font(.panel(11, scale: scale))
            .foregroundStyle(tint)
            .padding(.horizontal, 12 * scale).padding(.vertical, 2 * scale)
    }
}

// MARK: - Footer

/// The snapshot-age footer (issue #355 / #164 `generated_at`) — the design reference's freshness line,
/// "updated Ns ago". `next_swap` is NOT here (it lives in the swap-callout hero; a dropped daemon shows
/// no card, so the two never collide). Amber when the reading should be distrusted (a wedged poll loop,
/// or a stale/disconnected connection), never frozen-as-fresh (#137).
struct FooterView: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let text: String
    let stale: Bool

    var body: some View {
        VStack(spacing: 0) {
            Divider()
            HStack(spacing: 5 * scale) {
                Image(systemName: "clock")
                    .font(.panel(style: .caption2, .medium, scale: scale))
                    .accessibilityHidden(true)
                Text(text)
                    .font(.panel(11, scale: scale))
                    .monospacedDigit()
                Spacer(minLength: 0)
            }
            // Mock `.pop-foot .fl2 { color: var(--text-3) }` — the snapshot-age line is tertiary; the mock's
            // `.fl2.stale { color: var(--ut-a) }` turns it amber only when the reading should be distrusted
            // (wedged poll loop / stale / disconnected). That amber is the SAME contrast-safe `--ut-a` token
            // as the stale auth glyph (#388) — small text on the vibrancy, so never raw system orange (< 4.5:1).
            .foregroundStyle(stale ? .panel(StatusPanelFormat.healthTint(.yellow)) : Color(nsColor: .tertiaryLabelColor))
            .padding(.horizontal, 14 * scale).padding(.top, 9 * scale).padding(.bottom, 11 * scale)
        }
        .padding(.top, 5 * scale)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(stale ? "\(text), stale" : text)
    }
}
