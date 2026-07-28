// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The CAPTURE CARD gate (issue #765) — the panel golden lane's one structural blind spot, closed.
//
// WHAT WAS MISSING. SwiftUI `ImageRenderer` cannot rasterize the AppKit-backed `TextField` in the issue
// #360 capture affordance: it draws a blank placeholder box. So the panel golden gate (#754), which
// otherwise certifies every panel state, is blind to precisely the card a FIRST-RUN operator meets — and
// `design/README.md` carried that as a standing harness limitation with a manual-check note attached.
//
// AC-1'S ROUTE, MEASURED RATHER THAN ASSUMED. Issue #765 offers four options (metrics assertions, a
// non-`ImageRenderer` raster path, the #761 XCUITest outcome, or manual-with-reason) and the correct
// answer turns out not to need the manual escape hatch at all, because the issue's framing conflates two
// different limitations:
//
//   • `ImageRenderer` cannot RASTERIZE the field. True, and unchanged.
//   • …therefore the field cannot be VERIFIED. False. Rasterization is one way to observe a view, not the
//     only one. Issue #758 established that the accessibility tree is reachable IN-PROCESS from this
//     headless bundle, and its `testEveryInteractiveSurfaceIsExposedAsAButton` already finds the capture
//     field on the `empty-roster` fixture and reads `AXTextField` off it. A view that draws as a blank
//     box is fully present in the tree; the blankness is a property of the raster, not of the view.
//
// So this suite verifies the card through two lanes that never need a pixel, both already inside the
// REQUIRED `swift` CI job:
//
//   1. TREE (issue #758's walker). Reach, role, and enablement of the field and the button, at EVERY
//      capture phase — idle, pending, done, failed. That phase coverage is genuinely new even against
//      #758: `PanelRenderHarness` has ONE `empty-roster` fixture, sitting at the model's default idle
//      phase, so no existing gate has ever seen this card in flight or after a failure. The
//      disabled-while-pending state in particular is a property a raster cannot express at all.
//   2. METRICS (issue #750/#762 `TextMetrics`). Every shipped string measured against the card's derived
//      text budget, including the full Dynamic Type sweep — the card carries the panel's longest prose,
//      so it is where a scale-up overflow would land first.
//
// The card is driven DIRECTLY here rather than through a new `PanelRenderHarness` fixture. That is
// deliberate: adding capture-phase fixtures would re-baseline 34 committed goldens and bump #758's
// shape-pin cardinality guard, for phases whose whole point is that the golden lane cannot see them.
//
// WHAT THIS DOES NOT CLAIM. Not that the card LOOKS right — the goldens own drift and the mock
// (`design/menubar-preview.html`, which does frame this card) owns fidelity, and neither is this. Not
// that SwiftUI's layout pass produces these exact widths: like `PanelTextMetricsTests`, this models the
// layout with the same CoreText primitives AppKit shapes through, and that distinction is stated plainly
// because the neighbouring `BarGlyphParityTests` header had to be corrected once (#749) for asserting an
// unexecuted belief as settled fact. And not the real popover's focus, keystroke routing or Esc/Return
// handling, which need a live `NSPanel` — those stay in `design/README.md`'s manual checklist.
//
// CONSTRAINT-A (issue #748): every predicate below ships a MUTATION canary driven through the SAME
// function the real assertion calls. The precedent is issue #437 — three render bugs misread five times
// as "the DESIGN fails distinctness", where a golden authored at that moment would have DEFENDED them.

#if DEBUG
import AppKit
import CoreText
import SwiftUI
import XCTest

@MainActor
final class PanelCaptureCardTests: XCTestCase {

    /// Roomy enough that nothing is clipped by the HOST rather than by the layout under test.
    private let cardSize = CGSize(width: 380, height: 460)

    /// The most lines any wrapping capture string may occupy in a correctly-scaled card.
    ///
    /// MEASURED, not chosen: across all 12 `DynamicTypeSize` classes the worst shipped string wraps to
    /// exactly 2 lines, so 3 is one line of headroom. That margin is what makes this a gate rather than a
    /// tautology — `testTheWrapGateFiresOnAnUnscaledCard` trips it at 4.
    private static let wrapLineBound = 3

    // MARK: - Fonts (each pinned to the view site whose `.font(...)` it mirrors)
    //
    // Rebuilt from the SAME (points, weight) pairs `StatusPanelCapture` passes to `Font.panel(_:_:scale:)`
    // / `Font.panel(style:_:scale:)`, so this measures the shipped typography rather than a parallel model
    // of it. Text-style sizes are READ at runtime for the same reason `PanelTextMetricsTests` reads them:
    // freezing today's numbers would stop tracking the platform metric.

