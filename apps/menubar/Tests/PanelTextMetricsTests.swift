// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Text-metrics layout gate for the status panel (issue #750; umbrella issue #748 R1 + R2).
//
// WHY THIS EXISTS. The panel implements a truncation POLICY — `.lineLimit(1)` + `.truncationMode(.middle)`
// on the roster label, the Stats handle, and the next-swap target — that is load-bearing for the issue #445
// identity-disambiguation kit: middle-elide so a same-local-part address's distinguishing DOMAIN survives,
// where tail-truncation hid exactly that part. Nothing tested it. The layout tests that did exist checked
// constants against constants (`AccountSwapTests` asserts `switchAffordanceSlotWidth == 28`); no text was
// ever laid out, so no cell's content was ever measured against the cell.
//
// WHY MEASUREMENT AND NOT A SCREENSHOT. A screenshot diff says "something changed" and needs a human to
// adjudicate; it also needs a baseline that rots. A metric says "`1000d23h` needs 55.32 pt in a 52 pt slot"
// — falsifiable, deterministic, no oracle, no golden, no windowserver, no TCC. The panel golden lane is a
// separate item (issue #754) and this is deliberately not it.
//
// WHAT A GREEN RUN PROVES, exactly: every string the shipped panel can reach FITS the cell it is laid out
// in, at the shipped frame constants, under this machine's system font — and where a string cannot fit, the
// truncation policy elides it rather than clipping it. What it does NOT prove: that SwiftUI's own layout
// pass produces these exact widths. This suite models the policy with the SAME CoreText primitive AppKit
// and SwiftUI truncate through (`CTLineCreateTruncatedLine`, and `kCTLineTruncationMiddle` is what
// `.truncationMode(.middle)` selects) over the SAME font descriptors the views declare — but it is a model
// of the policy, not an observation of the live view tree. Stating that plainly because the neighbouring
// `BarGlyphParityTests` header had to be corrected once (issue #749) for asserting an unexecuted belief as
// settled fact, and an over-claim here would be the same mistake in a new place.
//
// WHY THE CONSTANTS MOVED. Each budget below was a bare literal in a SwiftUI file that `MenubarTests`
// deliberately excludes (`project.yml`: this is a standalone logic bundle, `TEST_HOST: ""`, and every
// SwiftUI/AppKit source is out). Rather than pull the view graph into the headless bundle — which would
// contradict the bundle's stated architecture for the sake of reading a few numbers — the numbers moved
// DOWN into `StatusPanelFormat`, the Foundation-only layer that already holds `switchAffordanceSlotWidth`,
// `panelContentWidth` and `statsChartWidth` for exactly this reason. **No `project.yml` change was needed**,
// and none was made.
//
// Be precise about what that buys, because "there is no second copy" is only true of most of them. Eight
// are LINKED — the view lays out with that exact constant. Only three of those (the meter cells) are what
// this suite measures text against DIRECTLY; the other five reach the gate as inputs to `rosterLabelBudget`
// / `statsHandleBudget`, so a change to one moves the budget rather than an assertion. The remaining two
// are ALLOWANCES with no view site to link, because the element sizes to its own content and has no fixed
// frame: `authColumnAllowance` (60) and `statsSignalPillAllowance` (85). Each derived budget takes one of
// them, so each is good to about ±10 pt — which is why the issue #445 invariant is asserted across a RANGE
// and why the budget tests assert HEADROOM rather than an exact fit. `StatusPanelFormat`'s own § Text-cell
// layout budgets comment is where each constant is named as one kind or the other.
//
// CONSTRAINT-A (issue #748) — NO GATE WITHOUT A PROVEN FALSIFIER. A gate authored against broken output
// blesses the breakage and then DEFENDS it: issue #437's three render bugs were misread five times as "the
// DESIGN fails distinctness", and a golden authored then would have locked them in. So
// `testTheCanaryOverflowsWhileEveryShippedLabelFits` pushes a deliberately over-wide fixture through the
// SAME predicate the real assertions use and requires it to FAIL, in the same run that requires every
// shipped string to pass. A gate that cannot fail is not evidence.
//
// TWO MEASURED FACTS THAT CONTRADICT THE ISSUE'S OWN PREMISES — recorded here rather than quietly rounded,
// because the point of a measurement gate is to report what is true, not to confirm what was assumed:
//
//   1. Issue #750 AC-3 expects `365d23h` to OVERFLOW the 52 pt reset cell. It does not: it needs 48.32 pt
//      and fits, as does every three-digit day count (`999d23h`, same width — the digits are monospaced).
//      Overflow begins at FOUR digits (`1000d23h` = 55.32 pt). The gate is therefore placed at the measured
//      boundary rather than the assumed one, and `testExtremeResetDurationsAreMeasuredAgainstTheResetCell`
//      proves the cell CAN overflow using `Int64.max` (132.24 pt — a 154 % overrun), which is reachable
//      from a nonsense `resets_at` on the wire. Asserting a red at 365 days would have been a fabricated
//      failure; asserting nothing would have left the cell unguarded.
//
//   2. Issue #445's middle-truncation is not universally better than tail-truncation — it is better for the
//      case it was adopted for. Measured: for a DOMAIN-differing pair (`oleksii@company-one.com` /
//      `…-two.com`, the realistic fleet shape, ~170 pt) middle keeps them distinguishable at 90 and 120 pt
//      where tail collapses both onto one shared prefix (`oleksii@co` at 90 pt, `oleksii@compan` at 120).
//      For a pair differing at a MID-string character (`work@a.com` / `work@b.com`, ~81 pt) the ordering
//      REVERSES once elision actually starts — at 70 pt middle yields `worcom` for both while tail still
//      separates them (`work@a.` / `work@b.`) — because middle elides precisely where the difference lives.
//      Both are asserted, including the reversal.
//
//      The policy is NOT changed here, and the reversal is not a defect: at the shipped 171 pt budget the
//      AC's pair has better than 2× headroom and never elides at all, so in production the question does
//      not arise. What protects that pair is HEADROOM; what the elision POLICY protects is the long
//      domain-differing pair. The two tests are named for which of those they actually assert, because an
//      earlier draft conflated them and produced a policy "guard" that passed under `.tail` — recorded here
//      so the distinction is not re-lost.
//
// SCOPE. The cells this suite gates are the ones issue #750 enumerates: the three `UsageMeter` columns, the
// roster account label, and the Stats handle. The blind row's trailing `blind {dur}` chip is NOT among them
// and is not gated here — measuring it surfaced a real overflow, which per the issue's own "Out of scope:
// fixing any overflow this surfaces — findings get their own items" is filed as issue #781 rather than
// fixed or silently absorbed.

