// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The daemon event log, as the panel's `View log` affordance needs it (issue #776): WHERE it lives, whether
// there is anything to show, and WHAT to hand it to.
//
// WHY THE PANEL NEEDS THIS AT ALL. The ratified mock (`design/menubar-preview.html`) puts a `View log` action
// in exactly two panel states — daemon-starting and crash-looping — and it was never built; the deferral note
// in `StatusPanelView` pointed at issues #169/#171, both since delivered, so the spec was orphaned. Meanwhile
// the app already tells operators to go read a log it hands them no path to ("Check Console for details",
// `LoginItemModel`). This file closes that loop.
//
// THE BEHAVIOUR IS UMBRELLA DECISION D3, NOT THE MOCK. The mock fixes placement, label, icon and per-state
// style; it is deliberately SILENT on what a click does. D3 settled that: open the log in **Console.app** via
// `NSWorkspace` — consistent with the app's own existing copy above and with `src/observability.rs`, which
// documents the log as "surfaced in Console.app". Rejected: reveal-in-Finder (hands over a file, not a view)
// and an in-app log window (a whole new window surface).
//
// THE PATH IS A MIRROR, NOT A GUESS. `src/paths.rs::logs_dir()` on macOS is `apple_logs_dir_from(home)` =
// `~/Library/Logs/sessiometer`, and `src/observability.rs` names the file `sessiometer.log`. That macOS branch
// is FIXED — it reads no environment variable, and the `--log` override seam (`logs_dir_with_override`) is a
// daemon-side flag the app cannot observe. So a single tail is the whole contract, and `logPath(home:)` is
// pure so a test can assert it against that contract for any home.
//
// HONEST AFFORDANCE — the issue #169 rule, and the reason `existingLogPath` returns an OPTIONAL. A button that
// cannot act must not render as though it can. Three ways there is nothing to open, all collapsing to `nil`:
// the home is unresolvable, the app is sandboxed (ADR-0011 — `NSHomeDirectory()` would be a container path,
// not the daemon's log), or the daemon has simply never written a line yet. The panel renders no button in any
// of them, exactly as `StartDaemonCard` renders no Start button where it cannot start (issue #170).
//
// DETERMINISM — why the probe is INJECTED rather than called from a view. A filesystem read reached directly
// from `body` would make the panel's appearance depend on the MACHINE it renders on: the log exists on a
// developer's Mac and not on a fresh CI runner, so `PanelGoldenParityTests` would compare renders that
// legitimately differ. That is the same class of defect issue #754's header records for `Color.accentColor`
// (which resolved to the operator's system accent in the test bundle). `DaemonLogProbe` is therefore a seam:
// its default is `.unavailable`, the live app injects `.live`, and fixtures inject a fixed answer.

import Foundation
#if canImport(AppKit)
import AppKit
#endif

// MARK: - Location

enum DaemonLogLocation {

    /// The fixed tail under the user's home the daemon writes its event log to:
    /// `Library/Logs/sessiometer/sessiometer.log`. Mirrors `src/paths.rs` exactly — `logs_dir()` on macOS is
    /// `apple_logs_dir_from(home)` (`home + "Library/Logs" + "sessiometer"`) and `src/observability.rs` names
    /// the file. NATIVE-LOCAL and override-free: unlike the daemon's `--log` flag (`logs_dir_with_override`,
    /// which is not a thing the app can see), the macOS branch reads no environment variable at all, so this
    /// resolver reading none either is a match rather than an approximation.
    static let logTail = "Library/Logs/sessiometer/sessiometer.log"

    /// The daemon's event-log path for a given home — a pure string derivation, so a test can assert it
    /// against the `paths.rs` contract for any home without touching process state.
    static func logPath(home: String) -> String {
        (home as NSString).appendingPathComponent(logTail)
    }

    /// The log path IF there is a log to show — pure over an injected existence predicate, which is what lets
    /// both arms of the honest-affordance rule be driven in a test without creating or deleting a real file.
    static func existingLogPath(home: String, fileExists: (String) -> Bool) -> String? {
        let path = logPath(home: home)
        return fileExists(path) ? path : nil
    }