    private func titleFont(_ k: Double = 1) -> NSFont {
        NSFont.systemFont(ofSize: NSFont.preferredFont(forTextStyle: .subheadline).pointSize * k,
                          weight: .semibold)
    }
    private func explainerFont(_ k: Double = 1) -> NSFont {
        NSFont.systemFont(ofSize: NSFont.preferredFont(forTextStyle: .caption1).pointSize * k,
                          weight: .regular)
    }
    private func hintFont(_ k: Double = 1) -> NSFont { NSFont.systemFont(ofSize: 10.5 * k, weight: .regular) }
    private func fieldFont(_ k: Double = 1) -> NSFont { NSFont.systemFont(ofSize: 12 * k, weight: .regular) }
    private func buttonFont(_ k: Double = 1) -> NSFont { NSFont.systemFont(ofSize: 12 * k, weight: .semibold) }
    private func statusFont(_ k: Double = 1) -> NSFont { NSFont.systemFont(ofSize: 11 * k, weight: .regular) }

    // MARK: - The card's strings, by how the view lays each one out
    //
    // The distinction is the whole point of splitting these two lists, and it is not cosmetic: a `Text`
    // with `.fixedSize(horizontal: false, vertical: true)` WRAPS and its container grows, while a
    // single-line element that overruns its width is CLIPPED or elided. Measuring width alone would
    // report "overflows" for both and hide which is happening — so each list is judged by the predicate
    // that matches its layout.

    /// Strings the view lets WRAP (`.fixedSize(horizontal: false, vertical: true)`).
    private func wrappingStrings(_ k: Double) -> [(name: String, text: String, font: NSFont)] {
        [("explainer", StatusPanelFormat.captureCardExplainer, explainerFont(k)),
         ("scope hint", StatusPanelFormat.captureScopeHint, hintFont(k))]
            + captureFailures.map {
                ("error status (\($0.0))", StatusPanelFormat.captureErrorText($0.1), statusFont(k))
            }
    }

    /// Strings the view lays out on ONE line, which must therefore FIT.
    private func singleLineStrings(_ k: Double) -> [(name: String, text: String, font: NSFont)] {
        [("onboarding title", StatusPanelFormat.captureCardOnboardingTitle, titleFont(k)),
         ("add-account title", StatusPanelFormat.captureCardAddAccountTitle, titleFont(k)),
         ("field placeholder", StatusPanelFormat.captureFieldPlaceholder, fieldFont(k)),
         ("button title", StatusPanelFormat.captureButtonTitle, buttonFont(k)),
         ("button pending", StatusPanelFormat.capturePendingText, buttonFont(k))]
    }

    /// Every `CaptureFailure` the affordance can render, enumerated by hand because the type carries
    /// associated values and cannot be `CaseIterable`. `testEveryCaptureFailureIsEnumerated` is the guard
    /// that this list has not fallen behind the enum.
    private let captureFailures: [(String, CaptureFailure)] = [
        ("rejected/noActiveAccount", .rejected(.noActiveAccount)),
        ("rejected/keychainLocked", .rejected(.keychainLocked)),
        ("rejected/swapLockBusy", .rejected(.swapLockBusy)),
        ("rejected/failed", .rejected(.failed)),
        ("daemonError/unauthorized", .daemonError("unauthorized")),
        ("daemonError/other", .daemonError("something-else")),
        ("transport/connectionRefused", .transport(.connectionRefused(reason: "ECONNREFUSED"))),
        ("transport/timedOut", .transport(.timedOut)),
        ("transport/closedBeforeAck", .transport(.closedBeforeAck)),
        ("transport/encodeFailed", .transport(.encodeFailed(reason: "not encodable"))),
        ("transport/io", .transport(.io(reason: "EIO"))),
        ("undecodable", .undecodable),
        ("unavailable", .unavailable),
    ]

    // MARK: - The derived budget matches the shipped geometry (guards the arithmetic itself)

    /// `captureCardTextBudget` is a DERIVED expression, so a change to any panel-geometry constant
    /// silently moves every measurement below it. Pinning the arithmetic means a geometry change reddens
    /// HERE, with the derivation named, instead of quietly re-scaling the whole gate underneath itself.
    func testTheDerivedCaptureCardBudgetMatchesTheShippedGeometry() {
        XCTAssertEqual(StatusPanelFormat.panelContentWidth, 380, accuracy: 0.001)
        XCTAssertEqual(StatusPanelFormat.captureCardHorizontalInset, 12, accuracy: 0.001,
                       "the `.padding(.horizontal,)` both StatusPanelView call sites apply to the card")
        XCTAssertEqual(StatusPanelFormat.captureCardPadding, 12, accuracy: 0.001,
                       "CaptureCard's own `.padding()`")
        // 380 − 2 × 12 (card inset) − 2 × 12 (card padding)
        XCTAssertEqual(StatusPanelFormat.captureCardTextBudget, 332, accuracy: 0.001,
                       "the capture-card text budget derivation changed — re-derive, do not re-tune tests")
    }

