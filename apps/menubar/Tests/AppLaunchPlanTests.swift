// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Coverage for the app entry point's pure decision layer (issue #764) — the half of `main.swift` that is
// a function of plain values. `main.swift` itself is top-level AppKit entry code and can NEVER live in a
// unit-test bundle, so the decisions moved out to `AppLaunchPlan` and are covered here: which mode the
// process launched in (AC-2), how a socket-resolve failure is worded, and each control-command call
// site's timeout budget.
//
// AC-3's own examples are covered ELSEWHERE, and this file does not duplicate them. The issue names "what
// to do on first launch" and "when `canStartDaemon` is false" as startup decisions to extract; #170
// already extracted both into `LoginItemModel` (`registerAppLoginItemOnLaunch`, `canStartDaemon`,
// `reconcileDaemonAgentRegistration`), covered by `LoginItemModelTests` against a fake `SMAppService`.
// See the issue #764 PR body for the measurement, and `design/README.md` for what stays manual.
//
// CONSTRAINT-A (issue #748): each gate whose verdict runs through a DERIVED predicate ships a canary
// proving that predicate can FAIL, driven through the SAME comparison the real assertion uses. That is
// the dispatch gates (the canaries are mutant parsers — precedence-blind, and presence-rather-than-value
// gallery matching) and the cross-language budget gate (an under-bound and a hair-margin budget run
// through the same clearance and headroom comparisons). The rest compare a returned value directly.

import XCTest

final class AppLaunchPlanTests: XCTestCase {

    /// argv as AppKit hands it over: argv[0] is the executable path.
    private func argv(_ tail: String...) -> [String] { ["/Applications/Sessiometer.app/Contents/MacOS/Sessiometer"] + tail }

    private func mode(_ arguments: [String], _ environment: [String: String] = [:],
                      toolModes: Bool = true) -> AppLaunchMode {
        AppLaunchPlan.mode(arguments: arguments, environment: environment, toolModesAvailable: toolModes)
    }

    // MARK: - AC-2: tool-mode dispatch

    func testAPlainLaunchIsTheProductPath() {
        XCTAssertEqual(mode(argv()), .normal)
        XCTAssertEqual(mode(argv("-NSDocumentRevisionsDebugMode", "YES")), .normal,
                       "unrelated arguments (Xcode injects some) must not divert the product path")
    }

    func testRenderPanelTakesTheFollowingArgumentAsItsOutputDirectory() {
        XCTAssertEqual(mode(argv("--render-panel", "design/renders/panel")),
                       .renderPanel(outputDirectory: "design/renders/panel"))
    }

    func testRenderBarGlyphsTakesTheFollowingArgumentAsItsOutputDirectory() {
        XCTAssertEqual(mode(argv("--render-bar-glyphs", "design/renders/bar-glyphs")),
                       .renderBarGlyphs(outputDirectory: "design/renders/bar-glyphs"))
    }

    func testTheGlyphGalleryIsOptedIntoByTheEnvironment() {
        XCTAssertEqual(mode(argv(), ["SESSIOMETER_GLYPH_GALLERY": "1"]), .glyphGallery)
    }

    // Exact-"1", not mere presence: an empty or "0" value inherited from a shell profile must not silently
    // turn a normal launch into a gallery launch that wires no daemon and no transport.
    func testTheGalleryOptInIsAnExactValueNotMerePresence() {
        for value in ["", "0", "true", "yes", "2", " 1", "1 "] {
            XCTAssertEqual(mode(argv(), ["SESSIOMETER_GLYPH_GALLERY": value]), .normal,
                           "SESSIOMETER_GLYPH_GALLERY=\(value.debugDescription) must NOT enable the gallery")
        }
        XCTAssertEqual(mode(argv(), ["SESSIOMETER_GLYPH_GALLERY_EXTRA": "1"]), .normal,
                       "a differently-named variable must not enable the gallery")
    }

