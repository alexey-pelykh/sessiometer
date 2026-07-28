// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The STRUCTURAL half of issue #757's Dynamic Type gate — a lint over the panel's own source text.
//
// WHY A LINT AND NOT ANOTHER MEASUREMENT. Issue #757's AC-2 is explicit that this must be enforced
// "structurally … rather than hoping a metric assertion happens to cover the new call site", and that
// instruction is load-bearing. `PanelTextMetricsTests` measures five gated cells across all twelve size
// classes; it is a strong gate for the strings it measures and says NOTHING about the other forty-nine
// `.font(…)` sites. Someone adding a sixth cell with a bare `.font(.system(size: 11))` breaks Dynamic Type
// on that element and every existing assertion stays green, because no assertion ever names it. A lint
// inverts the default: a site is covered because it EXISTS, not because a test remembered it.
//
// WHAT COUNTS AS SCALED, and why the obvious answer is wrong on this platform. The relative text styles
// are INERT on macOS — `StatusPanelTypeScale`'s header carries the measurement (`@ScaledMetric` and
// `Text().font(.body)` both immobile across all twelve `DynamicTypeSize` cases). So a lint that treated
// `.font(.body)`, `.font(.caption)` or `.font(.headline)` as "already scaling" would wave through exactly
// the eleven call sites the panel already had before #756 and that demonstrably did not scale. This lint
// therefore accepts ONE form: `Font.panel(…)`, the explicit `\.panelScale` consumer #756 introduced.
// Everything else is a violation, relative styles included.
//
// WHY ONE SHAPE RATHER THAN "ANYTHING CONTAINING `scale`". A hand-scaled `.font(.system(size: 11 * scale))`
// would in fact render correctly today, and it is still rejected. The property being enforced is
// UNIFORMITY, which is what makes "did this site get scaled?" a grep instead of a reading exercise
// (`StatusPanelTypeScale` states that intent at `Font.panel`). Accepting a second spelling reduces the
// gate to "remember to multiply" — the habit-based check AC-2 exists to replace. A site with a genuine
// need for a fixed size has the per-line escape hatch below, which costs a written reason.
//
// SCOPE — read this before widening it. Issue #757 suggests `Sources/StatusPanel*.swift`. That is a
// FILENAME CONVENTION, and it is already too narrow: of the fourteen SwiftUI sources in this tree, FIVE
// sit outside that glob, and two of those (`PanelRenderHarness.swift`, `RenderPanelTool.swift`) are
// inside the panel-scaled set the lint must cover. So discovery is MECHANICAL instead: every `.swift`
// file under `Sources/` — RECURSIVELY — that imports SwiftUI. `.font(…)` is a SwiftUI `View` modifier,
// so a file that does not import SwiftUI cannot carry one — that is a property of the language, not a
// habit of ours. A new SwiftUI file is therefore linted the moment it lands, with no registry edit, and
// the fail-closed direction is the safe one: a new file must EARN an exemption rather than inherit one.
//
// Two details of that discovery are load-bearing, and both were fail-OPEN in a first draft. It walks
// SUBDIRECTORIES, because `project.yml` compiles `- Sources` recursively — a view at
// `Sources/Panel/NewCard.swift` ships in the app, and a flat scan would simply have moved the escape
// hatch from the file's NAME to its DIRECTORY. And the import test is a PREFIX match, because
// `import SwiftUI  // Font, View` is an ordinary way to write the line and exact equality silently
// dropped it. Both are pinned by `testDiscoveryFindsSwiftUISourcesInSubdirectoriesAndPastImportComments`
// rather than left to prose.
//
// THE EXEMPTIONS, explicit and greppable per AC. Four SwiftUI sources are deliberately NOT panel-scaled:
//
//   * `SettingsView.swift` / `SettingsWindowController.swift` — the Settings WINDOW, a separate surface
//     that does not participate in `\.panelScale` at all (issue #845). Its seven relative-style fonts are
//     correct as written; linting them would redden the tree on day one, which is how a lint gets
//     disabled a week later.
//   * `StatusItemController.swift` — the MENU-BAR chrome host. The status item is locked at bar size and
//     must NOT scale: `StatusGauge`'s monochrome template glyph is sized by AppKit outside the panel's
//     SwiftUI subtree, so `\.panelScale` cannot reach it and must not (issue #437's ratified brand lock).
//   * `StatusPanelTypeScale.swift` — the definition of `Font.panel` itself. `.system(size: points * scale)`
//     is what this file is FOR; linting the scaling primitive against its own rule is circular.
//
// `StatusGauge.swift` and `StatusItemChrome.swift` are the other menu-bar-locked sources and need no
// entry: neither imports SwiftUI, so neither is discovered. That is the mechanical boundary doing its job
// rather than a second list to keep in sync.
//
// CONSTRAINT-A (issue #748) — NO GATE WITHOUT A PROVEN FALSIFIER. Every assertion below runs through ONE
// function, `PanelScaleLint.violations(in:)`, and the canaries drive that SAME function by MUTATION: real
// panel sources are read, a real `.font(.panel(…))` site is reverted to the pre-#756 unscaled form in
// memory, and the lint is required to trip AT THAT LINE — while the unmutated file is required to stay
// clean in the same run. Both directions matter. A canary alone could pass while every real file was
// silently unreadable; a green run alone could mean the predicate cannot fail at all.
//
// HEADLESS (AC-4): reading source text and walking bytes is Foundation-only. No window, no screen,
// no TCC, no `project.yml` change — this file adds no view to the bundle, it reads the views as data.

