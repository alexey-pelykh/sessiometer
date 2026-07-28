// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The app entry point's PURE decision layer (issue #764) — the functional core `main.swift` is the
// imperative shell of. `main.swift` is top-level AppKit entry code, so it cannot live in a unit-test
// bundle AT ALL (not "is awkward to test" — a top-level entry point is structurally excluded). Every
// startup decision that is a function of plain values therefore lives here instead, where the headless
// `MenubarTests` bundle can reach it: which mode the process launched in, how a socket-resolve failure
// is worded for the operator, and what each control-command call site's timeout budget is.
//
// `AppDelegate` keeps only the wiring those decisions drive — constructing the store, the transport, the
// clients, the controller, and the workspace sleep/wake observers.
//
// WHAT IS NOT HERE. Issue #764's AC-3 names "what to do on first launch" and "when `canStartDaemon` is
// false" as startup decisions to extract. Both were extracted already, by #170: they live in
// `LoginItemModel` (`registerAppLoginItemOnLaunch`, `canStartDaemon`, `reconcileDaemonAgentRegistration`)
// and are covered by `LoginItemModelTests` against a fake `SMAppService` seam. This file does not
// re-model them; see the issue #764 PR body for the measurement.

import Foundation

/// How this process was launched. The three tool modes are DEBUG-only design/parity instruments — they
/// render committable artefacts and then exit (or, for the gallery, stand up bare status items and wire
/// nothing else). A release build has exactly one mode, `.normal`.
enum AppLaunchMode: Equatable {
    /// `--render-panel <dir>` (#355/#754) — render the panel fixtures to PNGs for the design-parity
    /// oracle, then exit. Never wires the status item.
    case renderPanel(outputDirectory: String)
    /// `--render-bar-glyphs <dir>` (#525) — render every status-item glyph as it appears in the bar
    /// (template-tinted, per appearance, @1x + @2x, plus menu-open), then exit.
    case renderBarGlyphs(outputDirectory: String)
    /// `SESSIOMETER_GLYPH_GALLERY=1` (#437) — install one real `NSStatusItem` per `StatusGlyph` and wire
    /// nothing else, so on-device shape-distinctness can be captured from ACTUAL status items. Unlike the
    /// render modes the app keeps RUNNING afterwards, so the items stay live to screenshot.
    case glyphGallery
    /// The product path: the full transport → store → status-item wiring.
    case normal
}

/// The app entry point's pure decision layer — a caseless `enum` namespace of total functions, the same
/// shape as `StatusGauge` / `SocketPathResolver` / `StatusItemChrome`.
enum AppLaunchPlan {

    // MARK: - Launch-mode dispatch

    static let renderPanelFlag = "--render-panel"
    static let renderBarGlyphsFlag = "--render-bar-glyphs"
    static let glyphGalleryEnvironmentKey = "SESSIOMETER_GLYPH_GALLERY"
    /// The gallery is opt-in on an EXACT `"1"`, not on mere presence — so an empty or `"0"` value
    /// inherited from a shell profile cannot silently turn a normal launch into a gallery launch.
    static let glyphGalleryEnabledValue = "1"

    /// Whether this build carries the DEBUG-only tool modes. Release builds compile the tool surface out
    /// entirely, so their only reachable mode is `.normal`.
    static var toolModesAvailableInThisBuild: Bool {
        #if DEBUG
        return true
        #else
        return false
        #endif
    }

    /// Resolve the launch mode from process state.
    ///
    /// Precedence — `--render-panel`, then `--render-bar-glyphs`, then the gallery environment variable,
    /// then normal — is the pre-extraction `AppDelegate` dispatch order, preserved. This function is now
    /// the ONLY place it is expressed: the delegate switches on an already-resolved mode, so its case
    /// order says nothing. Order is observable when more than one is supplied, so it is pinned by test.
    ///
    /// A render flag needs a FOLLOWING argument to be a render mode. Supplied as the last argument it has
    /// no output directory, and dispatch falls through — see `toolFlagMissingDirectory` for the honesty
    /// problem that creates, which this function reports rather than papers over.
    static func mode(arguments: [String],
                     environment: [String: String],
                     toolModesAvailable: Bool) -> AppLaunchMode {
        guard toolModesAvailable else { return .normal }

        if let directory = directoryArgument(after: renderPanelFlag, in: arguments) {
            return .renderPanel(outputDirectory: directory)
        }
        if let directory = directoryArgument(after: renderBarGlyphsFlag, in: arguments) {
            return .renderBarGlyphs(outputDirectory: directory)
        }
        if environment[glyphGalleryEnvironmentKey] == glyphGalleryEnabledValue {
            return .glyphGallery
        }
        return .normal
    }

