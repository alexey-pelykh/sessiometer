// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The text-measurement primitives every layout gate in this bundle drives its assertions — AND its
// CONSTRAINT-A canary — through.
//
// WHY THEY LIVE HERE. Issue #750 built these inside `PanelTextMetricsTests` as private methods, which was
// right when there was one metrics suite. Issue #762 adds a second (`SettingsTextMetricsTests`) for the
// Settings window, and the one thing a second suite must NOT do is re-derive the predicate: two copies of
// `overflows` can disagree at a boundary, and then a canary proves one gate falsifiable while the other
// silently cannot fail. So the predicate moved here and BOTH suites call it. `PanelTextMetricsTests` keeps
// its own thin private forwards so its assertions read unchanged; nothing about what it measures moved.
//
// WHAT THESE ARE, precisely. They model SwiftUI/AppKit's layout using the SAME CoreText primitives those
// frameworks shape and truncate through — `NSAttributedString` advance widths, `CTLineCreateTruncatedLine`
// (which is what `.truncationMode` selects), and `CTFramesetter` line-breaking (which is what an
// unconstrained multi-line `Text` performs). They are a MODEL of the layout, not an observation of a live
// view tree. That distinction is stated here once so neither suite has to over-claim it, and because the
// neighbouring `BarGlyphParityTests` header had to be corrected once (issue #749) for asserting an
// unexecuted belief as settled fact.
//
// HEADLESS by construction: text shaping needs no window, no screen and no TCC grant, which is why these
// run inside the standalone `TEST_HOST: ""` bundle in the required `swift` CI job.

#if DEBUG
import AppKit
import CoreText
import XCTest

enum TextMetrics {

    // MARK: - Advance width

    /// Required width in points for `text` rendered in `font`, on ONE line.
    ///
    /// Agrees with `CTLineGetTypographicBounds` to the last bit across every fixture in both suites
    /// (measured delta 0.0000) — `NSString.size` for a single-line attributed run is the same typographic
    /// advance — so a test may branch on this width and assert a `visibleSubstring` verdict without the
    /// two disagreeing at a boundary.
    static func width(_ text: String, _ font: NSFont) -> Double {
        Double((text as NSString).size(withAttributes: [.font: font]).width)
    }

    /// THE predicate. `true` when `text` does not fit `budget` on one line.
    ///
    /// Every real assertion and every CONSTRAINT-A canary in this bundle runs through this one function,
    /// so the thing proven falsifiable is the thing actually gating.
    static func overflows(_ text: String, _ font: NSFont, budget: Double) -> Bool {
        width(text, font) > budget
    }

    // MARK: - Truncation (the `.truncationMode` model)

    private static let tokenKey = NSAttributedString.Key("sessiometer.truncationToken")