#if DEBUG
import Foundation
import XCTest

// MARK: - The lint

/// The panel's Dynamic Type source rule, as a pure function over source TEXT.
///
/// Pure and Foundation-only so the real assertions, the accept/reject tables, and the mutation canaries
/// all drive the identical predicate — the issue #750 discipline applied to a lint instead of a
/// measurement: the thing proven falsifiable must be the thing actually gating.
///
/// It is a TEXT lint, not a compiler. It never parses or type-checks Swift; it redacts comments and string
/// literals and then looks for call shapes. That bound is stated here so no assertion over-claims it.
enum PanelScaleLint {

    /// The one comment marker that suppresses this rule, with a mandatory reason.
    ///
    /// A gate with no escape hatch gets deleted wholesale the first time it is genuinely wrong; a gate
    /// with a free escape hatch is decorative. The reason is what makes the difference, so a bare marker
    /// with nothing after the colon does NOT suppress anything (canaried below).
    ///
    /// SCOPE IS THE LINE, not the individual call site: a marker excuses every font site on its line. That
    /// is a deliberate simplification — the panel writes one `.font(…)` per line throughout — but it does
    /// mean `.font(.system(size: 11)); .font(.system(size: 12))  // <marker> only the first is fine`
    /// excuses both. The behaviour is asserted below rather than left for someone to discover.
    static let exemptionMarker = "panel-scale-exempt:"

    enum Kind: String, Equatable {
        case unscaledFontModifier = "`.font(…)` argument is not `.panel(…)` — see Font.panel (issue #756)"
        case rawFontConstructor = "raw font constructor outside `Font.panel(…)`"
        case unsupportedStringLiteral = "file uses a multiline or raw string literal this scanner does not model"
    }

    struct Violation: Equatable, CustomStringConvertible {
        let line: Int
        let kind: Kind
        let snippet: String

        var description: String { "line \(line): \(kind.rawValue) — \(snippet)" }
    }

    // MARK: Redaction

    private static let newline = UInt8(ascii: "\n"), space = UInt8(ascii: " ")
    private static let slash = UInt8(ascii: "/"), star = UInt8(ascii: "*")
    private static let quote = UInt8(ascii: "\""), backslash = UInt8(ascii: "\\")
    private static let hash = UInt8(ascii: "#"), tab = UInt8(ascii: "\t"), carriageReturn = UInt8(ascii: "\r")

    /// One pass over the source, three products — the two views the rule needs, plus the bail-out signal.
    struct Scan {
        /// Comments and string literals blanked. What the violation scan reads.
        let code: [UInt8]
        /// Code and string literals blanked, comment text KEPT. What the exemption marker is read from,
        /// so a marker sitting inside a string literal cannot suppress anything.
        let comments: [UInt8]
        /// A multiline (`"""`) or raw (`#"`) literal opener was seen in CODE position — not merely
        /// mentioned in prose. Checked here rather than by a `contains` over the raw source, so a comment
        /// that discusses those tokens does not make the whole file report a literal it does not have.
        let unmodelledLiteral: Bool
    }

