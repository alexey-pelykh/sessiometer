// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The standing gate under the usage meter's COMPENSATING-CONTROL argument (issue #1251).
//
// THE ARGUMENT. `design/README.md` argues the meter bar is defensible under WCAG 1.4.11 despite its
// fill failing 3:1 against the track it sits on (issue #831 measured `.green` 1.61 / `.orange` 1.67 /
// `.red` 2.59 in light), because the bar never carries its value ALONE. Two limbs:
//
//   LIMB 1 — the exact percent renders beside the bar as text (`UsageMeter` draws
//            `Text(StatusPanelFormat.pct(pct))`).
//   LIMB 2 — the bar itself is `.accessibilityHidden(true)`, so it is not a second, lower-contrast
//            channel a VoiceOver user has to read.
//
// That argument is load-bearing: it is the whole reason the bar ships at its measured contrast rather
// than being re-tinted. Issue #1251 established that NEITHER limb was held by a gate that could fail a
// merge, and this file is the gate. Re-measured on this branch's base (578c834), by mutation:
//
//   • Deleting `.accessibilityHidden(true)` from `UsageBar` left the whole suite green, and left the
//     ARMED golden gate green too, at zero drift on every cell — removing a view from the
//     accessibility tree changes no pixel, so a raster gate cannot see it even when armed. Nothing
//     held limb 2.
//   • Deleting the percent CELL from `UsageMeter` — the `Text(StatusPanelFormat.pct(pct))` and the
//     three modifiers that style and size it — left every gate that can fail a merge green, including
//     `StatusPanelFormatTests.testTheMeterBarFillIsCarriedByTheAdjacentPercentTextNotByItsOwnContrast`,
//     which says so itself in its own SCOPE paragraph. The ARMED golden gate did catch it, on panel
//     HEIGHT (the percent cell is the tallest thing in the meter row, so dropping it shortens every
//     roster-bearing state) — but it `XCTSkipUnless`es on `SESSIOMETER_PANEL_GOLDEN_GATE` and its CI
//     job sets `continue-on-error: true` on every step, so it can never fail a merge. Nothing held
//     limb 1 either.
//
//     DELETE THE WHOLE CELL when re-deriving that, or the measurement inverts. Removing only the
//     `Text(…)` LINE strands its `.frame(width: StatusPanelFormat.meterPercentCellWidth …)`, and Swift's
//     leading-dot continuation re-attaches the orphan to the PRECEDING expression — `UsageBar` — pinning
//     the flexible bar to the percent column's width. That moves the whole right-hand gutter and reds
//     `PanelExpiryGutterTests` (required, not env-gated) with a message naming `ExpiryLine`. It is a
//     LAYOUT break masquerading as coverage of limb 1, and it has already been mistaken for one.
//
// ─────────────────────────────────────────────────────────────────────────────────────────────────────
// WHAT THIS FILE COMPARES, stated first because the obvious reading overclaims it. The two assertions
// below read `Sources/StatusPanelRoster.swift` AS TEXT and compare the bodies of `UsageBar` and
// `UsageMeter` against a required modifier and a required call. They pin that the SOURCE still says
// those things. They do NOT observe a rendered panel, a raster, or an accessibility tree, and they
// therefore do NOT establish that the percent is legible, correctly placed, unclipped, or drawn at all
// — only that the view code still asks for it. The render-side question stays where it already lives:
// `PanelGoldenParityTests`, still env-gated and still soft. This file narrows the hole; it does not
// close it, and § Panel golden drift gate in `design/README.md` remains the place that says so.
//
// WHY SOURCE TEXT AND NOT THE ACCESSIBILITY TREE — measured, because the tree is the instrument this
// file's own neighbour (`PanelAccessibilityTreeTests`) would reach for, and issue #1251 nominated it.
// It cannot resolve limb 2, and the reason is worth writing down:
//
//   In the SHIPPED panel the bar contributes NOTHING to the accessibility tree whether or not it
//   carries `.accessibilityHidden(true)` — an ANCESTOR already suppresses it. Each roster row already
//   collapses to a SINGLE element, by one of two mechanisms: a switchable row through its `Button`
//   wrap, a non-switchable one through `.accessibilityElement(children: .ignore)` plus its own label
//   (`AccountRowView`). Either way the bar's subtree is discarded. The switchable arm is the one the
//   fixtures exercise, so `Button` is the mechanism that actually collapses them — forcing
//   `offersSwitch` to `false`, which routes every row through the `.ignore` arm instead, MOVES the
//   pinned histograms (measured: `healthy` goes from `AXButton:5 … AXUnknown:1` to
//   `AXButton:3 … AXUnknown:3` — the two switchable rows stop being `Button`s). That is why deleting
//   the modifier moved no role histogram in `PanelAccessibilityTreeTests` on any fixture, and why it
//   moved no pixel in the armed golden gate.
//
// So the modifier's effect is INVISIBLE at the panel level, and a tree assertion there would be green
// either way — a gate that cannot fail. What the modifier is NOT is inert: hosted on its own, outside a
// collapsing ancestor, a bar-shaped view publishes a real `AXUnknown` element, and the modifier removes
// it. `testHidingABarShapedViewRemovesARealElementFromTheTree` below asserts exactly that, and is the
// reason limb 2 is worth pinning at all rather than deleted as redundant: the modifier is the guard
// that survives a future refactor which stops collapsing the row.
//
// KNOWN BOUNDS — spellings this file ACCEPTS although they would defeat the argument. Bounds 1-3 and 5
// were measured on this branch by mutating the real `StatusPanelRoster.swift`, each leaving the whole
// bundle green. Bound 4 needs no mutation — it is unreachable because this file never renders. Bounds
// 1-3 are span/compile-time holes a text predicate CAN close:
// `PanelReachabilityLintTests` closes the analogous one by scoping to the top-level chain and counting
// only modifiers at chain depth 0. They are recorded here, not closed.
//
//   1. Either limb applied to a DIFFERENT view inside the same braces — a helper property, a nested
//      subview — rather than to the type's own rendered chain. Measured: limb 2 deleted from `body` and
//      re-applied to an unused `legend` property reads as HELD. One caveat, itself measured: the bundle
//      is green only when the relocated copy is spelled the way the limb-2 canary deletes (newline plus
//      eight spaces); at another indentation that canary and the commented-out-modifier canary red
//      instead, reporting "no subject to delete" — a loud red, not a silent pass. The live limb-2
//      assertion accepts it either way, and that is the hole.
//   2. Either limb inside a conditional-compilation block excluded on this target — the real modifier
//      wrapped in `#if os(iOS)` / `#endif` is absent from the macOS binary and still reads as held,
//      because the scrubber has no `#if` handling and excluded text reads as live code.
//   3. The pinned type no longer on the render path at all: this pins the DECLARATION, not the shipped
//      composition, so `UsageMeter` drawing the bar inline while `UsageBar` survives as dead code still
//      satisfies limb 2. This is the sharpest of the five, and it has NO compensating gate: measured,
//      inlining `UsageBar`'s composition VERBATIM minus the modifier reds nothing — not the bundle, not
//      `PanelAccessibilityTreeTests`, and not the ARMED golden gate, which is blind to it by
//      construction because a verbatim inline moves no pixel. The rasters notice only when the
//      replacement ALSO changes how the bar looks, and they are soft even then. Do not read them as a
//      safety net here; an earlier draft of this header did, and it was wrong.
//   4. A percent `Text` present but drawn at zero size, clipped, or in a colour equal to its background.
//   5. The pinned call present but no longer carrying the ROW's value. The needle stops at the opening
//      paren — `Text(StatusPanelFormat.pct(` — so the ARGUMENT is unconstrained. Measured: shadowing
//      `pct` with a local inside `body` leaves the pinned literal byte-identical, renders `n/a` as
//      every account's percent while the bar keeps painting the true fraction, and reds NOTHING in the
//      bundle — the quietest of the five. Limb 1 says the EXACT percent renders beside the bar; this
//      gate only sees that a percent-shaped call is still written.
//
// That list is KNOWN-INCOMPLETE and should be read as such: it records the holes that have been found,
// not the holes that exist. All five are instances of one root property — this gate reads TEXT inside a
// brace span, so ANY edit that preserves the text while changing what it means is accepted. A sixth
// should be expected; adding it here is the repair.
//
// The OPPOSITE direction, measured too, because a red here is a prompt to LOOK and not by itself proof
// that a limb was removed: a binding-valued spelling reds this gate whatever the binding returns — even
// `.accessibilityHidden(isHidden)` with `isHidden` TRUE, so the bar really is hidden — and so does
// either literal broken across lines. A limb moved OUT of its type's braces reds it for the same
// reason. Three shapes, all measured, none of them a removed limb.
//
// A green here means STILL WRITTEN, never STILL WORKING. The behavioural half of that pair is the
// golden gate, and it is soft.
//
// CONSTRAINT-A (issue #748) — no gate without a proven falsifier. Every assertion routes through ONE
// function, `MeterCompensatingControl.verdict(in:)`, and the canaries drive that SAME function by
// MUTATION of the REAL `StatusPanelRoster.swift` read from disk: with the `UsageBar` modifier deleted it
// must report limb 2 unheld, with the percent `Text` deleted it must report limb 1 unheld. The
// span-scoping canary matters most — the file applies `.accessibilityHidden(true)` at many other sites
// (`HeldUsageBar`'s among them, two types further down), so a file-wide `contains` would stay green over
// exactly the deletion this gate exists to catch. The canary asserts that survivor set mechanically
// rather than counting it here, so no tally in this file can go stale against the source.

