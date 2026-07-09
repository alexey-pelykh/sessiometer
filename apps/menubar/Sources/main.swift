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
        let controller = StatusItemController(store: store)
        controller.start()
        statusItemController = controller

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