    /// Blank out comments and string literals, PRESERVING every newline and every byte offset.
    ///
    /// Offsets are preserved rather than the text compacted, so a violation reports the line number a
    /// human will actually open. Redaction is what stops the two failure directions a naive
    /// `contains(".font(.system(")` has: prose that merely QUOTES the anti-pattern must not trip the gate
    /// (this file's own header quotes it repeatedly, and so does `StatusPanelTypeScale`), and — the
    /// dangerous direction — a `//` inside a string literal must not blind the rest of that line.
    ///
    /// Works over UTF-8 BYTES, not `Character`s, and that is a deliberate cost decision rather than a
    /// micro-optimisation. Every token this lint matches is ASCII, while a `Character` is a grapheme
    /// cluster whose comparison in a DEBUG build costs orders of magnitude more; the canary below scans
    /// roughly 200 kB of source three times over (a control plus two mutants per file), and the
    /// `Character` version of this function took ~3 s to do it. `MenubarTests` runs its suites in
    /// parallel, so a slow suite is not merely slow — it steals CPU from timing-sensitive neighbours (the
    /// socket EOF tests, and the un-warmed render fixtures of issue #824). A gate that destabilises the
    /// suite it joins gets deleted, however correct it is. Multi-byte characters inside a redacted region
    /// simply become several spaces, which is harmless: only newlines (0x0A) and relative offsets have to
    /// survive, and both do.
    ///
    /// Known bound: Swift string INTERPOLATION segments are redacted along with the literal that contains
    /// them. That cannot hide a violation, because `.font(…)` yields a `View` and a `View` is not a value
    /// a string literal can interpolate. Multiline (`"""`) and raw (`#"`) literals are NOT modelled at
    /// all, and are reported as a violation rather than scanned blind — see `Kind.unsupportedStringLiteral`.
    static func scan(_ source: String) -> Scan {
        /// Where the walk currently is. `block` carries its nesting depth — Swift nests block comments.
        enum State { case code, string, lineComment, block(Int) }

        /// Which view keeps a byte. THREE dispositions, not two: string-literal content belongs to
        /// NEITHER view — keeping it in the comment view is what would let `Text("// <marker> x")`
        /// suppress a real violation.
        enum Keep { case code, comments, neither }

        let bytes = Array(source.utf8)
        var code = [UInt8](), comments = [UInt8]()
        code.reserveCapacity(bytes.count)
        comments.reserveCapacity(bytes.count)
        var state = State.code
        var escaped = false
        var unmodelled = false
        var i = 0

        /// Emit one byte, blanking it in every view that does not keep it. Newlines survive everywhere, so
        /// offsets and line numbers stay aligned across the two views.
        func emit(_ byte: UInt8, _ keep: Keep) {
            let blanked = byte == newline ? newline : space
            code.append(keep == .code ? byte : blanked)
            comments.append(keep == .comments ? byte : blanked)
        }

        while i < bytes.count {
            let c = bytes[i]
            let next: UInt8? = i + 1 < bytes.count ? bytes[i + 1] : nil

            switch state {
            case .code:
                if c == slash, next == slash {
                    // The `//` itself is kept in the comment view, so a marker can be required to sit
                    // inside a real comment rather than merely somewhere on the line.
                    state = .lineComment; emit(c, .comments); emit(slash, .comments)
                    i += 2; continue
                }
                if c == slash, next == star {
                    state = .block(1); emit(c, .comments); emit(star, .comments)
                    i += 2; continue
                }
                if c == quote {
                    if next == quote, i + 2 < bytes.count, bytes[i + 2] == quote { unmodelled = true }
                    state = .string; escaped = false; emit(c, .neither); i += 1; continue
                }
                if c == hash, next == quote { unmodelled = true }
                emit(c, .code); i += 1

            case .string:
                emit(c, .neither)
                if escaped {
                    escaped = false
                } else if c == backslash {
                    escaped = true
                } else if c == quote || c == newline {
                    // A newline inside a single-line literal means the source is unterminated; resync to
                    // code rather than swallowing the remainder of the file.
                    state = .code
                }
                i += 1

            case .lineComment:
                emit(c, .comments)
                if c == newline { state = .code }
                i += 1

            case .block(let depth):
                if c == slash, next == star {
                    state = .block(depth + 1); emit(c, .comments); emit(star, .comments)
                    i += 2; continue
                }
                if c == star, next == slash {
                    state = depth == 1 ? .code : .block(depth - 1)
                    emit(c, .comments); emit(slash, .comments)
                    i += 2; continue
                }
                emit(c, .comments); i += 1
            }
        }
        return Scan(code: code, comments: comments, unmodelledLiteral: unmodelled)
    }

    // MARK: The predicate

    /// THE predicate. Every real assertion and every CONSTRAINT-A canary in this file runs through here.
    static func violations(in source: String) -> [Violation] {
        let scanned = scan(source)

        // Fail CLOSED on literal forms the scanner does not model. A lint that silently mis-scans is worse
        // than one that says it cannot scan: the first reports a green it did not earn.
        if scanned.unmodelledLiteral {
            return [Violation(line: 1, kind: .unsupportedStringLiteral,
                              snippet: "extend `scan(_:)` before trusting this file")]
        }

        let originalLines = source.components(separatedBy: "\n")
        // Read the marker from the COMMENT view, never the raw line: a marker inside a string literal —
        // `Text("// panel-scale-exempt: x")` — must not license anything. That is the same false-negative
        // direction redaction closes for the violation scan, and the suppression path gets it too.
        let commentLines = String(decoding: scanned.comments, as: UTF8.self).components(separatedBy: "\n")
        let suppressed = Set(commentLines.indices
            .filter { carriesExemption(commentLines[$0]) }
            .map { $0 + 1 })

        // Offset → 1-based line, tabulated in one pass: the scan below reports by line, and counting
        // newlines per hit instead would make a file with many violations quadratic.
        let redacted = scanned.code
        var lineOfOffset = [Int](repeating: 1, count: redacted.count)
        var line = 1
        for (offset, byte) in redacted.enumerated() {
            lineOfOffset[offset] = line
            if byte == newline { line += 1 }
        }

        var found: [Violation] = []

        func record(_ offset: Int, _ kind: Kind) {
            let number = lineOfOffset[offset]
            guard !suppressed.contains(number) else { return }
            let text = number - 1 < originalLines.count
                ? originalLines[number - 1].trimmingCharacters(in: .whitespaces) : ""
            found.append(Violation(line: number, kind: kind, snippet: text))
        }

        for offset in redacted.indices {
            // A `.font(` whose first argument is not `.panel(` / `Font.panel(`. Scanned over the whole
            // redacted source rather than line by line, so a call split across lines reads correctly
            // instead of looking like a bare `.font(` with nothing after it.
            if matches(redacted, at: offset, Token.font) {
                var argument = offset + Token.font.count
                while argument < redacted.count, isBlank(redacted[argument]) { argument += 1 }
                if !matches(redacted, at: argument, Token.panel),
                   !matches(redacted, at: argument, Token.qualifiedPanel) {
                    record(offset, .unscaledFontModifier)
                }
            }
            // A font built OUTSIDE a `.font(…)` modifier — assigned to a `let` and applied later, say —
            // which the shape check above would never see. The label is matched across whitespace so a
            // wrapped or spaced call is not a hole in the rule.
            if matchesConstructor(redacted, at: offset, Token.system, label: Token.sizeLabel)
                || matchesConstructor(redacted, at: offset, Token.systemFont, label: Token.ofSizeLabel) {
                record(offset, .rawFontConstructor)
            }
        }
        return found.sorted { ($0.line, $0.kind.rawValue) < ($1.line, $1.kind.rawValue) }
    }

