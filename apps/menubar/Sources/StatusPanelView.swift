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
// distinct from daemon-down, and a breaking-schema daemon refuses its numbers. The commands the panel
// DOES run are the in-app capture affordance (issue #360) and the swap affordance (issue #169): both
// send a verb over the #358 control socket and render its redacted ack (pending → done → error) — the
// app still originates no credential, never inserts the captured row itself, and never mutates the
// active account itself (both arrive via the `watch` snapshot). Version-skew alone stays a
// `brew upgrade` copy-command (the app can't self-update).
//
// Two swap verbs, read differently (issue #169, Von Restorff): the footer **Swap** button is the
// panel's ONE accent/primary action — the daemon's own recommendation, sent WYSIWYG as the displayed
// `next_swap` target. A per-row manual switch is a quiet, neutral-weight affordance — persistent but
// low-key at rest, arming on hover (#448) — the operator choosing an arbitrary target. Both send the
// SAME `swap` command; the daemon re-validates every target from its own state, so the client never
// sends a viability hint.
//
// Provider-neutral by construction: the wire carries only the operator-chosen `label` (never an email
// — issue #15) and no provider field, so a row is plain text with no brand color or logo. Every row is
// one VoiceOver element speaking `StatusPanelFormat.rowAccessibilityLabel`.

import AppKit
import SwiftUI

