// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Offscreen render-PARITY renderer for the menu-bar status-item glyphs (issue #525) — TOOLING, compiled
// in DEBUG. The panel harness (`RenderPanelTool`, #380/#440) covers the `StatusPanelView`; it CANNOT cover
// the bar glyph, because the bar glyph is an AppKit template `NSImage` the SYSTEM re-tints, and SwiftUI
// `ImageRenderer` draws a SwiftUI view — it never exercises the tinting/inversion. This renderer drives
// AppKit directly so the thing under test is the TEMPLATE TINT itself.
//
// Three load-bearing subtleties, each learned the hard way (they are WHY the code is shaped like this;
// a naive rewrite reintroduces a silently-wrong render — the exact baseline trap #437 warned of):
//
//   1. THE 13 pt TRAP. A custom symbol resolves at the 13 pt system default (~13×12 pt) unless configured.
//      We size it through `StatusGauge.image(for:in:)`, the SAME `SymbolConfiguration(pointSize: 22)` the
//      live status item uses — so the parity render is the shipped glyph, not a half-size proxy.
//
//   2. THE TRANSPARENT-ACCENT TRAP. `NSColor.controlAccentColor` resolves to *transparent* in a process
//      with no running `NSApplication` (the logic-test bundle) — the menu-open background would vanish and
//      the white-on-accent glyph would be invisible-on-transparent. The app PINS its accent to the
//      `AccentColor` asset (#391), so the faithful AND deterministic background is that asset's literal
//      value, `#007AFF`, used explicitly here. Deterministic in both the app tool and the test bundle.
//
//   3. THE COPY-COMPOSITE TRAP. `NSBitmapImageRep.draw(in:)` composites with `.copy` — it REPLACES the
//      destination (alpha included), so drawing the tinted glyph straight onto the filled background wipes
//      the background to transparent wherever the glyph is transparent. We composite the tinted layer as an
//      `NSImage` with an explicit `.sourceOver` instead, which preserves the background.
//
// Template tinting is a TWO-LAYER operation: (a) draw the template glyph onto a TRANSPARENT canvas and
// `.sourceAtop`-fill the tint, giving a glyph whose alpha is its coverage and whose colour is the tint;
// (b) fill the opaque bar background, then `.sourceOver`-composite the tinted glyph over it. Filling the
// background FIRST and then `.sourceAtop`-tinting would flood the whole (now-opaque) box with the tint.

#if DEBUG
import AppKit

/// The three tint contexts the menu-bar status item is actually drawn in — the surface `ImageRenderer`
/// misses. `light`/`dark` are the resting bar in each appearance (the system tints the template with
/// `labelColor`, which resolves near-black in aqua and near-white in dark aqua — that per-appearance
/// resolution IS the thing under test). `menuOpen` is the inverted highlight while the panel is open:
/// `selectedMenuItemTextColor` (white) over the accent.
enum BarGlyphContext: String, CaseIterable {
    case light
    case dark
    case menuOpen

    /// The appearance whose semantic colours the render resolves under. `menuOpen` resolves under aqua: the
    /// inverted state is captured ONCE (not per appearance) because the glyph ink under test is white and
    /// survives either appearance's accent — the accent VALUE does differ (light #007AFF / dark #0A84FF),
    /// but the background is a representative surface, not an exact-hue target (see `background`).
    var appearance: NSAppearance {
        switch self {
        case .dark:              return NSAppearance(named: .darkAqua)!
        case .light, .menuOpen:  return NSAppearance(named: .aqua)!
        }
    }

    /// The template tint the system applies. `labelColor` for the resting bar (per-appearance, trap-free);
    /// `selectedMenuItemTextColor` (white) for the inverted highlight.
    var tint: NSColor {
        switch self {
        case .light, .dark: return .labelColor
        case .menuOpen:     return .selectedMenuItemTextColor
        }
    }