    /// The ASCII tokens the scan matches, encoded once rather than per offset.
    private enum Token {
        static let font = Array(".font(".utf8)
        static let panel = Array(".panel(".utf8)
        static let qualifiedPanel = Array("Font.panel(".utf8)
        static let system = Array(".system(".utf8)
        static let sizeLabel = Array("size:".utf8)
        static let systemFont = Array("systemFont(".utf8)
        static let ofSizeLabel = Array("ofSize:".utf8)
    }

    private static func isBlank(_ byte: UInt8) -> Bool {
        byte == space || byte == newline || byte == tab || byte == carriageReturn
    }

    /// Whether `text` imports SwiftUI, i.e. whether it can carry a `.font(…)` modifier at all.
    ///
    /// A PREFIX test rather than exact line equality: `import SwiftUI  // Font, View` is a real import and
    /// exact matching would silently drop the file from the lint's scope. Still anchored at the start of a
    /// trimmed line, and the character after the module name is checked, so `import SwiftUIX` — a
    /// different module — does not match, and neither does prose inside a comment (a comment line begins
    /// with `//`, so it fails `hasPrefix`).
    static func declaresSwiftUIImport(_ text: String) -> Bool {
        text.components(separatedBy: "\n").contains { line in
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard trimmed.hasPrefix("import SwiftUI") else { return false }
            let rest = trimmed.dropFirst("import SwiftUI".count)
            return rest.isEmpty || rest.first == " " || rest.first == "\t" || rest.hasPrefix("//")
        }
    }

    /// Whether `line` carries a well-formed exemption: the marker, inside a `//` comment, with a non-empty
    /// reason after the colon. Scoped to the whole LINE — see `exemptionMarker`.
    static func carriesExemption(_ line: String) -> Bool {
        guard let comment = line.range(of: "//"),
              let marker = line.range(of: exemptionMarker),
              marker.lowerBound > comment.lowerBound else { return false }
        return !line[marker.upperBound...].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private static func matches(_ bytes: [UInt8], at offset: Int, _ token: [UInt8]) -> Bool {
        guard offset >= 0, offset + token.count <= bytes.count else { return false }
        var index = 0
        while index < token.count {
            if bytes[offset + index] != token[index] { return false }
            index += 1
        }
        return true
    }

    /// `open` followed — across any whitespace — by `label`, e.g. `.system(` … `size:`.
    private static func matchesConstructor(_ bytes: [UInt8], at offset: Int,
                                           _ open: [UInt8], label: [UInt8]) -> Bool {
        guard matches(bytes, at: offset, open) else { return false }
        var next = offset + open.count
        while next < bytes.count, isBlank(bytes[next]) { next += 1 }
        return matches(bytes, at: next, label)
    }
}

// MARK: - The suite

final class PanelDynamicTypeLintTests: XCTestCase {

    /// SwiftUI sources that are deliberately NOT panel-scaled, each with the reason it is exempt.
    ///
    /// The reason travels WITH the entry, and `testEveryExemptionNamesAnExistingSwiftUISource` reads
    /// both, so a licence cannot outlive its justification or its file. Full rationale in the file
    /// header, § THE EXEMPTIONS.
    private static let exemptions: [String: String] = [
        "SettingsView.swift":
            "the Settings window is a separate surface that does not participate in `\\.panelScale` (issue #845)",
        "SettingsWindowController.swift":
            "hosts the Settings window, same surface and same exclusion as SettingsView (issue #845)",
        "StatusItemController.swift":
            "menu-bar chrome — the status item is locked at bar size and must not scale (issue #437)",
        "StatusPanelTypeScale.swift":
            "defines `Font.panel`; linting the scaling primitive against its own rule is circular (issue #756)",
    ]

    /// The panel view files that must be present for this suite to mean anything. Not the whole
    /// panel-scaled set — a floor, so the reachability guard is a real assertion rather than a restatement
    /// of whatever the directory happens to contain.
    private static let requiredPanelSources = [
        "StatusPanelCapture.swift", "StatusPanelChrome.swift", "StatusPanelRoster.swift",
        "StatusPanelSharedViews.swift", "StatusPanelStats.swift", "StatusPanelView.swift",
    ]

    // MARK: Discovery