    // MARK: - AC-1 (metrics lane): every shipped string fits, or wraps rather than clips

    func testEverySingleLineCaptureStringFitsTheCardBudget() {
        let budget = StatusPanelFormat.captureCardTextBudget
        var checked = 0
        for cell in singleLineStrings(1) {
            assertFits(cell.text, cell.font, budget: budget, "capture card \(cell.name)")
            checked += 1
        }
        XCTAssertEqual(checked, 5, "expected 5 single-line capture strings, measured \(checked)")
    }

    /// The wrapping strings must WRAP — bounded, and within a line budget the card can host — rather than
    /// run away vertically inside a panel whose height is finite.
    func testEveryWrappingCaptureStringWrapsWithinTheCard() {
        let budget = StatusPanelFormat.captureCardTextBudget
        var checked = 0
        for cell in wrappingStrings(1) {
            let wrapped = TextMetrics.wrapped(cell.text, cell.font, budget: budget)
            XCTAssertTrue(wrapped.bounded, """
                capture card \(cell.name) did not fit the 10 000 pt probe height, so its measured line \
                count is a FLOOR rather than a total and every verdict about it here is unsound.
                """)
            XCTAssertGreaterThan(wrapped.lines, 0, "capture card \(cell.name) measured as zero lines")
            XCTAssertLessThanOrEqual(wrapped.lines, Self.wrapLineBound, """
                capture card \(cell.name) wraps to \(wrapped.lines) lines in a \
                \(String(format: "%.2f", budget)) pt card. It will not clip — the card grows — but the \
                panel does not, so this is the copy running past what the onboarding state can host. \
                Shorten the copy rather than raising this bound.
                  text: "\(cell.text)"
                """)
            checked += 1
        }
        XCTAssertEqual(checked, 2 + captureFailures.count,
                       "expected \(2 + captureFailures.count) wrapping strings, measured \(checked)")
    }

    /// The done line is the ONE capture string that carries an identity (the label the daemon assigned),
    /// so it is `.lineLimit(1)` under the shipped elision policy. A long handle must ELIDE, not clip —
    /// and a realistic one must not elide at all.
    func testTheDoneStatusLineElidesALongHandleUnderTheShippedPolicy() {
        let budget = StatusPanelFormat.captureCardTextBudget
        let font = statusFont()
        let mode: CTLineTruncationType = StatusPanelFormat.identityElision == .middle ? .middle : .end

        let realistic = StatusPanelFormat.captureDoneText(label: "oleksii@company-one.com")
        assertFits(realistic, font, budget: budget, "capture done line (realistic handle)")

        // A pathological handle: the wire's label is operator-chosen, so length is not bounded upstream.
        let long = StatusPanelFormat.captureDoneText(label: String(repeating: "handle-", count: 40))
        XCTAssertTrue(TextMetrics.overflows(long, font, budget: budget),
                      "the pathological fixture fits — this test would prove nothing about elision")
        let elided = TextMetrics.visibleSubstring(long, font: font, budget: budget, mode: mode)
        XCTAssertTrue(elided.truncated, "a string measured as overflowing came back un-truncated")
        XCTAssertFalse(elided.text.isEmpty, "elision consumed the entire line")
        XCTAssertTrue(TextMetrics.width(elided.text, font) <= budget,
                      "the elided remainder still exceeds the cell, so it CLIPS rather than eliding")
    }

    /// The failure list above is hand-maintained (associated values rule out `CaseIterable`), so it can
    /// silently fall behind the enum — and a `switch` in `StatusPanelFormat` would still compile. This
    /// reads that switch's SOURCE and requires every case it handles to appear in the fixture list.
    func testEveryCaptureFailureIsEnumerated() throws {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/StatusPanelFormat.swift")
        let source = try XCTUnwrap(try? String(contentsOf: url, encoding: .utf8),
                                   "could not read StatusPanelFormat.swift")
        guard let start = source.range(of: "static func captureErrorText"),
              let end = source.range(of: "// MARK: - Capture CARD copy") else {
            return XCTFail("could not bound captureErrorText — re-point this assertion")
        }
        let body = source[start.lowerBound..<end.lowerBound]
        let rendered = Set(captureFailures.map { StatusPanelFormat.captureErrorText($0.1) })

        // Every `return "…"` in the mapper must be reachable from the fixture list, so a NEW failure copy
        // that nothing above renders reddens here instead of going unmeasured.
        let literals = Self.returnedLiterals(in: String(body))
        XCTAssertFalse(literals.isEmpty, "extracted no copy at all — the extractor is not matching")
        XCTAssertTrue(literals.isSubset(of: rendered), """
            captureErrorText renders copy no fixture in `captureFailures` reaches, so that copy is never \
            measured against the card:
              unmeasured: \(literals.subtracting(rendered).sorted())
            Add the missing CaptureFailure case to the list above.
            """)
    }

