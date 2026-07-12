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

        let controller = StatusItemController(store: store,
                                              captureClient: captureClient,
                                              swapClient: swapClient,
                                              statsClient: statsClient)
        controller.start()
        statusItemController = controller

        // Native swap / all-accounts-exhausted notifications (issue #267, REQ-MBR-B-017): a thin
        // observer over the SAME redacted store the panel renders. It posts a GENERIC macOS
        // notification (the EVENT, never the account — no label / email / credential, the redaction AC)
        // when the active account changes or the fleet runs out of viable targets. A `UserDefaults`
        // on/off toggle (default on) is the persisted home #268's settings UI will later surface;
        // authorization + display are OS-bound (a manual pre-release step). Zero egress:
        // `UNUserNotificationCenter` is a local OS call, no network. Installed BEFORE `store.start(...)`
        // below so the observer never misses the first snapshot's transition.
        let notifier = AccountEventNotifier(preferences: NotificationPreferences(),
                                            presenter: UserNotificationPresenter())
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