#if DEBUG
import SwiftUI
import XCTest

// MARK: - The predicate every assertion AND every canary routes through

enum MeterCompensatingControl {

    /// One reading of the two limbs. The `…DeclarationFound` flags are separate from the limb verdicts so
    /// a RENAMED type reports as "cannot find the subject" rather than as "the modifier was removed" —
    /// two very different repairs, and a single false would conflate them.
    struct Verdict: Equatable {
        var barDeclarationFound: Bool
        var meterDeclarationFound: Bool
        var barIsAccessibilityHidden: Bool
        var meterRendersPercentText: Bool
    }

    /// `source` with every comment and string-literal INTERIOR blanked to spaces, offsets preserved.
    ///
    /// Blanking rather than deleting keeps brace matching honest: a `{` inside a comment or a string must
    /// not open a scope, and a length-preserving scrub means the span indices still address the original.
    /// The comment case is not hypothetical here — `UsageBar` and `UsageMeter` both carry multi-paragraph
    /// doc comments that quote `.accessibilityHidden(true)` in prose, so a predicate reading raw text
    /// would be satisfied by the COMMENTARY about the modifier after the modifier itself was deleted.
    static func scrubbed(_ source: String) -> String {
        var out = ""
        out.reserveCapacity(source.count)
        var chars = Array(source)
        var i = 0
        enum Mode { case code, lineComment, blockComment, string }
        var mode = Mode.code
        while i < chars.count {
            let c = chars[i]
            let next: Character? = i + 1 < chars.count ? chars[i + 1] : nil
            switch mode {
            case .code:
                if c == "/", next == "/" { mode = .lineComment; out += "  "; i += 2; continue }
                if c == "/", next == "*" { mode = .blockComment; out += "  "; i += 2; continue }
                if c == "\"" { mode = .string; out.append(c); i += 1; continue }
                out.append(c); i += 1
            case .lineComment:
                if c == "\n" { mode = .code; out.append(c) } else { out.append(" ") }
                i += 1
            case .blockComment:
                if c == "*", next == "/" { mode = .code; out += "  "; i += 2; continue }
                out.append(c == "\n" ? "\n" : " "); i += 1
            case .string:
                if c == "\\", next != nil { out += "  "; i += 2; continue }
                if c == "\"" { mode = .code; out.append(c) } else { out.append(" ") }
                i += 1
            }
        }
        return out
    }

