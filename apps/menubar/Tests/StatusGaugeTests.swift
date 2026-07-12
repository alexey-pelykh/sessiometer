// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Pure tests for the status-item gauge (issue #325): the SHAPE-encoded template `NSImage` set the
// menu-bar `NSStatusItem` renders. They pin the load-bearing gauge contract WITHOUT any AppKit UI (no
// `NSStatusItem`, no window) so they run headless under `xcodebuild test`, exactly like the honest-
// state / store suites:
//
//   * SHAPE encodes state — every `StatusGlyph` maps to a DISTINCT symbol (shape is the only channel a
//     monochrome template image has; two states sharing a silhouette would be indistinguishable).
//   * TEMPLATE — every gauge image is `isTemplate`, so it re-tints in light AND dark menu bars (AC).
//   * RESOLVES — every symbol actually loads on this macOS (a typo'd / unavailable name would ship a
//     blank menu bar; pinning it here turns that into a red test and guards the macOS-13 floor).
//   * PROVIDER-NEUTRAL — the symbols are generic geometric shapes, not any provider's mark (AC).

import AppKit
import XCTest

final class StatusGaugeTests: XCTestCase {

    /// Every glance state the store can emit — the domain the gauge must total over.
    private let allGlyphs: [StatusGlyph] =
        [.connecting, .healthy, .empty, .stale, .disconnected, .unsupported, .crashLooping]

    // MARK: - AC: shape (not color) encodes state → one distinct silhouette per glyph

    func testEveryGlyphMapsToADistinctSymbol() {
        let names = allGlyphs.map(StatusGauge.symbolName(for:))
        XCTAssertEqual(Set(names).count, allGlyphs.count,
                       "each state needs its own shape — a template image has no color channel")
    }

    // MARK: - AC: tints correctly in light AND dark menu bars → template images

    func testEveryGaugeImageIsATemplate() {
        for glyph in allGlyphs {
            XCTAssertTrue(StatusGauge.image(for: glyph).isTemplate,
                          "\(glyph) gauge must be a template so the menu bar can re-tint it")
        }
    }

    // MARK: - The symbols actually resolve on this macOS (guards the macOS-13 floor + typos)

    func testEverySymbolResolvesToARealImage() {
        for glyph in allGlyphs {
            let name = StatusGauge.symbolName(for: glyph)
            XCTAssertNotNil(NSImage(systemSymbolName: name, accessibilityDescription: nil),
                            "SF Symbol '\(name)' for \(glyph) does not resolve on this macOS")
        }
    }

    // MARK: - AC: no provider-specific artwork → generic geometric symbols only

    func testSymbolsAreProviderNeutralGeometry() {
        // The gauge is a generic circle-family shape set; assert every symbol is one of that neutral
        // vocabulary. A provider mark (a brand glyph / logo symbol) would not match — this fails if a
        // future edit swaps in anything but neutral geometry.
        let neutralPrefixes = ["circle", "exclamationmark"]
        for glyph in allGlyphs {
            let name = StatusGauge.symbolName(for: glyph)
            XCTAssertTrue(neutralPrefixes.contains { name == $0 || name.hasPrefix($0 + ".") },
                          "gauge symbol '\(name)' for \(glyph) is not provider-neutral geometry")
        }
    }

    // MARK: - Healthy is the ONE full/solid shape; the states not to confuse with it differ

    func testHealthyIsTheSolidShapeAndDegradedStatesDiffer() {
        // The one healthy glyph is the filled disc; connecting (forming) and disconnected (slashed) —
        // the states most important not to mistake for healthy — are distinct silhouettes.
        XCTAssertEqual(StatusGauge.symbolName(for: .healthy), "circle.fill")
        XCTAssertNotEqual(StatusGauge.symbolName(for: .healthy),
                          StatusGauge.symbolName(for: .connecting))
        XCTAssertNotEqual(StatusGauge.symbolName(for: .healthy),
                          StatusGauge.symbolName(for: .disconnected))
    }

    // MARK: - Crash-looping (#169): a distinct fault TRIANGLE, never confused with the circle family

    func testCrashLoopingIsADistinctFaultShape() {
        let crash = StatusGauge.symbolName(for: .crashLooping)
        XCTAssertEqual(crash, "exclamationmark.triangle")
        // Distinct from healthy, and from `.unsupported` — the other marked/degraded shape.
        XCTAssertNotEqual(crash, StatusGauge.symbolName(for: .healthy))
        XCTAssertNotEqual(crash, StatusGauge.symbolName(for: .unsupported),
                          "crash-looping (triangle) must not read as version-skew (circle)")
        XCTAssertEqual(StatusGauge.accessibilityDescription(for: .crashLooping), "crash-looping")
    }
}
