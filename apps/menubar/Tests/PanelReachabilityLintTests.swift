// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// SPIKE-2 (issue #981) — can a source-as-data predicate tell a VARIABLE-SOURCED size-class injection
// from a LITERAL one? This file is the answer: yes, within the bounds stated below and canaried.
//
// WHY THE OBVIOUS PREDICATE IS WRONG. The reachability gate (issue #982) wants to assert that
// `StatusItemController.swift` injects a size class at the `StatusPanelView()` construction site. The
// predicate that suggests itself — "the file contains `.dynamicTypeSize(`" — is satisfied by a
// hardcoded `.dynamicTypeSize(.large)`. `.large` maps to factor exactly 1.0
// (`StatusPanelTypeScale.swift`, `PanelTypeScale.factor`), so the panel renders IDENTICALLY to today
// and the gate is green over the very defect it exists to catch. The design records this as the
// non-literal-source clause (§ 5.2) and routes it here rather than assuming it is tractable.
//
// THE DISTINCTION IS SYNTACTIC AND THAT IS ENOUGH — FOR THIS QUESTION. A leading-dot argument
// (`.large`) is an enum-case member reference: a compile-time constant. An argument that starts with
// an identifier (`sizeClass`, `store.textSize`, `flag ? .large : .xLarge`) DERIVES from something the
// program can vary. That is a real, decidable textual difference, and it is exactly the difference
// between a dead injection and a live one. What it does NOT decide is whether the variable's source
// ever actually moves — design § 5.2 states that bound at length, and this file does not re-litigate
// it: a green verdict here means WIRED, never DELIVERED.
//
// THE RANGE OVERLOAD. SwiftUI overloads `.dynamicTypeSize(_:)` — one arm takes a value (an
// INJECTION), another takes a RANGE (a CLAMP, which drives nothing). The two are not distinguishable
// by "does the argument start with a dot", and the direction of the failure depends on how the range
// is spelled:
//
//   `.dynamicTypeSize(...PanelTypeScale.ceiling)`                     leads with `.` → read as a
//       LITERAL by a dot-prefix test, so that test FAILS it. Safe direction. This partial-range form
//       is the one `StatusPanelView.swift` ships today.
//   `.dynamicTypeSize(PanelTypeScale.floor...PanelTypeScale.ceiling)` leads with an IDENTIFIER → read
//       as VARIABLE-SOURCED, so a dot-prefix test PASSES it. That is the false accept, and it is why
//       `classify` tests for `...` BEFORE anything else and why `clampOnly` is a first-class verdict.
//
// An earlier revision of this file claimed the shipped `...ceiling` form was itself the trap. It is
// not — it starts with a dot, trivially, because `...` does. The hazard is the identifier-leading
// spelling. Correcting this matters beyond pedantry: a reader who checked the named example, found it
// already handled one line later, and concluded the range branch was redundant would delete a guard
// that IS load-bearing for the other spelling.
//
// THE FULLY-QUALIFIED LITERAL. `.dynamicTypeSize(DynamicTypeSize.large)` is as dead as `.large` and
// starts with an identifier, so the leading-dot test alone passes it. It is rejected explicitly.
// `SwiftUI.DynamicTypeSize.large` is rejected too, by stripping the module qualifier first, as are
// the parenthesised and whitespace-separated spellings (`(.large)`, `DynamicTypeSize .large`) — Swift
// permits both and a bare prefix test reads them as variable.
//
// TWO SPELLINGS REACH THE PANEL, NOT ONE. `.dynamicTypeSize(x)` is the obvious one.
// `.environment(\.dynamicTypeSize, x)` writes the same environment value and is at least as likely
// for a value the controller computes. A predicate that knew only the first would report a live
// key-path injection as `.absent` — under the defect-pin polarity design § 5.2 keeps live, `.absent`
// is the GREEN arm, so that miss is a false PASS, not a false alarm. Both are recognised here.
//
// SCOPE IS THE TOP-LEVEL CHAIN, not the file and not the span. A `.dynamicTypeSize(…)` anywhere in
// `StatusItemController.swift` satisfies a file-wide `contains`, including one on an unrelated view.
// But restricting to the construction expression's byte SPAN is not enough either: that span also
// contains every subview built inside a modifier's argument list, so
// `.overlay(Badge().dynamicTypeSize(x))` would be read as injecting into the panel when it injects
// into `Badge`. Only modifiers at chain depth 0 count here.
//
// THE HONEST BOUNDS. Three spellings are accepted as reachable although they are dead, and no text
// predicate can close them without type resolution (the T3 tier's job — design § 5.2, § 11):
//
//   1. a constant bound through a `let` — `let dead = DynamicTypeSize.large` … `.dynamicTypeSize(dead)`
//   2. a constant reached through a typealias — `typealias DTS = DynamicTypeSize` … `DTS.large`
//   3. a variable whose source never moves — the WIRED-vs-DELIVERED bound above
//
// Bound 1 is asserted below rather than merely admitted, so it cannot quietly widen. All three are
// recorded in `docs/findings/0981-reachability-predicate.md`.
//
// CONSTRAINT-A (issue #748) — NO GATE WITHOUT A PROVEN FALSIFIER. Every assertion runs through ONE
// function, `PanelReachabilityLint.verdict(in:)`, and the canaries drive that SAME function by
// MUTATION of the REAL `StatusItemController.swift` read from disk: the unmutated file is required to
// report `.absent` (no driver exists yet — issue #817 owns that), the same file with
// `.dynamicTypeSize(.large)` spliced into the chain is required to report `.deadLiteral`, and with
// `.dynamicTypeSize(panelSizeClass)` spliced in the same place, `.reachable`. Three outcomes, one
// function, real source text.
//
// HEADLESS, and NO BUNDLE CHANGE. This reads `StatusItemController.swift` as DATA — the same
// mechanism `PanelDynamicTypeLintTests` already uses on the same file, which it must in order to
// exempt it. `project.yml`'s exclusion of that file from `MenubarTests` bars a COMPILED gate, not a
// source-as-data one (design A-4 verdict: FEASIBLE). Foundation only; no window, no screen, no TCC.
// Redaction is REUSED from `PanelScaleLint.scan` rather than reimplemented, so comment- and
// string-handling cannot drift between the two lints over the same tree.

