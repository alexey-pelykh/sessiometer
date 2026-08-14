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
        // Design-parity / debug TOOL modes — not product paths, and DEBUG-only by construction, so a
        // release build has exactly one reachable mode. Which mode this process launched in is a pure
        // argv+env decision (issue #764): `AppLaunchPlan.mode` owns it — including the precedence among
        // them, where issue #850's malformed-invocation refusal sits in it, and the exact-`"1"` gallery
        // opt-in — and `AppLaunchPlanTests` covers it headlessly.
        // This shell only ACTS on the resolved mode. Dispatch runs here so the full AppKit environment
        // (fonts, system colors) is up before either renderer draws.
        switch AppLaunchPlan.mode(arguments: CommandLine.arguments,
                                  environment: ProcessInfo.processInfo.environment,
                                  toolModesAvailable: AppLaunchPlan.toolModesAvailableInThisBuild) {
        case .renderPanel(let outputDirectory):
            // Renders the panel to PNGs for diffing against the mock, then exits — never wires the status item.
            RenderPanelTool.run(outputDir: outputDirectory)
            exit(0)

        case .renderBarGlyphs(let outputDirectory):
            // Issue #525: renders every status-item glyph — template-tinted, per appearance, @1x + @2x, plus
            // the menu-open inverted state — to committable PNGs the parity gate diffs against, then exits.
            // Like `--render-panel` it never wires the status item; unlike the panel it exists because
            // `NSStatusItem` template tinting is applied by the system and is invisible to `ImageRenderer`.
            RenderBarGlyphTool.run(outputDir: outputDirectory)
            exit(0)

        case .toolFlagMissingDirectory(let flag):
            // Issue #850: the flag arrived with no output directory after it, so there is nothing to
            // render. Report which flag and exit non-zero, rather than falling through to a real launch
            // the operator never asked for — one that installs a live status item and, because it never
            // exits, hangs a script that was waiting for a render.
            FileHandle.standardError.write(
                Data((AppLaunchPlan.missingDirectoryMessage(flag: flag) + "\n").utf8))
            exit(AppLaunchPlan.missingDirectoryExitCode)

        case .glyphGallery:
            // Issue #437: installs one real menu-bar status item per StatusGlyph — the four bespoke template
            // gauges side by side — and wires nothing else (no daemon, no transport). It exists so #437's
            // PRIORITY-1 falsifier — shape-distinctness at real bar size (light + dark, Increase Contrast,
            // over a bright wallpaper, beside system icons) — can be captured from ACTUAL NSStatusItems,
            // which a headless raster proxy cannot settle. It never JUDGES distinctness, it only makes the
            // on-device capture possible. The app keeps RUNNING afterwards (no exit) so the items stay live
            // to screenshot.
            galleryItems = StatusGlyph.allCases.map { glyph in
                let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
                item.button?.image = StatusGauge.image(for: glyph)
                item.button?.setAccessibilityLabel(
                    "Sessiometer glyph gallery: \(StatusGauge.accessibilityDescription(for: glyph))")
                return item
            }
            appLog.info("glyph gallery installed: \(self.galleryItems.count, privacy: .public) items (SESSIOMETER_GLYPH_GALLERY)")
            return

        case .normal:
            break
        }
        #endif

        // The always-visible chrome: the status item consumes the store's glance stream.
        let store = WatchStatusStore()
        self.store = store

        // The in-app capture affordance's write path (issue #360): the short-lived control-command client
        // over the SAME daemon control socket the watch transport uses, on `AppLaunchPlan.captureTimeout`.
        // Built via `.production()` (the ADR-0011 non-sandbox tripwire); a resolve failure (sandboxed /
        // home unresolved) degrades to a nil client so a capture attempt surfaces an honest "unreachable"
        // rather than a dead button — and in that case the watch transport ALSO fails, so the panel shows
        // disconnected and never renders the affordance anyway.
        let captureClient: ControlCommandClient?
        switch ControlCommandClient.production(timeout: AppLaunchPlan.captureTimeout) {
        case .success(let client):
            captureClient = client
        case .failure(let error):
            appLog.error("capture client unavailable: \(String(describing: error), privacy: .public)")
            captureClient = nil
        }

        // The swap affordance's write path (issue #169): the SAME short-lived control-command transport,
        // but with its OWN, larger budget. `AppLaunchPlan.swapTimeout` owns the value AND its derivation
        // (it must clear the cross-process single-writer lock a `swap` can wait behind — a Rust bound
        // `AppLaunchPlanTests` re-reads from `src/swap.rs`, so the two cannot drift apart silently). That
        // it is BOUNDED at all is what makes a lost ack recover instead of sticking the spinner.
        let swapClient: ControlCommandClient?
        switch ControlCommandClient.production(timeout: AppLaunchPlan.swapTimeout) {
        case .success(let client):
            swapClient = client
        case .failure(let error):
            appLog.error("swap client unavailable: \(String(describing: error), privacy: .public)")
            swapClient = nil
        }

        // The Stats-tab read path (issue #446): the SAME short-lived control-command transport, for the
        // one-shot `stats` query (#356) the panel runs when the operator opens the Stats tab — on
        // `AppLaunchPlan.statsTimeout`, which owns the budget and why a lock-free READ needs no swap-sized
        // headroom. A resolve failure degrades to a nil client → the tab shows an honest "unavailable" (and
        // the watch transport ALSO fails, so the panel is disconnected and never offers the seg anyway).
        let statsClient: ControlCommandClient?
        switch ControlCommandClient.production(timeout: AppLaunchPlan.statsTimeout) {
        case .success(let client):
            statsClient = client
        case .failure(let error):
            appLog.error("stats client unavailable: \(String(describing: error), privacy: .public)")
            statsClient = nil
        }

        // The Settings window's config read/write path (issue #268): the SAME short-lived control-command
        // transport for the one-shot config-get / config-set exchanges, on `AppLaunchPlan.configTimeout`
        // (which owns the budget and why an off-run-loop validate + atomic write needs no swap-sized
        // headroom). A resolve failure degrades to a nil client → the Settings window shows an honest
        // "not connected" and never writes config locally (AC 7).
        let configClient: ControlCommandClient?
        switch ControlCommandClient.production(timeout: AppLaunchPlan.configTimeout) {
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

        // Repair a daemon-agent registration this app update left stale (issue #788): every Release build
        // re-embeds the daemon (#171), and `SMAppService.h` is explicit that a changed executable "must be
        // re-registered or it may not launch" — unregistering first. Nothing else does it; the Start affordance
        // stands down once a daemon holds the lock (#742). Async and unawaited so a re-register (and its #745
        // liveness wait) never delays the status item coming up; a no-op on the overwhelmingly common
        // unchanged-version launch, and it never registers an agent the operator never started.
        Task { await loginItemModel.reconcileDaemonAgentRegistration() }

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
            store.start(consuming: AppLaunchPlan.disconnectedStream(reason: AppLaunchPlan.degradeReason(for: error)))
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
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
