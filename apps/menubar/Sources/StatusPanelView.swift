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
// distinct from daemon-down, and a breaking-schema daemon refuses its numbers. The one command the panel
// DOES run is the in-app capture affordance (issue #360): the onboarding card + add-account row send a
// `capture` verb over the #358 control socket and render its redacted ack (pending → done → error) — the
// app still originates no credential and never inserts the captured row itself (that arrives via the
// `watch` snapshot). Version-skew alone stays a `brew upgrade` copy-command (the app can't self-update).
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
        .frame(width: 380, alignment: .leading)
        .fixedSize(horizontal: false, vertical: true)
        // A translucent `.regularMaterial` scrim over the host's `.popover` vibrancy (StatusItemController):
        // the desktop blur reads through (matching the design reference's `backdrop-filter` translucency)
        // while the material's built-in frosting keeps every label + metric legible against a busy wallpaper
        // — the contrast guarantee we previously bought only by going fully opaque, which defeated the
        // vibrancy. Restores #390 (I5); the scrim is what makes the restore safe (ratified: vibrancy+scrim).
        .background(.regularMaterial)
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
        let state = store.connectionState
        let activeLabel = store.rows.first(where: \.isActive)?.label
        let subtitle = StatusPanelFormat.headerSubtitle(state: state,
                                                        accountCount: store.rows.count,
                                                        activeLabel: activeLabel,
                                                        ageStale: ageStale)

        // The design reference's chrome (`apps/menubar/design/menubar-preview.html`): an app-identity
        // header, a hairline divider, the state's body, and a snapshot-age footer. Sections own their
        // insets (no uniform padding) so the header/roster/callout/footer spacing matches the reference.
        // Honest-state is carried by the header sub-line (never a false "active" on a degraded daemon)
        // plus, on a dropped connection, an explicit strip over a dimmed last-known roster.
        VStack(alignment: .leading, spacing: 0) {
            PanelHeader(subtitle: subtitle)

            switch state {
            case .emptyRoster:
                // A live onboarding state, not stale data — distinct from daemon-down.
                Divider().padding(.horizontal, 14)
                OnboardingCard()
                    .padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 10)

            case .connecting, .unsupported:
                // No retained reading — a plain honest message (the reference's centered message card
                // for these states, with its distinct per-state glyph, is #169).
                Divider().padding(.horizontal, 14)
                BannerView(banner: StatusPanelFormat.banner(for: state, accountCount: store.rows.count))
                    .padding(.horizontal, 14).padding(.vertical, 14)

            case .disconnected:
                // Dropped connection: an explicit honest strip over the DIMMED last-known roster — never
                // frozen-as-live (#137). No swap callout / add-account (swaps are paused while dropped).
                HonestStrip(banner: StatusPanelFormat.banner(for: state, accountCount: store.rows.count,
                                                             ageText: ageText, ageStale: ageStale))
                if !store.rows.isEmpty {
                    RosterView(rows: store.rows, now: now).opacity(0.55)
                }

            case .connected, .stale:
                // Live (or connected-but-stale — the roster stays full-strength, the header/footer carry
                // the "stale" mark). The full design reference: roster, swap-callout hero, add-account.
                Divider().padding(.horizontal, 14)
                if !store.rows.isEmpty {
                    RosterView(rows: store.rows, now: now)
                }
                if let target = StatusPanelFormat.swapCalloutTarget(store.nextSwap) {
                    SwapCalloutCard(target: target, rows: store.rows)
                }
                AddAccountRow()
            }

            if let ageText {
                // Freshness reads amber whenever the numbers should be distrusted — a wedged-but-
                // heartbeating poll loop (ageStale) OR any non-live connection (stale / disconnected)
                // showing a last-known reading — never a frozen-as-fresh green (#137).
                FooterView(text: ageText, stale: ageStale || !state.isHealthy)
            }
        }
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
        VStack(alignment: .leading, spacing: 2) {
            ForEach(rows) { row in
                AccountRowView(row: row, now: now)
            }
        }
        // The design reference insets the roster (`.accts { padding: 6px 8px 2px }`): 8px horizontal so
        // the active row's accent card aligns with the swap-callout card below (also inset 8) instead of
        // bleeding edge-to-edge, plus 6px above / 2px below for breathing room under the divider.
        .padding(.horizontal, 8).padding(.top, 6).padding(.bottom, 2)
    }
}