fileprivate extension Color {
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

/// The panel's fixed layout constants — thin references to the source-of-truth in `StatusPanelFormat`
/// (the testable layer that also owns the width gate). The panel is FIXED-width by construction
/// (`.frame(width:)` below), so a roster row's available width is a DERIVED CONSTANT, not something to
/// measure. `StatusPanelFormat.rowFitsSwitchAffordance` gates the manual-switch affordance on it (issue
/// #169's "gate the affordance on available row width"). If the panel ever becomes resizable or gains a
/// compact mode, feed a MEASURED width into that same gate — the gate itself does not change.
private enum PanelMetrics {
    /// The panel's fixed content width.
    static let width = CGFloat(StatusPanelFormat.panelContentWidth)
    /// The roster's horizontal inset (`RosterView`), which the rows sit inside.
    static let rosterInset = CGFloat(StatusPanelFormat.rosterHorizontalInset)
    /// The width available to one roster row.
    static var rowWidth: Double { StatusPanelFormat.defaultRowWidth }
}

/// The root panel. Observes the store and re-derives the reset-in against the client's own wall clock
/// on a periodic `TimelineView` tick (issue #326: "computed against the client's own clock"), so a
/// resting popover keeps its "resets in" honest without a manual refresh.
struct StatusPanelView: View {
    @EnvironmentObject private var store: WatchStatusStore
    /// The capture affordance's model (issue #360), observed here for its `captureSurfaceRequested` flag
    /// (issue #394): when the operator picks the status-item "Add account…" menu item, the panel presents
    /// the capture surface over whatever state it is in, reusing the panel's own key/first-responder
    /// plumbing. (`CaptureAffordance` reads the same model as its own `@EnvironmentObject`.)
    @EnvironmentObject private var capture: AccountCaptureModel
    /// The Stats-tab model (issue #446): the panel's Status|Stats tab selection + the one-shot `stats`
    /// query's phase. Observed here to render the seg control's on-state and to switch the body to the
    /// Stats view. (`StatsView` and `PanelHeader`'s seg read the same model.)
    @EnvironmentObject private var stats: PanelStatsModel

    /// How often the resting panel re-derives clock-relative text (reset-in). A minute is finer than
    /// the reset-in's own minute granularity, so the displayed value never visibly lags the clock.
    private static let clockTick: TimeInterval = 60

    var body: some View {
        TimelineView(.periodic(from: .now, by: Self.clockTick)) { context in
            content(now: Int64(context.date.timeIntervalSince1970))
        }
        .frame(width: PanelMetrics.width, alignment: .leading)
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

        // The Status|Stats switcher (issue #446) is offered ONLY where the Stats tab can deliver: a live
        // roster (`.connected` / `.stale`) and NOT while the #394 capture surface is up. In every degraded
        // state the header carries just the honest identity — a Stats affordance that can only fail is not
        // an honest affordance (matches the mock, which shows the seg only in the healthy Status/Stats states).
        let showsSwitcher = (state == .connected || state == .stale) && !capture.captureSurfaceRequested
        let onStatsTab = showsSwitcher && stats.tab == .stats

        // The Stats tab replaces the honest-state sub-line with the mock's "Usage stats · last 7 days" (from
        // the loaded window when present, else the default phrase for the always-`week` query). Derived in a
        // closure — a single `let` binding the enclosing `@ViewBuilder` skips as a declaration, where a bare
        // `if/else` assignment would instead be read as a (non-`View`) conditional branch.
        let subtitle: String = {
            if onStatsTab, let window = stats.phase.wire?.window {
                return StatusPanelFormat.statsHeaderSubtitle(window)
            } else if onStatsTab {
                return StatusPanelFormat.statsDefaultHeaderSubtitle
            } else {
                return StatusPanelFormat.headerSubtitle(state: state,
                                                        accountCount: store.rows.count,
                                                        activeLabel: activeLabel,
                                                        ageStale: ageStale)
            }
        }()

        // The design reference's chrome (`apps/menubar/design/menubar-preview.html`): an app-identity
        // header (with the Status|Stats seg when offered), a hairline divider, the state's body, and a
        // snapshot-age footer. Sections own their insets (no uniform padding) so the spacing matches the
        // reference. Honest-state is carried by the header sub-line (never a false "active" on a degraded
        // daemon) plus, on a dropped connection, an explicit strip over a dimmed last-known roster.
        VStack(alignment: .leading, spacing: 0) {
            PanelHeader(subtitle: subtitle, showsSwitcher: showsSwitcher)

            if capture.captureSurfaceRequested {
                // The status-item "Add account…" capture surface (issue #394) — a focused capture card
                // hosted in THIS panel (reusing its key/first-responder plumbing), reached only from the
                // right-click menu now that the populated panel carries no persistent capture bar. The
                // header above stays, so its honest state sub-line still governs; a capture attempt over a
                // degraded daemon surfaces its own honest error through the affordance, never a false ok.
                Divider().padding(.horizontal, 14)
                CaptureCard(title: "Add account")
                    .padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 10)
            } else if onStatsTab {
                // The Stats tab (issue #446): the mock's per-account 7-day sparklines + numeric body,
                // aggregate callout, and signal legend — fed by the socket `stats` verb (never a store read).
                // A separate view from the Status body; the footer's `next_swap` line stays the Status tab's.
                Divider().padding(.horizontal, 14)
                StatsView()
            } else {
                stateBody(state: state, now: now, ageText: ageText, ageStale: ageStale)
            }
        }
    }

    /// The panel's normal, connection-state-driven body (roster / banner / onboarding card) plus the
    /// snapshot-age footer — everything below the header when the operator has NOT summoned the #394
    /// capture surface. A populated (`.connected` / `.stale`) roster carries NO capture bar: capture is
    /// an empty-roster / first-run onboarding affordance, and adding an account otherwise lives off-panel
    /// in the status-item right-click menu (issue #394; matches the re-locked mock, #387).
    @ViewBuilder
    private func stateBody(state: ConnectionState, now: Int64, ageText: String?, ageStale: Bool) -> some View {
        switch state {
        case .emptyRoster:
            // A live onboarding state, not stale data — distinct from daemon-down.
            Divider().padding(.horizontal, 14)
            CaptureCard(title: "Capture your first account")
                .padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 10)

        case .connecting, .starting, .notRunning, .unsupported, .crashLooping:
            // No trustworthy reading to show — a plain honest message card. `.crashLooping` (#169) holds
            // here too: the daemon served a snapshot but keeps dropping before it stabilizes, so its
            // numbers are refused ("holding status until it stays up") rather than flickered as live —
            // the crown-jewel anti-#137 debounce. `.starting` / `.notRunning` (#499) are the cold-refused
            // daemon-absent states: neither ever held a reading, so both render the honest banner card. The
            // not-running state WOULD host a "Start daemon" button — launch-at-login is #170 (deferred,
            // signing-blocked), so it degrades to the inert explanatory banner (no button yet). (The fuller
            // per-state message-card fidelity and the lifecycle affordances — View log / Restart / Start —
            // are #169 / #170 siblings.)
            Divider().padding(.horizontal, 14)
            BannerView(banner: StatusPanelFormat.banner(for: state, accountCount: store.rows.count))
                .padding(.horizontal, 14).padding(.vertical, 14)

        case .disconnected, .reconnecting:
            // A warm drop: an explicit honest strip over the DIMMED last-known roster — never frozen-as-live
            // (#137). No swap callout (swaps are paused while dropped), and the roster rows are NOT switchable:
            // a retained last-known row is not a live target, and a click over a dead socket would be a dead
            // click (#169's honest-affordance rule). `.reconnecting` (#526, still within the warm dwell) shares
            // this exact treatment — the retained roster stays informative — and the strip's copy auto-derives
            // from `state`, so the dwell reads calm ("Reconnecting…") while the escalation reads loud ("Daemon
            // not responding"), both off the single `banner(for:)` switch.
            HonestStrip(banner: StatusPanelFormat.banner(for: state, accountCount: store.rows.count,
                                                         ageText: ageText, ageStale: ageStale))
            if !store.rows.isEmpty {
                RosterView(rows: store.rows, now: now, switchable: false).opacity(0.55)
            }

        case .connected, .stale:
            // Live (or connected-but-stale — the roster stays full-strength, the header/footer carry the
            // "stale" mark). The design reference: roster + swap-callout hero. The roster is switchable
            // exactly where the swap-callout card renders, so the panel's two swap paths (per-row manual,
            // footer recommendation) are live and dead together (#169). No capture bar — capture moved to
            // the status-item menu / empty-roster onboarding (issue #394).
            Divider().padding(.horizontal, 14)
            if let faultBanner = StatusPanelFormat.daemonFaultBanner(keychainLocked: store.keychainLocked,
                                                                     scrub: store.canonicalScrub,
                                                                     systemicRefreshFailure: store.systemicRefreshFailure) {
                // The single daemon-level fault banner (worst-first): a fleet-wide lockout or mechanism
                // failure NO per-row `auth` reflects (rows can read healthy while the shared item sits locked
                // or emptied, and while the refresh mechanism is down every account is still alive), so it
                // rides as its own honest banner ABOVE the roster — the connected-but-degraded panel reads
                // visibly DEGRADED (never healthy) while the live roster still renders below. The footer stays
                // the `next_swap` line (R-2: footer = next_swap; degraded daemon-level signals → honest
                // banner). The panel shows ONE banner, ranked worst-first over (fault, VARIANT) — never over
                // fault identity, so a calm self-healing state can never outrank one that cannot self-heal.
                // See `daemonFaultBanner` for the four ranks and why `recovering` sits last of them.
                //
                // This banner is what makes the menu-bar glance honest rather than cryptic: the locked glyph
                // taxonomy collapses every fault to one silhouette on the promise that "the *which* is one
                // click away in the panel" (#524). Each fault the glyph shouts MUST land here, or the click
                // that follows finds a healthy roster and no explanation.
                BannerView(banner: faultBanner)
                    .padding(.horizontal, 14).padding(.vertical, 14)
                Divider().padding(.horizontal, 14)
            }
            if !store.rows.isEmpty {
                RosterView(rows: store.rows, now: now, switchable: true)
            }
            if let target = StatusPanelFormat.swapCalloutTarget(store.nextSwap) {
                SwapCalloutCard(target: target,
                                reason: StatusPanelFormat.swapCalloutReason(store.nextSwap))
            }
            SwapStatusLine()
        }

        if let ageText {
            // Freshness reads amber whenever the numbers should be distrusted — a wedged-but-
            // heartbeating poll loop (ageStale) OR any non-live connection (stale / disconnected)
            // showing a last-known reading — never a frozen-as-fresh green (#137).
            FooterView(text: ageText, stale: ageStale || !state.isHealthy)
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
/// footer dims in lock-step) — this view just lays the rows out and decides, per row, whether it is a
/// manual-switch target (issue #169).
private struct RosterView: View {
    let rows: [AccountRow]
    let now: Int64
    /// Whether rows offer the manual-switch affordance at all. `false` on a dropped connection, where a
    /// retained last-known row is not a live target.
    let switchable: Bool

    var body: some View {
        // Resolve every row's smart monogram ONCE over the whole roster (issue #445), so collision-escalation
        // sees all sibling labels — a same-local-part roster gets distinct 2-char monograms, not one letter.
        let monograms = StatusPanelFormat.accountMonograms(rows.map(\.label))
        VStack(alignment: .leading, spacing: 2) {
            ForEach(rows) { row in
                // On a dropped connection every row is `notATarget` (non-interactive); otherwise the pure
                // `rowSwitchState` verdict decides (active → plain row, non-viable → disabled-with-reason,
                // else the switch affordance). The active-row-stays-plain and parked-still-switchable
                // rules live in that pure, unit-tested function — never re-decided here.
                let state: StatusPanelFormat.RowSwitchState = switchable
                    ? StatusPanelFormat.rowSwitchState(isActive: row.isActive,
                                                       isQuarantined: row.isQuarantined,
                                                       weeklyExhausted: row.weeklyExhausted,
                                                       isEnabled: row.isEnabled)
                    : .notATarget
                AccountRowView(row: row, monogram: monograms[row.label] ?? "?", now: now, switchState: state)
            }
        }
        // The design reference insets the roster (`.accts { padding: 6px 8px 2px }`): 8px horizontal so
        // the active row's accent card aligns with the swap-callout card below (also inset 8) instead of
        // bleeding edge-to-edge, plus 6px above / 2px below for breathing room under the divider.
        .padding(.horizontal, PanelMetrics.rosterInset).padding(.top, 6).padding(.bottom, 2)
    }
}

/// The per-row manual-switch button style (issue #169): a QUIET, neutral affordance — deliberately NOT
/// the accent/primary treatment the footer **Swap** button wears. Von Restorff: the accent action is
/// what the daemon SUGGESTS; the quiet ones are the operator CHOOSING. A subtle wash on hover (a live
/// row only) and a slightly deeper one while pressed; a blocked row never washes, so it can never read
/// as pressable.
private struct RowSwitchButtonStyle: ButtonStyle {
    let hovering: Bool
    let live: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            // MIS-CLICK GUARD (issue #169 falsifier b) — resolved deliberately, not by accident. The
            // checklist item forbids "an INVISIBLE whole-row click"; the watch-out phrases it as "not a
            // whole-row HOT-ZONE". Both are honored by ARMING, not by shrinking the hit target:
            //   * At rest the row shows only a QUIET chip (#448) — no wash, no `pointingHand`: the chip
            //     hints the row is actionable, but WITHOUT the wash + cursor it does not yet read as an
            //     armed, pressable control, so the invisible-click hazard the checklist names cannot occur
            //     (the persistent chip aids DISCOVERY; the wash + cursor still gate ARMING).
            //   * The hit rect is the whole row (per the explicit "implement the row as a Button"
            //     instruction + Fitts's law — a glyph-only target would be a worse, error-prone
            //     mechanism), but it is ARMED only once hover has added the wash + cursor and brightened
            //     the chip, so the operator always SEES the row is live before a press can land.
            //   * Residual accidental-press risk is bounded by three things the daemon and model already
            //     enforce: the daemon re-validates every target (a stray press can't do something unsafe),
            //     a swap is reversible (undo = switch back), and a sibling swap is `.disabled()` mid-flight.
            // Net: cheaper than a confirm dialog, honest at rest. The real-popover press feel is a #380
            // manual-check item.
            .contentShape(RoundedRectangle(cornerRadius: 9))
            .background(
                RoundedRectangle(cornerRadius: 9)
                    // #388-EXEMPT: a COMPUTED hover/press interaction wash, not one of the mock's absolute
                    // chrome fills (the static mock has no hover state), so it keeps `Color.secondary.opacity(k)`
                    // rather than a `panelFill` token — `wash` is 0 at rest, a faint neutral only while live+hovered.
                    .fill(Color.secondary.opacity(wash(pressed: configuration.isPressed)))
            )
    }

    private func wash(pressed: Bool) -> Double {
        guard live, hovering else { return 0 }
        return pressed ? 0.16 : 0.08
    }
}

/// One account row, built to the design reference (`apps/menubar/design/menubar-preview.html`). BOTH
/// reset windows show — R-2 parity with the `status` CLI, which prints both — never collapsed to one.
/// The whole row is a single VoiceOver element.
///
/// A non-active row is ALSO the manual-switch affordance (issue #169): a `Button` whose trailing swap
/// chip is PERSISTENT — quiet at rest, brightening when the row is armed on hover (#448). The resting
/// row carries the quiet chip; arming still gates the wash + `pointingHand` cursor.
private struct AccountRowView: View {
    let row: AccountRow
    /// The roster-resolved 2-char monogram for this row's label (issue #445), computed once by `RosterView`.
    let monogram: String
    let now: Int64
    /// The row's manual-switch verdict (issue #169). `.notATarget` — the ACTIVE row, or any row on a
    /// dropped connection — stays a plain, non-interactive display row.
    let switchState: StatusPanelFormat.RowSwitchState

    @EnvironmentObject private var swap: AccountSwapModel
    /// The active row's accent-tint fill opacity is theme-aware (#388): the mock raises it in dark mode.
    @Environment(\.colorScheme) private var colorScheme
    @State private var isHovering = false
    /// Whether this row currently owns a pushed `pointingHand` cursor — tracked so a push is always
    /// balanced by exactly one pop, even when the row stops being live WHILE the pointer is inside it
    /// (a sibling swap starting mid-hover would otherwise strand the cursor).
    @State private var cursorPushed = false

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

    // MARK: - Switch state (issue #169)

    /// The wire-visible reason this row cannot be switched to, if any.
    private var blockReason: StatusPanelFormat.SwitchBlock? {
        if case .blocked(let block) = switchState { return block }
        return nil
    }

    /// Whether the row is offered as a switch target AT ALL. `.notATarget` (active row / dropped
    /// connection) never is; otherwise it is, GATED on the row's available width — too narrow to host the
    /// affordance ⇒ not interactive, rather than an invisible whole-row hot-zone. The panel is
    /// fixed-width, so the width is a derived constant (`PanelMetrics`), not a measurement.
    private var offersSwitch: Bool {
        switchState != .notATarget
            && StatusPanelFormat.rowFitsSwitchAffordance(rowWidth: PanelMetrics.rowWidth)
    }

    /// This row's own swap is in flight.
    private var isSwitching: Bool { swap.phase.pendingTarget == row.label }

    /// Whether a click on this row would actually do something.
    private var isLiveSwitch: Bool {
        offersSwitch && blockReason == nil && !swap.phase.isPending
    }

    var body: some View {
        Group {
            if offersSwitch {
                // ROW-ACTION CARDINALITY (issue #169 watch-out — decide the count BEFORE the mechanism):
                // a viable row carries exactly ONE action today (switch), so wrapping the whole row in a
                // `Button` is sound, and it earns the VoiceOver button trait, native `.disabled()`, and
                // hover styling for free. If a row ever gains a SECOND action (an enable-toggle, a
                // remove), this wrap MUST be undone — nested interactive children inside a `Button` do
                // not receive their own events. Hoist the secondary control into a trailing accessory or
                // a context menu and shrink the button to the identity region.
                Button(action: submit) { rowContent }
                    .buttonStyle(RowSwitchButtonStyle(hovering: isHovering, live: isLiveSwitch))
                    .disabled(blockReason != nil || swap.phase.isPending)
                    .help(hoverText)
                    // The button trait + `dimmed` come from `Button` + `.disabled()`; the label carries
                    // the row's facts and, when blocked, WHY it is dimmed (a trait alone never says why).
                    .accessibilityLabel(StatusPanelFormat.rowSwitchAccessibilityLabel(
                        base: accessibilityLabel, block: blockReason))
                    .accessibilityHint(blockReason == nil
                                       ? StatusPanelFormat.switchHelpText(label: row.label) : "")
            } else {
                rowContent
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(accessibilityLabel)
            }
        }
        .onHover { hovering in
            isHovering = hovering
            syncCursor()
        }
        // Resync the cursor whenever the row's live-ness can change WITHOUT a hover event, so a lingering
        // `pointingHand` never contradicts the row's real state: a sibling swap starting/finishing
        // (`swap.phase`), or a fresh snapshot flipping this row's viability or making it the new active
        // account (`switchState`) while the pointer rests on it.
        .onChange(of: swap.phase) { _ in syncCursor() }
        .onChange(of: switchState) { _ in syncCursor() }
        .onDisappear { setCursor(pushed: false) }
    }

    /// Submit a manual switch to THIS row's account. The clicked row's target goes on the wire verbatim;
    /// the daemon re-validates it (`swap_command_verdict`) and may still refuse — a `cooldown`, say, which
    /// never rides the wire and so cannot be pre-empted here. That refusal renders in `SwapStatusLine`.
    private func submit() {
        Task { await swap.swap(to: row.label) }
    }

    /// The hover tooltip: the block reason for a non-viable row, otherwise the switch invitation.
    private var hoverText: String {
        blockReason.map(StatusPanelFormat.switchBlockedText)
            ?? StatusPanelFormat.switchHelpText(label: row.label)
    }

    /// Push / pop the `pointingHand` cursor to match whether a click here would do anything.
    private func syncCursor() {
        setCursor(pushed: isHovering && isLiveSwitch)
    }

    private func setCursor(pushed: Bool) {
        guard pushed != cursorPushed else { return }
        if pushed {
            NSCursor.pointingHand.push()
        } else {
            NSCursor.pop()
        }
        cursorPushed = pushed
    }

    /// The row's visual content — identical whether or not it is wrapped in a `Button`, so the two
    /// branches cannot drift.
    private var rowContent: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 9) {
                StatusDot(isActive: row.isActive)
                MonogramBadge(label: row.label, monogram: monogram)

                Text(row.label)
                    .font(.body)
                    .fontWeight(.semibold)
                    .lineLimit(1)
                    // MIDDLE-truncation (issue #445): a same-local-part label's distinguishing suffix /
                    // domain survives when it elides, where tail-truncation hid exactly that part.
                    .truncationMode(.middle)

                Spacer(minLength: 6)

                if row.isActive {
                    // The active tag — one of the row's THREE redundant "active" cues (leading filled dot +
                    // this tag + accent-tint row fill), so active never rides on colour alone (R-2 / WCAG
                    // 1.4.1). Treatment matches the perfected mock `.tag` (`menubar-preview.html:243`): a calm
                    // NEUTRAL sentence-case capsule — the same `--badge-bg` neutral fill as the monogram badge
                    // (`Color.panelFill(.badge, …)`) + `--text-2` text (`.secondary`), NO accent border, NO
                    // letter-spaced uppercase. The accent DOT already carries the active colour; a second
                    // accent element here (the old outlined uppercase "ACTIVE" pill) re-inflated the active
                    // over-signalling #387 M5 reduced and sank the same-hue label to ~3:1. The neutral label
                    // stays as the WCAG 1.4.1 non-colour cue (clears 1.4.11 on the capsule — see #501 tests);
                    // it is `accessibilityHidden` because the row's spoken label already says ", active" (#325).
                    Text(StatusPanelFormat.activeTagLabel)
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 7).padding(.vertical, 1.5)
                        .background(RoundedRectangle(cornerRadius: 5)
                            .fill(Color.panelFill(.badge, dark: colorScheme == .dark)))
                        .accessibilityHidden(true)
                }

                authView
                switchSlot
            }

            if let blind = row.blindActive {
                // The active account's poll is blind — replace the two (now `n/a`) live meters with the
                // SEMANTIC held-state block: a held session bar, blind duration, and the auto-protection
                // verdict (#485), the panel's render of the CLI's blind line. A healthy row keeps its meters.
                BlindMeter(blind: blind)
            } else {
                VStack(spacing: 6) {
                    UsageMeter(label: "Session", pct: row.sessionPct, severity: sessionSeverity,
                               reset: sessionReset)
                    UsageMeter(label: "Weekly", pct: row.weeklyPct, severity: weeklySeverity,
                               reset: weeklyReset)
                }
            }
        }
        .padding(.horizontal, 8)
        .padding(.top, 9)
        .padding(.bottom, 10)
        // Active emphasis follows the design reference: an accent-tint fill ONLY. The accent ring was
        // dropped (#387 M5, ratified) to cut active over-signaling — active stays redundantly encoded by
        // the filled leading dot (shape) + the "ACTIVE" tag + the tint, so color is never the SOLE signal
        // (WCAG 1.4.1 / R-2 state-parity holds). The mock's active-ring is dropped in lockstep
        // (menubar-preview.html `.acct.active` / `.stat.active`). The fill OPACITY is theme-aware (#388,
        // mock `--active-bg`): .08 light / .15 dark — the dark active row was ~1.5× too faint when hardcoded.
        .background(
            RoundedRectangle(cornerRadius: 9)
                .fill(row.isActive
                      ? Color.accentEmphasis(.activeRowFill, dark: colorScheme == .dark)
                      : Color.clear)
        )
        // #485: a DEGRADED blind active row gets an at-risk orange leading rule — a non-color-redundant
        // LOCALITY tell (the fault is THIS row's; the header/footer stay fresh, the AC-2 distinction from
        // #169's whole-snapshot "stale"). Absent on a blind-OK row (calm) and every non-blind row.
        .overlay(alignment: .leading) {
            if row.blindActive?.autoProtectionDegraded == true {
                Capsule()
                    .fill(Color.panel(StatusPanelFormat.healthTint(.orange)))
                    .frame(width: 3)
                    .padding(.vertical, 7)
                    .accessibilityHidden(true)
            }
        }
    }

    /// The swap glyph the chip draws — a swap arrow, or a DISTINCT `nosign` on a wire-blocked target
    /// ("you cannot switch here" is a different fact from "switch here", and shape carries it without
    /// color). The tint is applied by `switchSlot` per emphasis level, so this stays tint-free.
    private var chipGlyph: some View {
        Image(systemName: blockReason == nil ? "arrow.left.arrow.right" : "nosign")
            .font(.system(size: 11, weight: .semibold))
    }

    /// The trailing manual-switch chip (issue #169, made PERSISTENT by #448) — a quiet affordance shown at
    /// rest on every switch target, that BRIGHTENS when the row is armed (hover / focus). #169 revealed it
    /// only on hover, so on a transient popover a first-time operator never saw a row was actionable; the
    /// persistent-quiet chip makes the row discoverable without an always-loud control.
    ///
    /// The slot's WIDTH is laid out on every roster row, always — even where the chip is hidden (the active
    /// row) — so NEITHER the chip's resting presence NOR its hover-brighten can REFLOW the row: the label's
    /// available width is identical hidden / resting / armed, and so is its truncation (the issue's
    /// row-width watch-out). The auth column also stays aligned across active and non-active rows. The
    /// why-text never truncates: it is a native `.help` tooltip, not an inline label.
    ///
    /// The emphasis (hidden / resting / armed) is a pure `StatusPanelFormat.switchChipEmphasis` verdict, so
    /// the resting-visible-vs-armed-brighten distinction is unit-asserted; the view only maps it to a
    /// neutral system tint. ARMING (not the resting presence) is the mis-click guard — the full rationale
    /// lives on `RowSwitchButtonStyle`.
    @ViewBuilder
    private var switchSlot: some View {
        Group {
            if isSwitching {
                ProgressView().controlSize(.small)
            } else {
                switch StatusPanelFormat.switchChipEmphasis(offersSwitch: offersSwitch, armed: isHovering) {
                case .hidden:
                    Color.clear
                case .resting:
                    // Quiet at rest — `.tertiary` ≈ the mock's `--text-3` decorative token. Never `.tint`:
                    // the one accent action is the footer Swap (Von Restorff, one accent per panel).
                    chipGlyph.foregroundStyle(.tertiary)
                case .armed:
                    // Brightened once armed — `.secondary` ≈ the mock's `--text-2`. A SEMANTIC tint step,
                    // not a hardcoded opacity (#388 / #448).
                    chipGlyph.foregroundStyle(.secondary)
                }
            }
        }
        .frame(width: CGFloat(StatusPanelFormat.switchAffordanceSlotWidth), alignment: .trailing)
        .accessibilityHidden(true)
    }

    /// The auth glyph (modern path) or the legacy tag text (pre-#119), plus the DEAD/`disabled` cue.
    /// A blind active account (#485) shows the eye-slash blind glyph HERE instead — the credential may be
    /// fine; what's lost is usage visibility, so the health slot reports that, not a false auth verdict.
    @ViewBuilder
    private var authView: some View {
        if let blind = row.blindActive {
            // Usage visibility lost (#485): an eye-slash, a DISTINCT shape from every auth glyph. OK is
            // calm secondary; DEGRADED tints it at-risk orange (redundant with the row's rule + verdict).
            // If the credential is ITSELF in a warning state (stale/at-risk — orthogonal to usage-blindness),
            // its glyph rides ALONGSIDE the eye-slash so the warning isn't suppressed (the CLI keeps both;
            // #137 honest-state one axis over). Healthy/unknown → eye-slash alone (the common, ratified case).
            HStack(spacing: 4) {
                if let auth = row.auth, StatusPanelFormat.blindCoShowsAuthWarning(auth) {
                    let authSymbol = StatusPanelFormat.healthSymbol(auth)
                    Image(systemName: authSymbol.name)
                        .symbolRenderingMode(.hierarchical)
                        .foregroundStyle(healthColor(authSymbol.tint))
                        .accessibilityHidden(true)
                }
                let symbol = StatusPanelFormat.blindSymbol(degraded: blind.autoProtectionDegraded)
                Image(systemName: symbol.name)
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(healthColor(symbol.tint))
                    .accessibilityHidden(true)
            }
        } else if let auth = row.auth {
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
        // Each cue sits in the same row as its glyph, so it takes the SAME contrast-safe tint (#388) —
        // a system color beside the token-tinted glyph would read as two different shades. A healing
        // account's `recovering` cue stays neutral (it is holding, not acting). #427: the 🟠 degraded
        // cue is orange (`--ut-o`), the 🔴 dead cue red (`--ut-r`).
        guard !row.isRecovering else { return .secondary }
        switch auth {
        case .dead:     return .panel(StatusPanelFormat.healthTint(.red))
        case .degraded: return .panel(StatusPanelFormat.healthTint(.orange))
        default:        return .secondary
        }
    }

    /// Map the pure `HealthTint` role to its contrast-safe panel tint (#388) — never `accentColor` (the
    /// AUTH glyph is never app-tinted, #84); `.neutral` (unknown) stays `.secondary`, the #137 "no false
    /// green". The role→token table lives in `StatusPanelFormat.healthTint` (Foundation-only, unit-tested).
    private func healthColor(_ tint: StatusPanelFormat.HealthTint) -> Color {
        .panel(StatusPanelFormat.healthTint(tint))
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
            weeklyReset: weeklyReset,
            blind: row.blindActive)
    }
}