    /// The first `"…"` of every `return "…"` line, as the enumeration pin reads them out of source.
    /// `static` so the canary below drives the identical extractor over synthetic source.
    ///
    /// Same tripwire caveat as its sibling in `NotificationDeliveryTests`: it catches the realistic
    /// regression — a new `return "…"` arm nothing renders — not copy assembled from a variable or
    /// interpolated from another function.
    private static func returnedLiterals(in source: String) -> Set<String> {
        var literals: Set<String> = []
        for line in source.split(separator: "\n") {
            guard let open = line.range(of: "return \"") else { continue }
            let rest = line[open.upperBound...]
            guard let close = rest.range(of: "\"") else { continue }
            literals.insert(String(rest[..<close.lowerBound]))
        }
        return literals
    }

    /// CANARY for the enumeration pin: an unrendered `return "…"` arm must escape the fixture list, and a
    /// fully-rendered mapper must not. Both verdicts through `returnedLiterals`, the extractor the real
    /// assertion uses — a non-vacuity guard alone would not show the pin can actually go red.
    func testTheFailureEnumerationPinFiresOnAnUnrenderedFailureArm() {
        let rendered: Set<String> = ["Could not reach the daemon.", "The daemon refused the capture."]

        let complete = """
            case .connectionRefused: return "Could not reach the daemon."
            case .rejected:          return "The daemon refused the capture."
            """
        XCTAssertTrue(Self.returnedLiterals(in: complete).isSubset(of: rendered),
                      "the pin reddens on a mapper every fixture already renders, so it fires on everything")

        let withNewArm = complete + "\ncase .timedOut: return \"The capture timed out.\""
        let extracted = Self.returnedLiterals(in: withNewArm)
        XCTAssertEqual(extracted.subtracting(rendered), ["The capture timed out."],
                       "the extractor missed the new arm, so the real assertion's verdict is a statement "
                       + "about what it happened to match rather than about the mapper")
        XCTAssertFalse(extracted.isSubset(of: rendered),
                       "the pin accepted failure copy no fixture renders — it would go unmeasured")
    }

    // MARK: - AC-1 (metrics lane): the Dynamic Type sweep

    // Same reasoning as `PanelTextMetricsTests`' AC-3 sweep: the card and its text both scale by one
    // factor, but GLYPH ADVANCE IS NOT LINEAR IN POINT SIZE, so the two sides drift by fractions of a
    // point at every step. This card carries the panel's longest prose, so it is where that drift crosses
    // an edge first.

    func testEveryCaptureStringStillFitsAtEveryDynamicTypeSizeClass() {
        var checked = 0
        for size in DynamicTypeSize.allCases {
            let k = PanelTypeScale.factor(for: size)
            let budget = StatusPanelFormat.captureCardTextBudget * k
            for cell in singleLineStrings(k) {
                assertFits(cell.text, cell.font, budget: budget,
                           "capture card \(cell.name) at \(size) (k=\(String(format: "%.4f", k)))")
                checked += 1
            }
            for cell in wrappingStrings(k) {
                let wrapped = TextMetrics.wrapped(cell.text, cell.font, budget: budget)
                XCTAssertTrue(wrapped.bounded, "\(cell.name) at \(size) exceeded the probe height")
                XCTAssertLessThanOrEqual(wrapped.lines, Self.wrapLineBound, """
                    capture card \(cell.name) wraps to \(wrapped.lines) lines at \(size) \
                    (k=\(String(format: "%.4f", k))). The card grows to fit, but the panel does not.
                    """)
                checked += 1
            }
        }
        // Degenerate-subject guard, EXACT rather than non-zero: a sweep that silently evaluated one size
        // class would otherwise pass and prove nothing.
        let perClass = 5 + 2 + captureFailures.count
        XCTAssertEqual(checked, DynamicTypeSize.allCases.count * perClass,
                       "expected \(DynamicTypeSize.allCases.count * perClass) measurements, ran \(checked)")
    }

    // MARK: - CONSTRAINT-A: the metrics lane PROVES it can fail, in the same run it passes