    /// The live resolution: the passwd-DB home (never `$HOME`, never XDG — the same source
    /// `src/paths.rs::home_dir()` uses), the ADR-0011 non-sandbox tripwire, then existence.
    ///
    /// Every failure degrades to `nil` rather than to a guessed path: under App Sandbox the daemon's log is
    /// genuinely unreachable, and a button pointing at a container path would be precisely the dead affordance
    /// issue #169 forbids. The sandbox check is reused from `SocketPathResolver` rather than restated, so the
    /// tripwire keeps exactly one definition.
    static func existingLogPath(fileExists: (String) -> Bool = { FileManager.default.fileExists(atPath: $0) })
        -> String? {
        guard let home = SocketPathResolver.passwdHome() else { return nil }
        guard case .ok = SocketPathResolver.sandboxCheck(passwdHome: home, nsHome: NSHomeDirectory()) else {
            return nil
        }
        return existingLogPath(home: home, fileExists: fileExists)
    }
}

// MARK: - The open decision (pure) and its AppKit shell

/// Console.app's bundle identifier — resolved through `NSWorkspace` rather than hardcoded as a path, because
/// `/System/Applications/Utilities/Console.app` is an implementation detail of a given macOS layout.
private let consoleBundleIdentifier = "com.apple.Console"

/// WHAT to hand the log to — extracted from the `NSWorkspace` call so the decision is testable without a
/// workspace, the same split `StatusItemChrome` / `AppLaunchPlan` (issue #764) make for their own shells.
enum DaemonLogOpen: Equatable {
    /// Console.app resolved: open the log explicitly in it, which is D3's stated behaviour.
    case console(app: URL, log: URL)
    /// Console.app did not resolve. Rather than fail the click, fall back to the system's own handler for the
    /// file — a degraded outcome, but still a VIEW of the log, so the affordance is not a dead one. Console is
    /// a system app, so this arm is a belt-and-braces path, not an expected one.
    case systemDefault(log: URL)
}

extension DaemonLogOpen {
    /// Pure: decide how to open `logPath` given whatever Console.app URL the workspace resolved (or `nil`).
    static func plan(logPath: String, consoleApp: URL?) -> DaemonLogOpen {
        let log = URL(fileURLWithPath: logPath)
        guard let consoleApp else { return .systemDefault(log: log) }
        return .console(app: consoleApp, log: log)
    }
}

#if canImport(AppKit)
extension DaemonLogOpen {
    /// The imperative shell: resolve Console.app, take the decision above, and perform it. Returns the plan it
    /// executed so a caller can say which arm was taken.
    ///
    /// Deliberately not main-actor-isolated: the two `NSWorkspace` calls below hand off to Launch Services and
    /// are safe off the main thread, and requiring isolation here would force the panel's button action — a
    /// plain synchronous closure — through a `Task` hop for no benefit.
    @discardableResult
    static func perform(logPath: String, workspace: NSWorkspace = .shared) -> DaemonLogOpen {
        let console = workspace.urlForApplication(withBundleIdentifier: consoleBundleIdentifier)
        let plan = DaemonLogOpen.plan(logPath: logPath, consoleApp: console)
        switch plan {
        case .console(let app, let log):
            workspace.open([log], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
        case .systemDefault(let log):
            workspace.open(log)
        }
        return plan
    }
}
#endif

// MARK: - The injected availability seam

/// How the panel learns whether there is a log to view — a seam, not a call.
///
/// See this file's header for why: a direct filesystem read from a SwiftUI `body` would make the panel's
/// rendered appearance depend on the machine, which the committed panel goldens compare across machines. The
/// default is `.unavailable`, so a host that forgets to inject one renders NO button rather than a dead one —
/// the honest-affordance rule holding by construction rather than by remembering.
///
/// The closure is read during `body`, not cached: the daemon can start writing its first line while the panel
/// is already on screen, and re-probing per render (a `stat` on a card that only exists in two cold states) is
/// cheaper than the staleness of resolving once at app launch and never again.
struct DaemonLogProbe {
    /// The path of an existing daemon log, or `nil` when there is nothing to show.
    let existingLogPath: () -> String?

    init(existingLogPath: @escaping () -> String?) {
        self.existingLogPath = existingLogPath
    }

    /// No log to view — the safe default, and what every render fixture that is not exercising the affordance
    /// gets.
    static let unavailable = DaemonLogProbe { nil }

    /// The real thing: passwd-DB home → ADR-0011 sandbox tripwire → existence.
    static let live = DaemonLogProbe { DaemonLogLocation.existingLogPath() }

    /// A fixed answer, for fixtures and tests that need the affordance rendered deterministically.
    static func fixed(_ path: String?) -> DaemonLogProbe { DaemonLogProbe { path } }
}
