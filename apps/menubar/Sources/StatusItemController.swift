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
import SwiftUI

@MainActor
final class StatusItemController {
    private let statusItem: NSStatusItem
    private let panel: FloatingPanel
    /// The SwiftUI host, kept so `openPanel` can size the panel to the current content.
    private let hostingView: NSView
    private let store: WatchStatusStore
    private var presentationTask: Task<Void, Never>?
    /// The UX gap between the menu bar and the panel's top edge.
    private let panelGap: CGFloat = 6
    /// The outside-click monitor installed WHILE the panel is open (see `openPanel`). `nil` when closed.
    private var dismissMonitor: Any?

    init(store: WatchStatusStore) {
        self.store = store
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        // #326's status panel reads the store via `@EnvironmentObject` (a thin view over the
        // `src/cli.rs`-mirroring `StatusPanelFormat`), so inject it here rather than through an init.
        let hosting = NSHostingView(rootView: StatusPanelView().environmentObject(store))
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

    /// The secondary-click lifecycle menu. Today it carries only "Quit Sessiometer" — a pure-CLIENT
    /// control that terminates the menu-bar app itself (`NSApp.terminate`, which runs the clean
    /// `applicationWillTerminate` transport-stop path); it does NOT touch the daemon, whose
    /// quit/restart lifecycle is #170. It is a right-click menu rather than a panel button so the
    /// status panel stays a pure display + manual-swap surface (design C-005 IA scope guard), and is
    /// the natural future home for the other runtime controls (#170 daemon quit/restart,
    /// launch-at-login). Shown via a TRANSIENT `statusItem.menu` so AppKit positions and highlights
    /// it natively under the item, then cleared so the primary click keeps toggling the panel.
    private func showLifecycleMenu() {
        // Close the panel first if it is open: the click-outside global monitor never sees our own
        // status-item events, so without this a secondary click would leave the panel lingering.
        if panel.isVisible { closePanel() }
        let menu = NSMenu()
        let quitItem = NSMenuItem(title: "Quit Sessiometer", action: #selector(quit), keyEquivalent: "")
        quitItem.target = self
        menu.addItem(quitItem)
        statusItem.menu = menu
        statusItem.button?.performClick(nil)
        statusItem.menu = nil
    }

    /// Quit the menu-bar app (a pure-client control). The daemon keeps running — its lifecycle is #170.
    @objc private func quit() {
        NSApp.terminate(nil)
    }

    /// Show the panel a `panelGap` below the status item, centered under the icon and clamped on-screen.
    /// Positioning is derived from the icon's OWN window frame, so it is correct on any display and any
    /// menu-bar height (notch or not) without hardcoding — the icon stays visible above the gap.
    private func openPanel() {
        guard let button = statusItem.button, let iconWindow = button.window else { return }
        hostingView.layoutSubtreeIfNeeded()
        var size = hostingView.fittingSize
        if size.width < 1 || size.height < 1 { size = NSSize(width: 360, height: 240) }
        let iconFrame = iconWindow.frame
        let screenFrame = (iconWindow.screen ?? NSScreen.main)?.frame ?? iconFrame
        var x = iconFrame.midX - size.width / 2
        x = min(max(x, screenFrame.minX + 8), screenFrame.maxX - size.width - 8)
        let y = iconFrame.minY - panelGap - size.height   // hang below the icon's bottom edge, with the gap
        panel.setFrame(NSRect(x: x, y: y, width: size.width, height: size.height), display: true)
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
            self.closePanel()
        }
    }

    private func closePanel() {
        panel.orderOut(nil)
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