#if DEBUG
import Foundation
import XCTest

// MARK: - The predicate

/// Whether the panel's construction site injects a size class, and whether that injection is alive.
///
/// Pure and Foundation-only so the real assertions, the accept/reject tables, and the mutation
/// canaries all drive the identical predicate — the issue #750 discipline: the thing proven
/// falsifiable must be the thing actually gating.
enum PanelReachabilityLint {

    /// What the construction site says. Ordered by the gate's interest, not alphabetically.
    enum Verdict: Equatable {
        /// An injection whose argument derives from a variable or property. The positive gate PASSES.
        /// Means WIRED, not DELIVERED — see the file header.
        case reachable(line: Int, argument: String)
        /// An injection whose argument is a compile-time enum case (`.large`, `DynamicTypeSize.large`).
        /// Semantically dead: k = 1.0, indistinguishable from today. The positive gate FAILS.
        case deadLiteral(line: Int, argument: String)
        /// Only the RANGE overload is present — a ceiling/floor constraint, which drives nothing.
        /// Not an injection, and the identifier-leading spelling is what a naive test accepts.
        case clampOnly(line: Int, argument: String)
        /// No size-class injection in the construction chain at all. Today's shipped state, and the
        /// green arm of the defect-pin polarity (design § 5.2) if issue #971 returns "ship no driver".
        ///
        /// Carries the site's line because the spec's failure mode is "FAILS, naming the construction
        /// site" (`docs/specs/accessibility-reachability-gate.feature.md`, Rule 1) — the verdict that
        /// represents that failure is the one that most needs to name it.
        case absent(line: Int)
        /// The subject could not be evaluated. NEVER conflate with `.absent`: absent is a measurement,
        /// indeterminate is the absence of one (issue #748's degenerate-subject guard).
        case indeterminate(reason: String)
    }

    /// The construction site this gate is about.
    static let constructionToken = "StatusPanelView("
    /// The modifier that injects a size class directly.
    static let injectionModifier = "dynamicTypeSize"
    /// The modifier that injects the same value through the environment, and the key path it uses.
    static let environmentModifier = "environment"
    static let injectionKeyPath = "\\.dynamicTypeSize"

    // MARK: The one function

    /// Classify the size-class injection at the `StatusPanelView()` construction site in `source`.
    ///
    /// `source` is the TEXT of a Swift file — this never parses or type-checks. Comments and string
    /// literals are redacted first (via `PanelScaleLint.scan`) so neither can fake nor hide a match.
    static func verdict(in source: String) -> Verdict {
        let scanned = PanelScaleLint.scan(source)
        if scanned.unmodelledLiteral {
            return .indeterminate(reason: "file uses a multiline or raw string literal the scanner does not model")
        }
        let code = scanned.code

        // Degenerate-subject guard, both directions. Zero sites means the gate is asserting over
        // nothing; two or more means "the construction site" is not a well-defined subject and a
        // first-match rule would silently pick one.
        let sites = offsets(of: Array(constructionToken.utf8), in: code)
        guard sites.count == 1 else {
            return .indeterminate(reason: sites.isEmpty
                ? "no `\(constructionToken)` construction site found"
                : "\(sites.count) `\(constructionToken)` construction sites found — the site is ambiguous")
        }

        let chain = scanChain(in: code, startingAt: sites[0])
        guard case .ok(_, let modifiers) = chain else {
            guard case .failed(let reason) = chain else {
                return .indeterminate(reason: "the construction expression could not be walked")
            }
            return .indeterminate(reason: reason)
        }

        // Only modifiers at chain depth 0 count — `scanChain` returns exactly those, so an injection
        // nested inside some modifier's argument list is structurally not in this list.
        let found = modifiers.compactMap { modifier -> (line: Int, argument: String)? in
            guard let argument = injectedArgument(of: modifier) else { return nil }
            return (line: line(of: modifier.dot, in: code), argument: argument)
        }
        if found.isEmpty { return .absent(line: line(of: sites[0], in: code)) }

        // Precedence: a live injection anywhere in the chain wins; a dead one beats a mere clamp.
        // Deliberately NOT first-match — a clamp co-existing with a real injection is the shape the
        // panel is heading for (a ceiling plus a driver), and first-match would report the clamp.
        if let hit = found.first(where: { classify($0.argument) == .variable }) {
            return .reachable(line: hit.line, argument: hit.argument)
        }
        if let hit = found.first(where: { classify($0.argument) == .literal }) {
            return .deadLiteral(line: hit.line, argument: hit.argument)
        }
        return .clampOnly(line: found[0].line, argument: found[0].argument)
    }

