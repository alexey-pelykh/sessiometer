// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Pure tests for the status-item gauge (issue #325, re-mapped to the #524 attention axis, and now the
// #437 BESPOKE artwork): the SHAPE-encoded template `NSImage` set the menu-bar `NSStatusItem` renders.
// They pin the load-bearing gauge contract WITHOUT any AppKit UI (no `NSStatusItem`, no window) so they
// run headless under `xcodebuild test`, exactly like the honest-state / store suites:
//
//   * SHAPE encodes state — every `StatusGlyph` maps to a DISTINCT asset (shape is the only channel a
//     monochrome template image has; two states sharing a silhouette would be indistinguishable).
//   * TEMPLATE — every gauge image is `isTemplate`, so it re-tints in light AND dark menu bars (AC).
//   * ASSET EXISTS — every asset name has a real `.symbolset` in Assets.xcassets, so a name typo (which
//     `NSImage(named:)` would silently swallow into the fallbackRing at runtime) reddens here.
//   * PROVIDER-NEUTRAL — the glyphs are OUR own bespoke Cycle-Gauge mark, not any provider's (#173).
//
// The artwork is now the bespoke chassis + interior-mark family (#437): a custom `.symbolset` per state,
// emitted by `brand/generate.sh`. These tests assert the STATE contract (distinct, template, asset-exists,
// neutral) and the glyph→asset mapping. What they DELIBERATELY do NOT assert is on-device shape
// DISTINCTNESS at bar size — #437's PRIORITY-1 falsifier — which needs a real `NSStatusItem` render a
// headless raster proxy cannot settle (that is the `SESSIOMETER_GLYPH_GALLERY` harness in `main.swift`,
// captured by the orchestrator). Automated bar-glyph render-parity is a separate item (#525).
//
// Note on the standalone logic-test bundle: it compiles the pure `StatusGauge` source but NOT the app's
// compiled asset catalog, and `NSImage(named:)` searches `Bundle.main` (here, the xctest runner), so the
// named custom symbol does NOT resolve in-process — `image(for:)` exercises the `fallbackRing` path in
// this bundle. The primary named-symbol path is exercised by the app build (actool compiles the
// `.symbolset`s) and on-device (the gallery). The asset-exists test below bridges the gap by checking the
// source-tree `.symbolset`s directly, so assetName↔catalog drift is still caught in CI.

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

    // MARK: - AC: shape (not color) encodes state → one distinct asset per glyph

    func testEveryGlyphMapsToADistinctAsset() {
        let names = allGlyphs.map(StatusGauge.assetName(for:))
        XCTAssertEqual(Set(names).count, allGlyphs.count,
                       "each state needs its own shape — a template image has no color channel")
    }

    // MARK: - AC: tints correctly in light AND dark menu bars → template images

    func testEveryGaugeImageIsATemplate() {
        // In this standalone bundle the named symbol does not resolve (no catalog in the xctest main
        // bundle), so this exercises the `fallbackRing` path — which is ALSO a template, so the "always a
        // template" contract holds on both paths. The named-symbol path's `isTemplate = true` is exercised
        // by the app build + on-device (the gallery).
        for glyph in allGlyphs {
            XCTAssertTrue(StatusGauge.image(for: glyph).isTemplate,
                          "\(glyph) gauge must be a template so the menu bar can re-tint it")
        }
    }

    // MARK: - Every asset name has a real .symbolset (guards assetName↔catalog drift)

    func testEveryGlyphAssetHasASymbolset() {
        // The catalog is not compiled into this logic-test bundle, so assert against the SOURCE tree,
        // located from this file (`#filePath`) — CI checks the tree out at the same path it compiled from.
        // A drifted `assetName` (a typo `NSImage(named:)` would swallow into the fallbackRing at runtime)
        // reddens here instead of shipping a blank-shaped menu bar.
        let assets = URL(fileURLWithPath: #filePath)          // .../apps/menubar/Tests/StatusGaugeTests.swift
            .deletingLastPathComponent()                       // .../apps/menubar/Tests
            .deletingLastPathComponent()                       // .../apps/menubar
            .appendingPathComponent("Sources/Assets.xcassets")
        let fm = FileManager.default
        for glyph in allGlyphs {
            let name = StatusGauge.assetName(for: glyph)
            let symbolset = assets.appendingPathComponent("\(name).symbolset")
            var isDir: ObjCBool = false
            XCTAssertTrue(fm.fileExists(atPath: symbolset.path, isDirectory: &isDir) && isDir.boolValue,
                          "\(glyph) → asset '\(name)' has no .symbolset at \(symbolset.path) — assetName drifted from Assets.xcassets")
            XCTAssertTrue(fm.fileExists(atPath: symbolset.appendingPathComponent("Contents.json").path),
                          "\(name).symbolset is missing Contents.json")
            let svgs = (try? fm.contentsOfDirectory(atPath: symbolset.path))?.filter { $0.hasSuffix(".svg") } ?? []
            XCTAssertFalse(svgs.isEmpty, "\(name).symbolset ships no .svg glyph")
        }
    }

    // MARK: - AC: no provider-specific artwork → our own bespoke Cycle-Gauge family

    func testAssetsAreTheBespokeGaugeFamily() {
        // Provider-neutral by construction (#173): every glyph is OUR own Cycle-Gauge mark — a custom
        // `.symbolset` in Assets.xcassets — never a provider's brand mark and (now) not even a generic
        // system SF Symbol. This fails if a future edit points a glyph outside the bespoke Gauge family.
        for glyph in allGlyphs {
            let name = StatusGauge.assetName(for: glyph)
            XCTAssertTrue(name.hasPrefix("Gauge"),
                          "gauge asset '\(name)' for \(glyph) is not part of the bespoke Cycle-Gauge family")
        }
    }

    // MARK: - Each glyph maps to its bespoke asset (chassis + ratified interior mark ✓ / … / ! / ⊘)

    // Pins the glyph→asset mapping so a stray edit that swaps an asset reddens here. The interior mark each
    // asset carries is authored in `brand/generate.sh`; this pins the Swift side of the contract.
    func testEachGlyphMapsToItsBespokeAsset() {
        XCTAssertEqual(StatusGauge.assetName(for: .healthy), "GaugeHealthy", "Healthy → chassis + low check ✓")
        XCTAssertEqual(StatusGauge.assetName(for: .connecting), "GaugeConnecting", "Connecting → chassis + ellipsis …")
        XCTAssertEqual(StatusGauge.assetName(for: .attention), "GaugeAttention", "Attention → chassis + exclamation !")
        XCTAssertEqual(StatusGauge.assetName(for: .noRunway), "GaugeNoRunway", "No-runway → chassis + slash ⊘")
    }

    // MARK: - The load-bearing pairs must never share a silhouette

    // The states most consequential not to confuse: Healthy (ignore me) vs No-runway (act now) — the two
    // poles of the fleet verdict, and the ✓/⊘ diagonal-stroke pair the design record flags — and Healthy
    // vs Attention (a fault). A shared shape on any of these would be a glance-surface honesty failure.
    func testTheConsequentialPairsAreDistinctShapes() {
        let healthy = StatusGauge.assetName(for: .healthy)
        XCTAssertNotEqual(healthy, StatusGauge.assetName(for: .noRunway),
                          "Healthy and No-runway are opposite verdicts — they must not share a shape")
        XCTAssertNotEqual(healthy, StatusGauge.assetName(for: .attention),
                          "Healthy and Attention must not share a shape")
        XCTAssertNotEqual(healthy, StatusGauge.assetName(for: .connecting),
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
