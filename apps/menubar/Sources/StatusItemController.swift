// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The menu-bar status-item controller (issue #325): the IMPERATIVE SHELL that owns the always-visible
// menu-bar chrome and binds it to the store's glance. It creates the single `NSStatusItem` (the app is
// LSUIElement / `.accessory` — set in `main.swift`), renders the shape-encoded template gauge
// (`StatusGauge`) for the current `StatusGlyph`, keeps the button's VoiceOver label in step with the
// state, and toggles a floating panel (hosting `StatusPanelView`) on click.
//
// Panel placement: a borderless, NON-activating `NSPanel` sits a small UX gap BELOW the status item,
// positioned from the icon's OWN window frame — so it lands correctly on any display and any menu-bar
// height (notch or not) with no hardcoding, and crucially leaves the icon itself VISIBLE and CLICKABLE
// so a second click toggles the panel closed. (A prior `NSPopover` glued its arrow to the icon's edge
// and overlapped it, hiding the icon — a popover can't leave a gap because the arrow always bridges to
// the anchor; the floating panel gives us the gap and keeps the icon reachable.)
//
// It CONSUMES the store's `presentations` glance stream (#324): one `PresentationState` per state
// change drives one `apply(_:)`, updating BOTH the glyph image AND the accessibility label together.

import AppKit
import Combine
import SwiftUI

@MainActor
final class StatusItemController {
    private let statusItem: NSStatusItem
    private let panel: FloatingPanel
    /// The SwiftUI host, kept so `openPanel` can size the panel to the current content.
    private let hostingView: NSView
    private let store: WatchStatusStore
    /// The in-app capture affordance's model (issue #360). Owned here so the outside-click dismiss can be
    /// GATED on `captureModel.isBusy` (a mid-edit label / in-flight capture must not be lost), and so the
    /// non-activating panel can be re-asserted key when the label field takes focus.
    private let captureModel: AccountCaptureModel
    /// The swap affordance's model (issue #169). Owned here for the same reason as `captureModel`: the
    /// outside-click dismiss is GATED on `swapModel.isBusy`, so an in-flight swap — a real write against
    /// the operator's active account — cannot be hidden by a stray click before its outcome is seen.
    private let swapModel: AccountSwapModel
    /// The Stats-tab model (issue #446): owns the panel's Status|Stats selection + the one-shot `stats`
    /// query. Owned here so the controller can OBSERVE its changes (a tab switch, or the series loading in)
    /// and RE-SIZE the panel to the new content — the Stats tab is taller than Status, so a size fixed at
    /// open-time (`openPanel`) would clip it; switching back to Status must restore the smaller size.
    private let statsModel: PanelStatsModel
    /// The launch-at-login / Start-daemon model (issue #170). Built by `main.swift` and SHARED with the
    /// Settings window's `SettingsView` toggle (ONE instance, so the panel's Start affordance and the Settings
    /// launch-at-login toggle read/write the same registration state), injected into the panel environment below.
    private let loginItemModel: LoginItemModel
    /// The subscription that re-sizes the panel whenever the Stats model changes (installed in `init`).
    private var statsObserver: AnyCancellable?
    private var presentationTask: Task<Void, Never>?
    /// The UX gap between the menu bar and the panel's top edge.
    private let panelGap: CGFloat = 6
    /// The outside-click monitor installed WHILE the panel is open (see `openPanel`). `nil` when closed.
    private var dismissMonitor: Any?
    /// Injected by `main.swift` to open the Settings window (issue #268). The controller owns the menu ENTRY
    /// only; the window's lifecycle is the app's (a single app-retained `SettingsWindowController`), so this
    /// is a closure seam — the same pattern as `captureModel.panelKeyRequest` — not a window reference held
    /// here. `nil` (unwired) simply makes the menu item inert, exactly like the app degrading without it.
    var onOpenSettings: (@MainActor () -> Void)?