/// One account row, built to the design reference (`apps/menubar/design/menubar-preview.html`). BOTH
/// reset windows show — R-2 parity with the `status` CLI, which prints both — never collapsed to one.
/// The whole row is a single VoiceOver element.
private struct AccountRowView: View {
    let row: AccountRow
    let now: Int64

    /// Each window's reset-in against the client's own clock — both shown, never collapsed to one pick.
    private var sessionReset: String {
        StatusPanelFormat.resetCell(row.sessionResetsAt, now: now)
    }
    private var weeklyReset: String {
        StatusPanelFormat.resetCell(row.weeklyResetsAt, now: now)
    }

    private var sessionSeverity: StatusPanelFormat.UsageSeverity? {
        StatusPanelFormat.sessionSeverity(row.sessionPct)
    }
    private var weeklySeverity: StatusPanelFormat.UsageSeverity? {
        StatusPanelFormat.weeklySeverity(weeklyPct: row.weeklyPct, weeklyExhausted: row.weeklyExhausted)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 9) {
                StatusDot(isActive: row.isActive)
                MonogramBadge(label: row.label)

                Text(row.label)
                    .font(.body)
                    .fontWeight(.semibold)
                    .lineLimit(1)
                    .truncationMode(.tail)

                Spacer(minLength: 6)

                if row.isActive {
                    // The design reference's accent "ACTIVE" tag — one of the row's THREE active cues
                    // (leading filled dot + this tag + accent-tint row), so active never rides on color
                    // alone (R-2 / WCAG 1.4.1).
                    Text("ACTIVE")
                        .font(.system(size: 9, weight: .bold))
                        .tracking(0.6)
                        .foregroundStyle(.tint)
                        .padding(.horizontal, 5).padding(.vertical, 1)
                        .overlay(RoundedRectangle(cornerRadius: 4)
                            .strokeBorder(Color.accentColor.opacity(0.42), lineWidth: 0.5))
                        .accessibilityHidden(true)
                }

                authView
            }

            VStack(spacing: 6) {
                UsageMeter(label: "Session", pct: row.sessionPct, severity: sessionSeverity,
                           reset: sessionReset)
                UsageMeter(label: "Weekly", pct: row.weeklyPct, severity: weeklySeverity,
                           reset: weeklyReset)
            }
        }
        .padding(.horizontal, 8)
        .padding(.top, 9)
        .padding(.bottom, 10)
        // Active emphasis follows the design reference: an accent-tint fill ONLY. The accent ring was
        // dropped (#387 M5, ratified) to cut active over-signaling — active stays redundantly encoded by
        // the filled leading dot (shape) + the "ACTIVE" tag + the tint, so color is never the SOLE signal
        // (WCAG 1.4.1 / R-2 state-parity holds). The mock's active-ring is dropped in lockstep
        // (menubar-preview.html `.acct.active` / `.stat.active`).
        .background(
            RoundedRectangle(cornerRadius: 9)
                .fill(row.isActive ? Color.accentColor.opacity(0.08) : Color.clear)
        )
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
            sessionReset: sessionReset,
            weeklyReset: weeklyReset)
    }
}

// MARK: - Row building blocks (per the design reference)

/// The account's monogram — provider-neutral by construction (issue #15: the label's initial, never a
/// brand mark or color). Accessibility-hidden; the row's VoiceOver label already speaks the identity.
private struct MonogramBadge: View {
    let label: String

