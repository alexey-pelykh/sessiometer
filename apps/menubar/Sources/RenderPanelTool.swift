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
    /// Render the healthy-Status panel (light + dark) into `outputDir`. Any failure is written to
    /// stderr; the caller (`AppDelegate`) exits after this returns.
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
                       weeklyExhausted: false, isNextSwapTarget: false),
            AccountRow(label: "Personal", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 31, weeklyPct: 71,
                       sessionResetsAt: now + 3600 + 2 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: false),
            AccountRow(label: "Scratch", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 4, weeklyPct: 18,
                       sessionResetsAt: now + 5 * 3600 + 20 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: true),
        ]
        let store = WatchStatusStore.preview(state: .connected, rows: rows,
                                             nextSwap: .target(to: "Scratch"), generatedAt: now - 12)

        for scheme in [ColorScheme.light, .dark] {
            let name = scheme == .light ? "panel-healthy-light.png" : "panel-healthy-dark.png"
            let view = StatusPanelView()
                .environmentObject(store)
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