    /// The size-class argument this top-level modifier injects into the panel, or `nil` for a
    /// modifier that injects none.
    ///
    /// Recognises BOTH spellings — see the file header. A predicate knowing only `.dynamicTypeSize`
    /// reports a live `.environment(\.dynamicTypeSize, …)` as `.absent`, which is the defect-pin
    /// polarity's GREEN arm.
    static func injectedArgument(of modifier: Modifier) -> String? {
        if modifier.name == injectionModifier {
            return modifier.argument.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        guard modifier.name == environmentModifier else { return nil }
        let parts = topLevelSplit(modifier.argument, on: ",")
        guard parts.count == 2, normalize(parts[0]) == injectionKeyPath else { return nil }
        return parts[1]
    }

    // MARK: Argument classification

    enum ArgumentKind: Equatable { case literal, variable, range }

    /// Classify one size-class argument, already redaction-clean.
    ///
    /// The range test comes FIRST because an identifier-leading range
    /// (`PanelTypeScale.floor...PanelTypeScale.ceiling`) would otherwise fall through to `variable` —
    /// a clamp read as a live injection, which is the false accept the header describes. The
    /// dot-leading spelling (`...PanelTypeScale.ceiling`) would be caught by the `hasPrefix(".")` test
    /// one line down, but as a LITERAL, which is the wrong verdict for a range even though it fails
    /// safe.
    static func classify(_ argument: String) -> ArgumentKind {
        let text = normalize(argument)
        if text.isEmpty { return .literal }              // `.dynamicTypeSize()` — no argument, nothing live
        if text.contains("...") { return .range }
        if text.hasPrefix(".") { return .literal }

        // A fully-qualified case (`DynamicTypeSize.large`, `SwiftUI.DynamicTypeSize.large`) is as dead
        // as `.large` and starts with an identifier, so the leading-dot test alone would pass it.
        var qualified = text
        if qualified.hasPrefix("SwiftUI.") { qualified.removeFirst("SwiftUI.".count) }
        if qualified.hasPrefix("DynamicTypeSize.") {
            let member = qualified.dropFirst("DynamicTypeSize.".count)
            // Exactly one member and nothing after it — `DynamicTypeSize.allCases.first!` is a call
            // chain whose value the program computes, so it stays `variable`.
            if !member.isEmpty && member.allSatisfy({ $0.isLetter || $0.isNumber || $0 == "_" }) {
                return .literal
            }
        }
        return .variable
    }

    /// Strip redundant enclosing parentheses and whitespace around member dots.
    ///
    /// Swift accepts `(.large)`, `((.large))`, `DynamicTypeSize .large` and a newline before the
    /// member dot. Each is a compile-time constant that a bare prefix test reads as variable, so
    /// normalising first is what makes the prefix test a classification rather than a spelling check.
    static func normalize(_ argument: String) -> String {
        var text = argument.trimmingCharacters(in: .whitespacesAndNewlines)
        while text.hasPrefix("("), text.hasSuffix(")"), outerParensEnclose(text) {
            text = String(text.dropFirst().dropLast()).trimmingCharacters(in: .whitespacesAndNewlines)
        }

        var collapsed = ""
        var pending = ""
        for character in text {
            if character.isWhitespace { pending.append(character); continue }
            if character == "." || collapsed.last == "." { pending = "" }
            collapsed += pending
            pending = ""
            collapsed.append(character)
        }
        return collapsed
    }

    /// Whether `text`'s FIRST `(` is closed by its LAST character — i.e. the outer parens really do
    /// enclose the whole expression, so stripping them is safe. `(a)(b)` must not be stripped.
    private static func outerParensEnclose(_ text: String) -> Bool {
        var depth = 0
        for (index, character) in text.enumerated() {
            if character == "(" { depth += 1 }
            if character == ")" {
                depth -= 1
                if depth == 0 { return index == text.count - 1 }
            }
        }
        return false
    }

    /// Split on a separator that is not nested inside parens, brackets or braces.
    static func topLevelSplit(_ text: String, on separator: Character) -> [String] {
        var parts: [String] = []
        var current = ""
        var depth = 0
        for character in text {
            switch character {
            case "(", "[", "{": depth += 1; current.append(character)
            case ")", "]", "}": depth -= 1; current.append(character)
            case separator where depth == 0: parts.append(current); current = ""
            default: current.append(character)
            }
        }
        parts.append(current)
        return parts.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
    }

    // MARK: The chain walk

    /// One modifier applied directly to the construction expression — chain depth 0.
    struct Modifier: Equatable {
        /// Byte offset of the leading `.`, used for the reported line number.
        let dot: Int
        let name: String
        /// Text between the group's parentheses; empty for a closure-only modifier such as `.onAppear`.
        let argument: String
    }

    enum ChainScan {
        case ok(span: Range<Int>, modifiers: [Modifier])
        case failed(reason: String)
    }

    /// Walk `StatusPanelView(` … `)` plus every modifier applied directly to it.
    ///
    /// Every step that cannot be traversed FAILS rather than ending the chain quietly. That matters
    /// more than it looks: silently stopping the walk early makes everything after it invisible, and
    /// under the defect-pin polarity an invisible injection reads as `.absent`, which is GREEN. A
    /// chain the walk cannot follow is not a measurement of "no driver" — it is no measurement.
    ///
    /// `siteOffset` points at the `S` of `StatusPanelView(`.
    static func scanChain(in code: [UInt8], startingAt siteOffset: Int) -> ChainScan {
        let open = siteOffset + constructionToken.utf8.count - 1
        guard var cursor = matchingParen(in: code, openAt: open) else {
            return .failed(reason: "the construction expression's parentheses do not balance")
        }
        cursor += 1

        var modifiers: [Modifier] = []
        while true {
            var probe = cursor
            while probe < code.count, isBlank(code[probe]) { probe += 1 }
            // The chain genuinely ends here — this is the one non-failing exit.
            guard probe < code.count, code[probe] == UInt8(ascii: ".") else { break }

            var nameEnd = probe + 1
            while nameEnd < code.count, isIdentifier(code[nameEnd]) { nameEnd += 1 }
            guard nameEnd > probe + 1 else {
                return .failed(reason: "the modifier chain contains a construct the walk cannot traverse")
            }
            let name = String(decoding: code[(probe + 1)..<nameEnd], as: UTF8.self)

            var next = nameEnd
            while next < code.count, isBlank(code[next]) { next += 1 }
            guard next < code.count else {
                return .failed(reason: "the modifier chain runs off the end of the file at `.\(name)`")
            }

            var argument = ""
            if code[next] == UInt8(ascii: "(") {
                guard let close = matchingParen(in: code, openAt: next) else {
                    return .failed(reason: "modifier `.\(name)`'s parentheses do not balance")
                }
                argument = String(decoding: code[(next + 1)..<close], as: UTF8.self)
                cursor = close + 1

                // A paren group may still be followed by a trailing closure: `.onReceive(pub) { … }`.
                var after = cursor
                while after < code.count, isBlank(code[after]) { after += 1 }
                if after < code.count, code[after] == UInt8(ascii: "{") {
                    guard let brace = matchingBrace(in: code, openAt: after) else {
                        return .failed(reason: "modifier `.\(name)`'s trailing-closure braces do not balance")
                    }
                    cursor = brace + 1
                }
            } else if code[next] == UInt8(ascii: "{") {
                // `.onAppear { … }`, `.task { … }`, `.background { … }` — ubiquitous in SwiftUI, and
                // the shape a #817 driver installing an observer is most likely to take.
                guard let brace = matchingBrace(in: code, openAt: next) else {
                    return .failed(reason: "modifier `.\(name)`'s closure braces do not balance")
                }
                cursor = brace + 1
            } else {
                // A property access (`.body`) or some construct with no group at all. The walk cannot
                // tell where the chain continues, and guessing is how a truncated region happens.
                return .failed(reason: "the modifier chain contains a construct the walk cannot traverse: `.\(name)`")
            }

            modifiers.append(Modifier(dot: probe, name: name, argument: argument))
        }
        return .ok(span: siteOffset..<cursor, modifiers: modifiers)
    }

    // MARK: Byte helpers

    /// Index of the `)` matching the `(` at `openAt`, or `nil` if unbalanced.
    ///
    /// Safe over REDACTED bytes only: `scan` blanks string literals, so a `)` inside a string cannot
    /// be counted here.
    static func matchingParen(in code: [UInt8], openAt: Int) -> Int? {
        matchingDelimiter(in: code, openAt: openAt, open: "(", close: ")")
    }

    /// Index of the `}` matching the `{` at `openAt`, or `nil` if unbalanced.
    static func matchingBrace(in code: [UInt8], openAt: Int) -> Int? {
        matchingDelimiter(in: code, openAt: openAt, open: "{", close: "}")
    }

    private static func matchingDelimiter(in code: [UInt8], openAt: Int,
                                          open: Unicode.Scalar, close: Unicode.Scalar) -> Int? {
        guard openAt < code.count, code[openAt] == UInt8(ascii: open) else { return nil }
        var depth = 0
        var index = openAt
        while index < code.count {
            if code[index] == UInt8(ascii: open) { depth += 1 }
            if code[index] == UInt8(ascii: close) {
                depth -= 1
                if depth == 0 { return index }
            }
            index += 1
        }
        return nil
    }

    static func offsets(of token: [UInt8], in code: [UInt8]) -> [Int] {
        guard !token.isEmpty, code.count >= token.count else { return [] }
        var hits: [Int] = []
        for start in 0...(code.count - token.count) {
            var index = 0
            while index < token.count, code[start + index] == token[index] { index += 1 }
            if index == token.count { hits.append(start) }
        }
        return hits
    }

    /// 1-based line number of `offset`.
    static func line(of offset: Int, in code: [UInt8]) -> Int {
        var line = 1
        var index = 0
        while index < offset && index < code.count {
            if code[index] == UInt8(ascii: "\n") { line += 1 }
            index += 1
        }
        return line
    }

    private static func isBlank(_ byte: UInt8) -> Bool {
        byte == UInt8(ascii: " ") || byte == UInt8(ascii: "\n")
            || byte == UInt8(ascii: "\t") || byte == UInt8(ascii: "\r")
    }

    private static func isIdentifier(_ byte: UInt8) -> Bool {
        (byte >= UInt8(ascii: "a") && byte <= UInt8(ascii: "z"))
            || (byte >= UInt8(ascii: "A") && byte <= UInt8(ascii: "Z"))
            || (byte >= UInt8(ascii: "0") && byte <= UInt8(ascii: "9"))
            || byte == UInt8(ascii: "_")
    }
}

// MARK: - The suite

final class PanelReachabilityLintTests: XCTestCase {