    var body: some View {
        RoundedRectangle(cornerRadius: 8)
            .fill(Color.secondary.opacity(0.16))
            .frame(width: 30, height: 30)
            .overlay(
                Text(initial)
                    .font(.system(size: 13, weight: .bold))
                    .tracking(0.4)
                    .foregroundStyle(.secondary)
            )
            .accessibilityHidden(true)
    }

    /// The first character of the operator label, uppercased — `?` for an empty/whitespace label.
    private var initial: String {
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let first = trimmed.first else { return "?" }
        return String(first).uppercased()
    }
}

/// One usage window's meter. Both percents render at a uniform weight — the design reference (and the
/// `status` CLI) carry severity in COLOR, not weight; the fixed column widths + monospaced digits keep
/// Session and Weekly aligned.
private struct UsageMeter: View {
    let label: String
    let pct: UInt8?
    let severity: StatusPanelFormat.UsageSeverity?
    let reset: String

    var body: some View {
        HStack(spacing: 9) {
            Text(label.uppercased())
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(.secondary)
                .frame(width: 52, alignment: .leading)

            UsageBar(fraction: fraction, color: barColor)

            Text(StatusPanelFormat.pct(pct))
                .font(.system(size: 12, weight: .semibold)).monospacedDigit()
                .foregroundStyle(pctColor)
                .frame(width: 40, alignment: .trailing)

            Text(reset)
                .font(.system(size: 11)).monospacedDigit()
                .foregroundStyle(.secondary)
                .frame(width: 52, alignment: .trailing)
                .lineLimit(1)
        }
    }

    private var fraction: Double {
        pct.map { Double($0) / 100.0 } ?? 0
    }

    /// Bar fill = the green/amber/red usage band; a failed poll (`nil`) is muted, never a false green (#137).
    private var barColor: Color {
        switch severity {
        case .red:    return .red
        case .yellow: return .orange
        case .green:  return .green
        case .none:   return Color.secondary.opacity(0.45)
        }
    }

    /// The percent TEXT carries its severity band in color, matching the `status` CLI (which colors green
    /// percents green too — `Severity::Green => "32"`) and the design reference: green healthy, ≥75% amber
    /// (orange reads better than yellow), ≥90%/exhausted red. A failed poll (`n/a`) stays neutral — no
    /// false green (#137).
    private var pctColor: Color {
        switch severity {
        case .red:    return .red
        case .yellow: return .orange
        case .green:  return .green
        case .none:   return .primary
        }
    }
}

/// A capsule fill proportional to `fraction` (0…1), with a minimum sliver so a live-but-tiny percent
/// never reads as empty; a zero/failed reading shows a bare track. The number carries the real value.
private struct UsageBar: View {
    let fraction: Double
    let color: Color

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.secondary.opacity(0.20))
                Capsule().fill(color)
                    .frame(width: fillWidth(geo.size.width))
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }

    private func fillWidth(_ full: CGFloat) -> CGFloat {
        let clamped = min(1, max(0, fraction))
        guard clamped > 0 else { return 0 }
        // Mock `.m-fill { min-width: 5px }` — a live-but-tiny percent keeps a visible sliver.
        return max(5, full * clamped)
    }
}

// MARK: - Header + callouts (per the design reference)

/// The app-identity header — a neutral gauge glyph, the product name, and the honest identity sub-line
/// (`StatusPanelFormat.headerSubtitle`). Always present; the SUB-LINE — never the glyph — carries the
/// connection state, so a degraded daemon reads "last-known" / "· stale", never a false "active".
/// Provider-neutral (issue #15): a generic gauge, no brand mark or color.
private struct PanelHeader: View {
    let subtitle: String