    /// Located from this source file, exactly as `BarGlyphParityTests` locates its committed references —
    /// CI checks the tree out at the same path it compiled from.
    private var sourcesDirectory: URL {
        URL(fileURLWithPath: #filePath)      // …/apps/menubar/Tests/PanelDynamicTypeLintTests.swift
            .deletingLastPathComponent()     // …/apps/menubar/Tests
            .deletingLastPathComponent()     // …/apps/menubar
            .appendingPathComponent("Sources")
    }

    /// Every SwiftUI source under `Sources/`, RECURSIVELY — `project.yml` compiles `- Sources`
    /// recursively, so a view added at `Sources/Panel/NewCard.swift` ships in the app and a flat
    /// `contentsOfDirectory` would never see it (file header, § SCOPE). With 45 flat files today,
    /// foldering is a live prospect rather than a hypothetical.
    ///
    /// `root` is the seam the discovery test plants its own tree under; it is an `Optional` rather than
    /// a defaulted `URL` because a default argument cannot reference `sourcesDirectory`.
    private func swiftUISources(under root: URL? = nil) -> [(name: String, text: String)] {
        // Symlinks resolved on BOTH sides before comparing: on macOS `/var/…` is a symlink to
        // `/private/var/…`, so an unresolved prefix test silently fails and every file collapses back to
        // its bare filename — which is exactly the directory-collision this relative naming prevents.
        let root = (root ?? sourcesDirectory).resolvingSymlinksInPath()
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        let enumerator = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil)
        return (enumerator?.allObjects as? [URL] ?? [])
            .filter { $0.pathExtension == "swift" }
            .compactMap { url in
                guard let text = try? String(contentsOf: url, encoding: .utf8),
                      PanelScaleLint.declaresSwiftUIImport(text) else { return nil }
                // Path-relative, so two same-named files in different directories cannot collide in the
                // exemption registry or in a failure message.
                let path = url.resolvingSymlinksInPath().path
                let name = path.hasPrefix(prefix) ? String(path.dropFirst(prefix.count))
                                                  : url.lastPathComponent
                return (name, text)
            }
            .sorted { $0.name < $1.name }
    }

    private func panelScaledSources() -> [(name: String, text: String)] {
        swiftUISources().filter { Self.exemptions[$0.name] == nil }
    }

    // MARK: - Guard: the lint actually reached the real panel views

    // The whole suite is meaningless if discovery came back empty or short: `violations(in:)` over nothing
    // returns nothing, every assertion below passes, and the green means only that the loop never ran.
    // This is the degenerate-subject guard (issue #748), and it is deliberately the first test in the file.
    func testDiscoveryReachesTheRealPanelViewsAndTheirFontSites() {
        let sources = swiftUISources()
        // A FLOOR with headroom, not today's exact count (14): the named list below is the sharp guard,
        // and pinning the total would redden this suite with "truncated or missing tree" the first time
        // someone legitimately consolidates or deletes a view — a false explanation of a correct change.
        XCTAssertGreaterThanOrEqual(sources.count, 10,
            "discovered only \(sources.count) SwiftUI sources under \(sourcesDirectory.path) — the lint is "
            + "scanning a truncated or missing tree, so every assertion in this file passes vacuously")

        let scaled = panelScaledSources()
        let names = Set(scaled.map(\.name))
        for required in Self.requiredPanelSources {
            XCTAssertTrue(names.contains(required),
                          "\(required) is not in the panel-scaled set — either it stopped importing "
                          + "SwiftUI, it was renamed, or it was silently exempted")
        }

        // The sites themselves, not just the files: a file read as an empty string would clear every
        // check above and still gate nothing.
        let sites = scaled.reduce(0) { $0 + $1.text.components(separatedBy: ".font(").count - 1 }
        XCTAssertGreaterThanOrEqual(sites, 50,
            "counted \(sites) `.font(` sites across \(scaled.count) panel-scaled sources — issue #756 "
            + "scaled 54 of them, so a count this low means the files are not being read")
    }

    // MARK: - AC-2: every panel-view font goes through the scaled form

    func testEveryPanelViewFontGoesThroughTheScaledForm() {
        var scanned = 0
        for source in panelScaledSources() {
            let violations = PanelScaleLint.violations(in: source.text)
            XCTAssertTrue(violations.isEmpty,
                          "\(source.name) has \(violations.count) unscaled font site(s):\n"
                          + violations.map { "  • \($0)" }.joined(separator: "\n")
                          + "\n\nUse `.font(.panel(points, weight, scale: scale))` or "
                          + "`.font(.panel(style: .body, scale: scale))` (issue #756). If a fixed size is "
                          + "genuinely correct here, add `// \(PanelScaleLint.exemptionMarker) <reason>` "
                          + "on the line.")
            scanned += 1
        }
        // Floored at 6 — the size of `requiredPanelSources`, which the discovery guard checks BY NAME —
        // rather than at today's 10. Same reasoning as that guard's own floor: pinning the exact count
        // would redden this with "says nothing about the files it never read" the first time someone
        // legitimately consolidates a panel view, and that explanation would be false. The named list is
        // the sharp guard; this is only the not-vacuous floor.
        XCTAssertGreaterThanOrEqual(scanned, 6,
                                    "scanned only \(scanned) panel-scaled sources — this loop says nothing "
                                    + "about the files it never read; see the discovery guard above")
    }

    // MARK: - The exemption registry cannot rot

