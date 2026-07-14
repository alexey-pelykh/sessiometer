// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The provider-neutral status-item gauge (issue #325): the SHAPE-encoded template `NSImage` SET the
// menu-bar `NSStatusItem` renders — one image per `StatusGlyph`. Split out as a PURE, self-contained
// factory (no `NSStatusItem`, no store, no timing) so the whole shape-per-state contract is unit-
// testable headless — the functional-core split `HonestStateMachine` is to `WatchStatusStore`.
//
// WHY SHAPE, NOT COLOR (the load-bearing constraint): a menu-bar image is a TEMPLATE image — macOS
// re-tints it to match the bar (dark glyph on a light bar, light glyph on a dark bar, accented while
// the menu is open). A template is therefore MONOCHROME by definition, so COLOR cannot carry health —
// the SHAPE must. Every glyph here is a DISTINCT silhouette (`design-menubar`: "SF Symbol template
// image — shape, not color"). Because the images are `isTemplate = true`, they tint correctly in
// light AND dark menu bars for free.
//
// BESPOKE artwork (issue #437): the four shapes are now the Sessiometer "Cycle Gauge" mark redrawn at
// bar size — a custom SF Symbol `.symbolset` per state, NOT a generic system symbol (and NOT any
// provider's mark: our own gauge, satisfying #173). Each glyph is a SHARED CHASSIS (the open gauge arc +
// rotation arrowhead from `brand/src/icon.svg`, untransformed; the thin needle + pivot dot are DROPPED —
// they vanish at 16 pt) plus ONE bold interior mark that carries the state: a low check `✓` (healthy), a
// three-dot ellipsis `…` (connecting), an exclamation `!` (attention), a slash `⊘` (no-runway). The
// symbolsets are authored + emitted by `brand/generate.sh` (the asset SSOT — never hand-edit the
// generated files) into `Assets.xcassets`, and loaded here by name via `NSImage(named:)`.
//
// KNOWN + ACCEPTED (issue #437): because the chassis is shared, the four glyphs differ only in the small
// interior mark, so they are close in silhouette at bar size — the shared chassis owns most of the ink.
// Whether that is legible enough is an ON-DEVICE, real-`NSStatusItem` question (light + dark, Increase
// Contrast, over a bright wallpaper, beside system icons) that a raster proxy cannot settle; #437's
// PRIORITY-1 falsifier is exactly that on-device shape-distinctness check. This file ships the faithful
// locked artwork; the definitive distinctness verdict is captured separately (see the debug glyph gallery
// in `main.swift`, `SESSIOMETER_GLYPH_GALLERY`).

import AppKit

/// The status-item gauge image set — a pure `StatusGlyph → NSImage` factory. A caseless `enum` (a
/// namespace of pure functions, like `SocketPathResolver`), so there is nothing to instantiate.
enum StatusGauge {

    /// The bespoke custom-symbol ASSET NAME whose SHAPE encodes each attention state (issue #437/#524) — one
    /// DISTINCT silhouette per glyph so the state is legible from shape alone under monochrome template
    /// tinting. Each is the shared Cycle-Gauge chassis plus one bold interior mark:
    ///
    ///   * `.healthy`    → `GaugeHealthy`    — chassis + a low check `✓`: alive ∧ fresh, ignore me
    ///   * `.connecting` → `GaugeConnecting` — chassis + a three-dot ellipsis `…`: can't vouch yet, self-resolving
    ///   * `.attention`  → `GaugeAttention`  — chassis + an exclamation `!`: act at your next break
    ///   * `.noRunway`   → `GaugeNoRunway`   — chassis + a slash `⊘`: the tool can't keep you working, act now
    ///
    /// These name custom `.symbolset`s in `Assets.xcassets`, emitted by `brand/generate.sh` from the 24-grid
    /// master (never hand-edited). The app's macOS 13 floor clears custom-symbol availability (needs 11+), so
    /// no PNG/PDF fallback ships — an unresolved name is the broken-environment `fallbackRing` path below.
    /// Pure and total — every `StatusGlyph` maps (checked exhaustively in tests).
    static func assetName(for glyph: StatusGlyph) -> String {
        switch glyph {
        case .healthy:    return "GaugeHealthy"
        case .connecting: return "GaugeConnecting"
        case .attention:  return "GaugeAttention"
        case .noRunway:   return "GaugeNoRunway"
        }
    }