    /// The representative bar background the tinted glyph is composited over. The real menu bar is a
    /// translucent vibrancy material over the wallpaper (not a solid) — these solids are representative,
    /// directional backgrounds chosen so the render is committable, eyeball-able, and faithful to the
    /// contrast the glyph must survive (dark ink on a light bar, light ink on a dark bar, white on accent).
    /// The `menuOpen` accent is the light-appearance value of the app's PINNED `AccentColor` (#007AFF, #391)
    /// as an explicit sRGB colour — see the transparent-accent trap above. One accent value suffices: the
    /// inverted glyph ink is white, legible over either appearance's accent, so a dark-accent cell would add
    /// no distinctness the gate needs.
    var background: NSColor {
        switch self {
        case .light:    return NSColor(white: 0.96, alpha: 1)
        case .dark:     return NSColor(white: 0.15, alpha: 1)
        case .menuOpen: return NSColor(srgbRed: 0, green: 122.0 / 255.0, blue: 1, alpha: 1)
        }
    }
}

/// A pure `NSImage → tinted raster` factory (a caseless namespace, like `StatusGauge`). It takes the glyph
/// image as input rather than resolving it, so it is testable with any image (the real symbol, or a
/// synthetic canary) and needs no asset catalog itself. Compiled into BOTH the app (for `RenderBarGlyphTool`)
/// and the test bundle (for `BarGlyphParityTests`), so the committed reference renders and the drift-gate's
/// fresh renders come out of the identical code.
enum BarGlyphRenderer {

    /// The @1x and @2x device scales. @2x is a genuinely higher-resolution raster (the symbol is vector),
    /// never an upscale of @1x — the explicit pixel size below is what makes that real.
    static let scales = [1, 2]

    /// The square draw grid, in points — the `.symbolset`'s own 24-unit authoring grid, so the configured
    /// glyph (~22×21 pt at `StatusGauge`'s 22 pt bar size) is centred within it without clipping the
    /// arrowhead or scaling its aspect. INVARIANT: this must exceed the configured glyph size; `centeredRect`
    /// produces a negative origin (clipping the glyph off the top/right) if `StatusGauge.barPointSize` is
    /// ever bumped past ~24 — bump this grid in lockstep if so.
    static let gridPoints: CGFloat = 24