// MARK: - Row building blocks (per the design reference)

/// The account's monogram badge — a smart 2-char MONOGRAM over a per-account identity COLOR (issue #445),
/// both seeded from the operator `label` (never a provider brand mark or logo — #15/#173: the color is a
/// LOW-CHROMA generic identity hue with the accent EXCLUDED, and it is only ever a REDUNDANT cue beside the
/// monogram glyph + the row's label text, never color-alone — WCAG 1.4.1). The monogram is PRE-RESOLVED by
/// the parent (`RosterView` / `StatsContent`) so its collision-escalation sees every sibling label.
/// Accessibility-hidden; the row's VoiceOver label already speaks the identity.
private struct MonogramBadge: View {
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
    /// The FILL deliberately keeps the system-bright colors (≈ the mock's `--u-*` fill family): a bar is a
    /// non-text fill (WCAG 3:1), so — unlike the small `pctColor` TEXT, which took the darker `--ut-*` tokens
    /// in #388 — it does NOT need the contrast-safe tint (leaving it here is intentional, not an oversight).
    private var barColor: Color {
        switch severity {
        case .red:    return .red
        case .yellow: return .orange
        case .green:  return .green
        // #388-EXEMPT: reached only when `severity == nil` ⇒ `pct == nil` ⇒ `fraction == 0` ⇒ the `UsageBar`
        // fill has ZERO width (a failed poll shows a BARE track, matching the mock), so this muted color never
        // actually paints. No absolute mock fill exists for the failed-poll bar → keeps `secondary.opacity`.
        case .none:   return Color.secondary.opacity(0.45)
        }
    }