    /// The substring that remains VISIBLE when `text` is truncated to `budget` under `mode`, plus whether
    /// truncation happened at all.
    ///
    /// Ranges, not per-character indices: an emoji ZWJ sequence or a surrogate pair sliced by codepoint
    /// would come back as lone surrogates and make every comparison meaningless. Runs are collected by
    /// `CTRunGetStringRange` and re-sorted into LOGICAL order, so a bidi (RTL) label reconstructs to what
    /// it semantically contains rather than to its visual ordering. The ellipsis token carries a marker
    /// attribute purely so its run can be excluded — it belongs to the token string, not to `text`.
    static func visibleSubstring(_ text: String, font: NSFont, budget: Double,
                                 mode: CTLineTruncationType) -> (text: String, truncated: Bool) {
        let attrs: [NSAttributedString.Key: Any] = [.font: font]
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attrs))
        guard CTLineGetTypographicBounds(line, nil, nil, nil) > budget else { return (text, false) }

        var tokenAttrs = attrs
        tokenAttrs[tokenKey] = true
        let token = CTLineCreateWithAttributedString(
            NSAttributedString(string: "\u{2026}", attributes: tokenAttrs))
        guard let cut = CTLineCreateTruncatedLine(line, budget, mode, token) else { return (text, true) }

        let ns = text as NSString
        var ranges: [NSRange] = []
        for run in CTLineGetGlyphRuns(cut) as? [CTRun] ?? [] {
            if (CTRunGetAttributes(run) as NSDictionary)[tokenKey] != nil { continue }
            let r = CTRunGetStringRange(run)
            guard r.location >= 0, r.length > 0, r.location + r.length <= ns.length else { continue }
            ranges.append(NSRange(location: r.location, length: r.length))
        }
        ranges.sort { $0.location < $1.location }
        return (ranges.map { ns.substring(with: $0) }.joined(), true)
    }

    /// The CoreText truncation type the panel's own elision POLICY selects — so a suite elides under what
    /// the panel actually ships rather than under a hardcoded `.middle`.
    ///
    /// Shared for the same reason `overflows` is, and with one extra edge: a suite that re-derives this
    /// inline as a `policy == .middle ? .middle : .end` ternary silently routes a THIRD policy case to tail
    /// elision, while this exhaustive `switch` refuses to compile until someone decides what it should be.
    static func truncationType(for policy: StatusPanelFormat.IdentityElision) -> CTLineTruncationType {
        switch policy {
        case .middle: return .middle
        case .tail:   return .end
        }
    }

    // MARK: - Wrapping (the model for a `Text` with NO `.lineLimit`)

    /// How many LINES `text` occupies when wrapped to `budget`, and the height that costs.
    ///
    /// The counterpart to `visibleSubstring`, and the distinction is the whole point: a `Text` WITHOUT a
    /// `.lineLimit` does not truncate — it wraps, and its container grows. Measuring only width would
    /// report "it overflows" for both cases and hide which one is actually happening.
    ///
    /// `lines` comes from `CTFramesetter`'s own line-breaker (an exact count); `height` from AppKit's
    /// `boundingRect`, i.e. the same wrap metric a layout pass uses. `bounded` says whether the whole
    /// string fitted the (deliberately generous) 10 000 pt probe height — a `false` means the count is a
    /// floor, not a total, so a caller can never silently read a clipped count as a complete one.
    static func wrapped(_ text: String, _ font: NSFont, budget: Double)
        -> (lines: Int, height: Double, bounded: Bool) {
        let attributed = NSAttributedString(string: text, attributes: [.font: font])
        let probeHeight = 10_000.0
        let framesetter = CTFramesetterCreateWithAttributedString(attributed)
        let path = CGPath(rect: CGRect(x: 0, y: 0, width: budget, height: probeHeight), transform: nil)
        let frame = CTFramesetterCreateFrame(framesetter, CFRange(location: 0, length: 0), path, nil)
        let lines = (CTFrameGetLines(frame) as? [CTLine])?.count ?? 0

        let visible = CTFrameGetVisibleStringRange(frame)
        let bounded = visible.length == (text as NSString).length

        let box = attributed.boundingRect(
            with: CGSize(width: budget, height: probeHeight),
            options: [.usesLineFragmentOrigin, .usesFontLeading])
        return (lines, Double(box.height), bounded)
    }

    /// The height ONE line of `font` occupies — the baseline a wrapped height is judged against.
    static func singleLineHeight(_ font: NSFont) -> Double {
        Double(font.ascender - font.descender + font.leading)
    }
}

// MARK: - Shared assertion

extension XCTestCase {

    /// Assert one cell, reporting measured required-vs-available in points on failure (issue #748 R1: the
    /// suite fails *reporting* the numbers, not merely reporting that something is wrong).
    func assertFits(_ text: String, _ font: NSFont, budget: Double, _ what: String,
                    file: StaticString = #filePath, line: UInt = #line) {
        let required = TextMetrics.width(text, font)
        XCTAssertFalse(TextMetrics.overflows(text, font, budget: budget),
                       "\(what): \"\(text)\" needs \(String(format: "%.2f", required)) pt "
                       + "in a \(String(format: "%.2f", budget)) pt slot "
                       + "(over by \(String(format: "%.2f", required - budget)) pt)",
                       file: file, line: line)
    }
}
#endif
