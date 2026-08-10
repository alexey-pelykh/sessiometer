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
///
/// Every constant here is the value at the DEFAULT text size; the `scale:` overloads are what the views
/// actually lay out with (issue #756). Because the panel scales UNIFORMLY — every font and every constant
/// multiplied by the same factor — the unscaled forms remain the honest base measurements, and anything
/// derived from them (`StatusPanelFormat.rosterLabelBudget` and friends) scales by simple multiplication.
enum PanelMetrics {
    /// The panel's content width at the default text size.
    static let width = CGFloat(StatusPanelFormat.panelContentWidth)
    /// The roster's horizontal inset (`RosterView`), which the rows sit inside, at the default text size.
    static let rosterInset = CGFloat(StatusPanelFormat.rosterHorizontalInset)
    /// The width available to one roster row at the default text size.
    static var rowWidth: Double { StatusPanelFormat.defaultRowWidth }

    /// The panel's content width at `scale`. The panel stays FIXED-width at any given size class — it is
    /// the size class, not the content, that moves it.
    static func scaledWidth(_ scale: Double) -> CGFloat { width * scale }
    /// The roster's horizontal inset at `scale`.
    static func scaledRosterInset(_ scale: Double) -> CGFloat { rosterInset * scale }

    /// The panel's maximum height at EVERY size class (issue #818) — the budget the scroll boundary hands
    /// its unbounded body once the popover would otherwise run off the screen. Derived, and pending
    /// design-owner ratification; `StatusPanelFormat.panelHeightBudget` states both the arithmetic and what
    /// it is an assumption about.
    static let heightBudget = CGFloat(StatusPanelFormat.panelHeightBudget)

    // There is deliberately NO `scaledRowWidth`. Its one would-be caller — `rowFitsSwitchAffordance` in
    // `RosterRow.offersSwitch` — compares the row against a constant the same `k` also scales, so that
    // verdict is scale-INVARIANT and reads the unscaled `rowWidth` on purpose. Offering a scaled accessor
    // would invite feeding it to an unscaled threshold, which is a real bug rather than a tidier call site.
    //
    // There is deliberately NO `scaledHeightBudget` either, for the OPPOSITE reason and it is worth reading
    // as its own rule rather than as a second instance of the one above: that accessor is absent because its
    // verdict is scale-invariant, this one because its subject is. The budget measures the SCREEN, and a
    // display does not get taller when the operator raises the text size — so scaling it would grant a
    // `.accessibility3` panel a 2014 pt allowance on the same 900 pt display and reinstate the very overflow
    // this bound exists to close, at the exact size class that forces it.
}

/// The panel's ONE scroll boundary (issue #818) — the seam that makes a state's unbounded body reachable
/// instead of clipped off the bottom of the screen.
///
/// WHAT GOES INSIDE, stated as the rule rather than a list, because the list is the thing that rots. A body
/// wrapped here is one whose height GROWS WITHOUT BOUND with something the panel does not control: the
/// roster and the Stats tab grow per account, and a message card grows with daemon-authored text. Everything
/// else the panel draws is fixed chrome that appears at most once, and it stays OUTSIDE — see
/// `StatusPanelView.stateBody` for what that buys and `design/README.md` § The scroll boundary (#818) for
/// the decision itself, which is PENDING DESIGN-OWNER RATIFICATION: the build reference authors no scroll
/// behaviour at all, so this is a silence being filled, not a specification being followed.
///
/// EXACTLY ONE IS EVER ON SCREEN. Every state routes its body through one instance, and no state has two
/// bodies — so an operator never has to work out which of two regions a scroll gesture will move. That is a
/// property of the call sites, not something this type can enforce; keep it when adding a state.
///
/// IT IS INERT WHEN THE CONTENT FITS, which is what keeps the default size class byte-identical (AC-4).
/// `ScrollView` takes its content's ideal height when that height is offered, and the budget only ever
/// CLAMPS a proposal — it never stretches a short body to fill it (measured: a 100 pt body inside a 300 pt
/// boundary still renders 100 pt). So on a three-account roster at the default text size this draws exactly
/// what a bare `VStack` drew.
///
/// NO `.scrollBounceBehavior(.basedOnSize)`, and the omission is deliberate rather than an oversight — it
/// is the modifier a reader will reach for first, so it is worth saying why it is not here. It would
/// suppress the rubber-band on a boundary that is not binding, which is the treatment a popover wants; it
/// is also macOS 13.3+, and `project.yml` pins the deployment target at 13.0. Adopting it means an
/// `if #available` branch whose 13.0 arm no runner in this project's CI can execute — a branch nobody can
/// prove works, bought for a bounce. Raise the deployment target and take the modifier unconditionally, or
/// leave both alone.
///
/// IT SPEAKS. A scroll region publishes an `AXScrollArea` of its own, so an unlabelled one is a container a
/// VoiceOver user lands on and hears nothing about — which is why `label` is required rather than optional.
/// The four names live in `StatusPanelFormat` beside the panel's other spoken strings.
///
/// IT IS ABSENT UNDER THE RENDER HARNESS, and that is the one case where this type draws nothing at all.
/// `ImageRenderer` cannot rasterize a `ScrollView`'s content — see `\.panelScrollBoundaryEnabled` for the
/// measurement and for why bypassing it leaves the goldens honest rather than fictional.
private struct PanelScrollBoundary<Content: View>: View {
    /// What this region holds, spoken on the way in. One of `StatusPanelFormat.scrollRegion*Label`.
    let label: String
    @ViewBuilder let content: Content