    var body: some View {
        HStack(spacing: 10) {
            RoundedRectangle(cornerRadius: 7)
                .fill(Color.secondary.opacity(0.16))
                .frame(width: 27, height: 27)
                .overlay(
                    Image(systemName: "gauge.medium")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(.primary)
                )
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text("Sessiometer")
                    .font(.system(size: 13.5, weight: .semibold))
                Text(subtitle)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14).padding(.top, 12).padding(.bottom, 11)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Sessiometer. \(subtitle)")
    }
}

/// The leading status dot — the design reference's per-row marker: a filled accent disc for the active
/// (being-consumed) account, a hollow ring otherwise. FILL-vs-RING is a SHAPE difference, so active is
/// legible without color (WCAG 1.4.1); the accent is a redundant cue and the row's "ACTIVE" tag +
/// VoiceOver label state it in words.
private struct StatusDot: View {
    let isActive: Bool

    var body: some View {
        Circle()
            .fill(isActive ? Color.accentColor : Color.clear)
            .overlay(
                Circle().strokeBorder(Color.secondary.opacity(0.55), lineWidth: isActive ? 0 : 1.5)
            )
            .frame(width: 8, height: 8)
            // The design reference rings the active disc with a soft accent halo (`box-shadow 0 0 0 3px`) —
            // a redundant emphasis behind the fill-vs-ring shape difference, never the sole active cue.
            .background {
                if isActive {
                    Circle().fill(Color.accentColor.opacity(0.20)).frame(width: 14, height: 14)
                }
            }
            .accessibilityHidden(true)
    }
}

/// The honest strip shown over a dimmed last-known roster on a DROPPED connection — the design
/// reference's disconnected bar. States the degradation loudly (tinted, titled) so the retained numbers
/// below are never mistaken for live (#137). Richer per-state strips (keychain-locked "paused", a
/// Reconnect action) are #169.
private struct HonestStrip: View {
    let banner: StatusPanelFormat.Banner

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "bolt.horizontal.circle")
                .font(.caption)
                .accessibilityHidden(true)
            Text(banner.title)
                .font(.system(size: 11.5, weight: .semibold))
            Text(banner.detail)
                .font(.system(size: 11.5))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 0)
        }
        .foregroundStyle(stripTint)
        .padding(.horizontal, 14).padding(.vertical, 9)
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

/// The swap-callout hero — the design reference's primary action: the daemon's `next_swap` target, a
/// client-derived "why" line, and the Swap button. Accent-tinted (the panel's ONE accent action). The
/// button's on-click WIRING is #169 (the daemon swap command #167 already exists); until then it is
/// present-but-DISABLED — honest that the affordance is not yet live, never a dead-click.
private struct SwapCalloutCard: View {
    let target: String
    let rows: [AccountRow]

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "arrow.left.arrow.right")
                .font(.system(size: 16, weight: .semibold))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                (Text("Next swap → ") + Text(target).fontWeight(.semibold))
                    .font(.system(size: 12))
                    .lineLimit(1)
                    .truncationMode(.tail)
                Text(reason)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            Spacer(minLength: 6)
            Button("Swap") {}
                .font(.system(size: 12, weight: .semibold))
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(true)
                .help("Swap-on-click wiring is tracked in #169")
        }
        .padding(.leading, 11).padding(.trailing, 8).padding(.vertical, 9)
        .background(
            RoundedRectangle(cornerRadius: 9)
                .fill(Color.accentColor.opacity(0.10))
                .overlay(RoundedRectangle(cornerRadius: 9)
                    .strokeBorder(Color.accentColor.opacity(0.20), lineWidth: 0.5))
        )
        .padding(.horizontal, 8).padding(.top, 9).padding(.bottom, 4)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Next swap to \(target). \(reason). Swap action pending.")
    }

    /// The client-derived "why" line — the wire carries only the target label (#15), so the reason is
    /// computed here: the target's weekly headroom, flagged "lowest weekly" ONLY when it truly has the
    /// least weekly usage among the viable (non-active, enabled) swap candidates.
    private var reason: String {
        let targetRow = rows.first { $0.label == target }
        let candidates = rows.filter { !$0.isActive && $0.isEnabled }
        let knownWeekly = candidates.compactMap(\.weeklyPct)
        let isLowest = targetRow?.weeklyPct.map { tw in knownWeekly.allSatisfy { tw <= $0 } } ?? false
        return StatusPanelFormat.swapCalloutReason(targetWeeklyPct: targetRow?.weeklyPct,
                                                   isLowestWeekly: isLowest)
    }
}