    // Precedence is observable when more than one is supplied, so it is pinned rather than left to
    // whichever `if` happens to be written first in the delegate.
    func testDispatchPrecedenceIsPanelThenBarGlyphsThenGallery() {
        XCTAssertEqual(mode(argv("--render-bar-glyphs", "b", "--render-panel", "p"),
                            ["SESSIOMETER_GLYPH_GALLERY": "1"]),
                       .renderPanel(outputDirectory: "p"),
                       "--render-panel outranks both, regardless of argv order")
        XCTAssertEqual(mode(argv("--render-bar-glyphs", "b"), ["SESSIOMETER_GLYPH_GALLERY": "1"]),
                       .renderBarGlyphs(outputDirectory: "b"),
                       "--render-bar-glyphs outranks the gallery")
    }

    func testARepeatedFlagResolvesOnItsFirstOccurrence() {
        XCTAssertEqual(mode(argv("--render-panel", "first", "--render-panel", "second")),
                       .renderPanel(outputDirectory: "first"))
    }

    // The shipped dispatch takes `arguments[idx + 1]` verbatim — it does not check that the value is not
    // itself a flag. Pinned as shipped behaviour, not asserted as desirable.
    func testTheOutputDirectoryIsTakenVerbatimEvenWhenItLooksLikeAFlag() {
        XCTAssertEqual(mode(argv("--render-panel", "--render-bar-glyphs")),
                       .renderPanel(outputDirectory: "--render-bar-glyphs"),
                       "the value after the flag is taken verbatim — no flag-shaped-value rejection ships")
    }

    // A render flag with no following argument falls through to `.normal` — so a mistyped invocation
    // SILENTLY launches the real app and installs a live status item instead of reporting the mistake.
    // Asserted as the shipped behaviour it is, and filed rather than changed under a coverage item
    // (acting on it changes the app's CLI surface). See issue #850.
    func testATrailingToolFlagWithNoDirectorySilentlyFallsThroughToANormalLaunch() {
        XCTAssertEqual(mode(argv("--render-panel")), .normal)
        XCTAssertEqual(mode(argv("--render-bar-glyphs")), .normal)
        // The condition IS nameable — the dispatch just does not act on it yet.
        XCTAssertTrue(AppLaunchPlan.toolFlagMissingDirectory(arguments: argv("--render-panel")),
                      "the missing-directory condition must be detectable even while dispatch ignores it")
        XCTAssertTrue(AppLaunchPlan.toolFlagMissingDirectory(arguments: argv("--render-bar-glyphs")))
        XCTAssertFalse(AppLaunchPlan.toolFlagMissingDirectory(arguments: argv("--render-panel", "dir")))
        XCTAssertFalse(AppLaunchPlan.toolFlagMissingDirectory(arguments: argv()),
                       "no flag at all is not a missing-directory case")
    }

    // A release build compiles the tool surface out entirely, so its ONLY reachable mode is `.normal`.
    // This is what makes it safe that the flags carry no authentication: they do not exist in a shipped app.
    func testAReleaseBuildHasExactlyOneReachableMode() {
        let everyToolInvocation: [([String], [String: String])] = [
            (argv("--render-panel", "p"), [:]),
            (argv("--render-bar-glyphs", "b"), [:]),
            (argv(), ["SESSIOMETER_GLYPH_GALLERY": "1"]),
            (argv("--render-panel", "p", "--render-bar-glyphs", "b"), ["SESSIOMETER_GLYPH_GALLERY": "1"]),
        ]
        for (arguments, environment) in everyToolInvocation {
            XCTAssertEqual(mode(arguments, environment, toolModes: false), .normal,
                           "\(arguments.dropFirst()) + \(environment) must be inert in a release build")
        }
        XCTAssertEqual(everyToolInvocation.count, 4, "every tool entry point must be checked, not a sample")
    }