    init(store: WatchStatusStore,
         captureClient: ControlCommandClient?,
         swapClient: ControlCommandClient?,
         statsClient: ControlCommandClient?,
         loginItemModel: LoginItemModel) {
        self.store = store
        self.loginItemModel = loginItemModel
        let captureModel = AccountCaptureModel(client: captureClient)
        self.captureModel = captureModel
        let swapModel = AccountSwapModel(client: swapClient)
        self.swapModel = swapModel
        let statsModel = PanelStatsModel(client: statsClient)
        self.statsModel = statsModel
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        // #326's status panel reads its dependencies via `@EnvironmentObject`: the store (a thin view over the
        // `src/cli.rs`-mirroring `StatusPanelFormat`), the #360 capture affordance's `AccountCaptureModel`, the
        // #169 swap affordance's `AccountSwapModel`, the #446 Stats tab's `PanelStatsModel`, and the #170
        // launch-at-login / Start-daemon `LoginItemModel`. Inject the COMPLETE set here through the shared
        // `statusPanelEnvironment` modifier — the SAME wiring the DEBUG `--render-panel` harness uses, so the
        // app and the harness cannot drift (issue #504).
        let hosting = NSHostingView(rootView: StatusPanelView()
            .statusPanelEnvironment(store: store, capture: captureModel, swap: swapModel, stats: statsModel,
                                    loginItem: loginItemModel))
        hosting.translatesAutoresizingMaskIntoConstraints = false
        self.hostingView = hosting

        // Vibrancy + rounded corners — the popover chrome we lose by dropping `NSPopover`, minus arrow.
        let effect = NSVisualEffectView()
        effect.material = .popover
        effect.state = .active
        effect.blendingMode = .behindWindow
        effect.wantsLayer = true
        effect.layer?.cornerRadius = 12
        effect.layer?.masksToBounds = true
        effect.addSubview(hosting)

        // Borderless + NON-activating: non-activating keeps the menu-bar icon live so a second click on
        // it toggles the panel closed; borderless + clear background gives the floating-card look.
        let panel = FloatingPanel(contentRect: NSRect(x: 0, y: 0, width: 360, height: 200),
                                  styleMask: [.borderless, .nonactivatingPanel],
                                  backing: .buffered, defer: false)
        panel.isFloatingPanel = true
        panel.level = .popUpMenu
        panel.hidesOnDeactivate = false
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = true
        panel.contentView = effect
        self.panel = panel

        // Re-assert the non-activating panel as key when the capture label field takes focus (issue #360):
        // a `FloatingPanel` (hidesOnDeactivate=false) can lose key when focus moves, leaving the SwiftUI
        // `TextField` unable to accept keystrokes. `[weak panel]` — the controller already retains it.
        captureModel.panelKeyRequest = { [weak panel] in panel?.makeKey() }

        NSLayoutConstraint.activate([
            hosting.leadingAnchor.constraint(equalTo: effect.leadingAnchor),
            hosting.trailingAnchor.constraint(equalTo: effect.trailingAnchor),
            hosting.topAnchor.constraint(equalTo: effect.topAnchor),
            hosting.bottomAnchor.constraint(equalTo: effect.bottomAnchor),
        ])

        // Seed the glyph + label synchronously from the store's current glance so the item is never
        // blank in the gap between attach and the first streamed update.
        configureButton()
        apply(store.currentPresentation)

        // Re-size the panel WHEN OPEN as the Stats model changes (issue #446): a Status↔Stats tab switch, or
        // the stats series loading in, changes the SwiftUI content height. `objectWillChange` fires BEFORE the
        // value updates and BEFORE SwiftUI re-lays-out, so defer the re-size to the next run-loop turn — by
        // then `hostingView.fittingSize` reflects the new content. The Stats model is the ONLY driver here:
        // the Status tab's own updates come from the store (a different object), so this never fires for them.
        statsObserver = statsModel.objectWillChange.sink { [weak self] in
            DispatchQueue.main.async { self?.resizePanelToContentIfOpen() }
        }
    }