    /// The real file the gate will read. Located from this source file, exactly as
    /// `PanelDynamicTypeLintTests` locates `Sources/` — CI checks the tree out at the same path it
    /// compiled from.
    private var statusItemControllerURL: URL {
        URL(fileURLWithPath: #filePath)      // …/apps/menubar/Tests/PanelReachabilityLintTests.swift
            .deletingLastPathComponent()     // …/apps/menubar/Tests
            .deletingLastPathComponent()     // …/apps/menubar
            .appendingPathComponent("Sources")
            .appendingPathComponent("StatusItemController.swift")
    }

    private func realSource(file: StaticString = #filePath, line: UInt = #line) throws -> String {
        try XCTUnwrap(try? String(contentsOf: statusItemControllerURL, encoding: .utf8),
                      "StatusItemController.swift must be readable as data — this gate reads it, "
                    + "it does not compile it", file: file, line: line)
    }

    // MARK: - Guard: the subject is real (degenerate-subject, issue #748)

    // Deliberately the first test. Everything below asserts over this file's text; if it were
    // unreadable or had no construction site, `verdict(in:)` would return `.indeterminate` and a
    // careless suite could read that as "no violation" and go green over nothing.
    func testTheRealControllerIsReadableAndHasExactlyOneConstructionSite() throws {
        let source = try realSource()
        XCTAssertGreaterThan(source.utf8.count, 2000,
                             "read a real file, not a stub — StatusItemController is a substantial source")

        let code = PanelReachabilityLint.scanCode(source)
        let sites = PanelReachabilityLint.offsets(of: Array(PanelReachabilityLint.constructionToken.utf8),
                                                  in: code)
        XCTAssertEqual(sites.count, 1,
                       "the gate's subject is 'the construction site'; that phrase needs exactly one referent")

        // And the walk really does reach the shipped modifier chain, rather than stopping at `()`.
        guard case .ok(_, let modifiers) = PanelReachabilityLint.scanChain(in: code, startingAt: sites[0]) else {
            return XCTFail("the shipped construction chain must be walkable — if it is not, every "
                         + "verdict below is `.indeterminate` and this gate measures nothing")
        }
        XCTAssertEqual(modifiers.map(\.name), ["statusPanelEnvironment"],
                       "the walk must see the shipped chain exactly — that is where an injection sits")
    }

    // MARK: - The shipped state

    func testTheShippedControllerInjectsNothingYet() throws {
        // Not a defect: issue #817 owns adding the driver, and issue #971 has not yet chosen one.
        // Pinning it means the day someone wires an injection, this test tells them which gate arm
        // now applies instead of silently continuing to pass.
        guard case .absent = PanelReachabilityLint.verdict(in: try realSource()) else {
            return XCTFail("no driver exists yet (issue #817); if this fails, a driver landed — "
                         + "re-read design § 5.2")
        }
    }

    func testAbsentNamesTheConstructionSite() throws {
        // The spec's Rule 1 failure mode is "FAILS, naming the construction site". `.absent` IS that
        // failure under the positive-gate polarity, so it has to carry the line.
        guard case .absent(let line) = PanelReachabilityLint.verdict(in: try realSource()) else {
            return XCTFail("expected the shipped absent verdict")
        }
        let sourceLines = try realSource().components(separatedBy: "\n")
        XCTAssertTrue(sourceLines[line - 1].contains("StatusPanelView("),
                      "line \(line) should be the construction site, was: \(sourceLines[line - 1])")
    }

    // MARK: - CONSTRAINT-A: three outcomes, one function, real source text

    func testSplicingALiteralInjectionIntoTheRealChainIsRejected() throws {
        let mutated = try spliceIntoChain(".dynamicTypeSize(.large)")
        guard case .deadLiteral(_, let argument) = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("a hardcoded `.large` must be REJECTED — it renders identically to today "
                         + "(factor 1.0), which is the exact defect the gate exists to catch")
        }
        XCTAssertEqual(argument, ".large")
    }