#if DEBUG
import AppKit
import CoreText
// `DynamicTypeSize` + `PanelTypeScale` — the AC-3 sweep (issue #756) reads the SAME factor the views
// lay out with, never a second copy of the curve.
import SwiftUI
import XCTest

final class PanelTextMetricsTests: XCTestCase {

    // MARK: - Fonts (each pinned to the view site whose `.font(...)` it mirrors)

    /// `Text(row.label).font(.body).fontWeight(.semibold)` — `StatusPanelRoster`'s account label.
    /// The size is READ from the text style rather than hardcoded to 13, so this tracks the platform's own
    /// body metric instead of asserting against a number that could quietly stop being what `.body` means.
    private let rosterLabelFont = NSFont.systemFont(
        ofSize: NSFont.preferredFont(forTextStyle: .body).pointSize, weight: .semibold)

    /// `.font(.system(size: 10, weight: .semibold))` — the `SESSION` / `WEEKLY` window-name cell.
    private let meterLabelFont = NSFont.systemFont(ofSize: 10, weight: .semibold)

    /// `.font(.system(size: 12, weight: .semibold)).monospacedDigit()` — the percent cell.
    private let meterPercentFont = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold)

    /// `.font(.system(size: 11)).monospacedDigit()` — the reset-in cell.
    private let meterResetFont = NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .regular)

    /// `.font(.system(size: 13, weight: .semibold))` — `StatStripRow`'s handle.
    private let statsHandleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)

    // MARK: - The gate predicate (one definition, used by every assertion AND by the canary)

    /// Required width in points for `text` rendered in `font`.
    ///
    /// `visibleSubstring` below decides whether to truncate via `CTLineGetTypographicBounds` instead, and
    /// several tests branch on THIS width then assert THAT verdict — so the two must not disagree at a
    /// boundary. Measured across every fixture in this file, they agree to the last bit (delta 0.0000):
    /// `NSString.size` for a single-line attributed run is the same typographic advance.
    private func width(_ text: String, _ font: NSFont) -> Double {
        Double((text as NSString).size(withAttributes: [.font: font]).width)
    }

    /// THE predicate. `true` when `text` does not fit `budget`. The CONSTRAINT-A canary runs through this
    /// same function so the thing proven falsifiable is the thing actually gating.
    private func overflows(_ text: String, _ font: NSFont, budget: Double) -> Bool {
        width(text, font) > budget
    }

    /// Assert one cell, reporting measured required-vs-available in points on failure (issue #748 R1: the
    /// suite fails *reporting* the numbers, not merely reporting that something is wrong).
    private func assertFits(_ text: String, _ font: NSFont, budget: Double, _ what: String,
                            file: StaticString = #filePath, line: UInt = #line) {
        let required = width(text, font)
        XCTAssertFalse(overflows(text, font, budget: budget),
                       "\(what): \"\(text)\" needs \(String(format: "%.2f", required)) pt "
                       + "in a \(String(format: "%.2f", budget)) pt slot "
                       + "(over by \(String(format: "%.2f", required - budget)) pt)",
                       file: file, line: line)
    }

    // MARK: - Truncation model (the same CoreText primitive SwiftUI's `.truncationMode` selects)

    private static let tokenKey = NSAttributedString.Key("sessiometer.truncationToken")

    /// The substring that remains VISIBLE when `text` is truncated to `budget` under `mode`, plus whether
    /// truncation happened at all.
    ///
    /// Ranges, not per-character indices: an emoji ZWJ sequence or a surrogate pair sliced by codepoint
    /// would come back as lone surrogates and make every comparison below meaningless. Runs are collected by
    /// `CTRunGetStringRange` and re-sorted into LOGICAL order, so a bidi (RTL) label reconstructs to what it
    /// semantically contains rather than to its visual ordering. The ellipsis token carries a marker
    /// attribute purely so its run can be excluded — it belongs to the token string, not to `text`.
    private func visibleSubstring(_ text: String, font: NSFont, budget: Double,
                                  mode: CTLineTruncationType) -> (text: String, truncated: Bool) {
        let attrs: [NSAttributedString.Key: Any] = [.font: font]
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attrs))
        guard CTLineGetTypographicBounds(line, nil, nil, nil) > budget else { return (text, false) }

        var tokenAttrs = attrs
        tokenAttrs[Self.tokenKey] = true
        let token = CTLineCreateWithAttributedString(
            NSAttributedString(string: "\u{2026}", attributes: tokenAttrs))
        guard let cut = CTLineCreateTruncatedLine(line, budget, mode, token) else { return (text, true) }

        let ns = text as NSString
        var ranges: [NSRange] = []
        for run in CTLineGetGlyphRuns(cut) as? [CTRun] ?? [] {
            if (CTRunGetAttributes(run) as NSDictionary)[Self.tokenKey] != nil { continue }
            let r = CTRunGetStringRange(run)
            guard r.location >= 0, r.length > 0, r.location + r.length <= ns.length else { continue }
            ranges.append(NSRange(location: r.location, length: r.length))
        }
        ranges.sort { $0.location < $1.location }
        return (ranges.map { ns.substring(with: $0) }.joined(), true)
    }

    /// The CoreText truncation type the panel's own elision POLICY selects. Everything below elides through
    /// this rather than through a hardcoded `.middle`, so these are assertions about the panel's policy, not
    /// about CoreText's repertoire.
    private func coreTextType(_ policy: StatusPanelFormat.IdentityElision) -> CTLineTruncationType {
        switch policy {
        case .middle: return .middle
        case .tail:   return .end
        }
    }

    /// Elide under the SHIPPED policy.
    ///
    /// This and the two mode-pinned siblings below are all fixed to `rosterLabelFont` — they exist for the
    /// issue #445 roster-label comparisons and nothing else. Measuring any OTHER cell goes through
    /// `visibleSubstring` directly with that cell's own font (as the Stats-handle test does); reaching for
    /// these would silently measure the wrong typeface.
    private func elided(_ text: String, budget: Double) -> String {
        visibleSubstring(text, font: rosterLabelFont, budget: budget,
                         mode: coreTextType(StatusPanelFormat.identityElision)).text
    }

    private func middle(_ text: String, budget: Double) -> String {
        visibleSubstring(text, font: rosterLabelFont, budget: budget, mode: .middle).text
    }

    private func tail(_ text: String, budget: Double) -> String {
        visibleSubstring(text, font: rosterLabelFont, budget: budget, mode: .end).text
    }

    // MARK: - The shipped elision POLICY is middle (issue #445's actual substance)

    // Without this, every truncation assertion below would be a fact about CoreText rather than about the
    // panel: a view rewritten to `.truncationMode(.tail)` would leave a suite that only ever exercised
    // `.middle` entirely green, and issue #445 would silently un-ship.
    //
    // RESIDUAL GAP, stated rather than papered over: this asserts the POLICY VALUE, and the views now read
    // it through `IdentityElision.truncationMode`. The two-case map itself lives in `StatusPanelSharedViews`
    // — a SwiftUI file this headless bundle cannot compile — so a mis-map there (`.middle` → `.tail`) is not
    // reachable from here. That map is kept to four lines beside its enum precisely so it is verifiable by
    // reading; closing it mechanically needs the panel golden lane (issue #754).
    func testTheShippedIdentityElisionPolicyIsMiddle() {
        XCTAssertEqual(StatusPanelFormat.identityElision, .middle,
                       "the panel no longer middle-elides identity labels — issue #445's "
                       + "same-local-part disambiguation is undone at every site that reads this policy")
    }

    // MARK: - The derived budgets match the shipped geometry (guards the arithmetic itself)

    // `rosterLabelBudget` and `statsHandleBudget` are DERIVED expressions, so a change to any panel-geometry
    // constant silently moves them — and every measurement below is relative to them. Pinning the arithmetic
    // means a geometry change reddens HERE, with the derivation named, instead of quietly re-scaling the
    // whole gate underneath itself.
    func testTheDerivedTextBudgetsMatchTheShippedPanelGeometry() {
        XCTAssertEqual(StatusPanelFormat.defaultRowWidth, 364, accuracy: 0.001,
                       "380 pt panel less the 8 pt roster inset per side")
        // 364 − 16 (row padding) − 8 (dot) − 30 (monogram) − 28 (swap slot) − 45 (5 × 9 pt gaps)
        //     − 6 (collapsed spacer) − 60 (auth allowance)
        XCTAssertEqual(StatusPanelFormat.rosterLabelBudget, 171, accuracy: 0.001,
                       "the roster label budget derivation changed — re-derive, do not re-tune the tests")
        // 380 − 16 (roster insets) − 16 (card padding) − 8 (dot) − 30 (monogram) − 27 (3 × 9 pt gaps)
        //     − 85 (signal-pill allowance)
        XCTAssertEqual(StatusPanelFormat.statsHandleBudget, 198, accuracy: 0.001,
                       "the Stats handle budget derivation changed — this is the ~198 pt issue #700 bought")
    }

    // MARK: - AC: every fixed meter cell fits its widest REACHABLE content

    func testEveryShippedMeterCellFitsItsWidestReachableContent() {
        var checked = 0

        // The window-name cell renders exactly two strings — the labels `AccountRowView` hands `UsageMeter`,
        // uppercased BY the view. The fixtures are therefore the view's INPUT, so the `.uppercased()` below
        // models that transform instead of restating its output.
        for label in ["Session", "Weekly"] {
            assertFits(label.uppercased(), meterLabelFont,
                       budget: StatusPanelFormat.meterLabelCellWidth, "meter label cell")
            checked += 1
        }

        // The percent cell's widest reachable content is `255%` — NOT `100%`. `WireModel` decodes
        // `session_pct` / `weekly_pct` as a bare `UInt8` with no clamp, so the ceiling is the type's, not
        // the domain's. (Same width as `100%` in practice: the digits are monospaced.)
        for pct in [UInt8?.none, .some(0), .some(100), .some(255)] {
            assertFits(StatusPanelFormat.pct(pct), meterPercentFont,
                       budget: StatusPanelFormat.meterPercentCellWidth, "meter percent cell")
            checked += 1
        }

        // The reset cell across every SHAPE `humanizeUntil` can emit, at each form's widest.
        for secs: Int64 in [0, 30, 59 * 60, 23 * 3600 + 59 * 60, 999 * 86_400 + 23 * 3600] {
            assertFits(StatusPanelFormat.humanizeUntil(secs), meterResetFont,
                       budget: StatusPanelFormat.meterResetCellWidth, "meter reset cell")
            checked += 1
        }

        // Degenerate-subject guard: a green is evidence only if it evaluated the whole planned set.
        XCTAssertEqual(checked, 2 + 4 + 5, "expected 11 cell measurements, ran \(checked)")
    }

    // MARK: - AC: pathological labels are MEASURED against the roster label budget

    /// The pathological classes issue #750 AC-1 enumerates. Shared by the budget test below and by the
    /// narrow-budget elision test, so the two can never cover different sets.
    private let pathologicalLabels: [(name: String, label: String)] = [
        ("40 characters",   String(repeating: "a", count: 40)),
        ("CJK",             "用户@例子公司.中国"),
        ("RTL Arabic",      "مستخدم@شركة.مصر"),
        ("RTL Hebrew",      "משתמש@חברה.co.il"),
        ("emoji + ZWJ",     "👨‍👩‍👧‍👦@family.example"),
        ("whitespace-only", "     "),
        ("empty",           ""),
    ]

    // Each class is measured and its verdict asserted both ways: what fits must render WHOLE, and what does
    // not must be ELIDED by the truncation policy rather than clipped. An overflowing label is not a
    // failure of the panel — it is what `.truncationMode(.middle)` exists for; the failure mode this
    // catches is a label that overflows and is NOT elided (clipping), or one that fits and is elided anyway.
    func testPathologicalLabelsAreMeasuredAgainstTheRosterLabelBudget() {
        let budget = StatusPanelFormat.rosterLabelBudget
        let cases = pathologicalLabels

        var checked = 0
        for (name, label) in cases {
            let required = width(label, rosterLabelFont)
            let result = visibleSubstring(label, font: rosterLabelFont, budget: budget,
                                          mode: coreTextType(StatusPanelFormat.identityElision))
            let report = "\(name): needs \(String(format: "%.2f", required)) pt of "
                       + "\(String(format: "%.2f", budget)) pt available"

            if required > budget {
                XCTAssertTrue(result.truncated,
                              "\(report) — it overflows but was NOT elided, so it clips (issue #748 R2)")
                XCTAssertFalse(result.text.isEmpty,
                               "\(report) — elided to nothing, which is uninformative truncation (R2)")
                XCTAssertNotEqual(result.text, label,
                                  "\(report) — reported truncated yet nothing was removed")
            } else {
                XCTAssertFalse(result.truncated, "\(report) — it fits, yet the policy elided it")
                XCTAssertEqual(result.text, label, "\(report) — it fits, so it must render whole")
            }
            checked += 1
        }
        XCTAssertEqual(checked, cases.count, "expected \(cases.count) label classes, ran \(checked)")
    }

    // Degenerate content must not be silently upgraded into something the operator could misread as an
    // identity. Empty stays empty; whitespace stays whitespace-width — neither may acquire glyphs.
    func testDegenerateLabelsMeasureAsDegenerateRatherThanAsContent() {
        XCTAssertEqual(width("", rosterLabelFont), 0, accuracy: 0.001,
                       "an empty label must measure zero — anything else means glyphs appeared from nowhere")
        XCTAssertGreaterThan(width("     ", rosterLabelFont), 0,
                             "whitespace occupies width; a zero here means the run was dropped, not rendered")
        XCTAssertLessThan(width("     ", rosterLabelFont), StatusPanelFormat.rosterLabelBudget,
                          "five spaces must not fill the label budget")
    }

    // At the shipped 171 pt budget only ONE of the seven classes actually overflows (40 × `a` = 296.33 pt);
    // CJK, both RTL scripts, the emoji ZWJ sequence, whitespace and empty all fit. That leaves the ELIDE
    // branch above — and with it the grapheme/bidi reassembly `visibleSubstring` performs — unexercised for
    // exactly the shaping-sensitive classes it exists to handle. This runs every class at a deliberately
    // narrow budget so that machinery is covered rather than merely present.
    //
    // The sharpest assertion here is the U+FFFD one: reconstructing the visible substring by CHARACTER index
    // rather than by run RANGE would slice the emoji's surrogate pairs and yield replacement characters. A
    // panel label containing `` is a rendering defect the width measurement alone cannot see.
    func testEveryPathologicalClassElidesCleanlyAtANarrowBudget() {
        let narrow = 40.0
        var elidedCount = 0

        for (name, label) in pathologicalLabels {
            let required = width(label, rosterLabelFont)
            let result = visibleSubstring(label, font: rosterLabelFont, budget: narrow,
                                          mode: coreTextType(StatusPanelFormat.identityElision))
            let report = "\(name) at a \(narrow) pt budget (needs \(String(format: "%.2f", required)) pt)"

            XCTAssertFalse(result.text.contains("\u{FFFD}"),
                           "\(report): the visible substring contains U+FFFD — a grapheme cluster was sliced "
                           + "mid-codepoint during reassembly")

            guard required > narrow else {
                XCTAssertFalse(result.truncated, "\(report): it fits, yet the policy elided it")
                continue
            }
            elidedCount += 1
            XCTAssertTrue(result.truncated, "\(report): it overflows but was not elided — it clips")
            XCTAssertFalse(result.text.isEmpty, "\(report): elided to nothing (issue #748 R2)")
            XCTAssertNotEqual(result.text, label, "\(report): reported truncated yet nothing was removed")
            XCTAssertLessThanOrEqual(width(result.text, rosterLabelFont), required,
                                     "\(report): the elided text is WIDER than the original")
        }

        // Degenerate-subject guard: this test is only evidence if the narrow budget actually forced elision
        // on the classes that carry shaping complexity — not just on the 40-`a` filler.
        XCTAssertGreaterThanOrEqual(elidedCount, 5,
                                    "only \(elidedCount) classes elided at \(narrow) pt — the narrow budget "
                                    + "is no longer narrow enough to exercise the elide path")
    }

    // MARK: - AC: the issue #445 invariant — a same-local-part pair stays distinguishable

    // The AC's own pair, at the shipped row width. It does not truncate there, so the invariant holds by
    // FITTING rather than by eliding — asserted explicitly, because "the suffix survived" would otherwise be
    // a pass that proves nothing about the truncation policy at all.
    func testSameLocalPartLabelsStayDistinguishableAtTheShippedRowWidth() {
        let budget = StatusPanelFormat.rosterLabelBudget
        let a = "work@a.com"
        let b = "work@b.com"

        for label in [a, b] {
            assertFits(label, rosterLabelFont, budget: budget, "roster label (issue #445 pair)")
        }
        let va = visibleSubstring(a, font: rosterLabelFont, budget: budget,
                                  mode: coreTextType(StatusPanelFormat.identityElision))
        let vb = visibleSubstring(b, font: rosterLabelFont, budget: budget,
                                  mode: coreTextType(StatusPanelFormat.identityElision))

        XCTAssertFalse(va.truncated, "\(a) does not need eliding at the shipped \(budget) pt budget")
        XCTAssertEqual(va.text, a, "an untruncated label must render whole")
        XCTAssertEqual(vb.text, b, "an untruncated label must render whole")
        XCTAssertNotEqual(va.text, vb.text,
                          "the two accounts render identically at the shipped row width — the operator "
                          + "cannot tell them apart (issue #445)")
    }

    // What actually protects the AC's pair is HEADROOM, not the elision policy — so that is what this
    // asserts, and it says so.
    //
    // The pair measures ~81 pt against a 171 pt budget, i.e. it never elides in production. An earlier
    // version of this test looped `XCTAssertNotEqual(elided(a), elided(b))` from 90 pt upward and looked
    // like a policy guard; it was not one — every iteration compared two whole strings, and it passed
    // unchanged under `identityElision = .tail`. A test that cannot fail under the inversion it purports to
    // guard is decoration. Replaced with the two things that are true and load-bearing:
    //
    //   1. across the whole plausible budget band the pair is rendered WHOLE (this reddens if the row's
    //      geometry ever squeezes the label down to the pair's own width — the real production risk), and
    //   2. the measured point below which middle-elision DOES collapse this pair, asserted rather than
    //      left implied, so nobody reads `.middle` as a guarantee that survives any budget.
    //
    // The policy itself is guarded by `testTheShippedIdentityElisionPolicyIsMiddle` and by the domain-pair
    // test below — which uses labels long enough to actually truncate, and does go red under `.tail`.
    func testTheIdentityPairHasRealHeadroomAndTheCollapsePointIsKnown() {
        let a = "work@a.com"
        let b = "work@b.com"
        let widest = max(width(a, rosterLabelFont), width(b, rosterLabelFont))

        // 1. Whole, not merely distinguishable, across the band the row could plausibly hand the label.
        for budget in stride(from: 90.0, through: 200.0, by: 10.0) {
            XCTAssertFalse(visibleSubstring(a, font: rosterLabelFont, budget: budget,
                                            mode: coreTextType(StatusPanelFormat.identityElision)).truncated,
                           "\(a) needs \(String(format: "%.2f", widest)) pt and was elided at a \(budget) pt "
                           + "budget — the row is squeezing an ordinary account label")
        }
        XCTAssertGreaterThan(StatusPanelFormat.rosterLabelBudget, widest * 1.5,
                             "the shipped label budget (\(StatusPanelFormat.rosterLabelBudget) pt) no longer "
                             + "clears an ordinary account label (\(String(format: "%.2f", widest)) pt) with "
                             + "50 % headroom — issue #445's pair is close to eliding in production")

        // 2. The known limit. Below the pair's own width, middle-elision removes precisely the character
        //    that distinguishes them (it is mid-string), so they DO collapse. This is not a defect — it is
        //    why the panel budgets 171 pt rather than 80 — but it must not be a surprise to a later reader.
        XCTAssertEqual(elided(a, budget: 70), elided(b, budget: 70),
                       "middle-elision no longer collapses this pair at 70 pt. That is a behaviour change in "
                       + "the elision policy; re-check the header's measured-facts note before adjusting.")
    }

    // WHY the policy is `.middle` — the domain-differing pair issue #445 was actually adopted for. Middle
    // keeps the distinguishing domain; tail spends the whole budget on the shared local part and collapses
    // both rows to one string. This is the assertion that would redden if someone "simplified" the panel
    // back to tail-truncation.
    func testMiddleTruncationKeepsTheDomainWhereTailTruncationCollapsesIt() {
        let one = "oleksii@company-one.com"
        let two = "oleksii@company-two.com"

        for budget in [90.0, 120.0] {
            XCTAssertNotEqual(middle(one, budget: budget), middle(two, budget: budget),
                              "middle-truncation lost the distinguishing domain at \(budget) pt — the "
                              + "issue #445 invariant is broken")
            XCTAssertEqual(tail(one, budget: budget), tail(two, budget: budget),
                           "tail-truncation is expected to collapse this pair at \(budget) pt; if it no "
                           + "longer does, the premise for `.truncationMode(.middle)` needs re-checking")

            // …and the SHIPPED policy must be the one that wins. Without this the two assertions above are
            // a fact about CoreText; with it, they are a fact about the panel.
            XCTAssertNotEqual(elided(one, budget: budget), elided(two, budget: budget),
                              "the shipped elision policy (\(StatusPanelFormat.identityElision)) collapses "
                              + "this pair at \(budget) pt — the panel would show two accounts as one")
        }
    }

    // MARK: - AC: extreme reset durations, measured against the 52 pt reset cell

    // Issue #750 AC-3 predicted `365d23h` overflows. Measured, it does not — see the header. This asserts
    // the MEASURED boundary instead: every three-digit day count fits, four digits do not, and the cell is
    // provably overflowable at the reachable extreme. `humanizeUntil`'s hour form maxes at `23h59m` (hours
    // roll into days), so the day form is the only unbounded one.
    func testExtremeResetDurationsAreMeasuredAgainstTheResetCell() {
        let cell = StatusPanelFormat.meterResetCellWidth
        let day: Int64 = 86_400

        // The widest bounded form: hours never exceed 23, minutes never 59.
        assertFits(StatusPanelFormat.humanizeUntil(23 * 3600 + 59 * 60), meterResetFont,
                   budget: cell, "reset cell, widest hour form")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(23 * 3600 + 59 * 60), "23h59m")

        // Three-digit days — the realistic ceiling for any true reset instant — fit.
        for days: Int64 in [100, 365, 999] {
            assertFits(StatusPanelFormat.humanizeUntil(days * day + 23 * 3600), meterResetFont,
                       budget: cell, "reset cell at \(days) d")
        }

        // The cell CAN overflow, at the extreme a nonsense `resets_at` reaches. `Int64.max` renders
        // `106751991167300d15h` — a ~154 % overrun, so this proves overflowability with margin no font
        // revision plausibly closes, rather than resting on the 4-digit boundary's ~6 %.
        let absurd = StatusPanelFormat.humanizeUntil(.max)
        let required = width(absurd, meterResetFont)
        XCTAssertTrue(overflows(absurd, meterResetFont, budget: cell),
                      "\"\(absurd)\" needs \(String(format: "%.2f", required)) pt and did NOT overflow the "
                      + "\(cell) pt reset cell — the cell is unguardable, so this gate proves nothing")

        // And the boundary itself, reported: four-digit days are where clipping begins.
        let fourDigit = StatusPanelFormat.humanizeUntil(1000 * day + 23 * 3600)
        XCTAssertEqual(fourDigit, "1000d23h")
        XCTAssertGreaterThan(width(fourDigit, meterResetFont), width("999d23h", meterResetFont),
                             "a four-digit day count must measure wider than a three-digit one")
    }

    // MARK: - AC: an out-of-range percent is measured, and the bar fill is clamped

    // `WireModel` decodes `session_pct` / `weekly_pct` as a bare `UInt8` and applies NO clamp, so 255 is
    // reachable from the wire. Two independent consequences, both locked: the TEXT must still fit its cell,
    // and the BAR must not paint past its track.
    func testOutOfRangePercentIsMeasuredAndTheBarFillIsClamped() {
        let rendered = StatusPanelFormat.pct(255)
        XCTAssertEqual(rendered, "255%", "the panel reports the wire value verbatim — it does not fake 100 %")
        assertFits(rendered, meterPercentFont,
                   budget: StatusPanelFormat.meterPercentCellWidth, "percent cell at the UInt8 ceiling")

        // The fill is a DRAWING bound, not a truth edit: the number above still says 255 %.
        let track = 120.0
        XCTAssertEqual(StatusPanelFormat.meterFillWidth(fraction: 2.55, full: track), track, accuracy: 0.001,
                       "a 255 % reading painted past its own track — the clamp is gone")
        XCTAssertEqual(StatusPanelFormat.meterFillWidth(fraction: 1.0, full: track), track, accuracy: 0.001)
        XCTAssertEqual(StatusPanelFormat.meterFillWidth(fraction: 0.5, full: track), 60, accuracy: 0.001)

        // A negative fraction cannot arise from a `UInt8`, but the clamp is two-sided and the lower half is
        // what stops a future signed source painting backwards.
        XCTAssertEqual(StatusPanelFormat.meterFillWidth(fraction: -1, full: track), 0, accuracy: 0.001,
                       "a negative fraction must draw nothing, never a reversed capsule")

        // Zero shows a BARE track (#137: never a fabricated fill); a live-but-tiny percent keeps a 5 pt
        // sliver so it does not read as empty.
        XCTAssertEqual(StatusPanelFormat.meterFillWidth(fraction: 0, full: track), 0, accuracy: 0.001)
        XCTAssertEqual(StatusPanelFormat.meterFillWidth(fraction: 0.001, full: track), 5, accuracy: 0.001,
                       "a 0.1 % reading must keep the minimum sliver, not vanish into the track")
    }

    // MARK: - AC: the Stats handle budget (issue #700's ~198 pt)

    func testStatsHandleBudgetFitsARepresentativeHandleAndElidesBeyondIt() {
        let budget = StatusPanelFormat.statsHandleBudget

        // The realistic fleet shape — a 23-character address — clears it with room.
        assertFits("oleksii@company-one.com", statsHandleFont, budget: budget, "Stats handle")

        // Issue #700 claims the budget is "enough for a 28-character handle untruncated". Measured, that
        // holds for representative text and is MARGINAL at the wide-glyph extreme: 28 `x` characters need
        // more than the budget. Recorded as the boundary it is, not rounded into a promise the metric does
        // not keep. The two fixtures below straddle #700's 28 deliberately — one longer in CHARACTERS and
        // still fitting, one at exactly 28 and not.
        let representative29 = "oleksii.pelykh@sessiometer.io"
        XCTAssertEqual(representative29.count, 29, "the fixture must stay longer than #700's 28-char claim")
        assertFits(representative29, statsHandleFont, budget: budget,
                   "Stats handle, 29 representative characters")

        let wide28 = String(repeating: "x", count: 28)
        XCTAssertTrue(overflows(wide28, statsHandleFont, budget: budget),
                      "28 wide glyphs unexpectedly fit \(budget) pt — issue #700's boundary moved; "
                      + "re-measure before trusting the 28-character claim")

        // Whatever overflows must still be elided, never clipped.
        let cut = visibleSubstring(wide28, font: statsHandleFont, budget: budget,
                                   mode: coreTextType(StatusPanelFormat.identityElision))
        XCTAssertTrue(cut.truncated)
        XCTAssertFalse(cut.text.isEmpty, "elided to nothing — uninformative truncation (issue #748 R2)")
    }

    // MARK: - CONSTRAINT-A: the gate PROVES it can fail, in the same run it passes

    // The whole suite is worthless if `overflows` can only return `false`. This drives a deliberately
    // over-wide fixture through the SAME predicate every assertion above uses and requires a FAIL — and, in
    // the same test, requires every shipped string to PASS. Both halves matter: a canary alone could pass
    // against a gate stuck on `true`, and shipped-labels-pass alone could pass against a gate stuck on
    // `false`. (Sibling of `BarGlyphParityTests.testTheCanaryDriftsAndAnIdenticalRenderDoesNot`.)
    func testTheCanaryOverflowsWhileEveryShippedLabelFits() {
        // ---- the canary must FAIL the gate ----
        let canary = String(repeating: "W", count: 60)   // ~ 3× the widest shipped label
        let canaryWidth = width(canary, rosterLabelFont)
        XCTAssertTrue(overflows(canary, rosterLabelFont, budget: StatusPanelFormat.rosterLabelBudget),
                      "the canary (\(String(format: "%.2f", canaryWidth)) pt) did NOT trip the "
                      + "\(StatusPanelFormat.rosterLabelBudget) pt budget — the gate cannot fail, so a "
                      + "green run is not evidence (issue #748 CONSTRAINT-A)")

        // Every fixed cell must be provably overflowable too, or its green is equally empty.
        let cells: [(String, NSFont, Double)] = [
            ("meter label",   meterLabelFont,   StatusPanelFormat.meterLabelCellWidth),
            ("meter percent", meterPercentFont, StatusPanelFormat.meterPercentCellWidth),
            ("meter reset",   meterResetFont,   StatusPanelFormat.meterResetCellWidth),
            ("stats handle",  statsHandleFont,  StatusPanelFormat.statsHandleBudget),
        ]
        for (name, font, budget) in cells {
            XCTAssertTrue(overflows(canary, font, budget: budget),
                          "\(name): the canary did not trip a \(budget) pt budget — that cell's gate "
                          + "cannot fail (CONSTRAINT-A)")
        }

        // ---- and every shipped string must PASS it, in this same run ----
        assertFits("SESSION", meterLabelFont,
                   budget: StatusPanelFormat.meterLabelCellWidth, "canary control")
        assertFits(StatusPanelFormat.pct(255), meterPercentFont,
                   budget: StatusPanelFormat.meterPercentCellWidth, "canary control")
        assertFits(StatusPanelFormat.humanizeUntil(999 * 86_400 + 23 * 3600), meterResetFont,
                   budget: StatusPanelFormat.meterResetCellWidth, "canary control")
        assertFits("work@a.com", rosterLabelFont,
                   budget: StatusPanelFormat.rosterLabelBudget, "canary control")

        // A gate whose budget were zero would "fail" on everything and look rigorous. Guard the budgets.
        for (name, _, budget) in cells {
            XCTAssertGreaterThan(budget, 0, "\(name) budget is non-positive — every string would overflow")
        }
    }

    // MARK: - AC-3 (issue #756): every gated cell still fits at EVERY Dynamic Type size class

    // The panel scales UNIFORMLY (`StatusPanelTypeScale`): one factor multiplies every font point size AND
    // every layout constant, so a cell and the text inside it grow together. The obvious objection is that
    // this makes the sweep below a tautology — if both sides are × k, of course it still fits.
    //
    // It is not, for one measured reason: GLYPH ADVANCE IS NOT LINEAR IN POINT SIZE. Hinting and device
    // rounding mean `width(text, font(size: 11k)) != k · width(text, font(size: 11))`, so the two sides
    // drift apart by fractions of a point at every step, and the sweep is what says the drift never
    // crosses a cell edge. The same non-linearity shows up one level up: the panel's rendered HEIGHT
    // tracks k only to within about −2.4 %…+1.2 % across the range (measured over the `healthy` and
    // `stats` fixtures), while its WIDTH — a `.frame(width:)` on a constant — is exact.
    //
    // The fonts are rebuilt here from the SAME (points, weight) pairs the views pass to `Font.panel(_:_:
    // scale:)`, at the SAME factor `PanelTypeScale.factor(for:)` returns, so this measures the shipped
    // typography rather than a parallel model of it.

    /// Every gated cell at factor `k`: its font, its budget, and the widest string it can actually reach.
    ///
    /// The three travel together in ONE row rather than through a name-keyed lookup, so a cell can never be
    /// measured against another cell's fixture — a mistyped or newly-added name has nowhere to silently
    /// fall through to. The strings are the same fixtures the default-size tests above justify, so the
    /// sweep inherits their reachability reasoning rather than inventing new content.
    private func scaledCells(_ k: Double) -> [(name: String, widest: String, font: NSFont, budget: Double)] {
        [("meter label", "SESSION",
          NSFont.systemFont(ofSize: 10 * k, weight: .semibold),
          StatusPanelFormat.meterLabelCellWidth * k),
         ("meter percent", StatusPanelFormat.pct(255),                   // bare UInt8 wire, no clamp
          NSFont.monospacedDigitSystemFont(ofSize: 12 * k, weight: .semibold),
          StatusPanelFormat.meterPercentCellWidth * k),
         ("meter reset", StatusPanelFormat.humanizeUntil(999 * 86_400 + 23 * 3600),
          NSFont.monospacedDigitSystemFont(ofSize: 11 * k, weight: .regular),
          StatusPanelFormat.meterResetCellWidth * k),
         ("roster label", "oleksii@company-one.com",                     // the realistic fleet shape
          NSFont.systemFont(
              ofSize: NSFont.preferredFont(forTextStyle: .body).pointSize * k, weight: .semibold),
          StatusPanelFormat.rosterLabelBudget * k),
         ("stats handle", "oleksii@company-one.com",
          NSFont.systemFont(ofSize: 13 * k, weight: .semibold),
          StatusPanelFormat.statsHandleBudget * k)]
    }

    func testEveryGatedCellFitsAtEveryDynamicTypeSizeClass() {
        var checked = 0
        for size in DynamicTypeSize.allCases {
            let k = PanelTypeScale.factor(for: size)
            for cell in scaledCells(k) {
                assertFits(cell.widest, cell.font, budget: cell.budget,
                           "\(cell.name) at \(size) (k=\(String(format: "%.4f", k)))")
                checked += 1
            }
        }
        // Degenerate-subject guard, exact rather than non-zero: 12 size classes × 5 gated cells. A sweep
        // that silently evaluated one class would otherwise pass and prove nothing.
        XCTAssertEqual(checked, 12 * 5,
                       "expected \(12 * 5) (size class × cell) measurements, ran \(checked)")
    }

    // MARK: - CONSTRAINT-A for the AC-3 sweep: a scaled font in an UNSCALED cell must FAIL

    // The sweep above passes because both sides scale. This proves the sweep can still FAIL, by mutating
    // exactly the defect AC-2 names — "a scaled font in a fixed cell is a clipping bug, not a fix" — and
    // pushing it through the SAME `overflows` predicate the sweep uses. That mutation is not hypothetical:
    // it is what the panel would look like if someone scaled the fonts and forgot the `.frame(width:)`
    // constants, which is the single most likely way this change regresses.
    func testAScaledFontInAnUnscaledCellTripsTheGate() {
        let k = PanelTypeScale.factor(for: .accessibility3)
        XCTAssertGreaterThan(k, 1.0, "the ceiling factor is not an enlargement — the mutation below is inert")

        var tripped = 0
        for cell in scaledCells(k) {
            // The MUTATION: keep the scaled font, revert the cell to its DEFAULT width.
            let unscaledBudget = cell.budget / k
            XCTAssertTrue(overflows(cell.widest, cell.font, budget: unscaledBudget),
                          "\(cell.name): the .accessibility3 font still fits the UNSCALED "
                          + "\(String(format: "%.2f", unscaledBudget)) pt cell, so the sweep would stay "
                          + "green even if the frame constants were never scaled — this cell's half of "
                          + "AC-3 is not actually gated (issue #748 CONSTRAINT-A)")
            // Control, same run: with the cell scaled too, the identical string fits.
            assertFits(cell.widest, cell.font, budget: cell.budget,
                       "\(cell.name) control (scaled cell)")
            tripped += 1
        }
        XCTAssertEqual(tripped, 5, "expected 5 gated cells in the mutation, ran \(tripped)")
    }

    // MARK: - Headless (issue #750 AC-6)

    // This suite touches no window, no screen and no status item: `NSAttributedString.size` and CoreText
    // line layout are pure text shaping. The proof it runs headless is that it runs at all in this bundle
    // (`TEST_HOST: ""`, no host app) under the `xcodebuild test` invocation CI runs verbatim — so this test
    // asserts only that the primitives are actually reachable, i.e. that a real shaping result came back
    // rather than a zero from an unavailable font stack.
    func testMeasurementIsAvailableWithoutAWindowServer() {
        XCTAssertGreaterThan(width("SESSION", meterLabelFont), 0,
                             "text shaping returned zero width — the font stack is unavailable in this "
                             + "environment, so every `assertFits` above would pass vacuously")
        XCTAssertGreaterThan(rosterLabelFont.pointSize, 0, "the body text style resolved to no size")
    }
}
#endif