    /// Whether a tool flag was supplied WITHOUT the output directory it needs — the case `mode(...)`
    /// resolves to `.normal`, which means a mistyped `--render-panel` silently launches the real app and
    /// installs a live status item instead of reporting the missing argument.
    ///
    /// Exposed as its own predicate so the condition is nameable and assertable even though dispatch does
    /// not yet act on it. Filed as issue #850 rather than changed under an issue #764 coverage item:
    /// acting on it is a behaviour change to the app's CLI surface, not coverage.
    static func toolFlagMissingDirectory(arguments: [String]) -> Bool {
        for flag in [renderPanelFlag, renderBarGlyphsFlag] {
            if let index = arguments.firstIndex(of: flag), index + 1 >= arguments.count { return true }
        }
        return false
    }

    /// The argument following `flag`, or `nil` when the flag is absent or is the last argument. Both
    /// quirks below are the pre-extraction `arguments[idx + 1]` behaviour, preserved deliberately rather
    /// than tightened under a coverage item: `firstIndex` resolves a REPEATED flag on its first
    /// occurrence, and the value is taken VERBATIM — `--render-panel --render-bar-glyphs` yields the
    /// literal second flag as the output directory.
    private static func directoryArgument(after flag: String, in arguments: [String]) -> String? {
        guard let index = arguments.firstIndex(of: flag), index + 1 < arguments.count else { return nil }
        return arguments[index + 1]
    }

    // MARK: - Degrade-loudly wording + feed

    /// The operator-facing reason carried on the single `.disconnected` event the app feeds its store when
    /// the daemon socket path cannot be resolved (ADR-0011's non-sandbox tripwire).
    ///
    /// Degrading to `.disconnected` — rather than leaving the glance at `.connecting` — is the honesty
    /// contract: a `connecting` that will never resolve is a lie, and the menu bar must show the state it
    /// can actually vouch for. The sentence is plain and non-secret; it names the condition, never a path.
    static func degradeReason(for error: SocketPathResolver.ResolveError) -> String {
        switch error {
        case .homeUnresolved:
            return "home directory unresolved"
        case .sandboxed:
            return "app is sandboxed — the daemon socket is unreachable"
        }
    }

    /// The event feed for an unresolvable socket path: yields EXACTLY ONE `.disconnected` carrying
    /// `reason`, then FINISHES.
    ///
    /// Both halves of that shape are the contract, and each fails differently if broken. Yielding nothing
    /// (or never finishing) would leave the store at its initial `.connecting` glance forever — the
    /// dishonest "connecting that will never resolve" the degrade path exists to prevent. Finishing is
    /// what tells the store no further events are coming, so it settles rather than waiting on a feed that
    /// can never produce one; there is nothing to reconnect to, since the path itself did not resolve.
    static func disconnectedStream(reason: String) -> AsyncStream<TransportEvent> {
        AsyncStream { continuation in
            continuation.yield(.disconnected(reason: reason))
            continuation.finish()
        }
    }

    // MARK: - Control-command timeout budgets

    /// The one-shot `capture` exchange (#360). The `ControlCommandClient` default: the daemon answers a
    /// capture off its run loop, so there is no lock to wait behind.
    static let captureTimeout: Duration = .seconds(2)

    /// The one-shot `swap` exchange (#169). The LARGEST budget, and the only one whose value is derived
    /// rather than chosen: a `swap` ack is written only AFTER the swap runs, and the swap may wait on the
    /// cross-process single-writer lock for up to `SWAP_LOCK_MAX_WAIT` (`src/swap.rs`) before failing
    /// closed. A budget at or below that bound would time out a swap that is merely QUEUED and about to
    /// succeed — reporting a false failure for a write that then commits. This clears the lock's own bound
    /// with headroom for the keychain read/write beneath it. `AppLaunchPlanTests` re-reads the Rust
    /// constant from source and fails if this stops clearing it.
    static let swapTimeout: Duration = .seconds(15)

    /// The one-shot `stats` query (#446/#356) — a bounded READ answered off the daemon's run loop (no
    /// lock, unlike `swap`), so a modest budget clears a slower store aggregation.
    static let statsTimeout: Duration = .seconds(5)

    /// The one-shot `config-get` / `config-set` exchanges (#268) — `config-set` validates and atomically
    /// writes `config.toml` off the run loop, so this clears a slower disk write without the swap path's
    /// lock headroom.
    static let configTimeout: Duration = .seconds(5)
}