    /// The brace-matched BODY of a top-level `struct <name>` declaration, or `nil` if it is not there.
    ///
    /// Scoped deliberately: see the file header's CONSTRAINT-A note on why a file-wide search is the one
    /// shape this gate must not take.
    static func structBody(_ name: String, in scrubbedSource: String) -> Substring? {
        let chars = Array(scrubbedSource)
        // `private struct UsageBar: View {` — match on the declaration keyword pair so a mention of the
        // type name elsewhere (a call site, a doc reference) cannot be mistaken for its declaration.
        guard let declRange = scrubbedSource.range(of: "struct \(name): View {") else { return nil }
        var depth = 0
        var i = scrubbedSource.distance(from: scrubbedSource.startIndex, to: declRange.lowerBound)
        let openIndex = i
        while i < chars.count {
            if chars[i] == "{" { depth += 1 }
            if chars[i] == "}" {
                depth -= 1
                if depth == 0 {
                    let lower = scrubbedSource.index(scrubbedSource.startIndex, offsetBy: openIndex)
                    let upper = scrubbedSource.index(scrubbedSource.startIndex, offsetBy: i + 1)
                    return scrubbedSource[lower..<upper]
                }
            }
            i += 1
        }
        return nil
    }

    /// The whole reading, from raw source text.
    static func verdict(in source: String) -> Verdict {
        let scrub = scrubbed(source)
        let bar = structBody("UsageBar", in: scrub)
        let meter = structBody("UsageMeter", in: scrub)
        return Verdict(
            barDeclarationFound: bar != nil,
            meterDeclarationFound: meter != nil,
            barIsAccessibilityHidden: bar?.contains(".accessibilityHidden(true)") ?? false,
            meterRendersPercentText: meter?.contains("Text(StatusPanelFormat.pct(") ?? false)
    }
}

