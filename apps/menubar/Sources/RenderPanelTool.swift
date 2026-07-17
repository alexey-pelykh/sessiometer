// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Offscreen design-parity renderer — TOOLING ONLY, compiled in DEBUG. Invoked as
// `Sessiometer.app/Contents/MacOS/Sessiometer --render-panel <dir>` (see `AppDelegate`): it renders
// `StatusPanelView` for the mock's "Healthy · Status" fixture to `panel-healthy-{light,dark}.png` via
// SwiftUI `ImageRenderer`, then exits WITHOUT starting the menu-bar app.
//
// Why it exists: the panel is an `NSPopover`-hosted view that can't be opened programmatically and
// can't be screen-captured without Screen-Recording TCC, so design-parity against the canonical mock
// (`apps/menubar/design/menubar-preview.html`) had no self-service path. `ImageRenderer` draws the view
// straight to a bitmap — no popover, no screen capture, no permission — giving a committable render to
// diff against the mock. It seeds a `WatchStatusStore.preview` (no transport), so it renders the SAME
// `@Published` state the panel reads, only pinned rather than machine-derived.

#if DEBUG
import AppKit
import SwiftUI

@MainActor
enum RenderPanelTool {
    /// A named panel state to render, so one run emits the whole set the panel supports for a
    /// screen-by-screen diff against the mock's `.pop` states.
    private struct Fixture {
        let name: String
        let state: ConnectionState
        let rows: [AccountRow]
        let nextSwap: NextSwap?
        let generatedAt: Int64?
    }