/// The in-app capture affordance (issue #360) — a "Capture active account" button + an inline operator-
/// label field, SHARED by the populated add-account row and the empty-roster onboarding card. It sends
/// `{"cmd":"capture","label":…}` over the #358 control-command transport (via `AccountCaptureModel`) and
/// renders the redacted ack's idle → pending → done → error phase. It NEVER inserts the captured row —
/// that arrives on its own via the `watch` snapshot (issue #360 AC); on success the affordance just
/// returns to idle. The client still originates NO credential (C-005 held): a verb + non-secret label out,
/// a redacted ack back.
///
/// Capture snapshots the account currently logged into Claude Code — it is NOT an account picker. To add a
/// DIFFERENT account the operator runs `claude /login` first, then captures (the honest scope boundary,
/// surfaced as the onboarding secondary hint). An already-active-and-rostered account is an idempotent
/// refresh.
private struct CaptureAffordance: View {
    /// Onboarding uses the prominent, stacked treatment (the first-run hero); the populated add-row uses
    /// the compact inline one.
    enum Style { case onboarding, addRow }
    let style: Style

    @EnvironmentObject private var capture: AccountCaptureModel
    @State private var label = ""
    @FocusState private var fieldFocused: Bool

    var body: some View {
        Group {
            switch style {
            case .onboarding: onboarding
            case .addRow:     addRow
            }
        }
        // Bridge the field's focus to the model — the panel-retain predicate (`isBusy`) gates the outside-
        // click dismiss on it, and focusing re-asserts the panel key so keystrokes land (issue #360).
        .onChange(of: fieldFocused) { focused in capture.setEditing(focused) }
        // Esc cancels: resign focus + clear back to idle so an outside click can dismiss again (Return
        // submits via the field's `.onSubmit`).
        .onExitCommand {
            fieldFocused = false
            capture.cancelEditing()
        }
        // A completed capture consumed its label; blank the field so the next capture starts clean.
        .onChange(of: capture.phase) { phase in
            if case .done = phase { label = "" }
        }
    }

    /// The label field — the placeholder invites an OPTIONAL label; blank → the daemon derives the handle
    /// from the account UUID (never the email). Disabled while a capture is in flight.
    private var field: some View {
        TextField("e.g. Work, Personal", text: $label)
            .textFieldStyle(.roundedBorder)
            .font(.system(size: 12))
            .focused($fieldFocused)
            .onSubmit(submit)
            .disabled(capture.phase.isPending)
            .accessibilityLabel("Account label, optional")
    }

    /// The "Capture active account" button — the primary action; disabled and spinner-labelled while
    /// pending (a real pending state is honest now that capture is a real daemon-routed action).
    private func button(prominent: Bool) -> some View {
        Button(action: submit) {
            if capture.phase.isPending {
                HStack(spacing: 5) {
                    ProgressView().controlSize(.small)
                    Text(StatusPanelFormat.capturePendingText)
                }
            } else {
                Label("Capture active account", systemImage: "rectangle.badge.plus")
            }
        }
        .font(.system(size: 12, weight: prominent ? .semibold : .regular))
        .controlSize(.small)
        .disabled(capture.phase.isPending)
        .accessibilityLabel(capture.phase.isPending ? "Capturing the active account"
                                                     : "Capture the active account")
    }