    // An exemption naming a file that no longer exists is a stale licence: it reads as a considered
    // decision while covering nothing, and it hides the fact that the surface it excused has moved.
    func testEveryExemptionNamesAnExistingSwiftUISource() {
        let discovered = Set(swiftUISources().map(\.name))
        for (name, reason) in Self.exemptions {
            XCTAssertTrue(discovered.contains(name),
                          "exemption for \(name) names a file that is not a discovered SwiftUI source — "
                          + "it was renamed, deleted, or stopped importing SwiftUI; drop the exemption or "
                          + "point it at the current file (reason on record: \(reason))")
            XCTAssertFalse(reason.trimmingCharacters(in: .whitespaces).isEmpty,
                           "the exemption for \(name) carries no reason")
        }
    }

    // MARK: - CONSTRAINT-A: the lint trips on a REAL panel source, at the REAL line

    // Failure shape the batch has already seen twice (issue #760): a canary that is true by construction
    // certifies nothing. So this does not assert over a hand-written fixture — it reads each shipped panel
    // source, reverts ONE real `.font(.panel(…))` site to the pre-#756 unscaled spelling, and requires the
    // SAME `violations(in:)` the assertion above uses to trip AT THAT SITE'S LINE. Asserting the line, not
    // merely non-emptiness, is what distinguishes "the lint detected the mutation" from "the lint objects
    // to something, somewhere, in a file it was handed".
    //
    // Sources with no `.font(` site to revert are mutated by INJECTION instead, so the loop covers every
    // panel-scaled file rather than quietly skipping the ones that would be hardest to notice regressing.
    func testRevertingARealFontSiteToTheUnscaledFormTripsTheLint() {
        let sources = panelScaledSources()
        // Floored at 6 for the reason given in `testEveryPanelViewFontGoesThroughTheScaledForm`: enough
        // to prove the loop is not vacuous, with headroom for a legitimate consolidation.
        XCTAssertGreaterThanOrEqual(sources.count, 6, "too few sources to mutate — see the discovery guard")

        var reverted: [String] = [], injected: [String] = []
        for source in sources {
            // Control, same run: the file as shipped is clean through this exact predicate.
            XCTAssertEqual(PanelScaleLint.violations(in: source.text), [],
                           "\(source.name) is not clean before mutation — the canary below would be "
                           + "measuring a pre-existing violation rather than its own")

            var revertedThisSource = false

            // TWO unscaled spellings, because running the mutation matrix (rather than reasoning about
            // it) showed one is not enough. `.font(.system(size: 11))` is AC-2's literal regression, but
            // it ALSO trips the raw-constructor check — so a build that loosened the `.panel(…)` shape
            // check to accept anything still caught it here, and this canary stayed green while the gate
            // was half dead. `.font(.body)` has no raw constructor to fall back on, so only the shape
            // check can catch it; it is also the exact spelling issue #756 measured to be INERT on macOS,
            // which makes it the likeliest well-intentioned regression.
            for spelling in [".font(.system(size: 11))", ".font(.body)"] {
                let (corpse, expectedLine, wasReverted) = mutate(source.text, to: spelling)
                let violations = PanelScaleLint.violations(in: corpse)
                XCTAssertTrue(violations.contains { $0.line == expectedLine },
                              "\(source.name): rewriting the site on line \(expectedLine) as "
                              + "`\(spelling)` did NOT trip the lint there (got "
                              + "\(violations.map(\.description))) — this file's half of AC-2 cannot "
                              + "fail, so its green is not evidence (issue #748 CONSTRAINT-A)")
                // `mutate` reverts iff the file HAS a `.font(.panel(` site, so its choice is a property
                // of the SOURCE and both spellings above make the same one.
                revertedThisSource = wasReverted
            }
            if revertedThisSource { reverted.append(source.name) } else { injected.append(source.name) }
        }
        XCTAssertEqual(reverted.count + injected.count, sources.count,
                       "mutated \(reverted.count + injected.count) of \(sources.count) panel-scaled sources")
        XCTAssertGreaterThanOrEqual(reverted.count, 3,
            "only \(reverted.count) source(s) were mutated by REVERTING a real `.font(.panel(…))` site; "
            + "the rest fell back to appending one (\(injected.joined(separator: ", "))). Issue #756 "
            + "scaled 54 sites across 5 files. A floor of 3 (not today's 5) so that consolidating panel "
            + "views does not redden this with an explanation — \"the revert no longer matches the "
            + "shipped call shape\" — that would be false")
    }

    /// Rewrite the first real `.font(.panel(` site as `spelling`; fall back to appending one.
    ///
    /// The original argument list is kept as a trailing comment, which `scan(_:)` redacts — so it cannot
    /// change the verdict, and the rewritten line reads to the lint exactly as a shipped unscaled site
    /// would. The corpse is never compiled: a mutation only has to be textually faithful.
    private func mutate(_ text: String, to spelling: String) -> (corpse: String, line: Int, reverted: Bool) {
        var lines = text.components(separatedBy: "\n")
        if let index = lines.firstIndex(where: { $0.contains(".font(.panel(") }) {
            let indent = String(lines[index].prefix(while: { $0 == " " }))
            lines[index] = indent + spelling + "  // was: "
                + lines[index].trimmingCharacters(in: .whitespaces)
            return (lines.joined(separator: "\n"), index + 1, true)
        }
        lines.append("private let canaryText = Text(\"x\")\(spelling)")
        return (lines.joined(separator: "\n"), lines.count, false)
    }

