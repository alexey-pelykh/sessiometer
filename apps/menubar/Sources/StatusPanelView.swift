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
//
// File layout (issue #640): this file holds the root view and its metrics; the subviews live beside it, split
// along the seams this file already marked — `StatusPanelTint` (the one `Color` seam), `StatusPanelSharedViews`
// (the cross-cluster `BannerView` / `MonogramBadge` / `StatusDot`), `StatusPanelRoster`, `StatusPanelChrome`,
// `StatusPanelCapture`, and `StatusPanelStats`. The split moved declarations verbatim and widened only the
// access modifiers the new file boundaries require; no view tree, signature, or `body` changed.

import SwiftUI

/// The panel's fixed layout constants — thin references to the source-of-truth in `StatusPanelFormat`
/// (the testable layer that also owns the width gate). The panel is FIXED-width by construction
/// (`.frame(width:)` below), so a roster row's available width is a DERIVED CONSTANT, not something to
/// measure. `StatusPanelFormat.rowFitsSwitchAffordance` gates the manual-switch affordance on it (issue
/// #169's "gate the affordance on available row width"). If the panel ever becomes resizable or gains a
/// compact mode, feed a MEASURED width into that same gate — the gate itself does not change.
enum PanelMetrics {
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
    /// The launch-at-login / Start-daemon model (issue #170): observed here so the `.notRunning` body can
    /// render `StartDaemonCard` — the honest Start affordance that appears only where it can act
    /// (`canStartDaemon`) and otherwise degrades to the same inert banner the other cold states show.
    @EnvironmentObject private var loginItem: LoginItemModel

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

        case .connecting, .starting, .unsupported, .crashLooping:
            // No trustworthy reading to show — a plain honest message card. `.crashLooping` (#169) holds
            // here too: the daemon served a snapshot but keeps dropping before it stabilizes, so its
            // numbers are refused ("holding status until it stays up") rather than flickered as live —
            // the crown-jewel anti-#137 debounce. `.starting` (#499) is the cold-refused daemon-absent
            // state that never held a reading, so it renders the honest banner card. (`.notRunning` is its
            // sibling but now carries the #170 Start affordance — see the dedicated branch below.)
            Divider().padding(.horizontal, 14)
            BannerView(banner: StatusPanelFormat.banner(for: state, accountCount: store.rows.count))
                .padding(.horizontal, 14).padding(.vertical, 14)

        case .notRunning:
            // The daemon is installed-but-down (#499): it never held a reading, so — like its `.starting`
            // sibling — it shows the honest "Daemon not running" banner. Unlike the others it ALSO hosts the
            // #170 Start affordance: `StartDaemonCard` reuses that banner and, ONLY where a bundled agent is
            // registrable and no CLI owns the label (`loginItem.canStartDaemon`), offers a "Start daemon"
            // button that registers + launches the agent via `SMAppService`. In the #170 shipped state no
            // plist is bundled yet (that co-lands with #171), so `canStartDaemon` is false and the card is
            // exactly the inert banner it was before — never a dead button. (View log / Restart remain
            // #169/#171 siblings.)
            Divider().padding(.horizontal, 14)
            StartDaemonCard()
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
                                                                     systemicRefreshFailure: store.systemicRefreshFailure,
                                                                     canary: store.canary) {
                // The single daemon-level fault banner (worst-first): a fleet-wide lockout or mechanism
                // failure NO per-row `auth` reflects (rows can read healthy while the shared item sits locked
                // or emptied, and while the refresh mechanism is down every account is still alive), so it
                // rides as its own honest banner ABOVE the roster — the connected-but-degraded panel reads
                // visibly DEGRADED (never healthy) while the live roster still renders below. The footer stays
                // the `next_swap` line (R-2: footer = next_swap; degraded daemon-level signals → honest
                // banner). The panel shows ONE banner, ranked worst-first over (fault, VARIANT) — never over
                // fault identity, so a calm self-healing state can never outrank one that cannot self-heal.
                // See `daemonFaultBanner` for the seven ranks (over four faults) and why `recovering` sits
                // last of them.
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
                // #572: the active blind row composes the CORNERED verdict from `store.rosterNextSwap` — the
                // honest-state-gated next-swap that WITHHOLDS a retained `noViableTarget` under `.stale`
                // (watchdog elapsed) so it degrades to orange DEGRADED, matching the stale `!` glance rather
                // than a loud red "cannot act" off unvouched data (#137). MUST read `rosterNextSwap`, not the
                // raw `store.nextSwap`. (The dropped roster at `.disconnected`/`.reconnecting` above is dimmed
                // and passes no `nextSwap`.)
                RosterView(rows: store.rows, now: now, switchable: true, nextSwap: store.rosterNextSwap)
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
