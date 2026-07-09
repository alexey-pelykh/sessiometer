// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The `NSPopover`-hosted SwiftUI status panel (issue #326): the click-panel surface of the menu-bar
// app, showing the same per-account detail the `status` verb prints. It is a THIN VIEW over
// `WatchStatusStore` (#324) read through `.environmentObject` — every string/glyph it renders comes
// from the pure, `src/cli.rs`-mirroring `StatusPanelFormat` (byte-parity-tested in
// `StatusPanelFormatTests`), so the panel itself holds no formatting logic to get wrong.
//
// Honest-state discipline (the crown-jewel invariant, ADR-0003 UI analogue): a banner ALWAYS states
// the connection status, the roster renders LIVE only on `.connected` and DIMMED-but-retained on every
// degraded/absent state (never frozen-as-live), the empty-roster state shows an onboarding card
// distinct from daemon-down, and a breaking-schema daemon refuses its numbers. The panel never runs a
// command — the onboarding card COPIES `sessiometer capture` to the clipboard (design-menubar
// "copy-command, never a runner").
//
// Provider-neutral by construction: the wire carries only the operator-chosen `label` (never an email
// — issue #15) and no provider field, so a row is plain text with no brand color or logo. Every row is
// one VoiceOver element speaking `StatusPanelFormat.rowAccessibilityLabel`.

import AppKit
import SwiftUI

/// The root panel. Observes the store and re-derives the reset-in against the client's own wall clock
/// on a periodic `TimelineView` tick (issue #326: "computed against the client's own clock"), so a
/// resting popover keeps its "resets in" honest without a manual refresh.
struct StatusPanelView: View {
    @EnvironmentObject private var store: WatchStatusStore

    /// How often the resting panel re-derives clock-relative text (reset-in). A minute is finer than
    /// the reset-in's own minute granularity, so the displayed value never visibly lags the clock.
    private static let clockTick: TimeInterval = 60

    var body: some View {
        TimelineView(.periodic(from: .now, by: Self.clockTick)) { context in
            content(now: Int64(context.date.timeIntervalSince1970))
        }
        .frame(width: 360, alignment: .leading)
        .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder
    private func content(now: Int64) -> some View {
        // The snapshot's freshness, re-derived against the client's own clock on each `TimelineView`
        // tick so a resting panel's "updated Ns ago" keeps advancing (and a wedged-but-heartbeating
        // daemon's growing age is visible without a manual refresh). `nil` generatedAt → no age.
        let ageText = store.generatedAt.flatMap {
            StatusPanelFormat.snapshotAgeText(generatedAt: $0, now: now)
        }
        let ageStale = store.generatedAt.map {
            StatusPanelFormat.snapshotIsStale(generatedAt: $0, now: now)
        } ?? false

        VStack(alignment: .leading, spacing: 14) {
            // The banner is the live honest-state indicator — always full strength. It folds in the
            // snapshot age (connected/stale/disconnected) so "Live" never implies "fresh".
            BannerView(banner: StatusPanelFormat.banner(for: store.connectionState,
                                                        accountCount: store.rows.count,
                                                        ageText: ageText,
                                                        ageStale: ageStale))

            if case .emptyRoster = store.connectionState {
                // A live onboarding state, not stale data — full strength, distinct from daemon-down.
                OnboardingCard()
            } else {
                // The RETAINED reading (roster + its `next_swap` footer): shown LIVE only on
                // `.connected`, DIMMED as one on every degraded/absent state so last-known data is
                // never mistaken for live (the never-healthy-when-dead discipline). `.unsupported`
                // clears both (numbers refused), so this block renders nothing but the banner.
                VStack(alignment: .leading, spacing: 14) {
                    if !store.rows.isEmpty {
                        RosterView(rows: store.rows, now: now)
                    }
                    if let footer = StatusPanelFormat.nextSwapFooter(store.nextSwap) {
                        FooterView(text: footer)
                    }
                }
                .opacity(store.connectionState.isHealthy ? 1 : 0.55)
            }
        }
        .padding(16)
    }
}

// MARK: - Honest-state banner

/// The always-present honest-state header. A shape-and-color status dot plus a plain title/detail,
/// tinted by the banner's kind — the panel's promise that it never shows healthy on a degraded daemon.
private struct BannerView: View {
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

// MARK: - Roster

/// The per-account roster. Its live-vs-retained dimming is applied by the parent (so the `next_swap`
/// footer dims in lock-step) — this view just lays the rows out.
private struct RosterView: View {
    let rows: [AccountRow]
    let now: Int64

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(rows) { row in
                AccountRowView(row: row, now: now)
            }
        }
    }
}