    // MARK: - CONSTRAINT-A: the predicate accepts the scaled form and rejects every unscaled one

    // The mutation above proves the lint can fail. This proves it fails for the RIGHT reasons and — the
    // half that protects the tree — that it does not fail for the wrong ones. A lint that reddened the
    // shipped `.font(.panel(…))` spelling would be removed within the week, so the accept list is as
    // load-bearing as the reject list.
    func testThePredicateAcceptsTheScaledFormAndRejectsTheUnscaledOnes() {
        let accepted = [
            ".font(.panel(11, scale: scale))",
            ".font(.panel(11, .semibold, scale: scale))",
            ".font(.panel(10.5, scale: scale))",
            ".font(.panel(style: .body, scale: scale))",
            ".font(.panel(style: .caption2, .medium, scale: scale))",
            ".font(.panel(11, scale: scale)).monospacedDigit()",
            ".font(Font.panel(11, scale: scale))",
            ".font(\n    .panel(11, scale: scale))",          // split across lines
        ]
        for form in accepted {
            XCTAssertEqual(PanelScaleLint.violations(in: form), [],
                           "the lint rejected \(form.debugDescription), which is the form issue #756 "
                           + "established — this would redden the shipped tree")
        }

        let rejected = [
            ".font(.system(size: 11))",
            ".font(.system(size: 11, weight: .semibold))",
            // Relative styles: INERT on macOS (issue #756 measured it). Treating these as "already
            // scaling" is the specific mistake that would wave through the pre-#756 call sites.
            ".font(.body)",
            ".font(.caption)",
            ".font(.headline)",
            ".font(titleFont)",                                // a variable of unknown provenance
            ".font(.system(size: 11 * scale))",                // correct today, but not the one shape
            "let heading = Font.system(size: 13)",             // built outside any `.font(…)`
            "let heading = NSFont.systemFont(ofSize: 13)",
        ]
        for form in rejected {
            XCTAssertFalse(PanelScaleLint.violations(in: form).isEmpty,
                           "the lint accepted \(form.debugDescription) — an unscaled font would ship "
                           + "silently (issue #748 CONSTRAINT-A)")
        }
    }

    // MARK: - CONSTRAINT-A: comments and string literals can neither fake nor hide a violation

    // Both directions are defects. Prose that QUOTES the anti-pattern must not trip the gate — this very
    // file quotes it a dozen times and `StatusPanelTypeScale` quotes it four — or the rule becomes "do not
    // discuss the rule". And a `//` inside a string literal must not blind the remainder of its line, which
    // is precisely the false NEGATIVE a naive strip-from-the-first-slash would introduce.
    func testCommentsAndStringLiteralsNeitherFakeNorHideAViolation() {
        let benign = [
            "// never write .font(.system(size: 11)) in a panel view",
            "/* .font(.system(size: 11)) */",
            "/* outer /* .font(.system(size: 11)) */ still commented */",   // Swift nests block comments
            "let sample = \".font(.system(size: 11))\"",
            "/// Doc comment mentioning `.font(.body)` and `NSFont.systemFont(ofSize: 13)`.",
        ]
        for form in benign {
            XCTAssertEqual(PanelScaleLint.violations(in: form), [],
                           "the lint tripped on \(form.debugDescription), which is prose, not code — the "
                           + "rule would forbid documenting itself")
        }

        let hiding = [
            "let docs = \"https://example.invalid/a\"; Text(\"x\").font(.system(size: 11))",
            "/* note */ Text(\"x\").font(.system(size: 11))",
            "Text(\"x\").font(.system(size: 11))  // explained below",
            "let quoted = \"// .font(.panel(11, scale: s))\"; Text(\"x\").font(.body)",
        ]
        for form in hiding {
            XCTAssertFalse(PanelScaleLint.violations(in: form).isEmpty,
                           "the lint missed the real violation in \(form.debugDescription) — comment or "
                           + "string handling is swallowing live code (issue #748 CONSTRAINT-A)")
        }
    }

    // MARK: - CONSTRAINT-A: the escape hatch costs a reason

