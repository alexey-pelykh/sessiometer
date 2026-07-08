// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The menu-bar status-item controller (issue #325): the IMPERATIVE SHELL that owns the always-visible
// menu-bar chrome and binds it to the store's glance. It creates the single `NSStatusItem` (the app is
// LSUIElement / `.accessory` — set in `main.swift`), renders the shape-encoded template gauge
// (`StatusGauge`) for the current `StatusGlyph`, keeps the button's VoiceOver label in step with the
// state, and toggles an `NSPopover` (hosting `StatusPanelView` via `NSHostingController`) on click.
//
// It CONSUMES the store's `presentations` glance stream (#324): one `PresentationState` per state
// change drives one `apply(_:)`, updating BOTH the glyph image AND the accessibility label together —
// so VoiceOver always reads the CURRENT state on every change (AC). The pure shape/label content lives
// in `StatusGauge` + `PresentationState`; this shell only performs AppKit side effects, so there is no
// state logic here to get wrong (mirroring `WatchStatusStore` over the pure `HonestStateMachine`).

import AppKit
import SwiftUI

@MainActor
final class StatusItemController {
    private let statusItem: NSStatusItem
    private let popover: NSPopover
    private let store: WatchStatusStore
    private var presentationTask: Task<Void, Never>?
    /// The outside-click monitor installed WHILE the panel is open (see `openPanel`). `nil` when closed.
    private var dismissMonitor: Any?

    init(store: WatchStatusStore) {
        self.store = store
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        let popover = NSPopover()
        // We own dismissal (`.applicationDefined`), NOT `.transient`. Under `.transient`, re-clicking
        // the menu-bar item to close races: the transient monitor closes the popover on mouse-down,
        // then the button action fires with `isShown` already false and REOPENS it (the classic
        // status-item "won't close on a second click" bug). Owning dismissal makes the second click a
        // clean toggle-closed; `openPanel` adds its own outside-click monitor to replace what
        // `.transient` gave us.
        popover.behavior = .applicationDefined
        popover.animates = false
        // #326's status panel reads the store via `@EnvironmentObject` (a thin view over the
        // `src/cli.rs`-mirroring `StatusPanelFormat`), so inject it here rather than through an init.
        popover.contentViewController = NSHostingController(rootView: StatusPanelView().environmentObject(store))
        self.popover = popover

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
        button.action = #selector(togglePanel)
    }

    /// Mirror one glance onto the button: the shape-encoded template gauge PLUS the spoken VoiceOver
    /// label. Both move together on every state change (AC: the label tracks the current state).
    private func apply(_ presentation: PresentationState) {
        guard let button = statusItem.button else { return }
        button.image = StatusGauge.image(for: presentation.glyph)
        button.setAccessibilityLabel(presentation.accessibilityLabel)
    }

    /// Click handler: toggle the panel (AC — clicking the item shows AND hides it).
    @objc private func togglePanel() {
        if popover.isShown {
            closePanel()
        } else {
            openPanel()
        }
    }

    private func openPanel() {
        guard let button = statusItem.button else { return }
        NSApp.activate(ignoringOtherApps: true)     // activate BEFORE showing so the popover takes key
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        // Restore the click-outside-to-dismiss affordance `.applicationDefined` drops: a GLOBAL monitor
        // never sees our own popover's events, so clicking inside the panel keeps it open, while a click
        // in any other app (or the rest of the menu bar) closes it. Re-clicking our own item is handled
        // by `togglePanel` above.
        dismissMonitor = NSEvent.addGlobalMonitorForEvents(
            matching: [.leftMouseDown, .rightMouseDown]) { [weak self] _ in
            self?.closePanel()
        }
    }

    private func closePanel() {
        popover.performClose(nil)
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