    /// Begin consuming the store's glance stream, mirroring each `PresentationState` onto the button.
    /// Idempotent via the task guard. Kept separate from `init` so construction does no async work.
    func start() {
        guard presentationTask == nil else { return }
        let stream = store.presentations
        presentationTask = Task { [weak self] in
            for await presentation in stream {
                self?.apply(presentation)
            }
        }
    }

    private func configureButton() {
        guard let button = statusItem.button else { return }
        button.target = self
        button.action = #selector(handleClick)
        // Fire on BOTH mouse buttons so a secondary (right / control) click can raise the lifecycle
        // menu (Quit) while a primary click toggles the panel. Assigning `statusItem.menu`
        // permanently would hijack the primary click too and disable the click-to-toggle design
        // (#325/#326), so we route on the event in `handleClick` and set the menu only transiently
        // while it is shown (see `showLifecycleMenu`).
        button.sendAction(on: [.leftMouseUp, .rightMouseUp])
    }

    /// Mirror one glance onto the button: the shape-encoded template gauge PLUS the spoken VoiceOver
    /// label. Both move together on every state change (AC: the label tracks the current state).
    private func apply(_ presentation: PresentationState) {
        guard let button = statusItem.button else { return }
        button.image = StatusGauge.image(for: presentation.glyph)
        button.setAccessibilityLabel(presentation.accessibilityLabel)
    }

    /// The status-item action, fired on a primary OR secondary mouse-up (see `configureButton`). A
    /// secondary click (see `isSecondaryClick`) raises the lifecycle menu; a primary click toggles
    /// the panel.
    @objc private func handleClick() {
        if isSecondaryClick(NSApp.currentEvent) {
            showLifecycleMenu()
        } else {
            togglePanel()
        }
    }

    /// A secondary (menu-summoning) click: a right mouse-up, or a control-held left mouse-up. A
    /// `nil` event (the programmatic-click path) is treated as primary.
    private func isSecondaryClick(_ event: NSEvent?) -> Bool {
        guard let event else { return false }
        return event.type == .rightMouseUp
            || (event.type == .leftMouseUp && event.modifierFlags.contains(.control))
    }

    /// Toggle the panel (AC — clicking the item shows AND hides it).
    private func togglePanel() {
        if panel.isVisible {
            closePanel()
        } else {
            openPanel()
        }
    }