    // A MEASURED FACT THAT SHAPES BOTH CANARIES BELOW, recorded rather than quietly rounded. The obvious
    // mutation for a Dynamic Type sweep — the one `PanelTextMetricsTests` uses — is "scaled font, UNSCALED
    // cell", the defect you get by scaling the typography and forgetting the frame constants. Measured
    // here, that mutation trips only ONE of this card's two lanes:
    //
    //   • WRAPPING lane: it trips, decisively. The explainer wraps to 2 lines in a correctly-scaled card
    //     at every size class, and to 4 in an unscaled one — past the bound the real assertion enforces.
    //   • SINGLE-LINE lane: it does NOT trip. Two different measurements bound that, and they are easy to
    //     fuse into one wrong sentence, so both are stated. (1) In the SHIPPED configuration the widest
    //     single-line string uses at most ~43 % of its budget — the classes are not equally tight, because
    //     glyph advance is not linear in point size, so the maximum ratio sits at a SMALL class, not the
    //     largest one. (2) Under the MUTATION the tightest case is the onboarding title at
    //     .accessibility3 — 305.50 pt measured against the UNSCALED 332 pt card, about 92 % of it, still
    //     inside. The remaining margin is small, which is exactly why (2) is not left as a hand-copied
    //     number: `testTheScaleMutationIsProvablyInertOnTheSingleLineLane` re-measures it every run.
    //
    // Asserting that the single-line lane trips under that mutation would be a FABRICATED failure, so it
    // is not asserted. Its falsifier is an over-wide FIXTURE instead — the same shape
    // `PanelTextMetricsTests.testTheCanaryOverflowsWhileEveryShippedLabelFits` uses — which proves the
    // `overflows` predicate can fire without pretending the shipped copy is near an edge it is not.

    /// The claim above — that the scale mutation CANNOT trip the single-line lane — measured rather than
    /// transcribed. Every shipped single-line string is measured under `.accessibility3` type against the
    /// UNSCALED card, and none may overflow.
    ///
    /// Going red here is GOOD news, not a defect: it means the margin has closed and the over-wide fixture
    /// canary below can be replaced by the same scale mutation the wrapping lane uses, which proves more.
    func testTheScaleMutationIsProvablyInertOnTheSingleLineLane() {
        let k = PanelTypeScale.factor(for: .accessibility3)
        let budget = StatusPanelFormat.captureCardTextBudget
        var worst = (name: "none", ratio: 0.0)
        var checked = 0
        for cell in singleLineStrings(k) {
            checked += 1
            let ratio = TextMetrics.width(cell.text, cell.font) / budget
            if ratio > worst.ratio { worst = (cell.name, ratio) }
            XCTAssertFalse(TextMetrics.overflows(cell.text, cell.font, budget: budget), """
                capture card \(cell.name) overflows the UNSCALED card under .accessibility3 type. The \
                scale mutation is no longer inert on this lane — swap the over-wide fixture canary below \
                for that mutation, and update the note above.
                """)
        }
        XCTAssertGreaterThan(checked, 0, "measured nothing — the verdict above would be vacuous")
        // Same claim, carrying the number: this is where the 92 % in the note above comes from, and it is
        // re-derived on every run instead of rotting in a comment.
        XCTAssertLessThan(worst.ratio, 1.0,
                          "widest mutated string is \(worst.name) at "
                          + "\(String(format: "%.0f", worst.ratio * 100)) % of the unscaled card")
    }

    /// MUTATION for the WRAPPING predicate: an unscaled card under scaled type must exceed the line bound
    /// the real assertion enforces, through the SAME `TextMetrics.wrapped` call.
    func testTheWrapGateFiresOnAnUnscaledCard() {
        let k = PanelTypeScale.factor(for: .accessibility3)
        XCTAssertGreaterThan(k, 1.0, "the ceiling factor is not an enlargement — this mutation is inert")

        let text = StatusPanelFormat.captureCardExplainer
        let scaledFont = explainerFont(k)
        // The MUTATION: keep the scaled font, revert the card to its DEFAULT width.
        let mutated = TextMetrics.wrapped(text, scaledFont, budget: StatusPanelFormat.captureCardTextBudget)
        XCTAssertTrue(mutated.bounded, "the mutation fixture exceeded the probe height — count is a floor")
        XCTAssertGreaterThan(mutated.lines, Self.wrapLineBound, """
            the .accessibility3 explainer wraps to only \(mutated.lines) lines in an UNSCALED \
            \(String(format: "%.2f", StatusPanelFormat.captureCardTextBudget)) pt card, within the bound \
            of \(Self.wrapLineBound) the real assertion enforces. The sweep would then stay green even if \
            the card's padding constants were never scaled — this lane is not actually gated.
            """)
        // CONTROL, same run: with the card scaled too, the identical string is back inside the bound.
        let control = TextMetrics.wrapped(text, scaledFont,
                                          budget: StatusPanelFormat.captureCardTextBudget * k)
        XCTAssertLessThanOrEqual(control.lines, Self.wrapLineBound,
                                 "the control (scaled card) is already over the bound, so the mutation "
                                 + "above proves nothing about the card's scaling")
    }

