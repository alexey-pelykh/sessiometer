// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Pure tests for the status-item gauge (issue #325, re-mapped to the #524 attention axis): the SHAPE-
// encoded template `NSImage` set the menu-bar `NSStatusItem` renders. They pin the load-bearing gauge
// contract WITHOUT any AppKit UI (no `NSStatusItem`, no window) so they run headless under `xcodebuild
// test`, exactly like the honest-state / store suites:
//
//   * SHAPE encodes state — every `StatusGlyph` maps to a DISTINCT symbol (shape is the only channel a
//     monochrome template image has; two states sharing a silhouette would be indistinguishable).
//   * TEMPLATE — every gauge image is `isTemplate`, so it re-tints in light AND dark menu bars (AC).
//   * RESOLVES — every symbol actually loads on this macOS (a typo'd / unavailable name would ship a
//     blank menu bar; pinning it here turns that into a red test and guards the macOS-13 floor).
//   * PROVIDER-NEUTRAL — the symbols are generic geometric shapes, not any provider's mark (AC).
//
// The domain is now the 4-state attention axis (#524), not the pre-#524 nine connection glyphs. The
// placeholder SF Symbols each ECHO their ratified interior mark (✓ / … / ! / ⊘); the bespoke arc-chassis
// artwork is #437, which replaces them — so these tests assert the STATE contract (distinct, template,
// resolves, neutral) and the placeholder→mark mapping, not the eventual bespoke silhouettes.

import AppKit
import XCTest

final class StatusGaugeTests: XCTestCase {

    /// Every attention state the gauge must total over — the whole locked family (`CaseIterable`, so a
    /// future 5th state, if the operator ever ratifies one, auto-joins this coverage).
    private let allGlyphs = StatusGlyph.allCases

    // MARK: - The domain is exactly the ratified 4-state attention axis (#524)

    func testTheGlyphSetIsExactlyTheFourAttentionStates() {
        XCTAssertEqual(Set(allGlyphs), Set<StatusGlyph>([.healthy, .connecting, .attention, .noRunway]),
                       "the locked family is exactly 4 (#524) — a 5th needs operator escalation, not a code add")
    }

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
        // The placeholder gauge is generic system geometry (a ring + interior mark, or the universal
        // prohibition sign) — NOT any provider's brand mark. This fails if a future edit swaps in anything
        // outside that neutral vocabulary. `nosign` is the universal ⊘ prohibition glyph (generic system
        // geometry, like `power` was for the pre-#524 not-running), so it joins the vocabulary.
        let neutralPrefixes = ["checkmark", "ellipsis", "exclamationmark", "nosign"]
        for glyph in allGlyphs {
            let name = StatusGauge.symbolName(for: glyph)
            XCTAssertTrue(neutralPrefixes.contains { name == $0 || name.hasPrefix($0 + ".") },
                          "gauge symbol '\(name)' for \(glyph) is not provider-neutral geometry")
        }
    }

    // MARK: - Each placeholder echoes its ratified interior mark (✓ / … / ! / ⊘)

    // Pins the placeholder→ratified-mark mapping so a #437 hand-off starts from the intended shapes, and a
    // stray edit that swaps a mark reddens here. (These specific SF Symbol names are the D4 placeholder;
    // #437 replaces them with the bespoke arc+arrowhead `.symbolset` — the STATE contract above is what
    // survives that swap, this test is the placeholder pin.)
    func testEachPlaceholderEchoesItsRatifiedMark() {
        XCTAssertEqual(StatusGauge.symbolName(for: .healthy), "checkmark.circle", "Healthy → low check ✓")
        XCTAssertEqual(StatusGauge.symbolName(for: .connecting), "ellipsis.circle", "Connecting → ellipsis …")
        XCTAssertEqual(StatusGauge.symbolName(for: .attention), "exclamationmark.circle", "Attention → exclamation !")
        XCTAssertEqual(StatusGauge.symbolName(for: .noRunway), "nosign", "No-runway → slash ⊘")
    }

    // MARK: - The load-bearing pairs must never share a silhouette

    // The states most consequential not to confuse: Healthy (ignore me) vs No-runway (act now) — the two
    // poles of the fleet verdict, and the ✓/⊘ diagonal-stroke pair the design record flags — and Healthy
    // vs Attention (a fault). A shared shape on any of these would be a glance-surface honesty failure.
    func testTheConsequentialPairsAreDistinctShapes() {
        let healthy = StatusGauge.symbolName(for: .healthy)
        XCTAssertNotEqual(healthy, StatusGauge.symbolName(for: .noRunway),
                          "Healthy and No-runway are opposite verdicts — they must not share a shape")
        XCTAssertNotEqual(healthy, StatusGauge.symbolName(for: .attention),
                          "Healthy and Attention must not share a shape")
        XCTAssertNotEqual(healthy, StatusGauge.symbolName(for: .connecting),
                          "Healthy and Connecting must not share a shape")
    }

    // MARK: - The icon-layer a11y description names the attention state

    func testAccessibilityDescriptionsNameTheAttentionState() {
        XCTAssertEqual(StatusGauge.accessibilityDescription(for: .healthy), "healthy")
        XCTAssertEqual(StatusGauge.accessibilityDescription(for: .connecting), "connecting")
        XCTAssertEqual(StatusGauge.accessibilityDescription(for: .attention), "attention")
        XCTAssertEqual(StatusGauge.accessibilityDescription(for: .noRunway), "no runway")
    }
}