    // This suite runs under `xcodebuild test` in Debug, so the build-flavour constant must report true
    // here — otherwise every dispatch test above would be exercising the release short-circuit and
    // asserting nothing about the parser.
    func testThisTestBundleSeesTheToolModesAsAvailable() {
        XCTAssertTrue(AppLaunchPlan.toolModesAvailableInThisBuild,
                      "MenubarTests builds Debug — if this is false the dispatch tests are vacuous")
    }

    // CANARY — the dispatch assertions must reject mutant parsers, through the same equality comparisons.
    func testTheDispatchGatesCanFail() {
        // (a) a precedence-blind parser (bar-glyphs checked first) disagrees on the both-flags input
        func precedenceBlind(_ arguments: [String]) -> AppLaunchMode {
            if let i = arguments.firstIndex(of: AppLaunchPlan.renderBarGlyphsFlag), i + 1 < arguments.count {
                return .renderBarGlyphs(outputDirectory: arguments[i + 1])
            }
            if let i = arguments.firstIndex(of: AppLaunchPlan.renderPanelFlag), i + 1 < arguments.count {
                return .renderPanel(outputDirectory: arguments[i + 1])
            }
            return .normal
        }
        let both = argv("--render-bar-glyphs", "b", "--render-panel", "p")
        XCTAssertNotEqual(precedenceBlind(both), mode(both),
                          "the precedence-blind canary agreed with the real parser — the precedence gate cannot fail")

        // (b) a presence-rather-than-value gallery match disagrees on SESSIOMETER_GLYPH_GALLERY=0
        func presenceMatched(_ environment: [String: String]) -> AppLaunchMode {
            environment[AppLaunchPlan.glyphGalleryEnvironmentKey] != nil ? .glyphGallery : .normal
        }
        let zeroed = ["SESSIOMETER_GLYPH_GALLERY": "0"]
        XCTAssertNotEqual(presenceMatched(zeroed), mode(argv(), zeroed),
                          "the presence-matching canary agreed with the real parser — the exact-value gate cannot fail")
    }

    // MARK: - Degrade-loudly wording

    func testEveryResolveFailureHasItsOwnOperatorFacingSentence() {
        let home = AppLaunchPlan.degradeReason(for: .homeUnresolved)
        let sandboxed = AppLaunchPlan.degradeReason(for: .sandboxed(passwdHome: "/Users/op",
                                                                    containerHome: "/Users/op/Library/Containers/x/Data"))
        for (label, reason) in [("homeUnresolved", home), ("sandboxed", sandboxed)] {
            XCTAssertFalse(reason.isEmpty, "\(label): a blank reason degrades silently, which is the one thing "
                           + "the honest-degrade contract forbids")
            XCTAssertFalse(reason.hasPrefix(" "), "\(label): the reason is rendered inline — no leading padding")
        }
        XCTAssertNotEqual(home, sandboxed, "two distinct failures must not read identically — the reason is the "
                          + "only thing that tells the operator which one happened")
    }

    // The reason is operator-facing and is carried on the event. `ResolveError.sandboxed` carries both
    // paths for the LOG; the rendered sentence must name the condition, never echo a filesystem path.
    func testTheSandboxedReasonNamesTheConditionWithoutEchoingPaths() {
        let reason = AppLaunchPlan.degradeReason(for: .sandboxed(passwdHome: "/Users/op",
                                                                 containerHome: "/Users/op/Library/Containers/x/Data"))
        XCTAssertTrue(reason.lowercased().contains("sandbox"), "the sentence must name the condition: \(reason)")
        XCTAssertFalse(reason.contains("/Users/op"), "a rendered reason must not echo a home path")
        XCTAssertFalse(reason.contains("Containers"), "a rendered reason must not echo a container path")
    }

    func testTheHomeUnresolvedReasonNamesTheCondition() {
        let reason = AppLaunchPlan.degradeReason(for: .homeUnresolved)
        XCTAssertTrue(reason.lowercased().contains("home"), "the sentence must name the condition: \(reason)")
    }

    // MARK: - The degrade FEED (the other half of the honest-degrade contract)

