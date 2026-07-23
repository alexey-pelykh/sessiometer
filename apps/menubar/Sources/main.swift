// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The menu-bar app entry point (issue #325, part of #168). An LSUIElement / `.accessory` agent app —
// no Dock icon, no main window, just the always-visible `NSStatusItem` chrome. It wires the honest
// vertical slice built across #322–#325:
//
//   WatchTransport (#323, raw AF_UNIX, zero egress) → AsyncStream<TransportEvent>
//     → WatchStatusStore (#324, the honest-state store)
//       → StatusItemController (#325, the shape-encoded gauge + VoiceOver + click-to-toggle panel)
//
// The transport is built via `WatchTransport.production()`, which resolves the daemon's control-socket
// path and applies the ADR-0011 non-sandbox tripwire. If it CANNOT resolve (sandboxed / home
// unresolved), the app degrades LOUDLY and honestly: the store is fed a single `.disconnected`, so the
// menu bar shows the slashed "disconnected" glyph — never a dishonest "connecting" that will never
// resolve. The specific reason is logged and carried on the event; the D2 baseline glance (#324)
// speaks a fixed "disconnected" sentence, so surfacing the reason itself is #169's richer degraded UX.

import AppKit
import os

private let appLog = Logger(subsystem: "org.sessiometer.menubar", category: "app")

