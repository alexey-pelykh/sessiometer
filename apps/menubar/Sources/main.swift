// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The menu-bar (LSUIElement/`.accessory`) app entry (issues #168 / #326): a single `NSStatusItem`
// whose click toggles an `NSPopover` hosting the SwiftUI `StatusPanelView`, backed by the honest-state
// `WatchStatusStore` (#324) fed by the zero-egress `WatchTransport` (#323). Per ADR-0010 it links no
// Rust and shares no build graph with the crate; the AF_UNIX control socket is the ENTIRE boundary.
//
// This assembles the parts (#322 wire ← #323 transport ← #324 store ← #326 panel) into a working
// smoke-phase app: the transport is built for the daemon's resolved socket path, or — when the path
// won't resolve (sandboxed / home-unresolved, ADR-0011 tripwire) — the store is fed a synthetic
// `.disconnected` so the panel degrades LOUDLY to "daemon not responding" rather than a perpetual
// "connecting". The rich per-account health map + glance-icon shapes are #168/#169; here the glance is
// a single template symbol whose VoiceOver label tracks the honest state.

import AppKit
import SwiftUI
import os

private let appLog = Logger(subsystem: "com.sessiometer.menubar", category: "app")

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem?
    private var popover: NSPopover?
    private let store = WatchStatusStore()
    private var transport: WatchTransport?
    private var glanceTask: Task<Void, Never>?

    func applicationDidFinishLaunching(_ notification: Notification) {
        setUpStatusItem()
        setUpPopover()
        startGlance()
        startTransport()
    }

    // MARK: - Status item (the glance surface)

    private func setUpStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = item.button {
            // A shape-coded template symbol (color-agnostic, follows the menu-bar tint). The rich
            // per-state glance shapes are #168/#169; #326's honesty rides the VoiceOver label below.
            let image = NSImage(systemSymbolName: "gauge.medium",
                                accessibilityDescription: "Sessiometer")
            image?.isTemplate = true
            button.image = image
            button.target = self
            button.action = #selector(togglePanel)
        }
        statusItem = item
    }

    // MARK: - Popover (the click-panel surface)

    private func setUpPopover() {
        let popover = NSPopover()
        popover.behavior = .transient          // click-away dismisses
        popover.animates = true
        popover.contentViewController =
            NSHostingController(rootView: StatusPanelView().environmentObject(store))
        self.popover = popover
    }

    @objc private func togglePanel() {
        guard let popover, let button = statusItem?.button else { return }
        if popover.isShown {
            popover.performClose(nil)
        } else {
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            // Bring the popover's window forward so keyboard/VoiceOver focus lands in the panel.
            popover.contentViewController?.view.window?.makeKey()
        }
    }

    // MARK: - Glance stream → status-item accessibility

    /// Consume the store's `presentations` glance stream and mirror each honest-state onto the status
    /// item's VoiceOver label — the AppKit counterpart of the SwiftUI panel's `@Published` surface.
    private func startGlance() {
        glanceTask = Task { [weak self] in
            guard let self else { return }
            for await presentation in store.presentations {
                self.statusItem?.button?.setAccessibilityLabel(presentation.accessibilityLabel)
            }
        }
    }

    // MARK: - Transport → store

    /// Build the zero-egress transport for the daemon's resolved socket path and pump the store from
    /// it; on a resolve failure, degrade LOUDLY by feeding the store a synthetic `.disconnected`
    /// (ADR-0011 non-sandbox tripwire) so the panel is honest rather than stuck "connecting".
    private func startTransport() {
        switch WatchTransport.production() {
        case .success(let transport):
            self.transport = transport
            store.start(consuming: transport.events)
            Task { await transport.start() }
        case .failure(let error):
            appLog.error("watch: socket path unresolved — degrading to disconnected: \(self.reason(for: error), privacy: .public)")
            let (events, continuation) = AsyncStream<TransportEvent>.makeStream()
            store.start(consuming: events)
            continuation.yield(.disconnected(reason: reason(for: error)))
            continuation.finish()
        }
    }

    /// A safe, non-secret reason string for a resolve failure — no home path is echoed (the sandboxed
    /// variant carries the container path, which we deliberately do not surface).
    private func reason(for error: SocketPathResolver.ResolveError) -> String {
        switch error {
        case .homeUnresolved:
            return "home directory could not be resolved"
        case .sandboxed:
            return "app is sandboxed; the daemon socket is unreachable"
        }
    }
}

// `main.swift` top-level code runs on the main thread at process start, so it is safe to assume
// MainActor isolation here to build the MainActor-isolated `AppDelegate` (it owns the @MainActor
// `WatchStatusStore`). `app.run()` blocks inside the closure until the app terminates, keeping the
// delegate — held only weakly by `NSApplication` — alive for the whole app lifetime.
MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.accessory)
    app.run()
}