    // The wording says WHY; this says the glance actually moves. Both halves have to hold: a feed that
    // yields nothing leaves the store at its initial `.connecting` forever — precisely the "connecting
    // that will never resolve" the degrade path exists to prevent.
    func testTheDegradeStreamYieldsExactlyOneDisconnectedThenFinishes() async {
        var received: [TransportEvent] = []
        for await event in AppLaunchPlan.disconnectedStream(reason: "home directory unresolved") {
            received.append(event)
        }
        // Terminating at all IS half the assertion: a stream that never finished would hang this test
        // rather than fail it, so reaching this line is itself the evidence.
        XCTAssertEqual(received.count, 1, "exactly one event — not zero (a silent stall), not a repeat")
        guard case .disconnected(let reason) = received.first else {
            return XCTFail("the degrade feed must yield `.disconnected`, got \(String(describing: received.first)) — "
                           + "any other event would let the bar claim a state the app cannot vouch for")
        }
        XCTAssertEqual(reason, "home directory unresolved", "the reason must be carried through, not dropped")
    }

    // The two halves compose: whatever `degradeReason` renders is what the feed carries.
    func testEveryResolveFailureReachesTheFeedWithItsOwnReason() async {
        let failures: [SocketPathResolver.ResolveError] = [
            .homeUnresolved,
            .sandboxed(passwdHome: "/Users/op", containerHome: "/Users/op/Library/Containers/x/Data"),
        ]
        var reasons: Set<String> = []
        for failure in failures {
            var received: [TransportEvent] = []
            for await event in AppLaunchPlan.disconnectedStream(reason: AppLaunchPlan.degradeReason(for: failure)) {
                received.append(event)
            }
            guard case .disconnected(let reason) = received.first else {
                return XCTFail("\(failure) did not reach the feed as `.disconnected`")
            }
            reasons.insert(reason)
        }
        XCTAssertEqual(reasons.count, failures.count,
                       "each resolve failure must arrive with its OWN sentence — a collapsed reason leaves the "
                       + "operator unable to tell which failure happened")
    }

    // MARK: - Control-command timeout budgets

    func testEachCallSiteCarriesItsRatifiedBudget() {
        XCTAssertEqual(AppLaunchPlan.captureTimeout, .seconds(2), "#360 capture — the client default")
        XCTAssertEqual(AppLaunchPlan.swapTimeout, .seconds(15), "#169 swap — clears the cross-process lock bound")
        XCTAssertEqual(AppLaunchPlan.statsTimeout, .seconds(5), "#446 stats — a bounded read off the run loop")
        XCTAssertEqual(AppLaunchPlan.configTimeout, .seconds(5), "#268 config — validate + atomic write")
    }

    // The ORDERING is the actual policy; the numbers are its current expression. Swap is the only call
    // site that waits behind a lock, so it must be the largest by a clear margin; capture is answered off
    // the run loop with no disk write, so it is the smallest.
    func testTheBudgetOrderingReflectsWhatEachExchangeWaitsOn() {
        XCTAssertGreaterThan(AppLaunchPlan.swapTimeout, AppLaunchPlan.statsTimeout,
                             "only swap waits on the single-writer lock — it must outrank every read")
        XCTAssertGreaterThan(AppLaunchPlan.swapTimeout, AppLaunchPlan.configTimeout)
        XCTAssertGreaterThan(AppLaunchPlan.statsTimeout, AppLaunchPlan.captureTimeout,
                             "a store aggregation is slower than a capture answered off the run loop")
        XCTAssertEqual(AppLaunchPlan.statsTimeout, AppLaunchPlan.configTimeout,
                       "both are bounded, lock-free daemon-side operations — they share one budget on purpose")
    }

