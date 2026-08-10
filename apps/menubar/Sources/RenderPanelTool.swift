// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Offscreen design-parity renderer — TOOLING ONLY, compiled in DEBUG. Invoked as
// `Sessiometer.app/Contents/MacOS/Sessiometer --render-panel <dir>` (see `AppDelegate`): it renders every
// panel state the mock covers, light + dark, to `panel-<state>-<theme>.png` via SwiftUI `ImageRenderer`,
// then exits WITHOUT starting the menu-bar app.
//
// Why it exists: the panel is an `NSPopover`-hosted view that can't be opened programmatically and
// can't be screen-captured without Screen-Recording TCC, so design-parity against the canonical mock
// (`apps/menubar/design/menubar-preview.html`) had no self-service path. `ImageRenderer` draws the view
// straight to a bitmap — no popover, no screen capture, no permission — giving a committable render to
// diff against the mock. It seeds a `WatchStatusStore.preview` (no transport), so it renders the SAME
// `@Published` state the panel reads, only pinned rather than machine-derived.
//
// WHAT THIS FILE IS, since #754: the app-target ENTRY POINT only — argument-shaped `run(outputDir:)` plus
// PNG encoding. The fixture catalog and the render call itself moved to `PanelRenderHarness`, shared with
// the in-bundle golden gate (`Tests/PanelGoldenParityTests.swift`) so the automated gate and this human
// oracle can never render different states or a differently-configured view. See that file's header for
// the #504 drift precedent that motivates the sharing.
//
// The two consumers stay distinct in PURPOSE: this one writes PNGs a human eyeballs beside the mock
// (`design/build-comparison.py`); the gate re-renders the same fixtures in-process and diffs them against
// committed goldens. Neither replaces the other — a golden gate certifies "unchanged since the last
// blessed render", never "matches the mock".

#if DEBUG
import AppKit
import SwiftUI

@MainActor
enum RenderPanelTool {
    /// Render every panel-supported state (light + dark) into `outputDir` as `panel-<state>-<theme>.png`.
    /// Any failure is written to stderr; the caller (`AppDelegate`) exits after this returns.
    static func run(outputDir: String) {
        let now = Int64(Date().timeIntervalSince1970)
        for fixture in PanelRenderHarness.fixtures(now: now) {
            for scheme in PanelRenderHarness.themes {
                let name = PanelRenderHarness.fileName(fixture: fixture.name, scheme: scheme)
                guard let cg = PanelRenderHarness.render(fixture, scheme: scheme) else {
                    FileHandle.standardError.write(Data("render failed: \(name)\n".utf8))
                    continue
                }
                write(cg, to: outputDir + "/" + name)
            }
        }
        // The steady-state guarantee, reported rather than assumed (issue #824). `PanelRenderHarness.render`
        // returns a raster it has CONFIRMED reproducible — unless its budget valve fires first, in which case
        // it hands back an unconfirmed one instead of trapping, because this harness also compiles into the
        // shipping app. The in-bundle gate would catch that eventually through its own byte assertions;
        // NOTHING here would. These PNGs are read by a human as the design oracle, so an unconfirmed raster
        // has to announce itself or it is invisible.
        let unsettled = PanelRenderHarness.unsettledRenders
        if unsettled > 0 {
            let warning = "WARNING: \(unsettled) render(s) exhausted the settle budget, so the matching "
                + "PNG(s) may hold cold, non-reproducible pixels. Re-run and compare before treating this "
                + "output as a design oracle or blessing anything from it.\n"
            FileHandle.standardError.write(Data(warning.utf8))
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