    /// The secondary-click menu — the OFF-PANEL home for cold-path actions, so the status panel stays a
    /// pure display + manual-swap surface (design C-005 IA scope guard). It carries "Add account…" (issue
    /// #394 — the capture entry point now that the populated panel has no persistent capture bar; capture
    /// is a rare, deliberate action, neither display nor swap, so it belongs here) and "Quit Sessiometer"
    /// (a pure-CLIENT control that terminates the menu-bar app via `NSApp.terminate`, the clean
    /// `applicationWillTerminate` transport-stop path; it does NOT touch the daemon, whose quit/restart
    /// lifecycle is #170). It is the natural future home for the remaining runtime controls (daemon
    /// quit/restart); launch-at-login now ships in Settings (the "General" toggle) plus the not-running
    /// panel card's Start affordance (#170). Shown via a TRANSIENT `statusItem.menu` so AppKit positions and
    /// highlights it natively under the item, then cleared so the primary click keeps toggling the panel
    /// (setting `statusItem.menu` permanently would hijack the primary click, #325/#326).
    private func showLifecycleMenu() {
        // Close the panel first if it is open: the click-outside global monitor never sees our own
        // status-item events, so without this a secondary click would leave the panel lingering.
        if panel.isVisible { closePanel() }
        let menu = NSMenu()
        let addItem = NSMenuItem(title: "Add account…", action: #selector(addAccount), keyEquivalent: "")
        addItem.target = self
        menu.addItem(addItem)
        let settingsItem = NSMenuItem(title: "Settings…", action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        menu.addItem(settingsItem)
        menu.addItem(.separator())
        let quitItem = NSMenuItem(title: "Quit Sessiometer", action: #selector(quit), keyEquivalent: "")
        quitItem.target = self
        menu.addItem(quitItem)
        statusItem.menu = menu
        statusItem.button?.performClick(nil)
        statusItem.menu = nil
    }

    /// Open the capture surface from the "Add account…" menu item (issue #394). Sets the model flag the
    /// panel observes, then opens (and keys) the panel — the capture surface then renders IN this panel,
    /// reusing its key/first-responder plumbing (`captureModel.panelKeyRequest` + the label-field focus
    /// bridge), not a second popover / window / alert. `closePanel` resets the flag, so this mode never
    /// outlives the panel: a later primary click opens the normal roster.
    @objc private func addAccount() {
        captureModel.requestCaptureSurface()
        openPanel()
    }

    /// Open the Settings window (issue #268) from the secondary-click menu — the OFF-PANEL home for cold-path
    /// actions (the panel stays a pure display + manual-swap surface, C-005). Editing daemon tunables + roster
    /// labels only; add / remove / re-auth stay in the CLI (never a GUI credential write). The window is owned
    /// + lifecycle-managed by the app via the injected `onOpenSettings` seam, so an unwired controller simply
    /// no-ops rather than reaching across into window management it does not own.
    @objc private func openSettings() {
        onOpenSettings?()
    }

    /// Quit the menu-bar app (a pure-client control). The daemon keeps running — its lifecycle is #170.
    @objc private func quit() {
        NSApp.terminate(nil)
    }

    /// Size the panel to its CURRENT SwiftUI content and position it a `panelGap` below the icon, clamped
    /// on-screen on BOTH axes. This is the single sizing seam (issue #446): `openPanel` calls it once at
    /// open, and `resizePanelToContentIfOpen` re-calls it whenever the hosted content changes height (a
    /// Status↔Stats tab switch, or the stats series loading in). The prior `openPanel` sized ONCE at
    /// open-time and clamped X only, so a taller Stats tab appearing after open both clipped and ran off the
    /// bottom. Placement is derived from the icon's OWN window frame, correct on any display / menu-bar
    /// height without hardcoding.
    private func setPanelFrameToContent() {
        guard let button = statusItem.button, let iconWindow = button.window else { return }
        hostingView.layoutSubtreeIfNeeded()
        var size = hostingView.fittingSize
        if size.width < 1 || size.height < 1 { size = NSSize(width: 360, height: 240) }
        let iconFrame = iconWindow.frame
        // The VISIBLE frame (excludes the menu bar + Dock) is the correct bound for on-screen clamping — a
        // physical-frame clamp would still let a tall panel slide under the Dock.
        let visible = (iconWindow.screen ?? NSScreen.main)?.visibleFrame ?? iconFrame
        var x = iconFrame.midX - size.width / 2
        x = min(max(x, visible.minX + 8), visible.maxX - size.width - 8)
        // Hang below the icon's bottom edge with the gap; then Y-CLAMP so a tall panel keeps its bottom
        // on-screen instead of clipping off the bottom (the #446 bug: X was clamped, Y was not). When the
        // full height fits below the icon, `y` is unchanged and the panel hangs normally under the gap;
        // switching back to the shorter Status tab recomputes a higher `y`, restoring the original look.
        var y = iconFrame.minY - panelGap - size.height
        if y < visible.minY + 8 { y = visible.minY + 8 }
        panel.setFrame(NSRect(x: x, y: y, width: size.width, height: size.height), display: true)
    }

    /// Re-fit the panel to its content, but ONLY while it is open (issue #446). The Stats model's observer
    /// (see `init`) calls this after a tab switch or the series loading in; a change while the panel is
    /// closed is a no-op — the next `openPanel` sizes fresh.
    private func resizePanelToContentIfOpen() {
        guard panel.isVisible else { return }
        setPanelFrameToContent()
    }

    /// Show the panel a `panelGap` below the status item, centered under the icon and clamped on-screen.
    /// Positioning is derived from the icon's OWN window frame, so it is correct on any display and any
    /// menu-bar height (notch or not) without hardcoding — the icon stays visible above the gap.
    private func openPanel() {
        // Bail before showing if the icon has no window yet: `setPanelFrameToContent` would then no-op,
        // so `orderFrontRegardless` below would flash the panel at a stale, unpositioned frame.
        guard statusItem.button?.window != nil else { return }
        setPanelFrameToContent()
        panel.orderFrontRegardless()
        // Make the panel key so VoiceOver focus moves INTO it (the borderless-panel regression: a
        // non-key window is not in VoiceOver's navigation, leaving the well-labelled rows unreachable).
        // `.nonactivatingPanel` keeps the app inactive, so keying it does NOT steal app activation — the
        // icon stays live for the second-click toggle. `orderFrontRegardless` still governs SHOWING it
        // while the accessory app is inactive; `makeKey` only adds focus.
        panel.makeKey()

        // Click-outside-to-dismiss. A GLOBAL monitor CAN also observe the menu-bar mouse-DOWN on our OWN
        // status item; ignore a click that lands on the icon so `togglePanel` owns the toggle-closed
        // (else it would close here on mouse-down and let the button's mouse-up action reopen it — the
        // classic status-item "won't close on the second click" bug).
        dismissMonitor = NSEvent.addGlobalMonitorForEvents(
            matching: [.leftMouseDown, .rightMouseDown]) { [weak self] _ in
            guard let self else { return }
            if let button = self.statusItem.button, let window = button.window,
               window.frame.contains(NSEvent.mouseLocation) {
                return
            }
            // #360: don't dismiss while the operator is mid-edit or a capture is in flight — an accidental
            // outside-click must not drop a typed-but-unsubmitted label or hide an in-flight capture. #169
            // extends the same retain to an in-flight SWAP, which writes the active account: its outcome
            // (committed, or refused with a reason) must not be hidden before the operator reads it. The
            // Esc key (field `.onExitCommand`) and the status-item toggle remain the deliberate closers.
            if self.captureModel.isBusy || self.swapModel.isBusy { return }
            self.closePanel()
        }
    }

    private func closePanel() {
        panel.orderOut(nil)
        // Reset the #394 capture surface on every close path (toggle, secondary-click, outside-click) so a
        // menu-summoned "Add account…" mode never outlives the panel — the next primary click opens the
        // normal roster. A no-op when the surface was not requested; releases the outside-click retain
        // predicate (a no-op while a capture is in flight, which runs to completion).
        captureModel.dismissCaptureSurface()
        // Reset the Stats tab to the Status glance (issue #446): each fresh open starts on Status, and the
        // Stats tab re-queries live on the next selection rather than flashing a stale window from a prior
        // open. A no-op resize while closed (the observer guards on `panel.isVisible`).
        statsModel.reset()
        if let monitor = dismissMonitor {
            NSEvent.removeMonitor(monitor)
            dismissMonitor = nil
        }
    }

    deinit {
        presentationTask?.cancel()
        if let monitor = dismissMonitor { NSEvent.removeMonitor(monitor) }
    }
}

/// A borderless `NSPanel` that CAN become key — the one override a borderless window needs so the
/// status panel is VoiceOver-navigable. A plain borderless window returns `false` from `canBecomeKey`,
/// so VoiceOver never focuses into it and the well-labelled rows stay unreachable (the regression from
/// the `NSPopover`, which auto-focused). The panel is still constructed `.nonactivatingPanel`, so
/// becoming key does NOT activate the accessory app — the non-activating design (icon stays live for
/// the second-click toggle) is preserved; only keyboard / VoiceOver focus is enabled while it is open.
/// Never main — it is a transient utility surface, not a document window.
final class FloatingPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}