    /// CANARY for the SINGLE-LINE predicate: an over-wide fixture must overflow while every shipped string
    /// fits, in the same run — both verdicts through the same `TextMetrics.overflows`.
    func testTheOverflowPredicateFiresOnAnOverWideFixtureWhileEveryShippedStringFits() {
        let budget = StatusPanelFormat.captureCardTextBudget
        let font = buttonFont()

        let overWide = String(repeating: "Capture active account ", count: 4)
        XCTAssertTrue(TextMetrics.overflows(overWide, font, budget: budget), """
            a string four times the widest shipped button title does not overflow the card, so \
            `overflows` cannot report a failure here and every "it fits" verdict above is vacuous.
            """)
        for cell in singleLineStrings(1) {
            assertFits(cell.text, cell.font, budget: budget, "capture card \(cell.name) (canary control)")
        }
    }

    // MARK: - AC-1 (tree lane): the field ImageRenderer cannot draw is REACHABLE, at every phase
    //
    // WHICH ATTRIBUTE EACH ELEMENT PUBLISHES ITS TEXT ON, measured — because guessing it wrong is the
    // exact failure issue #758's header calls out and issue #761's spike lost a round to. In this card:
    //
    //   • the three `Text`s publish their string as `value`, with an empty `label`;
    //   • the `TextField` and the `Button` publish their `.accessibilityLabel(…)` as `label`, with an
    //     empty `value`.
    //
    // The consequence that matters: `StatusPanelFormat.captureButtonTitle` ("Capture active account", the
    // SF `Label`'s text) NEVER appears in the tree — the button's `.accessibilityLabel` ("Capture THE
    // active account") replaces it. Querying for the rendered title therefore returns nothing and reads
    // as "the button is absent". `A11yNode.text` folds both attributes, and `assertReachable` is what
    // stops a wrong-attribute query from being reported as an absence.

    /// The headline claim. `ImageRenderer` draws this field as a blank box; the tree sees a real,
    /// correctly-typed, enabled text field.
    func testTheCaptureFieldImageRendererCannotRasterizeIsInTheAccessibilityTree() throws {
        let nodes = idleTree()
        assertReachable(nodes, StatusPanelFormat.captureCardOnboardingTitle, "the capture-field reach check")

        let field = try XCTUnwrap(nodes.firstContaining(StatusPanelFormat.captureFieldAccessibilityLabel), """
            the operator-label field is not in the accessibility tree at all. This is the field the panel \
            golden gate cannot rasterize, so the tree is the ONLY automated evidence it exists — a \
            VoiceOver user would have no way to reach the first-run capture input.
              tree: \(nodes)
            """)
        XCTAssertEqual(field.role, "AXTextField", """
            the capture label field publishes '\(field.role)', not AXTextField — VoiceOver would not \
            announce it as an editable field on the very first screen a new operator sees.
            """)
        XCTAssertTrue(field.enabled, "the capture field is disabled at rest, so a label cannot be typed")
    }

    /// The card's tree SHAPE, pinned as a role histogram.
    ///
    /// The companion to the per-element checks above, and not redundant with them: they name the two
    /// elements they expect, so a decorative view leaking INTO the card — a status glyph that lost its
    /// `.accessibilityHidden(true)`, say — passes them all. This reddens on anything entering or leaving,
    /// whatever its type. Role-only and string-free on purpose, so a copy edit does not break a test that
    /// was not testing copy (issue #761's copy-coupling warning); the metrics lane owns the strings.
    func testTheCaptureCardAccessibilityShapeIsUnchanged() {
        let nodes = idleTree()
        assertReachable(nodes, StatusPanelFormat.captureCardOnboardingTitle, "the card shape pin")
        XCTAssertEqual(nodes.roleHistogram, "AXButton:1 AXStaticText:3 AXTextField:1", """
            the capture card changed accessibility shape.
              observed: \(nodes.roleHistogram)
            The three static texts are the title, the explainer and the scope hint; the field and the \
            button are the affordance. An element entered or left: if a decorative view lost its \
            `.accessibilityHidden(true)`, restore it — if the card genuinely gained a control, update this \
            pin and #758's `empty-roster` fixture pin in the same commit.
            """)
    }

    /// Both entry points (#360 onboarding, #394 "Add account") host the same affordance, and differ only
    /// in title. Asserted rather than assumed: the #394 surface is not in ANY render fixture, so nothing
    /// else in the suite covers it.
    func testBothCaptureEntryPointsPublishTheSameAffordance() throws {
        for title in [StatusPanelFormat.captureCardOnboardingTitle,
                      StatusPanelFormat.captureCardAddAccountTitle] {
            let nodes = idleTree(title: title)
            assertReachable(nodes, title, "the \(title) entry point")
            XCTAssertNotNil(nodes.firstContaining(StatusPanelFormat.captureFieldAccessibilityLabel),
                            "the '\(title)' card has no label field")
            let button = try XCTUnwrap(nodes.firstContaining(
                StatusPanelFormat.captureButtonAccessibilityLabel(pending: false)),
                                       "the '\(title)' card has no capture button")
            XCTAssertEqual(button.role, "AXButton", "the '\(title)' capture button publishes \(button.role)")
        }
    }

