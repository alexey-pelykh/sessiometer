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
// D4 (functional placeholder): the shapes are generic SF Symbols — a coherent circle-gauge family, one
// per attention state — NOT any provider's mark (AC: "a generic gauge, not any provider's mark"). Each
// placeholder is chosen to ECHO its ratified interior mark (✓ / … / ! / ⊘) so it telegraphs the eventual
// bespoke design; the bespoke arc+arrowhead `.symbolset` is issue #437, which replaces these — this item
// (#524) owns only WHICH states exist, not the artwork.

import AppKit

/// The status-item gauge image set — a pure `StatusGlyph → NSImage` factory. A caseless `enum` (a
/// namespace of pure functions, like `SocketPathResolver`), so there is nothing to instantiate.
enum StatusGauge {

    /// The SF Symbol whose SHAPE encodes each attention state (issue #524) — one DISTINCT silhouette per
    /// glyph so the state is legible from shape alone under monochrome template tinting. Each placeholder
    /// echoes the ratified interior mark it stands in for:
    ///
    ///   * `.healthy`    → `checkmark.circle`      — a low check `✓` in a ring: alive ∧ fresh, ignore me
    ///   * `.connecting` → `ellipsis.circle`       — an ellipsis `…` in a ring: can't vouch yet, self-resolving
    ///   * `.attention`  → `exclamationmark.circle` — an exclamation `!` in a ring: act at your next break
    ///   * `.noRunway`   → `nosign`                 — a slash `⊘`: the tool can't keep you working, act now
    ///
    /// All are generic geometric system symbols (provider-neutral) shipped since macOS 11, so they resolve
    /// on the app's macOS 13 floor. Pure and total — every `StatusGlyph` maps (checked exhaustively in
    /// tests). `.noRunway`'s complete slashed ring (`nosign`) is the boldest, most unambiguous "blocked"
    /// silhouette and the one least confusable with the `.healthy` check at ~16 pt (the ✓/⊘ diagonal-stroke
    /// collision the design record flags for the bespoke chassis — a #437 on-device falsifier).
    static func symbolName(for glyph: StatusGlyph) -> String {
        switch glyph {
        case .healthy:    return "checkmark.circle"
        case .connecting: return "ellipsis.circle"
        case .attention:  return "exclamationmark.circle"
        case .noRunway:   return "nosign"
        }
    }

    /// The template gauge image for a glance state: a system-tinted (`isTemplate`) SF Symbol, so it
    /// reads correctly in light AND dark menu bars. Carries an `accessibilityDescription` for the icon
    /// layer; the full spoken status sentence is set separately on the status-item button
    /// (`PresentationState.accessibilityLabel`), updated on every state change.
    ///
    /// Defensive fallback: if a symbol somehow does not resolve (it will, on macOS 13+ — pinned by a
    /// unit test), draw a generic ring rather than hand the status bar a `nil` image (a blank menu-bar
    /// item is a worse failure than a plain shape). The primary path is always the SF Symbol.
    static func image(for glyph: StatusGlyph) -> NSImage {
        let description = accessibilityDescription(for: glyph)
        if let symbol = NSImage(systemSymbolName: symbolName(for: glyph),
                                accessibilityDescription: description) {
            symbol.isTemplate = true
            return symbol
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