// The `NSApplicationDelegate` methods are already `@MainActor` (the AppKit protocol is), so all the
// AppKit + store wiring below runs on the main actor without annotating the class — mirroring the
// original skeleton, which built the `NSStatusItem` here. The stored references are each Sendable (two
// `@MainActor` classes + an actor), so holding them on a non-isolated delegate is race-free.
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var store: WatchStatusStore?
    private var statusItemController: StatusItemController?
    private var transport: WatchTransport?
    private var accountEventNotifier: AccountEventNotifier?
    /// The Settings window's app-retained controller (issue #268) — one titled window reused across opens,
    /// opened from the status item's secondary-click menu. Held here so it (and its `SettingsModel`) outlive
    /// each open/close cycle.
    private var settingsWindowController: SettingsWindowController?
    /// The launch-at-login / Start-daemon model (issue #170), app-retained so the ONE shared instance outlives
    /// each panel open and Settings open/close cycle — see `applicationDidFinishLaunching`.
    private var loginItemModel: LoginItemModel?
    #if DEBUG
    /// Retains the debug glyph-gallery status items (the issue #437 `SESSIOMETER_GLYPH_GALLERY` harness) so
    /// they are not deallocated while the gallery-only app runs; empty in normal operation.
    private var galleryItems: [NSStatusItem] = []
    #endif

    func applicationDidFinishLaunching(_ notification: Notification) {
        #if DEBUG
        // Design-parity tooling (not a product path): `--render-panel <dir>` renders the panel to PNGs
        // for diffing against the mock, then exits — never wires the status item. Runs here so the full
        // AppKit environment (fonts, system colors) is up before `ImageRenderer` draws.
        if let idx = CommandLine.arguments.firstIndex(of: "--render-panel"),
           idx + 1 < CommandLine.arguments.count {
            RenderPanelTool.run(outputDir: CommandLine.arguments[idx + 1])
            exit(0)
        }

        // Bar-glyph render-parity tooling (issue #525): `--render-bar-glyphs <dir>` renders every status-
        // item glyph — template-tinted, per appearance, @1x + @2x, plus the menu-open inverted state — to
        // committable PNGs the parity gate diffs against, then exits. Like `--render-panel` it never wires
        // the status item; unlike the panel it exists because `NSStatusItem` template tinting is applied by
        // the system and is invisible to SwiftUI `ImageRenderer`.
        if let idx = CommandLine.arguments.firstIndex(of: "--render-bar-glyphs"),
           idx + 1 < CommandLine.arguments.count {
            RenderBarGlyphTool.run(outputDir: CommandLine.arguments[idx + 1])
            exit(0)
        }

        // Glyph-gallery harness (issue #437): `SESSIOMETER_GLYPH_GALLERY=1` installs one real menu-bar
        // status item per StatusGlyph — the four bespoke template gauges side by side — and wires nothing
        // else (no daemon, no transport). It exists so #437's PRIORITY-1 falsifier — shape-distinctness at
        // real bar size (light + dark, Increase Contrast, over a bright wallpaper, beside system icons) —
        // can be captured from ACTUAL NSStatusItems, which a headless raster proxy cannot settle. Opt-in and
        // inert in normal operation; it never JUDGES distinctness, it only makes the on-device capture
        // possible. The app keeps running afterwards (no exit) so the items stay live to screenshot.
        if ProcessInfo.processInfo.environment["SESSIOMETER_GLYPH_GALLERY"] == "1" {
            galleryItems = StatusGlyph.allCases.map { glyph in
                let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
                item.button?.image = StatusGauge.image(for: glyph)
                item.button?.setAccessibilityLabel(
                    "Sessiometer glyph gallery: \(StatusGauge.accessibilityDescription(for: glyph))")
                return item
            }
            appLog.info("glyph gallery installed: \(self.galleryItems.count, privacy: .public) items (SESSIOMETER_GLYPH_GALLERY)")
            return
        }
        #endif

        // The always-visible chrome: the status item consumes the store's glance stream.
        let store = WatchStatusStore()
        self.store = store

        // The in-app capture affordance's write path (issue #360): the short-lived control-command client
        // over the SAME daemon control socket the watch transport uses. Built via `.production()` (the
        // ADR-0011 non-sandbox tripwire); a resolve failure (sandboxed / home unresolved) degrades to a nil
        // client so a capture attempt surfaces an honest "unreachable" rather than a dead button — and in
        // that case the watch transport ALSO fails, so the panel shows disconnected and never renders the
        // affordance anyway.
        let captureClient: ControlCommandClient?
        switch ControlCommandClient.production() {
        case .success(let client):
            captureClient = client
        case .failure(let error):
            appLog.error("capture client unavailable: \(String(describing: error), privacy: .public)")
            captureClient = nil
        }

        // The swap affordance's write path (issue #169): the SAME short-lived control-command transport,
        // but with its OWN, larger budget — exactly the per-call-site timeout `ControlCommandClient`
        // earmarks for this exchange. A `swap` ack is written only AFTER the swap runs, and the swap may
        // wait on the cross-process single-writer lock for up to `SWAP_LOCK_MAX_WAIT` (10 s, `src/swap.rs`)
        // before failing closed. The capture default (2 s) would therefore time out a swap that is merely
        // QUEUED and about to succeed — reporting a false failure for a write that then commits. 15 s
        // clears the lock's own bound with headroom for the keychain read/write beneath it. The bound is
        // what makes a lost ack recover instead of sticking the spinner (issue #169).
        let swapClient: ControlCommandClient?
        switch ControlCommandClient.production(timeout: .seconds(15)) {
        case .success(let client):
            swapClient = client
        case .failure(let error):
            appLog.error("swap client unavailable: \(String(describing: error), privacy: .public)")
            swapClient = nil
        }

        // The Stats-tab read path (issue #446): the SAME short-lived control-command transport, for the
        // one-shot `stats` query (#356) the panel runs when the operator opens the Stats tab. A bounded READ
        // answered off the daemon's run loop (no lock, unlike `swap`), so a modest 5 s budget clears a slower
        // store aggregation without the swap path's 15 s lock headroom. A resolve failure degrades to a nil
        // client → the tab shows an honest "unavailable" (and the watch transport ALSO fails, so the panel is
        // disconnected and never offers the seg anyway).
        let statsClient: ControlCommandClient?
        switch ControlCommandClient.production(timeout: .seconds(5)) {
        case .success(let client):
            statsClient = client
        case .failure(let error):
            appLog.error("stats client unavailable: \(String(describing: error), privacy: .public)")
            statsClient = nil
        }

        // The Settings window's config read/write path (issue #268): the SAME short-lived control-command
        // transport for the one-shot config-get / config-set exchanges. A 5 s budget — config-set validates
        // + atomically writes config.toml off the daemon's run loop (no swap.lock), clearing a slower disk
        // write without the swap path's 15 s lock headroom. A resolve failure degrades to a nil client → the
        // Settings window shows an honest "not connected" and never writes config locally (AC 7).
        let configClient: ControlCommandClient?
        switch ControlCommandClient.production(timeout: .seconds(5)) {
        case .success(let client):
            configClient = client
        case .failure(let error):
            appLog.error("config client unavailable: \(String(describing: error), privacy: .public)")
            configClient = nil
        }

        // The launch-at-login / Start-daemon model (issue #170): the ONE app-retained `LoginItemModel` over the
        // real `SMAppService` seam, SHARED by the panel's not-running Start affordance (through the controller
        // below) and the Settings "General" toggle (through the window controller further down) so the two never
        // disagree about registration state. No daemon dependency and no credential (issue #15), so it is built
        // unconditionally — independent of the control-socket clients above. Register the APP login item on this
        // launch (idempotent — a no-op when already enabled; touches ONLY the app login item, never the daemon
        // agent, which stays user-initiated via the Start affordance — the #170 keystone).
        let loginItemModel = LoginItemModel(service: SMAppServiceLoginItemService())
        self.loginItemModel = loginItemModel
        loginItemModel.registerAppLoginItemOnLaunch()

        let controller = StatusItemController(store: store,
                                              captureClient: captureClient,
                                              swapClient: swapClient,
                                              statsClient: statsClient,
                                              loginItemModel: loginItemModel)
        controller.start()
        statusItemController = controller

        // Notification preference + presenter — ONE source of truth shared by the #267 notifier below and
        // the #268 Settings toggle, so the toggle and the live notifier never drift (both bind the same
        // UserDefaults key) and the presenter's OS-authorization request is issued from one place.
        let notificationPreferences = NotificationPreferences()
        let notificationPresenter = UserNotificationPresenter()

        // The Settings window (issue #268): an app-retained controller owning one titled window over the
        // daemon config + the notification toggle, opened from the status item's secondary-click menu via the
        // injected `onOpenSettings` seam. Enabling the toggle asks the shared presenter for OS authorization.
        let settingsModel = SettingsModel(
            client: configClient,
            preferences: notificationPreferences,
            onRequestAuthorization: { notificationPresenter.requestAuthorization() })
        let settingsController = SettingsWindowController(model: settingsModel, loginItem: loginItemModel)
        self.settingsWindowController = settingsController
        controller.onOpenSettings = { [weak settingsController] in settingsController?.show() }

        // Native swap / all-accounts-exhausted notifications (issue #267, REQ-MBR-B-017): a thin
        // observer over the SAME redacted store the panel renders. It posts a GENERIC macOS
        // notification (the EVENT, never the account — no label / email / credential, the redaction AC)
        // when the active account changes or the fleet runs out of viable targets. A `UserDefaults`
        // on/off toggle (default on) is the persisted home the #268 Settings toggle now surfaces — this run
        // shares the ONE `notificationPreferences` + `notificationPresenter` built above between this notifier
        // and that toggle, so they read one source of truth and enabling the toggle drives OS authorization.
        // Zero egress: `UNUserNotificationCenter` is a local OS call, no network. Installed BEFORE
        // `store.start(...)` below so the observer never misses the first snapshot's transition.
        let notifier = AccountEventNotifier(preferences: notificationPreferences,
                                            presenter: notificationPresenter)
        notifier.start(observing: store)
        accountEventNotifier = notifier

        // Feed the store from the daemon's watch socket — or degrade loudly if the path won't resolve.
        switch WatchTransport.production() {
        case .success(let transport):
            self.transport = transport
            store.start(consuming: transport.events)
            Task { await transport.start() }
        case .failure(let error):
            appLog.error("watch transport unavailable: \(String(describing: error), privacy: .public)")
            store.start(consuming: Self.disconnectedStream(reason: Self.reason(for: error)))
        }

        // Sleep/wake gating of the warm-dwell escalation (issue #526): suspend the store's warm-dwell timer
        // across system sleep so a benign overnight lid-close — a long disconnect that resolves in ~1 s on
        // wake — never escalates a warm drop to Attention while asleep (the app would otherwise open on a
        // FALSE "!" at its most-seen moment every morning). `willSleep` suspends the dwell; `didWake` resets
        // it to a fresh window. These arrive on `NSWorkspace.shared.notificationCenter` (NOT the default
        // center) on the main thread; the store's `systemWillSleep` / `systemDidWake` are `@MainActor`, so
        // hop via `Task { @MainActor in }` (macOS 13 floor rules out `MainActor.assumeIsolated`, 14+). The
        // store methods are unit-tested directly with synthetic sleep/wake; only THIS OS wiring is the
        // on-device falsifier the issue asks the operator to verify post-merge.
        let workspaceCenter = NSWorkspace.shared.notificationCenter
        _ = workspaceCenter.addObserver(forName: NSWorkspace.willSleepNotification,
                                        object: nil, queue: .main) { [weak store] _ in
            Task { @MainActor in store?.systemWillSleep() }
        }
        _ = workspaceCenter.addObserver(forName: NSWorkspace.didWakeNotification,
                                        object: nil, queue: .main) { [weak store] _ in
            Task { @MainActor in store?.systemDidWake() }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        guard let transport else { return }
        Task { await transport.stop() }
    }

    /// A one-shot event stream that yields a single `.disconnected` then finishes — the honest feed for
    /// an unresolvable socket path, so the glance renders the "disconnected" glyph (the reason is logged
    /// above and carried on the event) instead of a perpetual "connecting".
    private static func disconnectedStream(reason: String) -> AsyncStream<TransportEvent> {
        AsyncStream { continuation in
            continuation.yield(.disconnected(reason: reason))
            continuation.finish()
        }
    }

    private static func reason(for error: SocketPathResolver.ResolveError) -> String {
        switch error {
        case .homeUnresolved:
            return "home directory unresolved"
        case .sandboxed:
            return "app is sandboxed — the daemon socket is unreachable"
        }
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