    // CROSS-LANGUAGE: the swap budget's whole reason to exist is that it must exceed the Rust
    // `SWAP_LOCK_MAX_WAIT` a swap may sit behind. If the Rust bound is raised and this is not, a swap that
    // is merely QUEUED and about to succeed reports a false failure for a write that then commits — the
    // exact regression the 15 s was chosen to prevent. Reading the constant from source (the same
    // source-tree technique `StatusGaugeTests` uses for `.symbolset`s) is what makes the two move together.
    func testTheSwapBudgetStillClearsTheRustLockBound() throws {
        let swapRS = URL(fileURLWithPath: #filePath)      // .../apps/menubar/Tests/AppLaunchPlanTests.swift
            .deletingLastPathComponent()                  // .../apps/menubar/Tests
            .deletingLastPathComponent()                  // .../apps/menubar
            .deletingLastPathComponent()                  // .../apps
            .deletingLastPathComponent()                  // repo root
            .appendingPathComponent("src/swap.rs")
        let source = try XCTUnwrap(try? String(contentsOf: swapRS, encoding: .utf8),
                                   "could not read \(swapRS.path) — the cross-language bound cannot be checked")

        // `pub(crate) const SWAP_LOCK_MAX_WAIT: Duration = Duration::from_secs(10);`
        let pattern = #"SWAP_LOCK_MAX_WAIT[^=]*=\s*Duration::from_secs\((\d+)\)"#
        let regex = try NSRegularExpression(pattern: pattern)
        let range = NSRange(source.startIndex..<source.endIndex, in: source)
        let match = try XCTUnwrap(regex.firstMatch(in: source, range: range),
                                  "SWAP_LOCK_MAX_WAIT is no longer declared as `Duration::from_secs(N)` in "
                                  + "src/swap.rs — this gate has gone blind and needs re-pointing, not deleting")
        let seconds = try XCTUnwrap(Range(match.range(at: 1), in: source).flatMap { Int64(source[$0]) })

        XCTAssertGreaterThan(AppLaunchPlan.swapTimeout, Duration.seconds(seconds),
                             "swap budget \(AppLaunchPlan.swapTimeout) does NOT clear the Rust lock bound of "
                             + "\(seconds)s — a queued-but-succeeding swap would report a false failure")
        // And with real headroom for the keychain read/write beneath the lock, not merely by a hair.
        XCTAssertGreaterThanOrEqual(AppLaunchPlan.swapTimeout, Duration.seconds(seconds + 3),
                                    "the budget must clear \(seconds)s with headroom for the keychain work "
                                    + "beneath the lock, not by a margin a slow disk erases")
    }

    // CANARY — the cross-language gate must reject a budget that does not clear the bound, through the
    // SAME comparison the real assertion uses.
    func testTheCrossLanguageBudgetGateCanFail() {
        let rustBound = Duration.seconds(10)
        let tooSmall = Duration.seconds(8)
        XCTAssertFalse(tooSmall > rustBound,
                       "the under-bound canary passed the clearance predicate — that gate cannot fail")
        XCTAssertTrue(AppLaunchPlan.swapTimeout > rustBound, "the shipped budget clears the same predicate")
        // A hair-margin budget clears the bound but fails the headroom check — both halves must bite.
        XCTAssertFalse(Duration.seconds(11) >= rustBound + .seconds(3),
                       "the hair-margin canary passed the headroom predicate — that half cannot fail")
    }

    // Every budget must be positive and finite — an unbounded client is exactly the wedged-daemon hang
    // `ControlCommandClient` exists to prevent.
    func testEveryBudgetIsBounded() {
        let budgets: [(String, Duration)] = [
            ("capture", AppLaunchPlan.captureTimeout), ("swap", AppLaunchPlan.swapTimeout),
            ("stats", AppLaunchPlan.statsTimeout), ("config", AppLaunchPlan.configTimeout),
        ]
        for (name, budget) in budgets {
            XCTAssertGreaterThan(budget, .zero, "\(name): a non-positive budget would fail every exchange instantly")
            XCTAssertLessThanOrEqual(budget, .seconds(60),
                                     "\(name): a budget this large is indistinguishable from a hang to the operator")
        }
        XCTAssertEqual(budgets.count, 4, "all four control-command call sites must be checked, not a sample")
    }
}