    func testSplicingAVariableInjectionIntoTheRealChainIsAccepted() throws {
        let mutated = try spliceIntoChain(".dynamicTypeSize(panelSizeClass)")
        guard case .reachable(_, let argument) = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("a variable-sourced injection must be ACCEPTED")
        }
        XCTAssertEqual(argument, "panelSizeClass")
    }

    func testTheUnmutatedFileStaysAbsentInTheSameRun() throws {
        // The other half of CONSTRAINT-A. A canary alone could pass while the real file was
        // unreadable; a green run alone could mean the predicate cannot fail at all. Both directions
        // are exercised here, over the same bytes, through the same function.
        let source = try realSource()
        guard case .absent = PanelReachabilityLint.verdict(in: source),
              case .deadLiteral = PanelReachabilityLint.verdict(in: try spliceIntoChain(".dynamicTypeSize(.large)", into: source)),
              case .reachable = PanelReachabilityLint.verdict(in: try spliceIntoChain(".dynamicTypeSize(sizeClass)", into: source))
        else { return XCTFail("all three outcomes must be reachable from the SAME source text") }
    }

    // MARK: - The second spelling: `.environment(\.dynamicTypeSize, …)`

    func testAKeyPathInjectionIsSeenAsAnInjection() throws {
        // Knowing only `.dynamicTypeSize(` reports this as `.absent` — the defect pin's GREEN arm.
        let mutated = try spliceIntoChain(".environment(\\.dynamicTypeSize, panelSizeClass)")
        guard case .reachable(_, let argument) = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("`.environment(\\.dynamicTypeSize, x)` writes the same environment value "
                         + "as `.dynamicTypeSize(x)` and must be read as a live injection")
        }
        XCTAssertEqual(argument, "panelSizeClass")
    }

    func testADeadKeyPathInjectionIsStillDead() throws {
        let mutated = try spliceIntoChain(".environment(\\.dynamicTypeSize, .large)")
        guard case .deadLiteral(_, let argument) = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("a literal is dead in either spelling")
        }
        XCTAssertEqual(argument, ".large")
    }

    func testAnUnrelatedEnvironmentKeyPathIsNotAnInjection() throws {
        let mutated = try spliceIntoChain(".environment(\\.colorScheme, .dark)")
        guard case .absent = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("only the dynamicTypeSize key path injects a size class")
        }
    }

    // MARK: - Chain traversal (the walk must not end quietly)

    func testAnInjectionAfterATrailingClosureModifierIsStillFound() throws {
        // `.onAppear { }` has no paren group. A walk that requires one stops here, and everything
        // after it — including the driver — becomes invisible. Under the defect-pin polarity that
        // invisibility reads as `.absent`, which is GREEN.
        let mutated = try spliceIntoChain(".onAppear { observe() }\n                "
                                        + ".dynamicTypeSize(panelSizeClass)")
        guard case .reachable(_, let argument) = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("a trailing-closure modifier must not truncate the chain — SwiftUI chains "
                         + "are full of them and a #817 observer is likely to be one")
        }
        XCTAssertEqual(argument, "panelSizeClass")
    }

    func testADeadLiteralAfterATrailingClosureModifierIsStillCaught() throws {
        // The same truncation, in the direction that matters most: a dead literal hidden behind
        // `.onAppear` must not be able to sidestep the gate by chain order alone.
        let mutated = try spliceIntoChain(".task { await warm() }\n                "
                                        + ".dynamicTypeSize(.large)")
        guard case .deadLiteral = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("chain order must not decide whether the defect is visible")
        }
    }

    func testAnInjectionAfterAParenGroupWithATrailingClosureIsStillFound() throws {
        let mutated = try spliceIntoChain(".onReceive(publisher) { _ in refresh() }\n                "
                                        + ".dynamicTypeSize(panelSizeClass)")
        guard case .reachable = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("`.onReceive(pub) { }` is a paren group AND a trailing closure; the walk "
                         + "must consume both")
        }
    }

    func testAnInjectionOnANestedSubviewDoesNotCount() throws {
        // Inside the construction expression's byte SPAN, but at chain depth 1 — the size class
        // reaches `Badge`, not the panel. A span-based filter reads this as a live panel injection,
        // which is a false PASS in the severe direction.
        let mutated = try spliceIntoChain(".overlay(Badge().dynamicTypeSize(panelSizeClass))")
        guard case .absent = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("an injection inside a modifier's argument reaches THAT view; reading it "
                         + "as the panel's would pass a gate over a panel with no driver")
        }

        // Positive control, so the `.absent` above cannot be passing because the walk gave up on
        // `.overlay(…)` rather than because it correctly ignored what was inside it: the SAME shape
        // with a real top-level injection must be found, and must report the OUTER argument.
        let both = try spliceIntoChain(".overlay(Badge().dynamicTypeSize(badgeSize))\n                "
                                     + ".dynamicTypeSize(panelSizeClass)")
        guard case .reachable(_, let argument) = PanelReachabilityLint.verdict(in: both) else {
            return XCTFail("a top-level injection alongside a nested one must still be found")
        }
        XCTAssertEqual(argument, "panelSizeClass", "the nested argument must not be the one reported")
    }

    func testAnInjectionOutsideTheConstructionChainDoesNotCount() throws {
        // File-wide `contains` would pass this. The chain walk is what makes the gate mean
        // "the panel receives a size class" rather than "this file mentions the modifier somewhere".
        let source = try realSource()
        let mutated = source + "\n\nprivate func unrelated() -> some View "
                             + "{ EmptyView().dynamicTypeSize(sizeClass) }\n"
        guard case .absent = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("an injection on some other view does not reach StatusPanelView")
        }
    }

    // MARK: - The range overload

    func testThePartialRangeFormIsNotMistakenForAnInjection() throws {
        // `...PanelTypeScale.ceiling` — the spelling `StatusPanelView.swift` ships. It leads with a
        // dot, so a dot-prefix test calls it a literal and fails safe; the point of `clampOnly` is
        // that a range is neither, and the gate should say so rather than report a wrong reason.
        let mutated = try spliceIntoChain(".dynamicTypeSize(...PanelTypeScale.ceiling)")
        guard case .clampOnly(_, let argument) = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("a RANGE constrains the size class; it does not drive one")
        }
        XCTAssertEqual(argument, "...PanelTypeScale.ceiling")
    }

    func testTheIdentifierLeadingRangeFormIsTheActualFalseAccept() throws {
        // THIS is the trap. `PanelTypeScale.floor...PanelTypeScale.ceiling` starts with an
        // identifier, so a dot-prefix test reads it as variable-sourced and PASSES a panel that has
        // a ceiling and no driver at all. It is why `classify` tests for `...` first.
        let mutated = try spliceIntoChain(".dynamicTypeSize(PanelTypeScale.floor...PanelTypeScale.ceiling)")
        guard case .clampOnly(_, let argument) = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("an identifier-leading range is the spelling a naive non-literal test "
                         + "accepts; if this is not caught the gate can go green over a clamp")
        }
        XCTAssertEqual(argument, "PanelTypeScale.floor...PanelTypeScale.ceiling")

        // And the false accept is real, not hypothetical: without the range test it IS `variable`.
        XCTAssertFalse(argument.hasPrefix("."),
                       "the whole hazard is that this spelling does NOT lead with a dot")
    }

    func testTheRangeOverloadIsPartOfThisCodebasesVocabulary() throws {
        // The clamp is a construct this project actually writes, so `clampOnly` is a verdict for a
        // real shape rather than an invented one. It lives in StatusPanelView today, which the
        // predicate does NOT read — if it ever moves into the controller's chain, the verdict is
        // already there to meet it.
        let panelView = statusItemControllerURL
            .deletingLastPathComponent()
            .appendingPathComponent("StatusPanelView.swift")
        let text = try XCTUnwrap(try? String(contentsOf: panelView, encoding: .utf8))
        XCTAssertTrue(text.contains(".dynamicTypeSize(..."),
                      "if this stops holding, revisit the reasoning rather than deleting the test")
    }

    // MARK: - Every `.indeterminate` arm is reachable

    // A verdict arm nothing drives is a branch that could be silently broken — the same shape as a
    // gate that cannot fail. `.indeterminate` is the arm that must NEVER be confused with `.absent`
    // (one is a measurement, the other its absence), so all EIGHT of its reasons are driven below:
    // unmodelled literal, no site, ambiguous site, unbalanced construction parens, unbalanced
    // modifier parens, unbalanced closure braces, chain running off the end, and an untraversable
    // construct.

    func testAMissingConstructionSiteIsIndeterminateAndNotAbsent() throws {
        let source = try realSource().replacingOccurrences(of: "StatusPanelView(", with: "SomeOtherView(")
        guard case .indeterminate(let reason) = PanelReachabilityLint.verdict(in: source) else {
            return XCTFail("no construction site means the gate has no subject — reporting `.absent` "
                         + "would be a green over nothing")
        }
        XCTAssertTrue(reason.contains("no `StatusPanelView(`"), "reason should name what was missing: \(reason)")
    }

    func testASecondConstructionSiteIsIndeterminateRatherThanFirstMatch() throws {
        let source = try realSource() + "\n// second site below is CODE, not a comment\n"
                                      + "let another = StatusPanelView().dynamicTypeSize(sizeClass)\n"
        guard case .indeterminate(let reason) = PanelReachabilityLint.verdict(in: source) else {
            return XCTFail("two sites make 'the construction site' ambiguous; a first-match rule would "
                         + "silently pick one and could be gamed by adding a second, live one")
        }
        XCTAssertTrue(reason.contains("ambiguous"), "reason should name the ambiguity: \(reason)")
    }

    func testAnUnmodelledStringLiteralIsIndeterminateRatherThanScannedBlind() throws {
        let source = try spliceIntoChain("._doc(\"\"\"\n  a multiline literal\n  \"\"\")")
        guard case .indeterminate(let reason) = PanelReachabilityLint.verdict(in: source) else {
            return XCTFail("the shared scanner does not model multiline literals; scanning blind past "
                         + "one could hide or fake an injection")
        }
        XCTAssertTrue(reason.contains("multiline"), "reason should name the construct: \(reason)")
    }

    func testAnUnclosedConstructionExpressionIsIndeterminate() throws {
        let truncated = try truncateAfter("StatusPanelView(")
        guard case .indeterminate(let reason) = PanelReachabilityLint.verdict(in: truncated) else {
            return XCTFail("a construction expression that never closes is not a measurement")
        }
        XCTAssertTrue(reason.contains("construction expression"), reason)
    }

    func testAnUnclosedModifierGroupIsIndeterminateRatherThanSwallowingTheRest() throws {
        // The reason this must be bounded: an unclosed group whose `)` is found later in the file
        // yields a plausible-looking argument spliced out of unrelated code, and a verdict computed
        // from it. Failing is the only honest answer.
        let truncated = try truncateAfter("StatusPanelView()") + "\n            .dynamicTypeSize(sizeClass"
        guard case .indeterminate(let reason) = PanelReachabilityLint.verdict(in: truncated) else {
            return XCTFail("an unbalanced modifier group must fail, not produce a garbage argument")
        }
        XCTAssertTrue(reason.contains("do not balance"), reason)
    }

    func testAnUnclosedClosureIsIndeterminate() throws {
        let truncated = try truncateAfter("StatusPanelView()") + "\n            .onAppear { observe()"
        guard case .indeterminate(let reason) = PanelReachabilityLint.verdict(in: truncated) else {
            return XCTFail("an unbalanced closure must fail rather than end the chain quietly")
        }
        XCTAssertTrue(reason.contains("braces do not balance"), reason)
    }

    func testAChainRunningOffTheEndOfTheFileIsIndeterminate() throws {
        let truncated = try truncateAfter("StatusPanelView()") + "\n            .dynamicTypeSize"
        guard case .indeterminate(let reason) = PanelReachabilityLint.verdict(in: truncated) else {
            return XCTFail("a chain with no group at all is not a measurement of 'no driver'")
        }
        XCTAssertTrue(reason.contains("runs off the end"), reason)
    }

    func testAnUntraversableChainConstructIsIndeterminate() throws {
        let truncated = try truncateAfter("StatusPanelView()") + "\n            .body.dynamicTypeSize(sizeClass)\n"
        guard case .indeterminate(let reason) = PanelReachabilityLint.verdict(in: truncated) else {
            return XCTFail("a property access mid-chain leaves the walk unable to say where the chain "
                         + "continues; guessing is how an injection becomes invisible")
        }
        XCTAssertTrue(reason.contains("cannot traverse"), reason)
    }

    // MARK: - The accept/reject table

    func testTheArgumentClassifierAcceptsLiveSourcesAndRejectsDeadOnes() {
        let literals = [".large", ".accessibility3", "DynamicTypeSize.large",
                        "SwiftUI.DynamicTypeSize.xxxLarge", "",
                        // Spellings Swift accepts that a bare prefix test reads as variable.
                        "(.large)", "((.large))", "( DynamicTypeSize.large )",
                        "DynamicTypeSize .large", "DynamicTypeSize\n    .large",
                        ".large as DynamicTypeSize"]
        for argument in literals {
            XCTAssertEqual(PanelReachabilityLint.classify(argument), .literal,
                           "`\(argument)` is a compile-time constant — factor is fixed, the panel cannot move")
        }

        let variables = ["sizeClass", "panelSizeClass", "store.textSize", "model.size.wrapped",
                         "flag ? .large : .xLarge", "DynamicTypeSize.allCases.first!", "resolve()"]
        for argument in variables {
            XCTAssertEqual(PanelReachabilityLint.classify(argument), .variable,
                           "`\(argument)` derives from something the program can vary")
        }

        let ranges = ["...PanelTypeScale.ceiling", ".large...", ".large....accessibility3",
                      "PanelTypeScale.floor...PanelTypeScale.ceiling",
                      "DynamicTypeSize.large...DynamicTypeSize.accessibility3"]
        for argument in ranges {
            XCTAssertEqual(PanelReachabilityLint.classify(argument), .range,
                           "`\(argument)` is a constraint, not a value")
        }
    }

    func testNormalisationDoesNotStripParensItShouldNotStrip() {
        // `(a)(b)` is not a parenthesised expression — stripping its outer parens would corrupt it.
        XCTAssertEqual(PanelReachabilityLint.normalize("(a)(b)"), "(a)(b)")
        XCTAssertEqual(PanelReachabilityLint.classify("(.large)(x)"), .variable)
    }

    // MARK: - The documented blind spot

    func testAConstantBoundToALetIsTheDocumentedBlindSpot() throws {
        // A text predicate cannot follow a binding. `let dead = DynamicTypeSize.large` then
        // `.dynamicTypeSize(dead)` is dead code that this gate reports as reachable. Asserted rather
        // than merely admitted, so the bound is a fact of record and cannot quietly widen: closing it
        // would require type resolution, which is the T3 tier's job (design § 5.2, § 11).
        //
        // The binding is spliced in TOO. Without it the test proves only that a bare identifier is
        // variable — which another test already covers — and would keep passing even against a
        // predicate that HAD gained binding resolution, so the tripwire below could never fire.
        let source = try realSource().replacingOccurrences(
            of: "let hosting = NSHostingView(",
            with: "let dead = DynamicTypeSize.large\n        let hosting = NSHostingView(")
        XCTAssertTrue(source.contains("let dead = DynamicTypeSize.large"),
                      "the binding must actually be in the source, or this test proves nothing")

        let mutated = try spliceIntoChain(".dynamicTypeSize(dead)", into: source)
        guard case .reachable(_, let argument) = PanelReachabilityLint.verdict(in: mutated) else {
            return XCTFail("expected the KNOWN false accept — if this now fails, the predicate gained "
                         + "binding resolution and docs/findings/0981 needs updating")
        }
        XCTAssertEqual(argument, "dead")
    }

    // MARK: - Redaction is inherited, not reimplemented

    func testACommentOrStringMentioningTheInjectionCannotFakeIt() throws {
        let source = try realSource()
        let commented = try spliceIntoChain("// .dynamicTypeSize(sizeClass) — discussed, not written",
                                            into: source)
        guard case .absent = PanelReachabilityLint.verdict(in: commented) else {
            return XCTFail("a mention in a comment is not an injection")
        }

        let stringified = try spliceIntoChain("._ignored(\".dynamicTypeSize(sizeClass)\")", into: source)
        guard case .absent = PanelReachabilityLint.verdict(in: stringified) else {
            return XCTFail("a mention inside a string literal is not an injection")
        }
    }

    // MARK: Mutation helpers

    /// Splice `modifier` into the REAL construction chain, immediately after `StatusPanelView()`.
    ///
    /// Textual and deliberately dumb: it does not need to produce compilable Swift (nothing compiles
    /// this), it needs to produce the exact byte shape the gate will meet in a real diff.
    private func spliceIntoChain(_ modifier: String, into source: String? = nil) throws -> String {
        let text = try source ?? realSource()
        let anchor = "StatusPanelView()"
        let range = try XCTUnwrap(text.range(of: anchor),
                                  "the shipped construction site is `\(anchor)` — if that changed, "
                                + "this helper must change with it")
        return text.replacingCharacters(in: range, with: anchor + "\n                " + modifier)
    }

    /// The real source cut off immediately after `anchor`, so a construct appended here genuinely has
    /// nothing closing it — the only way to reach the unbalanced arms over real text, since the
    /// shipped call sits inside `NSHostingView(…)` whose own `)` would otherwise close it by accident.
    private func truncateAfter(_ anchor: String) throws -> String {
        let text = try realSource()
        let range = try XCTUnwrap(text.range(of: anchor), "anchor `\(anchor)` must exist in the real source")
        return String(text[text.startIndex..<range.upperBound])
    }
}

// MARK: - Test-only access to the shared redaction

extension PanelReachabilityLint {
    /// The redacted code view, exposed so the degenerate-subject guard can walk the same bytes
    /// `verdict(in:)` walks rather than a second, differently-redacted copy of them.
    static func scanCode(_ source: String) -> [UInt8] { PanelScaleLint.scan(source).code }
}
#endif
