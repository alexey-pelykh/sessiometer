// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The Settings window's lifecycle owner (issue #268): a single app-retained controller that lazily builds
// one titled `NSWindow` hosting `SettingsView`, reuses it across opens, and manages the accessory-app
// activation dance a real editing window needs.
//
// Why a titled window, not the status panel: the status panel is a borderless, NON-activating `NSPanel`
// (StatusItemController) — deliberately, so the menu-bar icon stays live. That is the OPPOSITE of what text
// entry needs. A settings form has many `TextField`s, so it wants a normal key window with a title bar, the
// Edit menu (Cut/Copy/Paste/Select-All), and Cmd-Tab reachability. So while the window is open the app
// transiently becomes a `.regular` activation-policy app (Dock icon + menu bar + Cmd-Tab); on close it
// reverts to `.accessory` (the LSUIElement menu-bar-only identity set in main.swift). `NSApp.activate(
// ignoringOtherApps:)` is used (macOS 13 floor — the parameterless `activate()` is 14+).
//
// Single-instance: the window is built once and reused (`isReleasedWhenClosed = false`); each open re-runs
// `config-get` so the form reflects the daemon's CURRENT state, never a stale snapshot from a prior open.

import AppKit
import SwiftUI

@MainActor
final class SettingsWindowController: NSObject, NSWindowDelegate {
    private let model: SettingsModel
    /// The launch-at-login model (issue #170) — passed straight to `SettingsView` for the General toggle and
    /// refreshed on every open (below), the SAME app-retained instance the not-running panel card observes.
    private let loginItem: LoginItemModel
    private var window: NSWindow?

    init(model: SettingsModel, loginItem: LoginItemModel) {
        self.model = model
        self.loginItem = loginItem
        super.init()
    }

    /// Open (or re-focus) the Settings window and refresh it from the daemon. Builds the window on first
    /// call and reuses it thereafter. Transiently promotes the app to `.regular` so text fields get the
    /// system Edit menu + the window is Cmd-Tab reachable.
    func show() {
        let window = existingOrNewWindow()

        // Promote to a regular app so the titled window gets full key-window text behavior (Edit menu,
        // Cmd-Tab, Dock). Reverted to `.accessory` in `windowWillClose`.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)

        // Re-read the login-item status on every open (issue #170) so a change the user made directly in
        // System Settings › General › Login Items is reflected without a relaunch — the same open-time
        // freshness contract the daemon `config-get` below gives the tunables. Synchronous (no socket).
        loginItem.refreshStatus()

        // Re-fetch config on every open — the SINGLE load trigger (first open AND reopens); the view has no
        // competing `.task`, so first open never fires config-get twice. The model discards stale drafts.
        Task { await model.load() }
    }

    private func existingOrNewWindow() -> NSWindow {
        if let window { return window }

        let hosting = NSHostingController(rootView: SettingsView(model: model, loginItem: loginItem))
        let window = NSWindow(contentViewController: hosting)
        window.title = "Sessiometer Settings"
        // No `.miniaturizable`: minimizing does not fire `windowWillClose`, so a minimized window would strand
        // the app in `.regular` (a lingering Dock icon). A transient settings window doesn't need minimize.
        window.styleMask = [.titled, .closable]
        window.isReleasedWhenClosed = false  // single-instance: reuse across opens, never a dangling ref
        window.identifier = NSUserInterfaceItemIdentifier("SessiometerSettings")
        window.delegate = self
        window.setContentSize(NSSize(width: 460, height: 560))
        window.center()
        self.window = window
        return window
    }

    // MARK: - NSWindowDelegate

    /// Revert to the menu-bar-only accessory identity once the Settings window closes — unless another
    /// regular window is still open (defensive; this agent app has none today, but the guard keeps a future
    /// second window from being orphaned by a premature demotion).
    func windowWillClose(_ notification: Notification) {
        let hasOtherVisibleWindow = NSApp.windows.contains { other in
            other !== window && other.isVisible && other.canBecomeMain
        }
        if !hasOtherVisibleWindow {
            NSApp.setActivationPolicy(.accessory)
        }
    }
}