    @Environment(\.panelScrollBoundaryEnabled) private var enabled

    var body: some View {
        if enabled {
            ScrollView(.vertical) { content }
                .accessibilityLabel(label)
        } else {
            // No `.accessibilityLabel` on this arm: with no scroll region to name, the label would have
            // nowhere to land but the content itself, renaming whatever it wrapped. The harness rasterizes;
            // it does not read the tree, and every consumer that DOES read it takes the arm above.
            content
        }
    }
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
    /// (`canStartDaemon`) and otherwise degrades to the same inert banner the other cold states show. Since
    /// issue #820 that degraded card still carries a `.failed` REASON when one exists: the affordance is
    /// withheld, the explanation is not.
    @EnvironmentObject private var loginItem: LoginItemModel

    /// The operator's Dynamic Type size class (issue #756) — the panel's ONE reader of it. Every subview
    /// reads the derived `\.panelScale` this view injects below, so the factor is computed exactly once
    /// and no subview can disagree with another about what size class is in effect.
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    /// How often the resting panel re-derives clock-relative text (reset-in). A minute is finer than
    /// the reset-in's own minute granularity, so the displayed value never visibly lags the clock.
    private static let clockTick: TimeInterval = 60

    var body: some View {
        // The panel's uniform Dynamic Type multiplier (issue #756). `factor(for:)` clamps to
        // `PanelTypeScale.ceiling` itself, so this is the clamped value even before the limiting modifier
        // below; at the default `.large` it is EXACTLY 1.0, which is what keeps this whole change a no-op
        // at the default text size (and the issue #754 goldens valid without a re-baseline).
        let scale = PanelTypeScale.factor(for: dynamicTypeSize)
        TimelineView(.periodic(from: .now, by: Self.clockTick)) { context in
            content(now: Int64(context.date.timeIntervalSince1970), scale: scale)
        }
        .frame(width: PanelMetrics.scaledWidth(scale), alignment: .leading)
        // The scroll boundary's other half (issue #818). `PanelScrollBoundary` makes a state's unbounded
        // body WILLING to be shorter than its content; this is what ever asks it to be. Without a bound the
        // popover keeps sizing to the intrinsic height and every boundary below stays fully expanded — the
        // defect intact behind a `ScrollView` that never scrolls.
        //
        // ORDER IS THE INTENDED READING, and NOTHING GATES IT — stated plainly because an earlier revision
        // of this comment claimed the order was measured, and it does not reproduce: swapping these two
        // lines leaves every height identical and the whole suite green. The reasoning is that
        // `.fixedSize(vertical: true)` reports the IDEAL height, so a cap applied AFTER it would let the
        // content lay out tall and then crop the result, whereas applied BEFORE the cap is the proposal the
        // boundary shrinks against. SwiftUI evidently resolves both orders to the same layout here, so treat
        // that as the argument for writing them this way round and NOT as a proven invariant: if you reorder
        // them, no test will tell you, and the honest fix is a falsifier rather than a firmer sentence.
        .frame(maxHeight: PanelMetrics.heightBudget)
        .fixedSize(horizontal: false, vertical: true)
        // Inject the derived factor for the whole subtree — the single seam every panel subview reads.
        .environment(\.panelScale, scale)
        // DECLARE the supported range with SwiftUI's own limiting modifier, in addition to the clamp
        // inside `factor(for:)`. Both are deliberate: the clamp is what the panel's own arithmetic obeys,
        // while this modifier makes `\.dynamicTypeSize` itself report the clamped class to anything in the
        // subtree that reads it directly — so the two representations state the same ceiling instead of a
        // subview seeing `.accessibility5` while the layout is sized for `.accessibility3`.
        .dynamicTypeSize(...PanelTypeScale.ceiling)
        // A translucent `.regularMaterial` scrim over the host's `.popover` vibrancy (StatusItemController):
        // the desktop blur reads through (matching the design reference's `backdrop-filter` translucency)
        // while the material's built-in frosting keeps every label + metric legible against a busy wallpaper
        // — the contrast guarantee we previously bought only by going fully opaque, which defeated the
        // vibrancy. Restores #390 (I5); the scrim is what makes the restore safe (ratified: vibrancy+scrim).
        .background(.regularMaterial)
    }