    /// Render every panel-supported state (light + dark) into `outputDir` as `panel-<state>-<theme>.png`.
    /// Any failure is written to stderr; the caller (`AppDelegate`) exits after this returns.
    static func run(outputDir: String) {
        let now = Int64(Date().timeIntervalSince1970)
        let day: Int64 = 86_400

        // The mock's "Healthy · Status" example rows (same labels + percents, so the render is directly
        // comparable): Work active 42/88, Personal 31/71, Scratch 4/18 — next swap → Scratch. The
        // provider secondary line (#173) and the "Last swap …" footer (#88) are the documented Wave-1
        // reconciliations and correctly do NOT appear.
        let rows = [
            AccountRow(label: "Work", isActive: true, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 42, weeklyPct: 88,
                       sessionResetsAt: now + 2 * 3600 + 14 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil),
            AccountRow(label: "Personal", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 31, weeklyPct: 71,
                       sessionResetsAt: now + 3600 + 2 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil),
            AccountRow(label: "Scratch", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 4, weeklyPct: 18,
                       sessionResetsAt: now + 5 * 3600 + 20 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: true, blindActive: nil),
        ]

        // The active-account bounded-blindness rosters (#479/#485) — the ACTIVE "Work" row carries a
        // `blind_active` projection (its live meters are replaced by the SEMANTIC held-state block); the
        // siblings stay healthy. These give the mock's blind frames (`menubar-preview.html`, #571) a matching
        // built-panel capture, so the design-vs-capture harness can cover the blind row. The whole-snapshot
        // stays `.connected` — blindness is a per-row modifier, NOT a 10th daemon-state, and the header +
        // footer stay fresh (the locality that distinguishes it from a whole-snapshot `stale`, #137).
        // Only `blind.lastKnownSessionPct` drives the render (the held bar) — while blind, BOTH live meters
        // are replaced by the held block, so the row's own `sessionPct` / `weeklyPct` are inert. `sessionPct`
        // mirrors the blind anchor (so a non-blind read of the row agrees with the held bar instead of
        // contradicting it); `weeklyPct` stays at the healthy-Work value.
        func blindWork(_ blind: BlindActive) -> AccountRow {
            AccountRow(label: "Work", isActive: true, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: blind.lastKnownSessionPct,
                       weeklyPct: 88, sessionResetsAt: now + 2 * 3600 + 14 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: blind)
        }
        // OK: last-known session 58% (green band), blind 3m, auto-protection self-resolving.
        let blindOKRows = [blindWork(BlindActive(blindSecs: 180, lastKnownSessionPct: 58,
                                                 autoProtectionDegraded: false)), rows[1], rows[2]]
        // DEGRADED: last-known session 88% (amber band), blind 11m, auto-protection acting on a stale anchor
        // → orange eye-slash + orange leading rule + orange verdict.
        let blindDegradedRows = [blindWork(BlindActive(blindSecs: 660, lastKnownSessionPct: 88,
                                                       autoProtectionDegraded: true)), rows[1], rows[2]]

        // The panel-rendered states (the fuller 9-state fidelity's remaining facets are #169 siblings).
        // `stale` and `disconnected` retain the last-good roster (disconnected dims it); the account-less
        // states — including `crashLooping` (#169), which refuses the held snapshot's numbers behind an
        // honest message card — show a banner / onboarding card. Ages chosen so the footer reads live /
        // stale as intended.
        let fixtures = [
            Fixture(name: "healthy", state: .connected, rows: rows,
                    nextSwap: .target(to: "Scratch", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 12),
            Fixture(name: "stale", state: .stale, rows: rows,
                    nextSwap: .target(to: "Scratch", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 5400),
            Fixture(name: "disconnected", state: .disconnected(reason: "the daemon is not responding"),
                    rows: rows, nextSwap: nil, generatedAt: now - 240),
            Fixture(name: "connecting", state: .connecting, rows: [], nextSwap: nil, generatedAt: nil),
            // #499: the cold-refused daemon-absent states (no reading ever held) — a forming card for
            // starting, and the not-running card whose Start-daemon button degrades to an inert line (#170).
            Fixture(name: "starting", state: .starting, rows: [], nextSwap: nil, generatedAt: nil),
            Fixture(name: "not-running", state: .notRunning, rows: [], nextSwap: nil, generatedAt: nil),
            Fixture(name: "crash-looping", state: .crashLooping, rows: [], nextSwap: nil, generatedAt: nil),
            Fixture(name: "unsupported", state: .unsupported, rows: [], nextSwap: nil, generatedAt: nil),
            Fixture(name: "empty-roster", state: .emptyRoster, rows: [], nextSwap: nil, generatedAt: nil),
            // #571: the active-account blind row, OK + DEGRADED — a per-row modifier on a `.connected`
            // snapshot (fresh header/footer), rendered as the held session bar + auto-protection verdict.
            Fixture(name: "blind-ok", state: .connected, rows: blindOKRows,
                    nextSwap: .target(to: "Scratch", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 12),
            Fixture(name: "blind-degraded", state: .connected, rows: blindDegradedRows,
                    nextSwap: .target(to: "Scratch", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 12),
        ]

        for fixture in fixtures {
            let store = WatchStatusStore.preview(state: fixture.state, rows: fixture.rows,
                                                 nextSwap: fixture.nextSwap, generatedAt: fixture.generatedAt)
            for scheme in [ColorScheme.light, .dark] {
                let theme = scheme == .light ? "light" : "dark"
                let name = "panel-\(fixture.name)-\(theme).png"
                // Inject the COMPLETE panel environment via the shared `statusPanelEnvironment` modifier — the
                // SAME wiring `StatusItemController` uses for the live app, so the harness and the app cannot
                // drift and every `@EnvironmentObject` the panel reads is resolved instead of trapping (issue
                // #504: missing `PanelStatsModel` here was exactly that drift). All three take a NIL client, so
                // each renders its resting, socket-free surface:
                //   • `AccountCaptureModel` renders at `.idle` with `captureSurfaceRequested == false`, so the
                //     populated fixtures show the roster with NO capture bar (capture is off-panel / empty-
                //     roster only now, #394) and the empty-roster fixture shows the onboarding card. The nil
                //     client renders the idle field/button and never touches a socket — the label field itself
                //     stays a known ImageRenderer blank (see design/README.md).
                //   • `AccountSwapModel` renders at `.idle`, so the fixtures capture the RESTING row (no hover,
                //     no pending). As of #448 the per-row switch chip is PERSISTENT, so its resting glyph
                //     (`arrow.left.arrow.right`, or the `nosign` on a non-viable row) IS captured in a static
                //     render; only the ARMED hover/focus brighten and the in-flight `Switching…` spinner stay a
                //     manual-check surface (#380).
                //   • `PanelStatsModel` (#446) renders at its default `.status` tab / `.idle` phase, so every
                //     fixture shows the Status glance (never a not-yet-loaded Stats tab) and the nil client
                //     never fires a socket-bound `stats` query.
                let view = StatusPanelView()
                    .statusPanelEnvironment(store: store,
                                            capture: AccountCaptureModel(client: nil),
                                            swap: AccountSwapModel(client: nil),
                                            stats: PanelStatsModel(client: nil))
                    .environment(\.colorScheme, scheme)
                let renderer = ImageRenderer(content: view)
                renderer.scale = 2
                guard let cg = renderer.cgImage else {
                    FileHandle.standardError.write(Data("render failed: \(name)\n".utf8))
                    continue
                }
                write(cg, to: outputDir + "/" + name)
            }
        }
    }

    private static func write(_ cg: CGImage, to path: String) {
        let rep = NSBitmapImageRep(cgImage: cg)
        guard let png = rep.representation(using: .png, properties: [:]) else {
            FileHandle.standardError.write(Data("PNG encode failed: \(path)\n".utf8))
            return
        }
        do {
            try png.write(to: URL(fileURLWithPath: path))
            FileHandle.standardOutput.write(Data("wrote \(path) (\(cg.width)x\(cg.height))\n".utf8))
        } catch {
            FileHandle.standardError.write(Data("write failed \(path): \(error)\n".utf8))
        }
    }
}
#endif