/// One account row: the active marker + label + auth glyph/cue on the top line, then the usage
/// percents, the single reset-in, and the next-swap marker on a quieter second line. The whole row is
/// one VoiceOver element.
private struct AccountRowView: View {
    let row: AccountRow
    let now: Int64

    private var resetIn: String {
        StatusPanelFormat.resetIn(weeklyExhausted: row.weeklyExhausted,
                                  sessionResetsAt: row.sessionResetsAt,
                                  weeklyResetsAt: row.weeklyResetsAt,
                                  now: now)
    }

    /// SESSION is the swap-triggering (binding) window unless the account is weekly-exhausted — the
    /// metric that earns typographic primacy (the same window `resetIn` shows, so the emphasized
    /// percent and the reset stay coherent).
    private var sessionIsPrimary: Bool {
        StatusPanelFormat.sessionIsSwapTrigger(weeklyExhausted: row.weeklyExhausted)
    }
    private var sessionSeverity: StatusPanelFormat.UsageSeverity? {
        StatusPanelFormat.sessionSeverity(row.sessionPct)
    }
    private var weeklySeverity: StatusPanelFormat.UsageSeverity? {
        StatusPanelFormat.weeklySeverity(weeklyPct: row.weeklyPct, weeklyExhausted: row.weeklyExhausted)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                // Active marker: SHAPE-encoded, never color (R-2 "shape + 'ACTIVE', not color") — a
                // filled inset circle for the active account, a hollow ring otherwise (mirrors the
                // `status` table's `*`). The accent that used to fill this ALSO marked "→ next", so blue
                // meant two things; it is freed. Idiom-consistent with the health SF Symbol beside it.
                Image(systemName: row.isActive ? "circle.inset.filled" : "circle")
                    .font(.caption)
                    .foregroundStyle(row.isActive ? Color.primary : Color.secondary)
                    .accessibilityHidden(true)

                Text(row.label)
                    .font(.body)
                    .fontWeight(row.isActive ? .semibold : .regular)
                    .lineLimit(1)
                    .truncationMode(.middle)

                // The word-half of "shape + 'ACTIVE'": a quiet, accent-free tag on the active row that
                // also carries the emphasis the active account earns (it is the one in use).
                if row.isActive {
                    Text("ACTIVE")
                        .font(.caption2).fontWeight(.semibold)
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 4).padding(.vertical, 1)
                        .background(RoundedRectangle(cornerRadius: 3).fill(Color.secondary.opacity(0.14)))
                        .accessibilityHidden(true)
                }

                Spacer(minLength: 6)