    /// The bar-size symbol configuration. A custom symbol carries no intrinsic pixel size — it is scaled
    /// from the `Capline`/`Baseline` guides against a POINT SIZE, and an unconfigured `NSImage(named:)`
    /// renders at the 13 pt system default, which put ~8 px of ink in a 24 px bar (half the intended
    /// glyph, every interior mark mushed away). The gauge fills its cap band, so the point size that
    /// lands ~16 px of ink is larger than a typical symbol's: measured on-device, NOT guessed.
    private static let barPointSize: CGFloat = 22

    /// The template gauge image for a glance state: the bespoke custom symbol scaled to bar size and
    /// forced to a template (`isTemplate = true`) so macOS re-tints it correctly in light AND dark menu
    /// bars — a NAMED image is NOT template-tinted by default, so we set it explicitly (issue #437).
    /// Carries an `accessibilityDescription` for the icon layer; the full spoken status sentence is set
    /// separately on the status-item button (`PresentationState.accessibilityLabel`).
    ///
    /// Defensive fallback: if the custom symbol somehow does not resolve — it will, the `.symbolset`s ship
    /// in the app's `Assets.xcassets` (name↔asset match pinned by a unit test) — draw a generic ring rather
    /// than hand the status bar a `nil` image (a blank menu-bar item is a worse failure than a plain shape).
    /// The primary path is always the bespoke symbol.
    ///
    /// `bundle`: the asset catalog to resolve the custom symbol from. The app passes `nil` (the default) →
    /// `NSImage(named:)`, which searches `Bundle.main` (the running app, whose compiled catalog carries the
    /// symbols) — the production path, byte-identical to before this parameter existed. The render-parity
    /// gate (issue #525, `BarGlyphParityTests`) passes its OWN test bundle so the SAME configuration path
    /// (point size, `isTemplate`) resolves the real symbols in the standalone logic-test bundle, whose
    /// `Bundle.main` is the xctest runner (not the catalog); `NSImage(named:)` there would swallow every
    /// symbol into the `fallbackRing`. `imageForResource:` reads the catalog compiled into the given bundle.
    static func image(for glyph: StatusGlyph, in bundle: Bundle? = nil) -> NSImage {
        let description = accessibilityDescription(for: glyph)
        let name = assetName(for: glyph)
        let resolved: NSImage?
        if let bundle {
            resolved = bundle.image(forResource: NSImage.Name(name))
        } else {
            resolved = NSImage(named: name)
        }
        if let symbol = resolved {
            let configuration = NSImage.SymbolConfiguration(pointSize: barPointSize, weight: .regular)
            let sized = symbol.withSymbolConfiguration(configuration) ?? symbol
            sized.isTemplate = true
            sized.accessibilityDescription = description
            return sized
        }
        return fallbackRing(accessibilityDescription: description)
    }

    /// A terse icon-layer description naming the ATTENTION state (issue #524) — VoiceOver reads the
    /// button's full per-input label (`PresentationState.accessibilityLabel`), so this labels only the
    /// image itself for tooling that inspects it. Provider-neutral by construction.
    static func accessibilityDescription(for glyph: StatusGlyph) -> String {
        switch glyph {
        case .healthy:    return "healthy"
        case .connecting: return "connecting"
        case .attention:  return "attention"
        case .noRunway:   return "no runway"
        }
    }

    /// Last-resort template glyph if an SF Symbol fails to resolve — a stroked ring at menu-bar size.
    /// Generic (it does NOT shape-encode): reaching here means a broken symbol environment, so the goal
    /// is only "never blank the menu bar", not per-state legibility (the symbol path owns that).
    private static func fallbackRing(accessibilityDescription: String) -> NSImage {
        let side: CGFloat = 16
        let image = NSImage(size: NSSize(width: side, height: side), flipped: false) { rect in
            let ring = NSBezierPath(ovalIn: rect.insetBy(dx: 2, dy: 2))
            ring.lineWidth = 1.5
            NSColor.black.setStroke()   // template image → the menu bar re-tints this
            ring.stroke()
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = accessibilityDescription
        return image
    }
}