    /// The percent TEXT carries its severity band in color, matching the `status` CLI (which colors green
    /// percents green too — `Severity::Green => "32"`) and the design reference: green healthy, ≥75% amber,
    /// ≥90%/exhausted red. As small text it takes the contrast-safe `--ut-*` TEXT tints (#388) — a family
    /// apart from the bar's brighter `--u-*` fill (`barColor`, unchanged). A failed poll (`n/a`) stays
    /// neutral — no false green (#137).
    private var pctColor: Color {
        .panel(StatusPanelFormat.usageTextTint(severity))
    }
}

/// A capsule fill proportional to `fraction` (0…1), with a minimum sliver so a live-but-tiny percent
/// never reads as empty; a zero/failed reading shows a bare track. The number carries the real value.
private struct UsageBar: View {
    let fraction: Double
    let color: Color
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                // Track = mock `--track` neutral fill (#388) — replaces a washed `Color.secondary.opacity(0.20)`.
                Capsule().fill(Color.panelFill(.track, dark: colorScheme == .dark))
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

/// The active account's blind-state block (issues #479/#485) — the panel's render of the daemon
/// `BlindActive`, REPLACING the two live meters a healthy row shows. A HELD session bar (dashed — a frozen
/// last-known value, never a live fill, #137) at the last-known %, the `blind {dur}` chip, the
/// LAST-KNOWN·RATE-LIMITED caption, and the auto-protection verdict — every fact from a unit-tested
/// `StatusPanelFormat` verdict, so this View stays a thin, un-screenshot-tested consumer. The held row
/// reuses `UsageMeter`'s SESSION-label (52) and percent (40) columns so THOSE align with sibling rows; its
/// trailing chip is wider (58 vs the reset column's 52) to fit `blind {dur}` un-clipped, so the held bar
/// itself sits ~6 pt narrower than a live sibling's — an accepted legibility trade, not a lined-up column.
private struct BlindMeter: View {
    let blind: BlindActive
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        // The last-known session % carries the SAME severity band a live meter would (green/amber/red) —
        // the held bar shows "the last reading was at X%", while the blind OK/DEGRADED verdict rides the
        // eye glyph, the leading rule, and the shield line below (two orthogonal facts, two colour channels).
        let severity = StatusPanelFormat.utilSeverity(blind.lastKnownSessionPct)
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 9) {
                Text("SESSION")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .frame(width: 52, alignment: .leading)

                HeldUsageBar(fraction: Double(blind.lastKnownSessionPct) / 100.0, color: barColor(severity))

                Text(StatusPanelFormat.pct(blind.lastKnownSessionPct))
                    .font(.system(size: 12, weight: .semibold)).monospacedDigit()
                    .foregroundStyle(Color.panel(StatusPanelFormat.usageTextTint(severity)))
                    .frame(width: 40, alignment: .trailing)

                Text(StatusPanelFormat.blindDurationChip(blind.blindSecs))
                    .font(.system(size: 11, weight: .medium)).monospacedDigit()
                    .foregroundStyle(.secondary)
                    .frame(width: 58, alignment: .trailing)
                    .lineLimit(1)
            }