                authView
            }

            HStack(spacing: 8) {
                // The swap-triggering metric (session, or weekly when week-blocked) carries typographic
                // PRIMACY — semibold + full-strength; the other stays quiet. Both take a threshold color
                // only when depleted (≥75% Yellow, ≥90% / exhausted Red), so a healthy row gains NO
                // color — just the one semibold percent (issue #84 bands, shared with the CLI overlay).
                Text("session \(StatusPanelFormat.pct(row.sessionPct))")
                    .monospacedDigit()
                    .fontWeight(sessionIsPrimary ? .semibold : .regular)
                    .foregroundStyle(usageColor(sessionSeverity, primary: sessionIsPrimary))
                Text("·").foregroundStyle(.tertiary)
                Text("weekly \(StatusPanelFormat.pct(row.weeklyPct))")
                    .monospacedDigit()
                    .fontWeight(sessionIsPrimary ? .regular : .semibold)
                    .foregroundStyle(usageColor(weeklySeverity, primary: !sessionIsPrimary))
                Text("·").foregroundStyle(.tertiary)
                Text("resets in \(resetIn)")
                    .monospacedDigit()
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            // Keep the metrics on ONE line (a deliberate no-wrap choice, 715dc2d), but let the text
            // shrink to fit rather than truncate under large Dynamic Type — the numbers stay visible
            // down to 75%, and the row's VoiceOver label speaks the full metrics regardless.
            .lineLimit(1)
            .minimumScaleFactor(0.75)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    /// The auth glyph (modern path) or the legacy tag text (pre-#119), plus the DEAD/`disabled` cue.
    @ViewBuilder
    private var authView: some View {
        if let auth = row.auth {
            HStack(spacing: 4) {
                let symbol = StatusPanelFormat.healthSymbol(auth)
                Image(systemName: symbol.name)
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(healthColor(symbol.tint))
                    .accessibilityHidden(true)
                if let cue = StatusPanelFormat.authCue(auth: auth,
                                                       recovering: row.isRecovering,
                                                       enabled: row.isEnabled) {
                    Text(cue)
                        .font(.caption)
                        .foregroundStyle(cueColor(for: auth))
                }
            }
        } else {
            let legacy = StatusPanelFormat.legacyHealthTags(enabled: row.isEnabled,
                                                            quarantined: row.isQuarantined,
                                                            recovering: row.isRecovering)
            if !legacy.isEmpty {
                Text(legacy)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func cueColor(for auth: CredentialHealth) -> Color {
        auth == .dead && !row.isRecovering ? .red : .secondary
    }

    /// The metrics-line color for a usage severity: Red / Yellow escalate a depleted metric; Green (or a
    /// failed poll, `nil`) takes NO alarm color — the swap-triggering metric then shows full-strength
    /// (`.primary`) to carry its weight-primacy while the other recedes to `.secondary`. So color marks
    /// urgency and weight marks the swap-trigger, independently.
    private func usageColor(_ severity: StatusPanelFormat.UsageSeverity?, primary: Bool) -> Color {
        switch severity {
        case .red:          return .red
        case .yellow:       return .yellow
        case .green, .none: return primary ? .primary : .secondary
        }
    }

    /// Map the pure `HealthTint` role to a system semantic color — never `accentColor` (the AUTH glyph
    /// is never app-tinted, #84); `.neutral` (unknown) is `.secondary`, the #137 "no false green".
    private func healthColor(_ tint: StatusPanelFormat.HealthTint) -> Color {
        switch tint {
        case .green:   return .green
        case .yellow:  return .yellow
        case .orange:  return .orange
        case .red:     return .red
        case .neutral: return .secondary
        }
    }

    private var accessibilityLabel: String {
        StatusPanelFormat.rowAccessibilityLabel(
            label: row.label,
            isActive: row.isActive,
            auth: row.auth,
            recovering: row.isRecovering,
            enabled: row.isEnabled,
            quarantined: row.isQuarantined,
            sessionPct: row.sessionPct,
            weeklyPct: row.weeklyPct,
            resetIn: resetIn)
    }
}

// MARK: - Empty-roster onboarding

/// The first-run onboarding card (issue #326 AC): visually distinct from daemon-down, it explains the
/// empty roster and offers the `sessiometer capture` command to COPY — the app never runs it.
private struct OnboardingCard: View {
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Capture your first account")
                .font(.subheadline.weight(.semibold))
            Text("Run this in a terminal to add an account, then the daemon starts tracking it here.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 8) {
                Text(StatusPanelFormat.captureCommand)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(.vertical, 4)
                    .padding(.horizontal, 8)
                    .background(RoundedRectangle(cornerRadius: 6).fill(Color.secondary.opacity(0.12)))
                    .accessibilityLabel("command \(StatusPanelFormat.captureCommand)")

                Spacer(minLength: 0)

                Button(action: copy) {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                        .labelStyle(.titleAndIcon)
                        .font(.caption)
                }
                .accessibilityLabel(copied ? "Copied the capture command" : "Copy the capture command")
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.secondary.opacity(0.08)))
    }

    /// Copy the command to the clipboard — a pure AppKit pasteboard write, no execution (the app is a
    /// pure client). Shows a brief "Copied" confirmation.
    private func copy() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(StatusPanelFormat.captureCommand, forType: .string)
        copied = true
        Task {
            try? await Task.sleep(for: .seconds(1.6))
            copied = false
        }
    }
}

// MARK: - Footer

/// The `next_swap` footer (issue #326): the forward swap candidate, or absent when there is no active
/// anchor. Not swap history (that needs a new daemon source — deferred).
private struct FooterView: View {
    let text: String

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.caption2)
                .accessibilityHidden(true)
            Text(text)
                .font(.caption)
            Spacer(minLength: 0)
        }
        .foregroundStyle(.secondary)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(text)
    }
}