    /// The done / error status line — rendered from the PURE `StatusPanelFormat` copy, never a string the
    /// view invents. Pending is shown on the button itself; idle has no status.
    @ViewBuilder
    private var status: some View {
        switch capture.phase {
        case .idle, .pending:
            EmptyView()
        case .done(let doneLabel):
            Label(StatusPanelFormat.captureDoneText(label: doneLabel), systemImage: "checkmark.circle.fill")
                .font(.system(size: 11))
                .foregroundStyle(.green)
                .lineLimit(1)
                .truncationMode(.middle)
        case .failed(let failure):
            Label(StatusPanelFormat.captureErrorText(failure), systemImage: "exclamationmark.triangle.fill")
                .font(.system(size: 11))
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// The prominent, stacked treatment for the first-run onboarding card.
    private var onboarding: some View {
        VStack(alignment: .leading, spacing: 9) {
            field
            HStack(spacing: 8) {
                button(prominent: true).buttonStyle(.borderedProminent)
                Spacer(minLength: 0)
            }
            status
            Text("To add a different account, run claude /login first, then capture.")
                .font(.system(size: 10.5))
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// The compact inline treatment beneath the populated roster.
    private var addRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                field
                button(prominent: false).buttonStyle(.bordered)
            }
            status
        }
        .padding(.horizontal, 12).padding(.top, 5).padding(.bottom, 3)
    }

    /// Submit a capture of the currently-active account under the field's label (blank → the daemon derives
    /// the handle). The model owns the pending → done / failed transitions.
    private func submit() {
        let text = label
        Task { await capture.capture(rawLabel: text) }
    }
}

/// The always-on "Add account" row (issue #360) — the populated-roster capture affordance: the same
/// "Capture active account" button + label field as the onboarding card, always available beneath the
/// roster. Replaces the pre-#360 copy-command; the captured row arrives via the `watch` snapshot.
private struct AddAccountRow: View {
    var body: some View {
        CaptureAffordance(style: .addRow)
    }
}

// MARK: - Empty-roster onboarding

/// The first-run onboarding card (issue #326 / #360): visually distinct from daemon-down, it explains the
/// empty roster and offers the in-app capture affordance — a "Capture active account" button + label field
/// that sends the capture command over the #358 transport and renders pending → done → error. The captured
/// row then arrives on its own via the `watch` snapshot (the app still originates no credential — a verb +
/// label out, a redacted ack back).
private struct OnboardingCard: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            Text("Capture your first account")
                .font(.subheadline.weight(.semibold))
            Text("Capture the account you’re signed into — the daemon adds it to the roster and starts tracking it here.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            CaptureAffordance(style: .onboarding)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.secondary.opacity(0.08)))
    }
}

// MARK: - Footer

/// The snapshot-age footer (issue #355 / #164 `generated_at`) — the design reference's freshness line,
/// "updated Ns ago". `next_swap` is NOT here (it lives in the swap-callout hero; a dropped daemon shows
/// no card, so the two never collide). Amber when the reading should be distrusted (a wedged poll loop,
/// or a stale/disconnected connection), never frozen-as-fresh (#137).
private struct FooterView: View {
    let text: String
    let stale: Bool

    var body: some View {
        VStack(spacing: 0) {
            Divider()
            HStack(spacing: 5) {
                Image(systemName: "clock")
                    .font(.caption2)
                    .accessibilityHidden(true)
                Text(text)
                    .font(.system(size: 11))
                    .monospacedDigit()
                Spacer(minLength: 0)
            }
            // Mock `.pop-foot .fl2 { color: var(--text-3) }` — the snapshot-age line is tertiary; amber
            // only when the reading should be distrusted (wedged poll loop / stale / disconnected).
            .foregroundStyle(stale ? Color.orange : Color(nsColor: .tertiaryLabelColor))
            .padding(.horizontal, 14).padding(.top, 9).padding(.bottom, 11)
        }
        .padding(.top, 5)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(stale ? "\(text), stale" : text)
    }
}
