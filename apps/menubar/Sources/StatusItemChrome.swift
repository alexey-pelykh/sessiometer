// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The status item's PURE decision layer (issue #764) — the functional core `StatusItemController` is the
// imperative shell of. Every decision the controller makes that does NOT need an `NSStatusItem`, an
// `NSPanel`, or a live `NSEvent` lives here as a total function over plain values, so it is reachable
// from the headless `MenubarTests` bundle. This is the same extraction `StatusGauge` is to the glyph
// artwork, `HonestStateMachine` is to `WatchStatusStore`, and `LoginItemModel` is to `SMAppService`:
// the repo's standing answer to an untestable AppKit surface is to move the DECISION out, not to add a
// UI test around it.
//
// WHAT IS NOT HERE, and why. The controller's state→glyph step is NOT a decision — `apply(_:)` forwards
// `presentation.glyph` verbatim to `StatusGauge.image(for:)`. The actual `ConnectionState → StatusGlyph`
// projection lives in `PresentationState.make(for:accountCount:...)` and is already locked by
// `HonestStateMachineTests` as an exhaustive 10-row table; the one-distinct-silhouette-per-state brand
// lock is already held by `StatusGaugeTests.testEveryGlyphMapsToADistinctAsset` (asset injectivity) and
// `BarGlyphParityTests` (rendered pairwise distinctness in every context). Re-extracting a verbatim
// forward would add a tautological seam, not coverage — see the issue #764 PR body for the measurement.
//
// The controller keeps everything that genuinely needs AppKit: creating the `NSStatusItem`, hosting the
// SwiftUI panel, installing the global event monitor, and applying the frame this file computes.

import AppKit

/// The status item's pure decision layer — a caseless `enum` used as a namespace of total functions
/// (the `StatusGauge` / `SocketPathResolver` shape), so there is nothing to instantiate and no state
/// to get out of step with the controller.
enum StatusItemChrome {

    // MARK: - Click routing

    /// What a status-item mouse-up means. The item fires on BOTH buttons (`sendAction(on:)`), so the
    /// controller routes on the event rather than assigning `statusItem.menu` permanently — a permanent
    /// menu would hijack the primary click and disable the click-to-toggle design (#325/#326).
    enum Click: Equatable {
        /// Toggle the status panel.
        case primary
        /// Raise the lifecycle menu (the OFF-PANEL home for cold-path actions, design C-005).
        case secondary
    }

    /// Classify a status-item mouse-up. A secondary (menu-summoning) click is a right mouse-up, or a
    /// control-held left mouse-up — the two gestures macOS treats as "show me the contextual menu".
    ///
    /// `type` is optional because the controller reads `NSApp.currentEvent`, which is `nil` on the
    /// programmatic-click path (`performClick(nil)`, which `showLifecycleMenu` itself uses to present the
    /// transient menu). A `nil` event is `.primary`: treating it as secondary would make the menu
    /// re-enter itself.
    static func click(forEventType type: NSEvent.EventType?, modifiers: NSEvent.ModifierFlags) -> Click {
        guard let type else { return .primary }
        if type == .rightMouseUp { return .secondary }
        if type == .leftMouseUp, modifiers.contains(.control) { return .secondary }
        return .primary
    }

    // MARK: - Panel geometry

    /// The UX gap between the menu bar (the icon's bottom edge) and the panel's top edge.
    static let panelGap: CGFloat = 6

    /// How far the panel is kept from the visible frame's edges when clamped on-screen.
    static let screenInset: CGFloat = 8

    /// The size used when the hosted SwiftUI content reports a degenerate fitting size — a first layout
    /// pass can return zero before the hierarchy has measured. Showing a 0×0 panel is worse than showing
    /// a plausibly-sized one, so the controller substitutes this rather than skipping the open.
    static let fallbackContentSize = NSSize(width: 360, height: 240)

    /// Substitute `fallbackContentSize` for a degenerate measurement. "Degenerate" is sub-point on
    /// EITHER axis — a panel one axis of which is zero is as unusable as one that is zero on both.
    static func contentSize(fitting fittingSize: NSSize) -> NSSize {
        if fittingSize.width < 1 || fittingSize.height < 1 { return fallbackContentSize }
        return fittingSize
    }

    /// Where the panel goes: centered under the status item, hanging `panelGap` below the icon's bottom
    /// edge, clamped on-screen on BOTH axes with `screenInset` of margin.
    ///
    /// Derived from the icon's OWN window frame (never a hardcoded menu-bar height), so it is correct on
    /// any display and with or without a notch. `visibleFrame` — not the physical screen frame — is the
    /// correct bound: a physical-frame clamp would still let a tall panel slide under the Dock.
    ///
    /// Issue #446 is why the Y clamp exists at all: `openPanel` originally sized once at open-time and
    /// clamped X only, so a Stats tab appearing after open both clipped and ran off the bottom. Note the
    /// deliberate asymmetry that fix left in place — Y is clamped at the BOTTOM only. A panel taller than
    /// the gap below the icon therefore keeps its bottom on-screen and grows UPWARD past the icon rather
    /// than clipping. That is the ratified trade (a reachable state on a short display with the Stats tab
    /// open): visible-but-overlapping beats correctly-placed-but-cut-off.
    static func panelFrame(iconFrame: NSRect, visibleFrame: NSRect, contentSize size: NSSize) -> NSRect {
        // Center under the icon, then clamp horizontally inside the visible frame.
        //
        // Clamp ORDER is load-bearing and is pinned by test: `min(max(x, lo), hi)` resolves to `hi` when
        // `hi < lo`, i.e. when the panel is wider than the visible frame minus both insets — it would then
        // hang off the LEFT edge rather than the right. Unreachable at shipped sizes (the panel is ~360 pt
        // against a ≥1280 pt display), so this is documented and pinned as-is, not "fixed" speculatively.
        var x = iconFrame.midX - size.width / 2
        x = min(max(x, visibleFrame.minX + screenInset), visibleFrame.maxX - size.width - screenInset)

        // Hang below the icon, then floor-clamp so a tall panel keeps its bottom on-screen (#446).
        var y = iconFrame.minY - panelGap - size.height
        if y < visibleFrame.minY + screenInset { y = visibleFrame.minY + screenInset }

        return NSRect(x: x, y: y, width: size.width, height: size.height)
    }