            // WHY the bar is held — the value is last-known and the poll is rate-limited (the #137 tell,
            // so a held bar is never read as a live one).
            Text(StatusPanelFormat.blindLastKnownCaption)
                .font(.system(size: 9, weight: .semibold))
                .tracking(0.3)
                .foregroundStyle(.tertiary)

            // The auto-protection verdict — OK calm / DEGRADED orange — mirroring the CLI's blind line.
            let verdict = StatusPanelFormat.blindVerdict(degraded: blind.autoProtectionDegraded)
            HStack(spacing: 5) {
                Image(systemName: verdict.symbol)
                    .symbolRenderingMode(.hierarchical)
                    .font(.system(size: 11))
                    .foregroundStyle(Color.panel(StatusPanelFormat.healthTint(verdict.tint)))
                Text(verdict.text)
                    .font(.caption)
                    .foregroundStyle(Color.panel(StatusPanelFormat.healthTint(verdict.tint)))
            }
            // The row's VoiceOver label already speaks the whole blind state as one element (#485).
            .accessibilityHidden(true)
        }
    }

    /// The held bar's fill hue — the SAME bright severity family `UsageMeter.barColor` uses (a bar is a
    /// non-text fill, WCAG 3:1), keyed off the last-known session band.
    private func barColor(_ severity: StatusPanelFormat.UsageSeverity) -> Color {
        switch severity {
        case .red:    return .red
        case .yellow: return .orange
        case .green:  return .green
        }
    }
}

/// A HELD usage bar (#485) — the last-known fill under a DASHED capsule outline. The dash is the "held /
/// estimate, not live" tell that reads at the 6 px bar height where diagonal hatching would not, so a
/// frozen last-known value is never mistaken for a live meter (#137). The fill itself keeps the severity
/// hue (muted) so the band is still legible.
private struct HeldUsageBar: View {
    let fraction: Double
    let color: Color
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.panelFill(.track, dark: colorScheme == .dark))
                // Muted fill (0.5α) — a held value reads dimmer than a live meter, never a bright false-now.
                Capsule().fill(color.opacity(0.5))
                    .frame(width: fillWidth(geo.size.width))
                // Dashed outline over the whole track — the legible-at-6px "held" signal.
                Capsule().strokeBorder(color.opacity(0.9),
                                       style: StrokeStyle(lineWidth: 1, dash: [2.5, 2]))
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }

    private func fillWidth(_ full: CGFloat) -> CGFloat {
        let clamped = min(1, max(0, fraction))
        guard clamped > 0 else { return 0 }
        return max(5, full * clamped)
    }
}

// MARK: - Header + callouts (per the design reference)

/// The app-identity header — a neutral gauge glyph, the product name, and the honest identity sub-line
/// (`StatusPanelFormat.headerSubtitle`). Always present; the SUB-LINE — never the glyph — carries the
/// connection state, so a degraded daemon reads "last-known" / "· stale", never a false "active".
/// Provider-neutral (issue #173): a generic gauge, no brand mark or color.
private struct PanelHeader: View {
    let subtitle: String
    /// Whether to show the Status|Stats seg control (issue #446). Only where the Stats tab can deliver (a
    /// live roster, not the capture surface; gated in `content`). Defaults off, so every degraded-state
    /// header is byte-unchanged from before #446.
    var showsSwitcher: Bool = false
    @EnvironmentObject private var stats: PanelStatsModel
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 10) {
            // The identity block (glyph + name + sub-line) combines into ONE accessibility element; the seg
            // control keeps its own button traits alongside it, so VoiceOver reads "Sessiometer, …" then the
            // two tab buttons rather than one merged blob.
            HStack(spacing: 10) {
                RoundedRectangle(cornerRadius: 7)
                    // Mock `--badge-bg` neutral fill (#388) — replaces a washed `Color.secondary.opacity(0.16)`.
                    .fill(Color.panelFill(.badge, dark: colorScheme == .dark))
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
        .padding(.horizontal, 14).padding(.top, 12).padding(.bottom, 11)
    }
}

