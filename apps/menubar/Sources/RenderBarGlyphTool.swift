// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Offscreen bar-glyph reference renderer — TOOLING ONLY, compiled in DEBUG. Invoked as
// `Sessiometer.app/Contents/MacOS/Sessiometer --render-bar-glyphs <dir>` (see `AppDelegate`): it renders
// every menu-bar status-item glyph, in every tint context and scale, to `bar-<glyph>-<context>@<scale>x.png`
// and exits WITHOUT starting the menu-bar app. The committed set under `design/renders/bar-glyphs/` is these
// files; `BarGlyphParityTests` re-renders and diffs the fresh output against them so the set stays green as
// the mark / geometry evolves (issue #525).
//
// This is the bar-glyph peer of `RenderPanelTool` (the panel renderer, #380/#440), and exists for the same
// reason plus one more: the panel needs a bitmap because its `NSPopover` view can't be opened/screen-
// captured headless; the bar glyph additionally needs a bitmap because `NSStatusItem` TEMPLATE TINTING —
// the whole subject here — is applied by the system and is invisible to SwiftUI `ImageRenderer`. It runs
// inside the real app process so `Bundle.main` carries the compiled asset catalog and the glyphs resolve
// the same way the live status item resolves them (`StatusGauge.image(for:)`).
//
// The renders carry NO account data (they are bare glyphs), so unlike the panel captures they are safe to
// commit — see `design/README.md`.

#if DEBUG
import AppKit

@MainActor
enum RenderBarGlyphTool {
    /// Render every (glyph, context, scale) cell into `outputDir`. Any failure is written to stderr; the
    /// caller (`AppDelegate`) exits after this returns.
    static func run(outputDir: String) {
        var wrote = 0
        for glyph in StatusGlyph.allCases {
            // The SAME resolution + configuration the live status item uses (`Bundle.main` = this app).
            let image = StatusGauge.image(for: glyph)
            for context in BarGlyphContext.allCases {
                for scale in BarGlyphRenderer.scales {
                    let rep = BarGlyphRenderer.render(image, context: context, scale: scale)
                    let name = BarGlyphRenderer.referenceName(glyph: glyph, context: context, scale: scale)
                    guard let png = BarGlyphRenderer.pngData(rep) else {
                        FileHandle.standardError.write(Data("PNG encode failed: \(name)\n".utf8))
                        continue
                    }
                    let path = outputDir + "/" + name
                    do {
                        try png.write(to: URL(fileURLWithPath: path))
                        FileHandle.standardOutput.write(Data("wrote \(path) (\(rep.pixelsWide)x\(rep.pixelsHigh))\n".utf8))
                        wrote += 1
                    } catch {
                        FileHandle.standardError.write(Data("write failed \(path): \(error)\n".utf8))
                    }
                }
            }
        }
        FileHandle.standardOutput.write(Data("rendered \(wrote) bar-glyph reference(s)\n".utf8))
    }
}
#endif
