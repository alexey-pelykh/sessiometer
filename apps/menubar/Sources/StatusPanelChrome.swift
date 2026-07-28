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
///
/// TWO WRITERS, not one (issue #788): `startPhase` is not driven only by a press of the button below.
/// `LoginItemModel.reconcileDaemonAgentRegistration()` repairs a registration an app update left stale, and it
/// paints the SAME `.registering` beat and `.failed` reason on this card, with no press behind it. So the
/// spinner can appear on its own at launch, and a reason shown here may belong to that repair rather than to
/// anything the operator just did — this card is the sole surface for both. Issue #820 is what makes that
/// second writer survivable, in two places:
///
///  1. THE REASON IS NOT GATED ON THE BUTTON. The `.failed` line used to live INSIDE the `canStartDaemon`
///     branch, which was sound while a press was the only way to reach it — the operator had just pressed
///     and was watching. With a second, launch-time writer it is not: any daemon subsequently taking the
///     single-instance lock flips `canStartDaemon` false and the reason vanished, including the case where
///     the unregister threw and the repair genuinely failed. And nothing retries behind it — the repair is
///     one-shot by design (`reconcileDaemonAgentRegistration()` carries the why). The reason is therefore
///     not decoration, it is the SOLE recovery signal, so it renders whenever one exists. The BUTTON and its
///     hint stay gated — an affordance that cannot act is still withheld, which is the honest-degradation
///     rule this card was built on.
///  2. THE COPY SAYS WHICH WRITER. Both beats carry a `StartOrigin` and route through
///     `StatusPanelFormat.startDaemonPendingText(for:)` / `startDaemonFailureText(reason:origin:)`, so a
///     repair the operator never asked for no longer reads exactly like a button press that failed.
///
/// What issue #820 deliberately did NOT add: a notification, a badge, or a menu-bar glyph state. Those are a
/// separate UX decision with their own design gate; the panel remains the sole surface.
///
/// ONE WINDOW THE DECOUPLING OPENS, accepted knowingly. `startPhase` is not reset when a daemon appears
/// late, so between a `notStartedReason` timing out and the next `watch` snapshot moving the panel off
/// `.notRunning`, the card can say "registered but didn't start" about a daemon that has since come up. The
/// old coupling hid that window by hiding the reason with the affordance — which is exactly the bug, so this
/// is the cost side of the fix, not a regression to chase: the reason is the SOLE recovery signal for the
/// case that matters (no daemon, no retry), and it self-clears on the next snapshot. Do not "fix" it by
/// re-gating the render.
struct StartDaemonCard: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    /// Is there a log to view (issue #779)? The SAME injected seam `DaemonLogCard` reads — see `DaemonLog.swift`
    /// for why availability is injected rather than probed from `body`, and note the default is `.unavailable`,
    /// so a host that forgets to inject renders no button rather than a dead one.
    @Environment(\.daemonLogProbe) private var daemonLogProbe
    @EnvironmentObject private var loginItem: LoginItemModel

    /// The in-flight beat's writer (issue #788/#820), or nil when no registration is in flight — either the
    /// button below, or the launch-time re-registration.
    private var registeringOrigin: StartOrigin? {
        if case .registering(let origin) = loginItem.startPhase { return origin }
        return nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8 * scale) {
            BannerView(banner: StatusPanelFormat.banner(for: .notRunning, accountCount: 0))
            if loginItem.canStartDaemon {
                startButton
            }
            // OUTSIDE the gate above — see (1) in this view's header. The three branches keep the resting
            // `canStartDaemon` order (button → reason → hint) byte-identical to what issue #170 shipped;
            // issue #779's affordance is nested INSIDE the reason branch, so that ordering is untouched.
            if case .failed(let reason, let origin) = loginItem.startPhase {
                failureLine(reason: reason, origin: origin)
            }
            if loginItem.canStartDaemon {
                Text(StatusPanelFormat.startDaemonHint)
                    .font(.panel(10.5, scale: scale))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// The failure line, plus issue #779's `View log` affordance where the daemon's own log is both the
    /// evidence AND actually there to open.
    ///
    /// TWO GATES, AND-ed, in this order:
    ///
    ///  1. `evidenceIsInTheDaemonLog` — is this failure one the log can speak to at all? Only the #745
    ///     liveness timeout is; a register that threw never spawned anything to write a line (see
    ///     `StartFailureReason`). This gate is asked FIRST so the probe is not even consulted for a failure
    ///     the log is irrelevant to.
    ///  2. `existingLogPath()` — is there a log? This is the honest-affordance rule (issue #169) at the seam
    ///     issue #776 built for it: nil when the home is unresolvable, the app is sandboxed, or the daemon has
    ///     never written a line. "Registered but never started" is EXACTLY the state where a daemon may well
    ///     have written nothing, so this is not a theoretical arm — it is the common one.
    ///
    /// And the copy moves with the gate rather than beside it: `text(offeringLogAffordance:)` drops the
    /// "Check Console for details" instruction precisely when the button replaces it, and keeps it — restoring
    /// issue #745's shipped sentence byte-for-byte — when it cannot. What is never dropped either way is the
    /// diagnostic itself, which is issue #745's honest statement of what happened.
    @ViewBuilder
    private func failureLine(reason: LoginItemModel.StartFailureReason, origin: StartOrigin) -> some View {
        let logPath = reason.evidenceIsInTheDaemonLog ? daemonLogProbe.existingLogPath() : nil
        let text = StatusPanelFormat.startDaemonFailureText(
            reason: reason.text(offeringLogAffordance: logPath != nil), origin: origin)

        Label(text, systemImage: "exclamationmark.triangle.fill")
            .font(.panel(11, scale: scale))
            .foregroundStyle(.red)
            .fixedSize(horizontal: false, vertical: true)
        if let logPath {
            // `.link`, not `.bordered`, and this card is why the style is a PARAMETER rather than derived
            // (`DaemonLogCard.ActionStyle`). No mock frame governs this state — issue #779 ships with
            // `Build Reference: None` — so the mock's own rule is applied rather than copied: it reserves the
            // bordered `.btn` for an action that shares a row as a peer, and this one does not. The primary
            // remedy here is `Start daemon` (`.borderedProminent`, directly above); a neutral-chrome button
            // beneath a red failure line would read as a second co-equal action and outweigh the hint below
            // it. Reading a log is diagnosis, not the remedy — `.link` is what says so.
            ViewLogButton(logPath: logPath, actionStyle: .link)
        }
    }

    private var startButton: some View {
        // The pending label is the WRITER's, not the button's (issue #820): a launch repair disables this
        // button and drives its beat, so labelling that "Starting…" would credit the operator with a press
        // they never made.
        let pendingText = registeringOrigin.map(StatusPanelFormat.startDaemonPendingText(for:))
        return Button(action: { Task { await loginItem.startDaemon() } }) {
            if let pendingText {
                HStack(spacing: 5 * scale) {
                    ProgressView().controlSize(PanelTypeScale.controlSize(for: scale))
                    Text(pendingText)
                }
            } else {
                Label(StatusPanelFormat.startDaemonButtonTitle, systemImage: "play.fill")
            }
        }
        .font(.panel(12, .semibold, scale: scale))
        .buttonStyle(.borderedProminent)
        .controlSize(PanelTypeScale.controlSize(for: scale))
        .disabled(pendingText != nil)
        .accessibilityLabel(pendingText ?? StatusPanelFormat.startDaemonButtonTitle)
    }
}

/// The cold states that offer a `View log` action (issue #776) — the design mock's daemon-starting and
/// crash-looping message cards, and NO other state. It reuses the honest-state `BannerView` for the message
/// (exactly as `StartDaemonCard` does for its own state) and adds the mock's document-glyph action beneath it.
///
/// TWO STATES, TWO STYLES, ON PURPOSE. The mock renders this same action differently in each: a borderless
/// `.btn.link` where it is the SOLE action (daemon-starting) and a bordered `.btn` where it shares the row
/// (crash-looping). That divergence is the reference's intent, not an inconsistency to normalize, so the style
/// is an explicit parameter rather than something derived here — the call sites in `StatusPanelView` name it,
/// which is what makes a future change to either state a visible diff.
///
/// Honest degradation (the crown-jewel rule, and the same shape as `StartDaemonCard`'s `canStartDaemon` gate):
/// the button appears ONLY where it can act. `DaemonLogProbe` answers "is there a log to view?" and returns
/// `nil` when the home is unresolvable, the app is sandboxed, or the daemon has not written a line yet — in
/// all three the card degrades to exactly the inert banner it was before, never a button whose click does
/// nothing. See `DaemonLog.swift` for why that answer is INJECTED rather than read from the filesystem here.
///
/// What the mock does NOT author is what a click DOES; that is umbrella decision D3 (open the log in
/// Console.app via `NSWorkspace`), and it lives in `DaemonLogOpen`.
///
/// The mock's OTHER crash-looping action, `Restart…`, is deliberately absent: measured evidence
/// (`docs/findings/0777-manual-restart-under-conditional-keepalive.md`) showed a manual kickstart mid-throttle
/// LENGTHENS the outage launchd is already ending on its own, so it is dropped rather than deferred, and the
/// mock is stale on that one button until issue #856 removes it.
struct DaemonLogCard: View {
    /// Which of the mock's two treatments this state's action takes.
    enum ActionStyle {
        /// `.btn.link` — borderless over the message card, secondary-tinted (daemon-starting).
        case link
        /// `.btn` — the mock's bordered default (crash-looping).
        case bordered

        /// The mock's state→treatment mapping, as a pure function of the state.
        ///
        /// A FUNCTION rather than a literal at each call site, so the mapping itself is directly testable:
        /// a render-level check can only prove the two cases LOOK different, never that the right state
        /// gets the right one — crash-looping wrongly styled `.link` would sail past a
        /// "the styles diverge" assertion while violating the reference outright.
        ///
        /// `.link` is the default because it is the mock's treatment for a lone action; only crash-looping
        /// takes the bordered `.btn`. NOTE for whoever lands issue #856 (which removes `Restart…` from the
        /// mock): the reference's own logic for that divergence is sole-action vs shares-the-row, so once
        /// `Restart…` is gone crash-looping becomes a lone action too and the mock's rationale would argue
        /// for `.link` in BOTH. That is a design decision to take deliberately at #856 — not a drift to
        /// silently absorb, and not something to pre-empt here.
        static func forState(_ state: ConnectionState) -> ActionStyle {
            state == .crashLooping ? .bordered : .link
        }
    }

    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    /// Is there a log to view? See the type's docs for why this is a seam and not a filesystem call.
    @Environment(\.daemonLogProbe) private var daemonLogProbe
    @EnvironmentObject private var store: WatchStatusStore

    /// The cold state whose banner copy this card carries — `.starting` or `.crashLooping`.
    let state: ConnectionState
    let actionStyle: ActionStyle

    var body: some View {
        // Same 8 pt banner→action gap `StartDaemonCard` settled on for the mock's `.msg-actions{margin-top}`,
        // so the panel's two message cards space their actions identically.
        VStack(alignment: .leading, spacing: 8 * scale) {
            BannerView(banner: StatusPanelFormat.banner(for: state, accountCount: store.rows.count))
            if let logPath = daemonLogProbe.existingLogPath() {
                ViewLogButton(logPath: logPath, actionStyle: actionStyle)
            }
        }
    }
}

/// The ONE `View log` button (issue #776) — every surface that offers the action renders THIS, so the
/// affordance cannot fork.
///
/// Extracted from `DaemonLogCard` when issue #779 gave the action a second home (the not-running card's
/// `.failed` line, `StartDaemonCard` above). "Reuse whatever handler issue #776 establishes; do not fork a
/// second Console-opening path" is that issue's explicit constraint, and a copied button is how a fork starts
/// — two call sites of `DaemonLogOpen.perform` is not a fork, but two DEFINITIONS of the label, the glyph,
/// the VoiceOver name and the help text would drift into one. Everything below is issue #776's code moved
/// verbatim, not re-derived: the committed panel goldens for `starting` / `crash-looping` are the proof, and
/// they must not move by so much as a pixel over this extraction.
private struct ViewLogButton: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale

    let logPath: String
    let actionStyle: DaemonLogCard.ActionStyle

    var body: some View {
        // `doc.text` is the SF Symbol counterpart of the mock's document glyph (a rounded rect over three
        // text rules, the last one short). `Label` pairs it with the title exactly as `StartDaemonCard` pairs
        // `play.fill` with "Start daemon", so both cards' actions read as one family.
        let button = Button { DaemonLogOpen.perform(logPath: logPath) } label: {
            Label(StatusPanelFormat.viewLogButtonTitle, systemImage: "doc.text")
        }
        .font(.panel(12, scale: scale))
        .controlSize(PanelTypeScale.controlSize(for: scale))
        // The label is already the spoken text, but state it explicitly: the button's rendered content is a
        // `Label`, and pinning the string here is what keeps the VoiceOver name from following a future
        // change to the glyph or the layout. Help carries the D3 destination the label does not say.
        .accessibilityLabel(StatusPanelFormat.viewLogButtonTitle)
        .help(StatusPanelFormat.viewLogHelp)

        switch actionStyle {
        case .link:
            // `.btn.link`: no border, no fill, `--text-2` — a secondary-weight action, still a real button
            // (and so still keyboard-focusable and still `AXButton`), never colour-only.
            button.buttonStyle(.plain).foregroundStyle(.secondary)
        case .bordered:
            // `.btn`: the mock's hairline-bordered default — a NEUTRAL chrome button (`--btn-bg` fill,
            // `--btn-border` hairline, `color: var(--text)`), which is why the tint is reset to `.primary`
            // here. The panel pins `.tint(Color.panelAccent)` for the whole hierarchy (#391/#754), and a
            // `.bordered` button inherits it — which would paint this action in the brand blue the mock
            // reserves for `.btn.primary` (Start daemon, `.borderedProminent`). Two different actions must
            // not read as the same weight: viewing a log is not the primary thing to do here.
            //
            // MEASURED RECONCILIATION (a deliberate divergence, in the sense design/README.md § Expected
            // reconciliations means). Against the light golden the platform's `.bordered` renders a fill of
            // (231,231,231) — 8/255 DARKER than the (239,239,239) card behind it — where the mock's
            // `--btn-bg: rgba(255,255,255,.72)` composites LIGHTER, and its `.5px solid rgba(0,0,0,.14)`
            // hairline is not separately discernible in the raster. The direction is inverted and the border
            // is implicit. Taking the native style anyway is the considered call: it is what the mock's CSS
            // is itself approximating, and it inherits control sizing, Dynamic Type (#756), vibrancy and the
            // accessibility appearance settings that a hand-rolled rounded-rect background would all have to
            // re-implement and would then drift from. What the panel must preserve is the SEMANTIC contrast
            // the mock encodes — neutral-chrome `.btn` vs accent `.btn.primary` vs borderless `.btn.link` —
            // and that is intact and asserted.
            button.buttonStyle(.bordered).tint(.primary)
        }
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