// MARK: - Tests

final class MeterCompensatingControlTests: XCTestCase {

    /// Located from this source file, exactly as `PanelDynamicTypeLintTests` locates `Sources/` — CI
    /// checks the tree out at the same path it compiled from.
    private var rosterURL: URL {
        URL(fileURLWithPath: #filePath)   // …/apps/menubar/Tests/MeterCompensatingControlTests.swift
            .deletingLastPathComponent()  // …/apps/menubar/Tests
            .deletingLastPathComponent()  // …/apps/menubar
            .appendingPathComponent("Sources/StatusPanelRoster.swift")
    }

    private func realSource() throws -> String {
        try String(contentsOf: rosterURL, encoding: .utf8)
    }

    // MARK: - Corpus guard
    //
    // Every verdict below is computed over a string read off disk. A truncated or missing read yields
    // `false` for both limbs — which would present as a LOUD failure rather than a silent pass, so this
    // guard is not what makes the assertions evidence. What it does is name the real cause: a path that
    // stopped resolving must not be diagnosed as "someone deleted the modifier".

    func testTheRosterSourceIsActuallyReadable() throws {
        let source = try realSource()
        XCTAssertGreaterThan(source.count, 20_000, """
            StatusPanelRoster.swift read back as only \(source.count) bytes from \(rosterURL.path) — the \
            corpus is truncated or the path stopped resolving, so every verdict below is about the wrong \
            text. Fix discovery before reading any limb verdict as a statement about the panel.
            """)
        let verdict = MeterCompensatingControl.verdict(in: source)
        XCTAssertTrue(verdict.barDeclarationFound && verdict.meterDeclarationFound, """
            the source was read but `UsageBar` / `UsageMeter` were not found in it \
            (bar: \(verdict.barDeclarationFound), meter: \(verdict.meterDeclarationFound)). A rename is \
            the likely cause — re-point `structBody` rather than reading the limb verdicts as a defect.
            """)
    }

    // MARK: - The two limbs

    /// LIMB 2 — issue #1251's AC-1. Reds when `.accessibilityHidden(true)` is deleted from `UsageBar`.
    func testTheUsageBarStaysOutOfTheAccessibilityTreeByItsOwnModifier() throws {
        let verdict = MeterCompensatingControl.verdict(in: try realSource())
        XCTAssertTrue(verdict.barIsAccessibilityHidden, """
            `UsageBar` no longer carries `.accessibilityHidden(true)`.

            That modifier is limb 2 of the compensating-control argument in `design/README.md`: the meter \
            fill does not clear 3:1 (issue #831 measured 1.61 / 1.67 / 2.59 in light), and the argument \
            for shipping it anyway is that it is not a channel anyone has to read — it is decorative \
            reinforcement of a percent printed beside it.

            Do not restore this by editing this test. Either put the modifier back, or amend the \
            compensating-control argument in `design/README.md` FIRST — it is the thing that would then \
            be false.

            Note what this does NOT mean: the panel did not necessarily start leaking the bar to \
            VoiceOver. Each roster row already collapses to one element, which suppresses the bar \
            independently (file header). This gate holds the modifier itself, because that ancestor \
            collapse is not what the README's argument cites and is not guaranteed to survive a refactor.
            """)
    }

    /// LIMB 1 — issue #1251's AC-2. Reds when `Text(StatusPanelFormat.pct(pct))` is deleted from
    /// `UsageMeter`. This is the merge-failing half of limb 1's coverage; the render-side half stays in
    /// the env-gated, soft `PanelGoldenParityTests` (file header).
    func testTheUsageMeterStillDrawsThePercentBesideTheBar() throws {
        let verdict = MeterCompensatingControl.verdict(in: try realSource())
        XCTAssertTrue(verdict.meterRendersPercentText, """
            `UsageMeter` no longer renders `Text(StatusPanelFormat.pct(…))`.

            That text is limb 1 of the compensating-control argument in `design/README.md` — the bar's \
            non-colour channel, and the reason a fill measured at 1.61 / 1.67 / 2.59 against its track \
            (issue #831) is defensible under WCAG 1.4.11 rather than a violation. Without it the bar \
            carries the value alone, at those ratios.

            `StatusPanelFormatTests.testTheMeterBarFillIsCarriedByTheAdjacentPercentTextNotByItsOwnContrast` \
            does NOT cover this and says so itself: it pins the FORMATTER, and stays green with this \
            `Text` deleted (measured, issue #1251).
            """)
    }

    // MARK: - Canaries: every predicate proven able to FAIL, by mutation of the REAL source
    //
    // Each canary deletes from the real on-disk text exactly what a regression would delete, and requires
    // the SAME `verdict(in:)` the assertions above call to change its answer. A canary against a parallel
    // copy of the logic would prove nothing about the predicate actually in use.

    /// Removes `UsageBar`'s modifier and nothing else. This is the mutation issue #1251's AC-1 names.
    ///
    /// It is also the SPAN-SCOPING canary, and that is the more important of its two jobs: other
    /// applications of `.accessibilityHidden(true)` survive this deletion — asserted below, not assumed —
    /// so a file-wide `contains` would still report limb 2 held. If this canary ever passes while
    /// `testTheUsageBarStaysOutOfTheAccessibilityTreeByItsOwnModifier` is green, the scoping has widened
    /// and the real gate has stopped covering anything.
    func testDeletingTheBarModifierFlipsTheLimbTwoVerdict() throws {
        let source = try realSource()
        let scrub = MeterCompensatingControl.scrubbed(source)
        let barBody = try XCTUnwrap(MeterCompensatingControl.structBody("UsageBar", in: scrub))
        // Offsets are measured in the SCRUB and then applied to `source`. That is only sound because the
        // scrub is character-count preserving (asserted by
        // `testTheScrubberPreservesCodeAndLengthWhileBlankingCommentsAndStrings`), which is what lets this
        // canary delete from the real text at a span located in the cleaned text.
        let lower = scrub.distance(from: scrub.startIndex, to: barBody.startIndex)
        let upper = scrub.distance(from: scrub.startIndex, to: barBody.endIndex)
        XCTAssertEqual(scrub.count, source.count, "the scrub is not length-preserving; the splice would be off")

        let bodyStart = source.index(source.startIndex, offsetBy: lower)
        let bodyEnd = source.index(source.startIndex, offsetBy: upper)
        let head = String(source[source.startIndex..<bodyStart])
        let body = String(source[bodyStart..<bodyEnd])
        let tail = String(source[bodyEnd...])
        XCTAssertTrue(body.contains("\n        .accessibilityHidden(true)"), """
            precondition unmet: there is no `.accessibilityHidden(true)` inside `UsageBar` to delete, so \
            this canary has no subject. If `testTheUsageBarStaysOutOfTheAccessibilityTreeByItsOwnModifier` \
            is also red, THAT is the finding and this line is its echo — fix the source, not this test.
            """)
        let mutatedBody = body.replacingOccurrences(of: "\n        .accessibilityHidden(true)", with: "")
        let mutated = head + mutatedBody + tail

        XCTAssertTrue(mutated.contains(".accessibilityHidden(true)"), """
            precondition for the SPAN claim: the mutated file must still carry the modifier ELSEWHERE, \
            or this canary would also pass for a file-wide predicate and prove nothing about scoping.
            """)
        let verdict = MeterCompensatingControl.verdict(in: mutated)
        XCTAssertTrue(verdict.barDeclarationFound, "the mutation destroyed the declaration, not the modifier")
        XCTAssertFalse(verdict.barIsAccessibilityHidden, """
            deleting `.accessibilityHidden(true)` from `UsageBar` did NOT change the limb-2 verdict — the \
            predicate is reading some OTHER application of the modifier, of which this file has several, \
            so the green verdict on the real file means nothing.
            """)
        // The limbs are independent: this mutation must not disturb limb 1. Compared against the
        // UNMUTATED reading rather than against `true`, so that when limb 1 is legitimately failing on
        // disk this cross-check stays silent instead of blaming an overlap it did not observe.
        XCTAssertEqual(verdict.meterRendersPercentText,
                       MeterCompensatingControl.verdict(in: source).meterRendersPercentText,
                       "the bar mutation moved the limb-1 verdict — the two spans overlap")
    }

    /// Removes the percent `Text` from `UsageMeter` and nothing else — the mutation that leaves the named
    /// `StatusPanelFormatTests` case green (file header).
    func testDeletingThePercentTextFlipsTheLimbOneVerdict() throws {
        let source = try realSource()
        let needle = "            Text(StatusPanelFormat.pct(pct))\n"
        XCTAssertTrue(source.contains(needle), """
            precondition unmet: `UsageMeter` has no `Text(StatusPanelFormat.pct(pct))` line to delete, so \
            this canary has no subject. If `testTheUsageMeterStillDrawsThePercentBesideTheBar` is also \
            red, THAT is the finding and this line is its echo — fix the source, not this test.
            """)
        let mutated = source.replacingOccurrences(of: needle, with: "")

        let verdict = MeterCompensatingControl.verdict(in: mutated)
        XCTAssertTrue(verdict.meterDeclarationFound, "the mutation destroyed the declaration, not the call")
        XCTAssertFalse(verdict.meterRendersPercentText, """
            deleting `Text(StatusPanelFormat.pct(…))` from `UsageMeter` did NOT change the limb-1 verdict \
            — the predicate is matching something else (a doc comment, or `BlindMeter`'s own last-known \
            percent two types further down), so its green on the real file means nothing.
            """)
        // Against the UNMUTATED reading, for the reason given in the sibling canary above.
        XCTAssertEqual(verdict.barIsAccessibilityHidden,
                       MeterCompensatingControl.verdict(in: source).barIsAccessibilityHidden,
                       "the meter mutation moved the limb-2 verdict — the two spans overlap")
    }

    /// A COMMENTED-OUT modifier must not satisfy limb 2. `UsageBar`'s own doc comment quotes the modifier
    /// in prose, so a predicate over raw text would read the commentary as the code.
    func testACommentedOutModifierDoesNotSatisfyLimbTwo() throws {
        let source = try realSource()
        let mutated = source.replacingOccurrences(of: "\n        .accessibilityHidden(true)",
                                                  with: "\n        // .accessibilityHidden(true)")
        let verdict = MeterCompensatingControl.verdict(in: mutated)
        XCTAssertFalse(verdict.barIsAccessibilityHidden, """
            a commented-out `.accessibilityHidden(true)` satisfied limb 2 — the predicate is reading \
            prose, so `UsageBar`'s own doc comment (which quotes the modifier) would keep this gate green \
            with the modifier deleted.
            """)
    }

    /// The scrubber must not blank live code, or every verdict above would be `false` for the wrong
    /// reason and the gate would be a permanent, uninformative red.
    func testTheScrubberPreservesCodeAndLengthWhileBlankingCommentsAndStrings() {
        let sample = """
        struct Probe: View {          // .accessibilityHidden(true) in a trailing comment
            /* .accessibilityHidden(true) in a block comment { unbalanced */
            let s = ".accessibilityHidden(true) in a string {"
            var body: some View { Text("x").accessibilityHidden(true) }
        }
        """
        let scrub = MeterCompensatingControl.scrubbed(sample)
        XCTAssertEqual(scrub.count, sample.count, "the scrub changed length — span offsets no longer address the source")
        let body = MeterCompensatingControl.structBody("Probe", in: scrub)
        XCTAssertNotNil(body, "the scrubber left an unbalanced brace from a comment, breaking span matching")
        XCTAssertEqual(body?.contains(".accessibilityHidden(true)"), true,
                       "the scrubber blanked LIVE code, not just comments and strings")
        // Exactly one survivor: the live one. Three of the four occurrences were comment or string text.
        XCTAssertEqual(scrub.components(separatedBy: ".accessibilityHidden(true)").count - 1, 1,
                       "expected exactly one surviving occurrence (the live modifier), got: \(scrub)")
    }

    // MARK: - The behavioural fact the source pin stands in for

    /// Hosting a bar-shaped view OUTSIDE a collapsing ancestor: `.accessibilityHidden(true)` removes a
    /// real element from the tree. This is what makes limb 2 worth pinning rather than deleting as
    /// redundant — measured at 578c834, an unhidden bar publishes `AXUnknown:1` and a hidden one
    /// publishes nothing.
    ///
    /// SCOPE, precisely. This hosts a REPLICA of `UsageBar`'s composition (`GeometryReader` > `ZStack` >
    /// two `Capsule` fills), not `UsageBar` itself, which is file-private to `StatusPanelRoster.swift`.
    /// So it is a statement about the COMPOSITION and about SwiftUI's behaviour, not a second reading of
    /// the shipped type — the shipped type is covered by the source pin above. What it rules out is the
    /// reading that the modifier is decorative: it is not, and a refactor that stops collapsing the
    /// roster row would make it the only thing keeping the bar out of the tree.
    @MainActor
    func testHidingABarShapedViewRemovesARealElementFromTheTree() {
        struct BarShaped: View {
            let hidden: Bool
            var body: some View {
                VStack {
                    Text("BAR_SHAPE_ANCHOR")
                    Group {
                        GeometryReader { geo in
                            ZStack(alignment: .leading) {
                                Capsule().fill(Color.secondary)
                                Capsule().fill(Color.green).frame(width: geo.size.width * 0.6)
                            }
                        }
                        .frame(height: 6)
                    }
                    .accessibilityHidden(hidden)
                }
            }
        }
        let size = CGSize(width: 200, height: 120)
        let unhidden = PanelA11y.tree(for: BarShaped(hidden: false), size: size)
        let hidden = PanelA11y.tree(for: BarShaped(hidden: true), size: size)

        // Absence is evidence only against a populated tree — the same trap guard every absence claim in
        // `PanelAccessibilityTreeTests` routes through.
        assertKnownPresent(unhidden, "BAR_SHAPE_ANCHOR", "the bar-shape canary (unhidden)")
        assertKnownPresent(hidden, "BAR_SHAPE_ANCHOR", "the bar-shape canary (hidden)")

        XCTAssertNotEqual(unhidden.roleHistogram, hidden.roleHistogram, """
            hiding a bar-shaped view changed NOTHING in the tree, so `.accessibilityHidden(true)` on \
            `UsageBar` would be inert and limb 2 of the compensating-control argument would rest on the \
            roster row's collapse alone. Re-read the meter-bar-fill paragraph in `design/README.md` \
            before adjusting this: the argument names the modifier, not the row.
              unhidden: \(unhidden.roleHistogram)
              hidden:   \(hidden.roleHistogram)
            """)
        XCTAssertGreaterThan(unhidden.count, hidden.count,
                             "the unhidden tree is not the larger of the two — the difference is not the bar")
    }
}
#endif