    /// Render `glyph` in `context` at `scale`. The bitmap is constructed at an EXPLICIT pixel size
    /// (`gridPoints × scale`) with its point-size set to `gridPoints`, so the graphics context maps 1 pt →
    /// `scale` px and @1x (24 px) vs @2x (48 px) are real, distinct native rasters.
    static func render(_ glyph: NSImage, context: BarGlyphContext, scale: Int) -> NSBitmapImageRep {
        var result: NSBitmapImageRep!
        context.appearance.performAsCurrentDrawingAppearance {
            let full = NSRect(x: 0, y: 0, width: gridPoints, height: gridPoints)
            let dst = centeredRect(for: glyph)

            // Layer 1 — tinted glyph on a TRANSPARENT canvas: alpha becomes the glyph's coverage, colour
            // becomes the tint (`.sourceAtop` paints the tint only where the template drew).
            let tinted = newRep(scale: scale)
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: tinted)
            glyph.draw(in: dst)
            context.tint.set()
            full.fill(using: .sourceAtop)
            NSGraphicsContext.restoreGraphicsState()

            // Layer 2 — opaque bar background, then the tinted glyph composited over it. The tinted layer
            // is wrapped in an `NSImage` and drawn with an explicit `.sourceOver`: `NSBitmapImageRep.draw`
            // is `.copy` and would wipe the background (trap #3 above).
            let tintedImage = NSImage(size: NSSize(width: gridPoints, height: gridPoints))
            tintedImage.addRepresentation(tinted)
            let composite = newRep(scale: scale)
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: composite)
            context.background.setFill()
            full.fill()
            tintedImage.draw(in: full, from: .zero, operation: .sourceOver, fraction: 1.0)
            NSGraphicsContext.restoreGraphicsState()
            result = composite
        }
        return result
    }

    /// PNG bytes for a rendered rep — the committed-reference and temp-capture encoding.
    static func pngData(_ rep: NSBitmapImageRep) -> Data? {
        rep.representation(using: .png, properties: [:])
    }

    /// The drift / distinctness metric: the FRACTION of pixels whose largest single-channel difference
    /// exceeds `channelThreshold`. Chosen over whole-raster mean-error because it is localized-change
    /// sensitive and antialiasing-tolerant: a real geometry change (an interior mark moves or swaps) flips
    /// a CLUSTER of pixels fully (bar↔ink, Δ≈0.7 ≫ threshold); cross-machine antialiasing only nudges edge
    /// pixels by small amounts (Δ ≪ threshold) and is not counted. Calibrated across all contexts × scales
    /// (issue #525): identical renders score 0.000; the CLOSEST real pair (healthy vs no-runway, @1x dark —
    /// both a diagonal stroke) scores 0.0226; a full interior-mark swap up to ~0.06; the canary ~0.11. The
    /// gates in `BarGlyphParityTests` use this metric three ways — a same-run distinctness floor (0.01, below
    /// the 0.0226 real minimum, catching a 0.000 blob collapse), a relative cross-machine-immune nearest-
    /// reference identity gate (the primary drift catcher), and a loose absolute gross-drift ceiling (0.05,
    /// above cross-machine antialiasing) — see that file's header for why the split.
    static func diffFraction(_ a: NSBitmapImageRep, _ b: NSBitmapImageRep,
                             channelThreshold: CGFloat = 0.25) -> Double {
        guard a.pixelsWide == b.pixelsWide, a.pixelsHigh == b.pixelsHigh, a.pixelsWide > 0 else { return 1 }
        var differing = 0
        for y in 0..<a.pixelsHigh {
            for x in 0..<a.pixelsWide {
                guard let p = a.colorAt(x: x, y: y), let q = b.colorAt(x: x, y: y) else { continue }
                let d = max(abs(p.redComponent - q.redComponent),
                            abs(p.greenComponent - q.greenComponent),
                            abs(p.blueComponent - q.blueComponent),
                            abs(p.alphaComponent - q.alphaComponent))
                if d > channelThreshold { differing += 1 }
            }
        }
        return Double(differing) / Double(a.pixelsWide * a.pixelsHigh)
    }

    /// The fraction of pixels carrying glyph ink — coverage that departs from the background corner pixel.
    /// A non-blank glyph lands well inside (0, 1); 0 means nothing drew (a blank/broken render), ~1 means a
    /// solid fill (the glyph mushed into a blob). Used by the parity gate's non-blank assertion.
    static func inkCoverage(_ rep: NSBitmapImageRep) -> Double {
        guard rep.pixelsWide > 0, let bg = rep.colorAt(x: 0, y: 0) else { return 0 }
        var ink = 0
        for y in 0..<rep.pixelsHigh {
            for x in 0..<rep.pixelsWide {
                guard let c = rep.colorAt(x: x, y: y) else { continue }
                let d = abs(c.redComponent - bg.redComponent)
                      + abs(c.greenComponent - bg.greenComponent)
                      + abs(c.blueComponent - bg.blueComponent)
                if d > 0.15 { ink += 1 }
            }
        }
        return Double(ink) / Double(rep.pixelsWide * rep.pixelsHigh)
    }

    /// The stable filename token for a glyph in a committed reference name (`bar-<glyph>-<context>@<scale>x.png`).
    static func fileToken(for glyph: StatusGlyph) -> String {
        switch glyph {
        case .healthy:    return "healthy"
        case .connecting: return "connecting"
        case .attention:  return "attention"
        case .noRunway:   return "norunway"
        }
    }

    /// The reference filename for one (glyph, context, scale) cell.
    static func referenceName(glyph: StatusGlyph, context: BarGlyphContext, scale: Int) -> String {
        "bar-\(fileToken(for: glyph))-\(context.rawValue)@\(scale)x.png"
    }

    // A transparent RGBA bitmap `gridPoints × scale` pixels wide, whose POINT size is `gridPoints` — so the
    // drawing context scales 1 pt → `scale` px (a real @Nx raster, not an upscale).
    private static func newRep(scale: Int) -> NSBitmapImageRep {
        let px = Int(gridPoints) * scale
        let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px,
                                   bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                   colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        rep.size = NSSize(width: gridPoints, height: gridPoints)
        return rep
    }

    // The glyph's native (configured) size centred within the 24-pt grid — no scaling, no aspect change.
    private static func centeredRect(for glyph: NSImage) -> NSRect {
        let size = glyph.size
        return NSRect(x: (gridPoints - size.width) / 2, y: (gridPoints - size.height) / 2,
                      width: size.width, height: size.height)
    }
}
#endif