    /// The phase coverage no existing gate has: in flight, both controls must read DISABLED.
    ///
    /// A raster cannot express this at all — a disabled field and an enabled one differ by a tint, and the
    /// field does not rasterize in the first place. This is the assertion that makes the tree lane worth
    /// more than a screenshot rather than merely equivalent to one.
    func testTheFieldAndButtonAreDisabledWhileACaptureIsInFlight() async throws {
        let connection = GatedCaptureCardConnection(ack: #"{"result":"captured","label":"work"}"#)
        let model = AccountCaptureModel(
            client: ControlCommandClient(connector: CaptureCardOneShotConnector(connection: connection),
                                         timeout: .seconds(10)))
        let task = Task { await model.capture(rawLabel: "work") }
        try await waitUntil({ model.phase.isPending }, "pending")

        let nodes = tree(model: model)
        // In flight the button's accessibility label CHANGES, so the resting one is the wrong anchor —
        // pin a string that is present in every phase instead.
        assertReachable(nodes, StatusPanelFormat.captureCardOnboardingTitle, "the in-flight phase")

        let field = try XCTUnwrap(nodes.firstContaining(StatusPanelFormat.captureFieldAccessibilityLabel),
                                  "the label field left the tree while a capture was in flight")
        XCTAssertFalse(field.enabled, """
            the label field is still enabled mid-capture. Its `.disabled(capture.phase.isPending)` is what \
            stops an operator editing the label a request already carries, and a VoiceOver user \
            navigating by control state would be told it is editable.
            """)
        let button = try XCTUnwrap(nodes.firstContaining(
            StatusPanelFormat.captureButtonAccessibilityLabel(pending: true)), """
            the in-flight button does not publish its pending accessibility label, so a VoiceOver user is \
            still told the capture is on offer while it is already running.
            """)
        XCTAssertFalse(button.enabled, "the capture button is still enabled mid-capture — a double-submit")

        connection.release()
        await task.value
    }

    /// The terminal phases put their status copy in front of the operator. `.done` and `.failed` render
    /// different lines from `StatusPanelFormat`, and neither appears in any render fixture.
    func testTheDoneAndFailedStatusLinesReachTheTree() async throws {
        let done = try await capturedModel(ack: #"{"result":"captured","label":"work"}"#)
        let doneNodes = tree(model: done)
        assertReachable(doneNodes, StatusPanelFormat.captureCardOnboardingTitle, "the done phase")
        XCTAssertNotNil(doneNodes.firstContaining(StatusPanelFormat.captureDoneText(label: "work")), """
            the success confirmation is not in the tree, so a VoiceOver user gets no acknowledgement that \
            the capture landed.
            """)

        let failed = try await capturedModel(ack: #"{"result":"rejected","reason":"keychain-locked"}"#)
        let failedNodes = tree(model: failed)
        assertReachable(failedNodes, StatusPanelFormat.captureCardOnboardingTitle, "the failed phase")
        XCTAssertNotNil(
            failedNodes.firstContaining(StatusPanelFormat.captureErrorText(.rejected(.keychainLocked))), """
            the failure copy is not in the tree, so a capture that was REJECTED reads as one that simply \
            did nothing — the honest-error surface the affordance exists to provide.
            """)
    }

    // MARK: - CONSTRAINT-A: the tree lane PROVES it can fail
    //
    // Every assertion above is an absence-or-presence claim over a walked tree, and a tree that came back
    // empty (activation regressed, environment injection dropped) satisfies "is absent" perfectly and
    // makes "is present" fail loudly. `assertReachable` guards the second; these guard the predicates.

    /// MUTATION — the role predicate must distinguish a real `TextField` from a plain `Text` carrying the
    /// same accessibility label, or `AXTextField` above would be true of any labelled element.
    func testTheFieldRolePredicateDistinguishesATextFieldFromLabelledText() {
        struct Impostor: View {
            var body: some View {
                VStack {
                    Text("CARD_ANCHOR")
                    Text("").accessibilityLabel(StatusPanelFormat.captureFieldAccessibilityLabel)
                }
            }
        }
        let nodes = PanelA11y.tree(for: Impostor(), size: CGSize(width: 240, height: 120))
        assertReachable(nodes, "CARD_ANCHOR", "the field-role canary")
        let impostor = nodes.firstContaining(StatusPanelFormat.captureFieldAccessibilityLabel)
        XCTAssertNotEqual(impostor?.role, "AXTextField", """
            a plain Text carrying the field's accessibility label published AXTextField. The role check on \
            the real card would then pass on an element the operator cannot type into.
            """)
    }

    /// MUTATION — the enablement predicate must actually track `.disabled(…)`, or the in-flight assertion
    /// is reading a flag that never changes.
    func testTheEnablementPredicateTracksDisabledState() throws {
        struct Subject: View {
            let disabled: Bool
            var body: some View {
                VStack {
                    Text("CARD_ANCHOR")
                    TextField("placeholder", text: .constant(""))
                        .accessibilityLabel("CANARY_FIELD")
                        .disabled(disabled)
                }
            }
        }
        let live = PanelA11y.tree(for: Subject(disabled: false), size: CGSize(width: 240, height: 120))
        let dead = PanelA11y.tree(for: Subject(disabled: true), size: CGSize(width: 240, height: 120))
        assertReachable(live, "CARD_ANCHOR", "the enablement canary (live)")
        assertReachable(dead, "CARD_ANCHOR", "the enablement canary (disabled)")

        XCTAssertTrue(try XCTUnwrap(live.firstContaining("CANARY_FIELD")).enabled)
        XCTAssertFalse(try XCTUnwrap(dead.firstContaining("CANARY_FIELD")).enabled, """
            `.disabled(true)` did not change what the tree reports, so the in-flight assertion above \
            cannot fail — it would report a still-editable field as correctly disabled.
            """)
    }

    // MARK: - Helpers

    /// The card at rest. A client-less model is permanently `.idle` — it cannot start a capture at all —
    /// so this is the resting card with no fake wiring in the way.
    private func idleTree(title: String = StatusPanelFormat.captureCardOnboardingTitle) -> [A11yNode] {
        tree(model: AccountCaptureModel(client: nil), title: title)
    }

    /// The tree for the capture card, hosted exactly as the panel hosts it.
    private func tree(model: AccountCaptureModel,
                      title: String = StatusPanelFormat.captureCardOnboardingTitle) -> [A11yNode] {
        PanelA11y.tree(for: CaptureCard(title: title).environmentObject(model), size: cardSize)
    }

    /// The absence-trap guard, mirroring `PanelAccessibilityTreeTests.assertKnownPresent`: an activation or
    /// injection failure yields an empty tree, which satisfies every "is absent" check perfectly. The
    /// PREDICATE (`firstContaining`) is the shared one from `PanelAccessibilityTreeTests`; only this thin
    /// assertion wrapper is local, so the two files cannot disagree about what "found" means.
    private func assertReachable(_ nodes: [A11yNode], _ needle: String, _ context: String,
                                 file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertNotNil(nodes.firstContaining(needle), """
            ABSENCE EVIDENCE VOID for \(context): the known-present anchor '\(needle)' is missing from a \
            tree of \(nodes.count) node(s). Any verdict from this dump is vacuous — the tree is empty or \
            the capture model was not injected, not clean. Check PanelA11y.activate() first.
            """, file: file, line: line)
    }

    /// Run one capture to completion against a canned ack, leaving the model in its terminal phase.
    private func capturedModel(ack: String) async throws -> AccountCaptureModel {
        let connection = GatedCaptureCardConnection(ack: ack)
        let model = AccountCaptureModel(
            client: ControlCommandClient(connector: CaptureCardOneShotConnector(connection: connection),
                                         timeout: .seconds(10)))
        let task = Task { await model.capture(rawLabel: "work") }
        try await waitUntil({ model.phase.isPending }, "pending")
        connection.release()
        await task.value
        return model
    }

    /// Spin the cooperative executor until `predicate` holds (bounded), so a wiring bug fails the test
    /// instead of hanging — the same helper shape `AccountCaptureTests` uses.
    private func waitUntil(_ predicate: () -> Bool, _ label: String) async throws {
        for _ in 0..<10_000 {
            if predicate() { return }
            await Task.yield()
        }
        XCTFail("timed out waiting for \(label)")
    }
}

// MARK: - Test doubles
//
// Local rather than shared with `AccountCaptureTests`: that file's equivalents are `private` to it, and
// widening their access purely for reuse would couple two suites through a test double. These are the
// same four-line shapes.

/// Returns one pre-built `WatchConnection`.
private struct CaptureCardOneShotConnector: WatchConnector {
    let connection: WatchConnection
    func connect() throws -> WatchConnection { connection }
}

/// A one-shot control-command connection that HOLDS its ack until `release()`, so a test can observe the
/// model's `.pending` window before the ack resolves it.
private final class GatedCaptureCardConnection: WatchConnection, @unchecked Sendable {
    let lines: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private let ack: String

    init(ack: String) {
        self.ack = ack
        (lines, continuation) = AsyncStream<String>.makeStream()
    }

    func send(_ bytes: [UInt8]) throws {}
    func release() { continuation.yield(ack); continuation.finish() }
    func close() { continuation.finish() }
}
#endif