    @ViewBuilder
    private func content(now: Int64, scale: Double) -> some View {
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
                Divider().padding(.horizontal, 14 * scale)
                PanelScrollBoundary(label: StatusPanelFormat.scrollRegionCaptureLabel) {
                    CaptureCard(title: StatusPanelFormat.captureCardAddAccountTitle)
                        .padding(.horizontal, StatusPanelFormat.captureCardHorizontalInset * scale)
                        .padding(.top, 10 * scale).padding(.bottom, 10 * scale)
                }
            } else if onStatsTab {
                // The Stats tab (issue #446): the mock's per-account 7-day sparklines + numeric body,
                // aggregate callout, and signal legend — fed by the socket `stats` verb (never a store read).
                // A separate view from the Status body; the footer's `next_swap` line stays the Status tab's.
                //
                // Inside the #818 boundary: the Stats body carries one card PER ACCOUNT, so it grows on the
                // same unbounded axis the roster does — measured, it is the TALLEST surface the panel has
                // (1300.50 pt at `.accessibility3` against the healthy roster's 1031.00). The header above
                // stays pinned, which is what keeps the Status|Stats switcher reachable: a tab control that
                // scrolls away is a tab you cannot leave.
                Divider().padding(.horizontal, 14 * scale)
                PanelScrollBoundary(label: StatusPanelFormat.scrollRegionStatsLabel) { StatsView() }
            } else {
                stateBody(state: state, now: now, ageText: ageText, ageStale: ageStale, scale: scale)
            }
        }
    }

    /// The panel's normal, connection-state-driven body (roster / banner / onboarding card) plus the
    /// snapshot-age footer — everything below the header when the operator has NOT summoned the #394
    /// capture surface. A populated (`.connected` / `.stale`) roster carries NO capture bar: capture is
    /// an empty-roster / first-run onboarding affordance, and adding an account otherwise lives off-panel
    /// in the status-item right-click menu (issue #394; matches the re-locked mock, #387).
    ///
    /// ── WHAT SCROLLS AND WHAT STAYS PINNED (issue #818) ───────────────────────────────────────────────
    ///
    /// The split is by GROWTH, not by importance, and the two coincide here rather than being traded off.
    /// A body wrapped in `PanelScrollBoundary` is one that grows without bound — the roster and the Stats
    /// tab per account, a message card with daemon-authored text. Everything else appears AT MOST ONCE per
    /// state and is fixed chrome, so pinning the whole of it costs a bounded, one-off amount of height,
    /// leaving the boundary the rest. No figure is quoted here on purpose: the pinned set is what this
    /// split defines, so any number written down mirrors a base that moves the moment an element joins or
    /// leaves it. `PanelScrollBoundaryTests.testTheBoundaryHoldsMoreRosterThanTheViewportCanShowAtBothSizeClasses`
    /// measures it live at both size classes and FAILS if the chrome ever leaves the boundary no viewport,
    /// printing the figure it measured — a gate rather than a comment, which is the whole point.
    ///
    /// That is why the answer to "banner or footer?" is BOTH, and why it is not the false economy it sounds
    /// like. Pinning is what makes each of them honest:
    ///
    ///   * the honest-state banners — the daemon-level fault banner and the `.disconnected` `HonestStrip` —
    ///     because a banner an operator can scroll past is a banner they can MISS, and the #524 glance-glyph
    ///     taxonomy collapses every fault to one silhouette on the promise that "the *which* is one click
    ///     away in the panel". If the click lands on a healthy-looking roster with the explanation scrolled
    ///     off above it, that promise is broken exactly when it matters.
    ///   * the footer's Swap callout and its status line — the panel's ONE accent/primary action (#169).
    ///     Measured, a 50-account roster puts it 4901 pt down; an action reachable only after scrolling past
    ///     the whole fleet is not the recommendation the daemon is making.
    ///   * the snapshot-age footer — it is the freshness signal that reads amber whenever the numbers should
    ///     be distrusted (#137). Scrolling that away leaves stale numbers looking live.
    ///
    /// A banner that IS the body scrolls rather than pins, and the distinction is real rather than a
    /// carve-out: the fault banner and the `HonestStrip` are pinned because there is a roster BELOW them
    /// that would otherwise push them off. `.connecting` / `.unsupported` / `.starting` / `.crashLooping` /
    /// `.notRunning` have nothing below to push, so their card is the unbounded thing and pinning it would
    /// clip the honest message itself.
    @ViewBuilder
    private func stateBody(state: ConnectionState, now: Int64, ageText: String?,
                           ageStale: Bool, scale: Double) -> some View {
        switch state {
        case .emptyRoster:
            // A live onboarding state, not stale data — distinct from daemon-down.
            Divider().padding(.horizontal, 14 * scale)
            PanelScrollBoundary(label: StatusPanelFormat.scrollRegionCaptureLabel) {
                CaptureCard(title: StatusPanelFormat.captureCardOnboardingTitle)
                    .padding(.horizontal, StatusPanelFormat.captureCardHorizontalInset * scale)
                    .padding(.top, 10 * scale).padding(.bottom, 10 * scale)
            }

        case .connecting, .unsupported:
            // No trustworthy reading to show — a plain honest message card, and no action: the design mock
            // gives these two states no affordance at all, and its whole action inventory is only three
            // labels, so an extra button here would be invention rather than conformance.
            Divider().padding(.horizontal, 14 * scale)
            PanelScrollBoundary(label: StatusPanelFormat.scrollRegionMessageLabel) {
                BannerView(banner: StatusPanelFormat.banner(for: state, accountCount: store.rows.count))
                    .padding(.horizontal, 14 * scale).padding(.vertical, 14 * scale)
            }

        case .starting, .crashLooping:
            // The two cold states the mock gives a `View log` action (issue #776) — the same honest message
            // card as their `.connecting` / `.unsupported` siblings above, plus the affordance, which appears
            // only where there is a log to open (`DaemonLogCard`). `.crashLooping` (#169): the daemon served a
            // snapshot but keeps dropping before it stabilizes, so its numbers are refused ("holding status
            // until it stays up") rather than flickered as live — the crown-jewel anti-#137 debounce.
            // `.starting` (#499) is the cold-refused daemon-absent state that never held a reading.
            // (`.notRunning` is their third sibling, but carries the #170 Start affordance instead — see
            // the dedicated branch below; the mock gives it no `View log`.)
            //
            // The two take DIFFERENT action styles because the mock renders the same action differently in
            // each. That mapping is `ActionStyle.forState` — a pure function rather than a literal here, so
            // it can be asserted directly; a render check can only show the two treatments differ, never
            // that the right state got the right one.
            Divider().padding(.horizontal, 14 * scale)
            PanelScrollBoundary(label: StatusPanelFormat.scrollRegionMessageLabel) {
                DaemonLogCard(state: state, actionStyle: .forState(state))
                    .padding(.horizontal, 14 * scale).padding(.vertical, 14 * scale)
            }

        case .notRunning:
            // The daemon is installed-but-down (#499): it never held a reading, so — like its `.starting`
            // sibling — it shows the honest "Daemon not running" banner. Unlike the others it ALSO hosts the
            // #170 Start affordance: `StartDaemonCard` reuses that banner and, ONLY where a bundled agent is
            // registrable and no CLI owns the label (`loginItem.canStartDaemon`), offers a "Start daemon"
            // button that registers + launches the agent via `SMAppService`. In the #170 shipped state no
            // plist is bundled yet (that co-lands with #171), so `canStartDaemon` is false and — with nothing
            // able to have registered, hence no `.failed` reason to carry (issue #820) — the card is exactly
            // the inert banner it was before, never a dead button.
            //
            // The mock's two other actions, once deferred from here as "#169/#171 siblings", are both settled
            // and neither belongs in THIS state: `View log` is BUILT (issue #776) and the mock scopes it to
            // `.starting` / `.crashLooping` only — see the branch above; `Restart…` is DROPPED on measured
            // evidence (issue #777, `docs/findings/0777-manual-restart-under-conditional-keepalive.md`) and
            // issue #856 removes it from the mock. Nothing here is pending.
            Divider().padding(.horizontal, 14 * scale)
            PanelScrollBoundary(label: StatusPanelFormat.scrollRegionMessageLabel) {
                StartDaemonCard()
                    .padding(.horizontal, 14 * scale).padding(.vertical, 14 * scale)
            }

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
                // The strip above is PINNED and the retained roster scrolls under it (#818): the strip is
                // the only thing saying these numbers are last-known rather than live, so a long enough
                // roster must not be able to carry it off the top of the panel. The dimming stays on the
                // ROSTER rather than on the boundary — it is the retained data that is not live, not the
                // container.
                PanelScrollBoundary(label: StatusPanelFormat.scrollRegionAccountsLabel) {
                    RosterView(rows: store.rows, now: now, switchable: false).opacity(0.55)
                }
            }

        case .connected, .stale:
            // Live (or connected-but-stale — the roster stays full-strength, the header/footer carry the
            // "stale" mark). The design reference: roster + swap-callout hero. The roster is switchable
            // exactly where the swap-callout card renders, so the panel's two swap paths (per-row manual,
            // footer recommendation) are live and dead together (#169). No capture bar — capture moved to
            // the status-item menu / empty-roster onboarding (issue #394).
            Divider().padding(.horizontal, 14 * scale)
            if let faultBanner = StatusPanelFormat.daemonFaultBanner(keychainLocked: store.keychainLocked,
                                                                     scrub: store.canonicalScrub,
                                                                     systemicRefreshFailure: store.systemicRefreshFailure,
                                                                     systemicRefreshSource: store.systemicRefreshSource,
                                                                     canary: store.canary) {
                // The single daemon-level fault banner (worst-first): a fleet-wide lockout or mechanism
                // failure NO per-row `auth` reflects (rows can read healthy while the shared item sits locked
                // or emptied, and while the refresh mechanism is down every account is still alive), so it
                // rides as its own honest banner ABOVE the roster — the connected-but-degraded panel reads
                // visibly DEGRADED (never healthy) while the live roster still renders below. The footer stays
                // the `next_swap` line (R-2: footer = next_swap; degraded daemon-level signals → honest
                // banner). The panel shows ONE banner, ranked worst-first over (fault, VARIANT) — never over
                // fault identity, so a calm self-healing state can never outrank one that cannot self-heal.
                // See `daemonFaultBanner` for the eight ranks (over four faults) and why `recovering` sits
                // last of them.
                //
                // This banner is what makes the menu-bar glance honest rather than cryptic: the locked glyph
                // taxonomy collapses every fault to one silhouette on the promise that "the *which* is one
                // click away in the panel" (#524). Each fault the glyph shouts MUST land here, or the click
                // that follows finds a healthy roster and no explanation.
                BannerView(banner: faultBanner)
                    .padding(.horizontal, 14 * scale).padding(.vertical, 14 * scale)
                Divider().padding(.horizontal, 14 * scale)
            }
            if !store.rows.isEmpty {
                // #572: the active blind row composes the CORNERED verdict from `store.rosterNextSwap` — the
                // honest-state-gated next-swap that WITHHOLDS a retained `noViableTarget` under `.stale`
                // (watchdog elapsed) so it degrades to orange DEGRADED, matching the stale `!` glance rather
                // than a loud red "cannot act" off unvouched data (#137). MUST read `rosterNextSwap`, not the
                // raw `store.nextSwap`. (The dropped roster at `.disconnected`/`.reconnecting` above is dimmed
                // and passes no `nextSwap`.)
                //
                // The roster is the panel's unbounded axis, so it is what scrolls (#818): measured, one
                // account costs 96.00 pt at the default text size and ≈221.30 pt at `.accessibility3`,
                // linearly and with no plateau anywhere out to 50 accounts. The fault banner above and the
                // swap callout / status line / age footer below are all pinned OUTSIDE this boundary — see
                // this function's doc comment for why each of them has to be.
                PanelScrollBoundary(label: StatusPanelFormat.scrollRegionAccountsLabel) {
                    RosterView(rows: store.rows, now: now, switchable: true, nextSwap: store.rosterNextSwap)
                }
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