    // MARK: - Lifecycle menu

    /// What a lifecycle-menu row does. Carried as a value — rather than the controller matching on
    /// TITLE — so the wiring is by identity: renaming a row's copy cannot silently unbind its action, and
    /// adding a case forces the controller's `switch` to handle it.
    enum MenuAction: Equatable, CaseIterable {
        /// Open the capture surface in the panel (#394).
        case addAccount
        /// Open the Settings window (#268).
        case openSettings
        /// Terminate the menu-bar app — a pure-CLIENT control (#325). Never touches the daemon.
        case quit
    }

    /// One row of the secondary-click menu — an action plus its copy and key equivalent, or a separator.
    ///
    /// The memberwise initialiser is PRIVATE on purpose, so `action` and `title` can only ever be nil
    /// TOGETHER. A half-populated row would disagree with itself: an action with no title still reads as a
    /// real row here (`isSeparator` is false, so the tests count it), while the shell has no title to build
    /// an `NSMenuItem` with and drops it — silently unbinding that action. Constructing only through
    /// `item(_:_:)` and `separator` makes that state unrepresentable rather than merely unlikely.
    struct MenuEntry: Equatable {
        /// `nil` if and only if this is a separator row.
        let action: MenuAction?
        /// `nil` if and only if this is a separator row.
        let title: String?
        /// The empty string for "no shortcut" (AppKit's own convention for `NSMenuItem`).
        let keyEquivalent: String

        private init(action: MenuAction?, title: String?, keyEquivalent: String) {
            self.action = action
            self.title = title
            self.keyEquivalent = keyEquivalent
        }

        var isSeparator: Bool { action == nil }

        static func item(_ action: MenuAction, _ title: String, keyEquivalent: String = "") -> MenuEntry {
            MenuEntry(action: action, title: title, keyEquivalent: keyEquivalent)
        }

        static let separator = MenuEntry(action: nil, title: nil, keyEquivalent: "")
    }

    /// The secondary-click menu, in order — the OFF-PANEL home for cold-path actions, so the status panel
    /// stays a pure display + manual-swap surface (design C-005 IA scope guard).
    ///
    /// "Add account…" (#394) is the capture entry point now that the populated panel has no persistent
    /// capture bar — a rare, deliberate action, neither display nor swap, so it belongs off-panel;
    /// "Settings…" (#268) opens the daemon-tunables window; "Quit Sessiometer" is a
    /// pure-CLIENT control that terminates the menu-bar app only — it never touches the daemon, whose
    /// lifecycle is #170. The separator sets Quit apart as the one destructive row.
    static let lifecycleMenu: [MenuEntry] = [
        .item(.addAccount, "Add account…"),
        .item(.openSettings, "Settings…", keyEquivalent: ","),
        .separator,
        .item(.quit, "Quit Sessiometer"),
    ]

    // MARK: - Outside-click dismissal

    /// What an outside click should do to an open panel.
    enum DismissDecision: Equatable {
        /// The click landed on our own status item — let `togglePanel` own the close, or the panel would
        /// close here on mouse-DOWN and the button's mouse-UP action would immediately reopen it (the
        /// classic status-item "won't close on the second click" bug).
        case ignoreOwnStatusItem
        /// A capture or swap is mid-flight; keep the panel up so its outcome is not hidden.
        case retain
        /// Close the panel.
        case dismiss
    }

    /// Decide what a global mouse-down outside the panel does.
    ///
    /// The own-icon exclusion is checked FIRST because it is about event routing, not user intent: the
    /// global monitor also sees menu-bar clicks on our own item, and letting those fall through would
    /// break the toggle regardless of what the models are doing.
    ///
    /// The retention gate then honours #360 and #169: an accidental outside click must not drop a
    /// typed-but-unsubmitted account label or a capture in flight, and must not hide an in-flight SWAP —
    /// a real write against the operator's active account, whose outcome (committed, or refused with a
    /// reason) has to be read. Esc and the status-item toggle remain the deliberate closers.
    static func outsideClick(landedOnStatusItem: Bool, captureBusy: Bool, swapBusy: Bool) -> DismissDecision {
        if landedOnStatusItem { return .ignoreOwnStatusItem }
        if captureBusy || swapBusy { return .retain }
        return .dismiss
    }

    // MARK: - Open precondition

    /// Whether the panel may be shown yet. The frame is derived from the icon's own window, so with no
    /// window there is nothing to position against — `orderFrontRegardless` would then flash the panel at
    /// a stale, unpositioned frame. Bail BEFORE showing rather than showing something wrong.
    static func canOpenPanel(iconHasWindow: Bool) -> Bool { iconHasWindow }
}