/// The mock's `.seg` Status|Stats control (issue #446): a rounded two-button switcher, right-aligned in the
/// header. The active tab carries the raised `--seg-on` chip; the inactive is a quiet transparent button.
/// Provider-neutral, read-only chrome — selecting Stats only QUERIES (UI never acts). The seg colors are the
/// mock's exact `--seg-*` chrome values inline (decorative control chrome, not a data-bearing tint — the
/// data colors, `--spark` / `--sig-*`, live in the testable `StatusPanelFormat` layer).
private struct PanelTabSwitcher: View {
    let tab: PanelStatsModel.Tab
    let select: (PanelStatsModel.Tab) -> Void
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 2) {
            segButton("Status", target: .status)
            segButton("Stats", target: .stats)
        }
        .padding(2)
        .background(
            RoundedRectangle(cornerRadius: 7)
                .fill(segBackground)
                .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(segBorder, lineWidth: 0.5))
        )
    }

    private func segButton(_ title: String, target: PanelStatsModel.Tab) -> some View {
        let on = tab == target
        return Button { select(target) } label: {
            Text(title)
                .font(.system(size: 11, weight: on ? .semibold : .medium))
                .foregroundStyle(on ? Color.primary : Color.secondary)
                .padding(.horizontal, 9)
                .padding(.vertical, 2.5)
                .background(
                    RoundedRectangle(cornerRadius: 5)
                        .fill(on ? segOnFill : Color.clear)
                        .shadow(color: on ? Color.black.opacity(0.18) : .clear, radius: 0.75, y: 0.5)
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

/// The leading status dot — the design reference's per-row marker: a filled accent disc for the active
/// (being-consumed) account, a hollow ring otherwise. FILL-vs-RING is a SHAPE difference, so active is
/// legible without color (WCAG 1.4.1); the accent is a redundant cue and the row's "ACTIVE" tag +
/// VoiceOver label state it in words.
private struct StatusDot: View {
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

/// The swap-callout hero — the design reference's primary action: the daemon's `next_swap` target, the
/// daemon's OWN "why" line (issue #393 — carried on the wire, no longer client-derived), and the live
/// Swap button. Accent-tinted: this is the panel's ONE accent action, the daemon's RECOMMENDATION
/// (Von Restorff — the quiet per-row switches are the operator choosing instead).
///
/// WYSIWYG (issue #169): the button sends the `target` this card DISPLAYS — never a client re-pick, and
/// never a targetless "swap to whatever you'd choose" verb. It is the same `swap` command a per-row
/// switch sends; the daemon re-validates it either way.
private struct SwapCalloutCard: View {
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
        HStack(spacing: 10) {
            Image(systemName: "arrow.left.arrow.right")
                .font(.system(size: 16, weight: .semibold))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            // The card's TEXT is one combined VoiceOver element; the button below is a SEPARATE one.
            // (Combining the whole card, as this did while the button was dead, would now swallow a live
            // control and leave it unreachable.)
            VStack(alignment: .leading, spacing: 1) {
                // MIDDLE-truncate the TARGET label (issue #445), keeping the "Next swap →" prefix whole, so a
                // same-local-part target's distinguishing suffix survives the elision (the earlier "clunky"
                // read was a tail-truncated target). The prefix is `.fixedSize`d; the target absorbs the
                // squeeze. The spoken label (`accessibilityText`) is unchanged — it carries the full target.
                HStack(spacing: 0) {
                    Text("Next swap → ").fixedSize()
                    Text(target).fontWeight(.semibold)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .font(.system(size: 12))
                if let reason {
                    Text(reason)
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(accessibilityText)

            Spacer(minLength: 6)

            Button(action: { Task { await swap.swap(to: target) } }) {
                if isSwitchingToTarget {
                    HStack(spacing: 5) {
                        ProgressView().controlSize(.small)
                        Text(StatusPanelFormat.swapPendingText)
                    }
                } else {
                    Text("Swap")
                }
            }
            .font(.system(size: 12, weight: .semibold))
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
            // Any in-flight swap disables this button too — the footer and the rows are siblings on the
            // one `swap` verb, and the daemon holds a single-writer lock behind it.
            .disabled(swap.phase.isPending)
            .help(StatusPanelFormat.switchHelpText(label: target))
            .accessibilityLabel(isSwitchingToTarget
                                ? "Switching to \(target)"
                                : StatusPanelFormat.switchHelpText(label: target))
        }
        .padding(.leading, 11).padding(.trailing, 8).padding(.vertical, 9)
        // Fill + border opacities are theme-aware (#388, mock `--accent-tint` / `--accent-tint-border`):
        // .10/.20 light, .16/.30 dark — the dark callout was too faint hardcoded to the light values.
        .background(
            RoundedRectangle(cornerRadius: 9)
                .fill(Color.accentEmphasis(.swapCalloutFill, dark: colorScheme == .dark))
                .overlay(RoundedRectangle(cornerRadius: 9)
                    .strokeBorder(Color.accentEmphasis(.swapCalloutBorder, dark: colorScheme == .dark), lineWidth: 0.5))
        )
        .padding(.horizontal, 8).padding(.top, 9).padding(.bottom, 4)
    }

    /// The spoken label for the card's text: identity + the daemon's reason (when present). Omits the
    /// reason clause for a pre-#393 daemon (`reason == nil`), so VoiceOver never speaks a dangling ". ."
    /// where the "why" line is absent. The Swap button speaks for itself.
    private var accessibilityText: String {
        if let reason {
            return "Next swap to \(target). \(reason)."
        }
        return "Next swap to \(target)."
    }
}

/// The settled swap's inline outcome (issue #169) — one line beneath the swap-callout card, shared by
/// BOTH swap paths (the footer recommendation and a per-row manual switch), because the daemon holds a
/// single-writer swap lock: at most one swap is ever in flight, so at most one outcome needs a home.
///
/// PENDING renders nothing here — it is shown ON the clicked row / the footer button, where the operator
/// is already looking; a second spinner would be noise. `done` clears itself after a short beat; a
/// `failed` persists until the next swap attempt, so an error the operator has not read cannot vanish.
private struct SwapStatusLine: View {
    @EnvironmentObject private var swap: AccountSwapModel

    var body: some View {
        switch swap.phase {
        case .idle, .pending:
            EmptyView()
        case .done(let success):
            line(StatusPanelFormat.swapDoneText(success),
                 symbol: "checkmark.circle.fill", tint: .green)
                .lineLimit(1)
                .truncationMode(.middle)
        case .failed(let failure):
            line(StatusPanelFormat.swapErrorText(failure),
                 symbol: "exclamationmark.triangle.fill", tint: .red)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func line(_ text: String, symbol: String, tint: Color) -> some View {
        Label(text, systemImage: symbol)
            .font(.system(size: 11))
            .foregroundStyle(tint)
            .padding(.horizontal, 12).padding(.vertical, 2)
    }
}

/// The in-app capture affordance (issue #360) — a "Capture active account" button + an inline operator-
/// label field, hosted by BOTH capture surfaces: the empty-roster / first-run onboarding card and the
/// status-item "Add account…" menu surface (issue #394). It sends `{"cmd":"capture","label":…}` over the
/// #358 control-command transport (via `AccountCaptureModel`) and renders the redacted ack's
/// idle → pending → done → error phase. It NEVER inserts the captured row — that arrives on its own via the
/// `watch` snapshot (issue #360 AC); on success the affordance just returns to idle. The client still
/// originates NO credential (C-005 held): a verb + non-secret label out, a redacted ack back.
///
/// Capture snapshots the account currently logged into Claude Code — it is NOT an account picker. To add a
/// DIFFERENT account the operator runs `claude /login` first, then captures (the honest scope boundary,
/// surfaced as the secondary hint). An already-active-and-rostered account is an idempotent refresh.
private struct CaptureAffordance: View {
    @EnvironmentObject private var capture: AccountCaptureModel
    @State private var label = ""
    @FocusState private var fieldFocused: Bool

    var body: some View {
        // The prominent, stacked treatment — the field, the primary Capture button, the status line, then
        // the scope hint. Both capture surfaces (#360 onboarding, #394 menu) use this one treatment.
        VStack(alignment: .leading, spacing: 9) {
            field
            HStack(spacing: 8) {
                button
                Spacer(minLength: 0)
            }
            status
            Text("To add a different account, run claude /login first, then capture.")
                .font(.system(size: 10.5))
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
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
    private var button: some View {
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
        .font(.system(size: 12, weight: .semibold))
        .controlSize(.small)
        .buttonStyle(.borderedProminent)
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

    /// Submit a capture of the currently-active account under the field's label (blank → the daemon derives
    /// the handle). The model owns the pending → done / failed transitions.
    private func submit() {
        let text = label
        Task { await capture.capture(rawLabel: text) }
    }
}

// MARK: - Capture card

/// The capture card — an explanatory title + line, plus the shared `CaptureAffordance`. Its TWO entry
/// points differ only in `title`: the empty-roster / first-run onboarding state (issue #326 / #360:
/// "Capture your first account", visually distinct from daemon-down) and the status-item "Add account…"
/// menu surface (issue #394: "Add account", the populated-panel path now that the persistent capture bar
/// is gone). The capture mechanics + honest pending → done → error are identical either way — the affordance
/// sends the command over the #358 transport and renders the redacted ack; the captured row then arrives on
/// its own via the `watch` snapshot (the app still originates no credential — a verb + label out, a redacted
/// ack back).
private struct CaptureCard: View {
    let title: String
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            Text(title)
                .font(.subheadline.weight(.semibold))
            Text("Capture the account you’re signed into — the daemon adds it to the roster and starts tracking it here.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            CaptureAffordance()
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        // Mock `--card-bg` neutral fill (#388) — replaces a washed `Color.secondary.opacity(0.08)`.
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.panelFill(.card, dark: colorScheme == .dark)))
    }
}

// MARK: - Stats tab (issue #446 — the mock's `.stats` view over the socket `stats` verb)

/// The Stats tab body (issue #446): the mock's per-account 7-day sparklines + numeric body, aggregate
/// callout, and signal legend — fed by the socket `stats` verb (never a store read). Renders the stats
/// model's phase honestly: a loading placeholder, a failure message (never a blank tab), or the loaded
/// content. READ-ONLY — it queries and renders, it never acts (the crown-jewel + footer-`next_swap`
/// invariants belong to the Status tab).
private struct StatsView: View {
    @EnvironmentObject private var store: WatchStatusStore
    @EnvironmentObject private var stats: PanelStatsModel

    var body: some View {
        switch stats.phase {
        case .idle, .loading:
            StatsMessage(text: "Loading usage stats…")
        case .failed(let failure):
            StatsMessage(text: StatusPanelFormat.statsFailureText(failure))
        case .loaded(let wire):
            StatsContent(wire: wire,
                         activeLabel: store.rows.first(where: \.isActive)?.label,
                         rosterOrder: store.rows.map(\.label))
        }
    }
}

/// A centered one-line Stats-tab message — the loading placeholder and the honest failure surface. Keeps the
/// tab from ever rendering blank (or a fabricated number) when there is no series to show.
private struct StatsMessage: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, 20).padding(.vertical, 22)
    }
}

/// The loaded Stats view: the per-account rows (ordered to match the Status roster), then the aggregate
/// callout + signal legend. Identity (name + monogram + active marker) joins the stats handles with the
/// live roster the panel already holds — provider-neutral (#173), exactly like the Status roster.
private struct StatsContent: View {
    let wire: StatsWire
    /// The active account's handle (from the watch snapshot the panel already renders) — marks the active
    /// stats row, the only roster fact the Stats tab reads. `nil` when none is active.
    let activeLabel: String?
    /// The roster's handle order, so the Stats rows list accounts identically to the Status tab.
    let rosterOrder: [String]

    var body: some View {
        let handles = StatusPanelFormat.orderedStatHandles(
            summaryHandles: Set(wire.summary.accounts.keys), rosterOrder: rosterOrder)
        // Resolve monograms over the SAME handle set the Stats tab lists (issue #445) — the disambiguation
        // kit applies identically on both tabs, since they render the same accounts.
        let monograms = StatusPanelFormat.accountMonograms(handles)
        VStack(alignment: .leading, spacing: 0) {
            // The daemon told us its config is unreadable (issue #642) — say so ABOVE the numbers, so the
            // caveat is read before the figures it qualifies, never as a footnote after they've landed.
            if let detail = wire.configUnreadable {
                StatsCaveat(text: StatusPanelFormat.statsConfigUnreadableNote(detail))
            }
            VStack(alignment: .leading, spacing: 2) {
                ForEach(handles, id: \.self) { handle in
                    if let account = wire.summary.accounts[handle] {
                        StatStripRow(handle: handle,
                                     monogram: monograms[handle] ?? "?",
                                     account: account,
                                     series: StatusPanelFormat.sparkSeries(wire.series, handle: handle),
                                     isActive: handle == activeLabel)
                    }
                }
            }
            // Mock `.stats { padding:6px 8px 2px }` — inset to align with the roster + aggregate below.
            .padding(.horizontal, PanelMetrics.rosterInset).padding(.top, 6).padding(.bottom, 2)

            StatsAggregate(roster: wire.summary.roster, window: wire.window)
            SignalLegend()
        }
    }
}

/// The Stats-tab caveat strip (issue #642): a dot-plus-sentence line telling the operator that the figures
/// below rest on DEFAULT tunables because the daemon could not read `config.toml`. Deliberately the same
/// dot + tint + secondary-text vocabulary as `BannerView` (the panel's established degraded idiom) rather
/// than a new affordance — and deliberately `.orange`/`.warning` weight, not `.error`: the series IS real
/// data, only its ceiling-dependent framing is off. Panel-surface only; it never escalates the menu-bar
/// glyph (a new glyph fault class is design-gated, and a stale tunable is not a fleet fault).
///
/// Part of this text is DAEMON-AUTHORED, so the panel does not own its length. The daemon keeps it short by
/// construction — it sends one of a small set of STATIC reasons, never the parser's own message (which
/// re-prints the operator's config file) — and `lineLimit` below is the panel-side suspenders to that belt:
/// the popover is fixed in WIDTH (380pt) but INTRINSIC in height, with no `ScrollView`, so an unbounded
/// reason from a drifted daemon would not clip, it would grow the whole panel arbitrarily tall.
private struct StatsCaveat: View {
    let text: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Circle()
                .fill(Color.orange)
                .frame(width: 6, height: 6)
                .accessibilityHidden(true)
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .lineLimit(5)
                .truncationMode(.tail)
            Spacer(minLength: 0)
        }
        // `rosterInset` is the stats block's own inset; the `+ 8` is the row card's internal padding
        // (mock `.stat { padding:10px 8px }`), so this dot lines up with a row's status dot, not its card edge.
        .padding(.horizontal, PanelMetrics.rosterInset + 8)
        .padding(.top, 8)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(text)
    }
}

/// One account's Stats row (mock `.stat`): identity + a 7-day session-peak sparkline + the neutral signal
/// pill, over a three-cell numeric body (session mean/peak, weekly peak, cap-hits). The active account wears
/// the accent-tint card fill (mock `.stat.active`, the SAME `--active-bg` token the Status roster uses).
private struct StatStripRow: View {
    let handle: String
    /// The roster-resolved 2-char monogram for this handle (issue #445), computed once by `StatsContent`.
    let monogram: String
    let account: StatsAccountStats
    /// The per-bucket session-peak series (0…1, fixed-scale) — the sparkline source.
    let series: [Double]
    let isActive: Bool
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let signal = StatusPanelFormat.statsSignal(account.band)
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 9) {
                StatusDot(isActive: isActive)
                MonogramBadge(label: handle, monogram: monogram)
                // Provider-neutral name (#15): the redacted handle, exactly as the Status roster shows it —
                // the mock's `.s-prov` provider brand is intentionally dropped (the shipped app names no provider).
                Text(handle)
                    .font(.system(size: 13, weight: .semibold))
                    // MIDDLE-truncation (issue #445) — same as the Status roster, so a same-local-part handle's
                    // distinguishing suffix survives elision on the Stats tab too.
                    .lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 6)
                Sparkline(values: series)
                SignalPill(signal: signal)
            }
            HStack(alignment: .top, spacing: 8) {
                StatCell(label: "Session m/pk", value: StatusPanelFormat.statsSessionMeanPeak(account))
                StatCell(label: "Weekly pk", value: StatusPanelFormat.statsWeeklyPeak(account))
                StatCell(label: "Cap-hits", value: "\(account.capHits)")
            }
            .padding(.leading, 17)  // mock `.stat-body { margin-left:17px }` — aligns the body under the name
        }
        .padding(.vertical, 10).padding(.horizontal, 8)  // mock `.stat { padding:10px 8px }`
        .background(
            RoundedRectangle(cornerRadius: 9)
                .fill(isActive ? Color.accentEmphasis(.activeRowFill, dark: colorScheme == .dark) : Color.clear)
        )
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    /// The spoken row summary — identity + signal + the numeric body, so the sparkline (accessibility-hidden,
    /// a purely visual trend) is still conveyed as facts.
    private var accessibilityLabel: String {
        let signal = StatusPanelFormat.statsSignal(account.band)
        let sessionMean = StatusPanelFormat.statsPercent(account.session.mean)
        let sessionPeak = StatusPanelFormat.statsPercent(account.session.peak)
        let weeklyPeak = StatusPanelFormat.statsPercent(account.weekly.peak)
        let active = isActive ? ", active" : ""
        return "\(handle)\(active). \(signal.label). Session mean \(sessionMean) percent, "
            + "peak \(sessionPeak) percent. Weekly peak \(weeklyPeak) percent. \(account.capHits) cap hits."
    }
}

/// One numeric cell of the Stats row's three-column body (mock `.sc`): an uppercase micro-label over a
/// tabular-figure value. Equal-width (`maxWidth: .infinity`), mirroring the mock's `repeat(3, 1fr)` grid.
private struct StatCell: View {
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased())
                .font(.system(size: 9, weight: .semibold)).tracking(0.5)
                .foregroundStyle(.tertiary)
                .lineLimit(1)
            Text(value)
                .font(.system(size: 13, weight: .semibold)).monospacedDigit()
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The per-account 7-day sparkline (mock `.spark`): a drawn area + line + end-dot over the per-bucket session
/// peaks, on the FIXED [0, 1] (0–100% of cap) scale — R-2 parity with the CLI trend sparkline (`src/stats.rs`),
/// NOT auto-normalised. The 96 × 28 box + inset 3 reproduce the mock's `.spark` viewBox exactly. The geometry
/// is the pure, unit-tested `StatusPanelFormat.sparkPoints`; this view only strokes/fills it.
private struct Sparkline: View {
    let values: [Double]
    @Environment(\.colorScheme) private var colorScheme

    private let boxWidth = 96.0
    private let boxHeight = 28.0
    private let inset = 3.0

    var body: some View {
        Canvas { context, _ in
            let points = StatusPanelFormat.sparkPoints(values, width: boxWidth, height: boxHeight, inset: inset)
            guard points.count >= 2 else { return }
            let color = Color.spark(dark: colorScheme == .dark)

            var line = Path()
            line.move(to: CGPoint(x: points[0].x, y: points[0].y))
            for point in points.dropFirst() {
                line.addLine(to: CGPoint(x: point.x, y: point.y))
            }

            // Area = the line closed down to the plot baseline (mock `.sp-area` closes to y = height − inset),
            // filled at a fraction of the stroke alpha (mock `.sp-area { fill-opacity:.2 }`).
            let baseline = boxHeight - inset
            var area = line
            area.addLine(to: CGPoint(x: points[points.count - 1].x, y: baseline))
            area.addLine(to: CGPoint(x: points[0].x, y: baseline))
            area.closeSubpath()
            context.fill(area, with: .color(color.opacity(0.2)))

            context.stroke(line, with: .color(color),
                           style: StrokeStyle(lineWidth: 1.75, lineCap: .round, lineJoin: .round))

            // The end dot marks the latest bucket (mock `.sp-dot`, r 1.7).
            let last = points[points.count - 1]
            let dot = Path(ellipseIn: CGRect(x: last.x - 1.7, y: last.y - 1.7, width: 3.4, height: 3.4))
            context.fill(dot, with: .color(color))
        }
        .frame(width: boxWidth, height: boxHeight)
        .accessibilityHidden(true)  // a purely visual trend; the row label speaks the numeric values
    }
}

/// The neutral utilisation signal pill (mock `.signal`): a colored dot + descriptor word (underused /
/// balanced / saturated), tinted by the mock's `--sig-*` tokens. A DESCRIPTOR of the session-peak band, never
/// a recommendation — the read-only Stats tab states the magnitude, it does not advise.
private struct SignalPill: View {
    let signal: StatusPanelFormat.StatSignal
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let dark = colorScheme == .dark
        HStack(spacing: 5) {
            Circle()
                .fill(Color.statsSignalText(signal, dark: dark))
                .frame(width: 6, height: 6)
            Text(signal.label)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Color.statsSignalText(signal, dark: dark))
                .lineLimit(1)
        }
        .padding(.horizontal, 8).padding(.vertical, 3)
        .background(Capsule().fill(Color.statsSignalFill(signal, dark: dark)))
        .fixedSize()
        .accessibilityHidden(true)  // spoken via the row's accessibility label
    }
}

