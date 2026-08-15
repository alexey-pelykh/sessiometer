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
// re-model them; the measurement is in the commit body of `3d4271c` (the issue #764 change). This repo
// squash-merges, so the PR body it was written in reaches no clone, and the commit body does.

import Foundation

/// How this process was launched. The three tool modes are DEBUG-only design/parity instruments — they
/// render committable artefacts and then exit (or, for the gallery, stand up bare status items and wire
/// nothing else); `.toolFlagMissingDirectory` is the DEBUG-only REFUSAL that answers a tool flag supplied
/// without the directory it needs. A release build has exactly one mode, `.normal`.
enum AppLaunchMode: Equatable {
    /// `--render-panel <dir>` (#355/#754) — render the panel fixtures to PNGs for the design-parity
    /// oracle, then exit. Never wires the status item.
    case renderPanel(outputDirectory: String)
    /// `--render-bar-glyphs <dir>` (#525) — render every status-item glyph as it appears in the bar
    /// (template-tinted, per appearance, @1x + @2x, plus menu-open), then exit.
    case renderBarGlyphs(outputDirectory: String)
    /// A tool flag supplied as the LAST argument, with no output directory after it (#850) — there is
    /// nothing to render, so the app reports the mistake on stderr and exits non-zero instead of launching.
    case toolFlagMissingDirectory(flag: String)
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
    /// Precedence — `--render-panel`, then `--render-bar-glyphs`, then a tool flag MISSING its output
    /// directory, then the gallery environment variable, then normal — is the pre-extraction `AppDelegate`
    /// dispatch order with issue #850's refusal rung inserted. This function is the ONLY place it is
    /// expressed: the delegate switches on an already-resolved mode, so its case order says nothing. Order
    /// is observable when more than one is supplied, so it is pinned by test.
    ///
    /// A render flag needs a FOLLOWING argument to be a render mode. Supplied as the last argument it has
    /// none, and resolves to `.toolFlagMissingDirectory` — see `missingDirectoryFlag`.
    ///
    /// That rung sits BELOW both well-formed render modes and ABOVE the gallery, deliberately. A
    /// well-formed higher-precedence flag still wins, so `--render-panel p --render-bar-glyphs` renders the
    /// panel exactly as it always did and nothing that worked stops working. But an EXPLICIT malformed flag
    /// outranks the AMBIENT gallery opt-in: letting a `SESSIOMETER_GLYPH_GALLERY=1` inherited from a shell
    /// profile turn a mistyped `--render-panel` into a gallery launch is the same silent-wrong-mode
    /// dishonesty as turning it into a normal launch, and the gallery does not exit either — so a script
    /// waiting for a render would still hang.
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
        if let flag = missingDirectoryFlag(in: arguments) {
            return .toolFlagMissingDirectory(flag: flag)
        }
        if environment[glyphGalleryEnvironmentKey] == glyphGalleryEnabledValue {
            return .glyphGallery
        }
        return .normal
    }

    /// The tool flag supplied WITHOUT the output directory it needs, or `nil` when none was. `mode(...)`
    /// resolves this to `.toolFlagMissingDirectory`, which `main.swift` reports via
    /// `missingDirectoryMessage` before exiting `missingDirectoryExitCode` (issue #850). Until #850 it
    /// resolved to `.normal` instead, so a mistyped `--render-panel` silently launched the real app and
    /// installed a live status item rather than reporting the missing argument.
    ///
    /// It returns the FLAG rather than a bare yes/no so the report can name the one the operator actually
    /// mistyped. At most one flag can ever be in this state, so that answer is unambiguous: only the LAST
    /// argument satisfies `index + 1 >= count`, and there is exactly one last argument.
    static func missingDirectoryFlag(in arguments: [String]) -> String? {
        for flag in [renderPanelFlag, renderBarGlyphsFlag] {
            if let index = arguments.firstIndex(of: flag), index + 1 >= arguments.count { return flag }
        }
        return nil
    }

    /// The operator-facing sentence for a tool flag supplied without its output directory, written to
    /// stderr by `main.swift`. Like `degradeReason` it is a plain value so it can be asserted from the
    /// headless bundle — `main.swift` cannot be. It names the flag the operator actually mistyped and the
    /// shape it wants, and nothing they cannot act on: no path, no issue number, no internal vocabulary.
    /// The trailing newline belongs to the writer, not to the sentence.
    static func missingDirectoryMessage(flag: String) -> String {
        "\(flag): missing output directory (usage: \(flag) <dir>)"
    }

    /// The exit status for that refusal. Non-zero is what makes a script notice at all; `EX_USAGE`
    /// (sysexits.h) additionally says WHICH kind of failure, so a caller can tell a malformed invocation
    /// from a render that ran and failed.
    static let missingDirectoryExitCode: Int32 = EX_USAGE

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