    func testTheExemptionMarkerSuppressesItsLineAndOnlyWithAWrittenReason() {
        let marker = PanelScaleLint.exemptionMarker
        XCTAssertEqual(
            PanelScaleLint.violations(in: ".font(.system(size: 11))  // \(marker) bar-size lock, issue #437"),
            [], "a marker with a written reason did not suppress the violation")

        for weak in ["  // \(marker)", "  // \(marker)   ", "  // panel-scale-exempt no colon"] {
            XCTAssertFalse(PanelScaleLint.violations(in: ".font(.system(size: 11))\(weak)").isEmpty,
                           "\(weak.debugDescription) suppressed the violation — an escape hatch with no "
                           + "reason is a silent opt-out (issue #748 CONSTRAINT-A)")
        }

        // A marker inside a STRING LITERAL licenses nothing. The marker is read from the comment view of
        // `scan(_:)`, so a literal that merely contains the token is not a comment and cannot suppress —
        // the same false-negative direction redaction closes for the violation scan itself.
        XCTAssertFalse(
            PanelScaleLint.violations(in: "Text(\"// \(marker) x\").font(.system(size: 11))").isEmpty,
            "a marker inside a string literal suppressed a real violation — the suppression path is "
            + "reading raw text instead of the comment view (issue #748 CONSTRAINT-A)")

        // The marker suppresses its OWN line only; the rule stays live on every other line.
        let twoLines = ".font(.system(size: 11))  // \(marker) justified\n.font(.system(size: 12))"
        XCTAssertEqual(PanelScaleLint.violations(in: twoLines).map(\.line), [2, 2],
                       "expected the unexcused line 2 to violate (twice: modifier shape + raw "
                       + "constructor) and line 1 to be suppressed")

        // Line scope, asserted rather than left to be discovered: a marker excuses EVERY site on its line.
        // Documented on `exemptionMarker`; pinned here so a future change to site-scoping is a deliberate
        // decision with a failing test, not a silent behaviour swap.
        XCTAssertEqual(
            PanelScaleLint.violations(in: ".font(.body); .font(.caption)  // \(marker) both justified"), [],
            "the marker no longer covers its whole line — if that is intended, update `exemptionMarker`")
    }

    // MARK: - Fail closed on literal forms the scanner does not model

    // Today no SwiftUI source in this app uses a multiline or raw literal. If one lands, the scanner would
    // mis-track quoting state and could report a green it did not earn, so it reports THIS instead.
    func testAnUnmodelledStringLiteralIsReportedRatherThanScannedBlind() {
        for form in ["let s = \"\"\"\nmultiline\n\"\"\"", "let s = #\"raw\"#"] {
            XCTAssertEqual(PanelScaleLint.violations(in: form).map(\.kind), [.unsupportedStringLiteral],
                           "\(form.debugDescription) was scanned rather than refused")
        }

        // …but only when the opener appears in CODE position. A comment that merely DISCUSSES `"""` or
        // `#"` must not make the file report a literal it does not contain: the bail-out is fail-closed,
        // so a false positive costs nothing in safety but hands the reader an untrue diagnosis.
        let prose = "// a \"\"\" and a #\" mentioned in prose\nText(\"x\").font(.panel(11, scale: s))"
        XCTAssertEqual(PanelScaleLint.violations(in: prose), [],
                       "prose mentioning an unmodelled literal was reported as containing one")
    }

    // MARK: - Discovery cannot be escaped by foldering or by an import with a trailing comment

    // The two discovery details the file header calls load-bearing, pinned here rather than left to
    // prose. Both were fail-OPEN before, and either one would have moved the escape hatch from the file's
    // NAME — which mechanical discovery exists to close — to its DIRECTORY or its import line.
    func testDiscoveryFindsSwiftUISourcesInSubdirectoriesAndPastImportComments() {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("panel-lint-discovery-\(UUID().uuidString)")
        let nested = root.appendingPathComponent("Panel/Deeper")
        defer { try? FileManager.default.removeItem(at: root) }
        XCTAssertNoThrow(try FileManager.default.createDirectory(
            at: nested, withIntermediateDirectories: true))

        let planted: [(String, String)] = [
            ("Flat.swift", "import SwiftUI\nText(\"x\").font(.system(size: 11))"),
            ("Panel/Deeper/Nested.swift", "import SwiftUI\nText(\"x\").font(.system(size: 12))"),
            ("Panel/Commented.swift", "import SwiftUI  // Font, View\nText(\"x\").font(.body)"),
            ("Panel/Lookalike.swift", "import SwiftUIX\nText(\"x\").font(.system(size: 13))"),
        ]
        for (path, text) in planted {
            XCTAssertNoThrow(try text.write(to: root.appendingPathComponent(path),
                                            atomically: true, encoding: .utf8))
        }

        let found = swiftUISources(under: root).map(\.name).sorted()
        XCTAssertEqual(found, ["Flat.swift", "Panel/Commented.swift", "Panel/Deeper/Nested.swift"],
                       "discovery missed a nested or comment-suffixed SwiftUI source, or picked up the "
                       + "`import SwiftUIX` lookalike — a panel view could ship unlinted")

        // And the discovered files are actually LINTED, not merely listed.
        for source in swiftUISources(under: root) {
            XCTAssertFalse(PanelScaleLint.violations(in: source.text).isEmpty,
                           "\(source.name) was discovered but its unscaled font was not flagged")
        }
    }

    // MARK: - The import predicate itself

    func testTheImportPredicateAcceptsRealImportsAndRejectsLookalikes() {
        for accepted in ["import SwiftUI", "  import SwiftUI  ", "import SwiftUI // Font, View",
                         "import Foundation\nimport SwiftUI\nimport AppKit"] {
            XCTAssertTrue(PanelScaleLint.declaresSwiftUIImport(accepted),
                          "\(accepted.debugDescription) is a real SwiftUI import and was not detected")
        }
        for rejected in ["import SwiftUIX", "import SwiftUICore", "// import SwiftUI",
                         "import AppKit", "/// mentions import SwiftUI in prose"] {
            XCTAssertFalse(PanelScaleLint.declaresSwiftUIImport(rejected),
                           "\(rejected.debugDescription) was treated as a SwiftUI import")
        }
    }
}
#endif