/// The aggregate callout under the Stats rows (mock `.agg`): the roster-wide all-accounts-high water + swap
/// count over the window, in a neutral card. Facts only (magnitudes + the span), never a recommendation.
private struct StatsAggregate: View {
    let roster: StatsRoster
    let window: StatsWindow
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "chart.line.uptrend.xyaxis")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            Text(StatusPanelFormat.statsAggregateText(roster: roster, window: window))
                .font(.system(size: 11.5))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.vertical, 9).padding(.horizontal, 11)
        .background(RoundedRectangle(cornerRadius: 9).fill(Color.panelFill(.card, dark: colorScheme == .dark)))
        .padding(.horizontal, 12).padding(.vertical, 6)  // mock `.agg { margin:6px 12px }`
    }
}

/// The signal legend (mock `.sig-legend`): the three descriptor pills + the neutrality note. Reinforces the
/// read-only ethos — "descriptive · equal weight · no action implied" — so the pills never read as an alarm.
private struct SignalLegend: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                Text("SIGNAL")
                    .font(.system(size: 9.5, weight: .semibold)).tracking(0.6)
                    .foregroundStyle(.tertiary)
                SignalPill(signal: .underused)
                SignalPill(signal: .balanced)
                SignalPill(signal: .saturated)
                Spacer(minLength: 0)
            }
            Text("descriptive · equal weight · no action implied")
                .font(.system(size: 10)).italic()
                .foregroundStyle(.tertiary)
        }
        // Mock `.sig-legend { margin:2px 12px 13px; border-top }` — a hairline over the note.
        .padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 13)
        .overlay(alignment: .top) {
            Divider().padding(.horizontal, 12)
        }
        .accessibilityHidden(true)  // static explanatory chrome; the per-row labels speak each signal
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
            // Mock `.pop-foot .fl2 { color: var(--text-3) }` — the snapshot-age line is tertiary; the mock's
            // `.fl2.stale { color: var(--ut-a) }` turns it amber only when the reading should be distrusted
            // (wedged poll loop / stale / disconnected). That amber is the SAME contrast-safe `--ut-a` token
            // as the stale auth glyph (#388) — small text on the vibrancy, so never raw system orange (< 4.5:1).
            .foregroundStyle(stale ? .panel(StatusPanelFormat.healthTint(.yellow)) : Color(nsColor: .tertiaryLabelColor))
            .padding(.horizontal, 14).padding(.top, 9).padding(.bottom, 11)
        }
        .padding(.top, 5)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(stale ? "\(text), stale" : text)
    }
}
