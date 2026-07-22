// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Hermetic tests for the Stats tab (issue #446): the `stats` wire DECODER (`StatsWire` / `decodeStatsReply`,
// WireModel.swift), the pure presentation + sparkline geometry (`StatusPanelFormat`), and the Stats-tab
// model's idle → loading → loaded → failed phase machine + tab selection (`PanelStatsModel`).
//
// The decoder is exercised against the SAME `Fixtures.statsBasic` the cross-language golden guard
// (`WireGoldenTests`) pins byte-for-byte to the Rust `wire-stats-basic.json` — so a decode assertion here
// and the byte-equality assertion there together prove the Swift mirror both MATCHES the daemon bytes and
// READS them into the right values. No parallel fixture (per #356's design: one source of truth).
//
// The model is driven by the SAME in-process fake connection the transport suite uses
// (`CommandFakeConnection` / `CommandFakeConnector` in `ControlCommandTransportTests`) — NO real socket, NO
// live daemon — so every phase transition and every reply variant (loaded / daemon-error / undecodable /
// transport-fault / no-client) is exercised deterministically. The Stats tab is READ-ONLY, so — unlike the
// swap suite — a test run can never mutate any daemon state.
//
// The sparkline geometry is R-2 parity with the CLI trend sparkline (`src/stats.rs`): per-bucket session
// PEAK on the FIXED [0, 1] (0–100% cap) scale, NOT auto-normalised — the same pick + scale the mock draws.

import Foundation
import os
import XCTest

final class StatsTests: XCTestCase {

    // MARK: - StatsCommand: the wire request

    // The panel reads the DEFAULT 7-day daily-bucket window — `period` = `week` (the mock's "last 7 days";
    // the CLI has no `7d` period, that is `--since` grammar). Keys in the client's deterministic sorted order.
    func testStatsCommandSerializesWeekPeriod() throws {
        XCTAssertEqual(try encode(StatsCommand()), #"{"cmd":"stats","period":"week"}"#)
    }

    // The request bytes carry a verb + a period tag and nothing else — no credential of any kind (issue #15).
    func testStatsCommandBytesCarryNoSecret() throws {
        let line = try encode(StatsCommand())
        XCTAssertFalse(line.contains("@"), "no email in the command bytes")
        XCTAssertFalse(line.lowercased().contains("token"), "no token in the command bytes")
        XCTAssertFalse(line.lowercased().contains("oauth"), "no oauth blob in the command bytes")
    }

    // MARK: - StatsWire: the decoder (against the byte-pinned golden fixture)

    // The core "decode against the golden fixture" assertion: the Swift mirror reads the daemon's bytes into
    // the right values across the whole shape — window, series, per-account aggregate, roster, enums.
    func testDecodesStatsGoldenReplyIntoTheRightValues() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsBasic) else {
            return XCTFail("expected a StatsWire document, not an error envelope")
        }
        XCTAssertEqual(wire.schema, 1)

        // Window
        XCTAssertEqual(wire.window.start, 1_782_864_000)
        XCTAssertEqual(wire.window.end, 1_782_907_200)
        XCTAssertEqual(wire.window.label, "last 24h (Jul 1–Jul 1)")
        XCTAssertEqual(wire.window.period, "day")
        XCTAssertNil(wire.window.since, "the golden is period-selected, so `since` is absent")

        // Filter + orphans: the socket verb never filters, and an ABSENT `orphans` key decodes to empty.
        XCTAssertEqual(wire.accounts, [], "an empty filter means all accounts")
        XCTAssertTrue(wire.orphans.isEmpty, "no orphans key present → empty map, never a decode failure")

        // Series: one bucket, carrying the per-account session peak the sparkline plots.
        XCTAssertEqual(wire.series.count, 1)
        let bucket = try XCTUnwrap(wire.series.first)
        XCTAssertEqual(bucket.start, 0)
        XCTAssertEqual(bucket.end, 21_600)
        let bucketWork = try XCTUnwrap(bucket.accounts["work"])
        XCTAssertEqual(bucketWork.session.peak, 0.9, accuracy: 1e-9)

        // Summary roster: the aggregate callout's source.
        XCTAssertEqual(wire.summary.roster.swapCount, 1)
        XCTAssertEqual(wire.summary.roster.swaps.session, 1)
        XCTAssertEqual(wire.summary.roster.swaps.weekly, 0)
        XCTAssertEqual(wire.summary.roster.allHighEpisodes, 0)
        XCTAssertEqual(wire.summary.roster.allHighSecs, 0)

        // Summary per-account: the numeric body + signal source.
        let work = try XCTUnwrap(wire.summary.accounts["work"])
        XCTAssertEqual(work.seen, 3)
        XCTAssertEqual(work.coverage, 1.0, accuracy: 1e-9)
        XCTAssertEqual(work.coverageClass, .complete)
        XCTAssertEqual(work.session.mean, 0.5, accuracy: 1e-9)
        XCTAssertEqual(work.session.peak, 0.9, accuracy: 1e-9)
        XCTAssertEqual(work.session.p95, 0.85, accuracy: 1e-9)
        XCTAssertEqual(work.weekly.mean, 0.3, accuracy: 1e-9)
        XCTAssertEqual(work.weekly.peak, 0.4, accuracy: 1e-9)
        XCTAssertEqual(work.capHits, 1)
        XCTAssertEqual(work.timeAtCapSecs, 300)
        XCTAssertEqual(work.contributionShare, 1.0, accuracy: 1e-9)
        XCTAssertEqual(work.band, .high)

        // Back-compat (#642): the golden is emitted from a HEALTHY report, so `config_unreadable` is
        // ABSENT (not null) and decodes to nil — the `decodeIfPresent` additive-default path. These
        // are the same bytes a PRE-#642 daemon sends, so the ~40 assertions above are also the proof
        // that an older daemon's reply still decodes field-for-field: that is what makes the field
        // safe WITHOUT a `schema` bump. The panel renders no caveat and reads the numbers as the
        // operator's own.
        XCTAssertNil(wire.configUnreadable,
                     "a healthy (or pre-#642) daemon omits `config_unreadable` entirely — no caveat to render")
    }

    // MARK: - #642: the malformed-config wire signal

    // THE #642 REGRESSION, decoder half. Before the fix the daemon served this exact document WITHOUT the
    // key, so the panel had no way to know every ceiling-dependent figure below rested on DEFAULT tunables.
    // The key now arrives and the panel can annotate rather than silently trust (honesty family #479/#582/#632).
    func testDecodesTheConfigUnreadableSignal() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsConfigUnreadable) else {
            return XCTFail("a degraded config must still yield a FULL document, not an error envelope")
        }
        let detail = try XCTUnwrap(wire.configUnreadable, "the #642 signal must decode")
        XCTAssertTrue(detail.contains("config validate"),
                      "the reason points at the command that prints the detail: \(detail)")
        // The daemon never derives this string from the config (the parser's own message re-prints
        // the operator's file, where e-mail labels live), so it is one of a small set of STATIC
        // reasons. Assert that contract at the point of consumption too — the panel renders it
        // verbatim into a fixed-width popover with no scroll view.
        XCTAssertFalse(detail.contains("\n"), "one line — no caret art in a fixed-size popover")
        XCTAssertFalse(detail.contains("|"), "no span-echo gutter of the operator's own config")
        XCTAssertFalse(detail.contains("@"), "no address-shaped token from an echoed config line")
        // The series is still fully served — the panel keeps its best-effort data and qualifies it, rather
        // than losing the tab. That is why the daemon does NOT degrade to an `{"error":…}` envelope here.
        XCTAssertEqual(wire.schema, 1, "an additive field, so still schema:1 — no bump")
        XCTAssertEqual(wire.series.count, 1, "the series survives the degraded path")
        XCTAssertEqual(try XCTUnwrap(wire.summary.accounts["work"]).capHits, 1)
    }

    // The panel copy: it must state the CONSEQUENCE (numbers rest on defaults), not merely that something
    // failed, and route the operator to the command that prints the real detail. Composed from the fixture's
    // reason rather than a hand-written stub, so the assertion is against the string the daemon actually
    // sends (`wire_config_reason`, `src/stats.rs`) and not one invented here to make the test pass.
    func testConfigUnreadableNoteStatesTheConsequenceAndCarriesTheDetail() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsConfigUnreadable) else {
            return XCTFail("expected a StatsWire document")
        }
        let detail = try XCTUnwrap(wire.configUnreadable)
        let note = StatusPanelFormat.statsConfigUnreadableNote(detail)
        XCTAssertTrue(note.contains("default tunables"),
                      "the caveat must say the numbers rest on defaults, not just that a read failed: \(note)")
        XCTAssertTrue(note.contains("config.toml"), "and name what could not be read: \(note)")
        XCTAssertTrue(note.hasPrefix("Computed against default tunables"),
                      "leading with the CONSEQUENCE, not with the fault: \(note)")
        XCTAssertTrue(note.contains("config validate"),
                      "and route the operator to the command that prints the detail: \(note)")
        XCTAssertFalse(note.contains("\n"), "and stay a single paragraph for the caveat strip: \(note)")
    }

    // The redacted `{"error":…}` envelope (an invalid period — off the panel's path, but honestly surfaced)
    // decodes to `.error`, NOT a StatsWire, so the model can render it distinctly.
    func testDecodesStatsErrorEnvelope() throws {
        XCTAssertEqual(try decodeStatsReply(#"{"error":"invalid period"}"#), .error("invalid period"))
    }

    // A well-formed-but-off-contract document (an UNKNOWN `band` — a drifted daemon) is a hard decode error,
    // mirroring serde's rejection of an unknown unit-enum variant — degrade loudly, never mis-read.
    func testUnknownBandIsADecodeError() {
        let line = Fixtures.statsBasic.replacingOccurrences(of: #""band":"high""#, with: #""band":"nova""#)
        XCTAssertThrowsError(try decodeStatsReply(line), "an unknown band must not silently decode")
    }

    // A non-JSON line throws (→ the model's `.undecodable`), exactly like the `watch` decoder.
    func testNonJSONReplyThrows() {
        XCTAssertThrowsError(try decodeStatsReply("not json at all"))
    }

    // MARK: - Sparkline geometry (R-2 parity: session peak, fixed [0,1] scale)

    // The x's are evenly spaced across the inset plot. 96 was the box the chart occupied INSIDE the head row
    // until issue #700 gave it a full-width row of its own; it is kept here as a plain second width, so the
    // even-spacing rule is pinned at more than one box. The mock correspondence moved with the chart — it now
    // lives in `testSparkPointsWidenTheBoxWithoutMovingTheSeries`, the sole mock pin.
    func testSparkPointsXSpacingIsEvenAcrossTheInsetPlot() {
        let pts = StatusPanelFormat.sparkPoints([0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
                                                width: 96, height: 28, inset: 3)
        XCTAssertEqual(pts.map(\.x), [3, 18, 33, 48, 63, 78, 93])
    }

    // The y is the FIXED-scale mapping (NOT auto-normalised): 0 → the floor (bottom − inset), 1 → the top
    // (inset), 0.5 → the midpoint. This is what keeps the sparkline R-2-consistent with the CLI's ramp.
    func testSparkPointsYIsFixedScale() {
        let pts = StatusPanelFormat.sparkPoints([0.0, 0.5, 1.0], width: 96, height: 28, inset: 3)
        XCTAssertEqual(pts.map(\.y), [25, 14, 3])  // bottom=25, mid=14, top=3
    }

    // Over-cap / negative readings clamp to the [top, floor] band — the CLI `ramp_level`'s `[0,1]` clamp.
    func testSparkPointsClampOutOfRange() {
        let pts = StatusPanelFormat.sparkPoints([1.5, -0.5], width: 96, height: 28, inset: 3)
        XCTAssertEqual(pts.map(\.y), [3, 25])  // 1.5 → top (clamped 1), -0.5 → floor (clamped 0)
    }

    // A single-bucket series centres its one point (no divide-by-zero on `n − 1`).
    func testSparkPointsSinglePointCentres() {
        let pts = StatusPanelFormat.sparkPoints([0.5], width: 96, height: 28, inset: 3)
        XCTAssertEqual(pts, [StatusPanelFormat.SparkPoint(x: 48, y: 14)])
    }

    // An empty series yields no points (the view draws nothing).
    func testSparkPointsEmpty() {
        XCTAssertTrue(StatusPanelFormat.sparkPoints([], width: 96, height: 28, inset: 3).isEmpty)
    }

    // The panel↔mock pin (issue #700). The chart's own row is `statsChartWidth` wide, and the build-reference
    // mock authors its `.spark` viewBox at that SAME number (`viewBox="0 0 331 28"` in menubar-preview.html).
    // The panel's Stats tab has no render path — `RenderPanelTool` renders every fixture at the Status tab —
    // so nothing else mechanically checks the two surfaces agree. Asserting the DERIVED width against the
    // mock's authored literal is what turns a panel-geometry change into a red test instead of a silent
    // divergence from the design reference.
    func testStatsChartWidthMatchesTheMockAuthoredViewBox() {
        XCTAssertEqual(StatusPanelFormat.statsChartWidth, 331, accuracy: 0.001,
                       "design/menubar-preview.html authors `.spark` at viewBox=\"0 0 331 28\" — change both")
    }

    // Widening re-spreads the x's and leaves every y untouched: the box geometry changes, the series
    // semantics do not. The x's asserted here are the vertices the mock's `.spark` viewBox carries, so this
    // pins the chart's shape across the two surfaces the way the 96 pt box did before #700.
    func testSparkPointsWidenTheBoxWithoutMovingTheSeries() {
        let series = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]
        let narrow = StatusPanelFormat.sparkPoints(series, width: 96, height: 28, inset: 3)
        let wide = StatusPanelFormat.sparkPoints(
            series, width: StatusPanelFormat.statsChartWidth, height: 28, inset: 3)
        for (actual, expected) in zip(wide.map(\.x), [3, 57.17, 111.33, 165.5, 219.67, 273.83, 328]) {
            XCTAssertEqual(actual, expected, accuracy: 0.01)
        }
        XCTAssertEqual(narrow.map(\.y), wide.map(\.y), "widening must not re-scale the series")
    }

    // A FLAT series keeps its absolute level — an auto-normalising chart would stretch any flat series to the
    // same line, losing the difference between an idle account and a pinned one. This is the ONLY test that
    // catches that: every other series in this section (`[0, 0.5, 1]`, `[1.5, -0.5]`, the monotonic ramp above)
    // already spans its own full range, so auto-normalisation is the identity on them and they stay green.
    // Asserted at the shipped `statsChartWidth`, since the wider, more prominent chart is where a silently
    // re-scaled series would mislead most.
    func testSparkPointsDoNotAutoNormaliseAFlatSeries() {
        let width = StatusPanelFormat.statsChartWidth
        let low = StatusPanelFormat.sparkPoints([0.1, 0.1, 0.1], width: width, height: 28, inset: 3)
        let high = StatusPanelFormat.sparkPoints([0.9, 0.9, 0.9], width: width, height: 28, inset: 3)
        for y in low.map(\.y) { XCTAssertEqual(y, 22.8, accuracy: 0.001) }
        for y in high.map(\.y) { XCTAssertEqual(y, 5.2, accuracy: 0.001) }
    }

    // A box too narrow to hold its own insets has no plot, so it yields NO points rather than folding the
    // series backwards (at width 0 the x's ran 3 → 0 → −3, descending). Latent rather than reachable — the
    // panel is a single fixed width — but the box became a parameter in #700, so the helper is now total
    // over the widths a caller can hand it instead of trusting every future one to be generous.
    func testSparkPointsDegenerateBoxYieldsNoPoints() {
        for width in [0.0, 4.0, 6.0] {
            XCTAssertTrue(StatusPanelFormat.sparkPoints([0.1, 0.5, 0.9], width: width, height: 28, inset: 3).isEmpty,
                          "width \(width) cannot hold a 3 pt inset per side")
        }
        XCTAssertFalse(StatusPanelFormat.sparkPoints([0.1, 0.5], width: 6.5, height: 28, inset: 3).isEmpty,
                       "just past 2 × inset there IS a plot, however thin")
    }

    // The series pick is the per-bucket SESSION PEAK (`src/stats.rs`), and a bucket with no reading for the
    // handle plots at the floor (0) rather than being dropped — an unmeasured bucket is a real low.
    func testSparkSeriesPicksSessionPeakPerBucket() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsBasic) else {
            return XCTFail("expected a StatsWire document")
        }
        XCTAssertEqual(StatusPanelFormat.sparkSeries(wire.series, handle: "work"), [0.9])
        XCTAssertEqual(StatusPanelFormat.sparkSeries(wire.series, handle: "absent-handle"), [0.0])
    }

    // MARK: - Signal (band → the mock's three-way descriptor)

    func testSignalCollapsesTheBandLikeTheCLI() {
        XCTAssertEqual(StatusPanelFormat.statsSignal(.idle), .underused)
        XCTAssertEqual(StatusPanelFormat.statsSignal(.low), .underused)
        XCTAssertEqual(StatusPanelFormat.statsSignal(.moderate), .balanced)
        XCTAssertEqual(StatusPanelFormat.statsSignal(.high), .saturated)
        XCTAssertEqual(StatusPanelFormat.statsSignal(.atCap), .saturated)
    }

    func testSignalLabels() {
        XCTAssertEqual(StatusPanelFormat.StatSignal.underused.label, "underused")
        XCTAssertEqual(StatusPanelFormat.StatSignal.balanced.label, "balanced")
        XCTAssertEqual(StatusPanelFormat.StatSignal.saturated.label, "saturated")
    }

    // MARK: - Numeric body + labels

    func testStatsPercentRoundsAndFloorsAtZero() {
        XCTAssertEqual(StatusPanelFormat.statsPercent(0.0), 0)
        XCTAssertEqual(StatusPanelFormat.statsPercent(0.5), 50)
        XCTAssertEqual(StatusPanelFormat.statsPercent(0.873), 87)
        XCTAssertEqual(StatusPanelFormat.statsPercent(1.0), 100)
        XCTAssertEqual(StatusPanelFormat.statsPercent(1.2), 120, "an over-cap peak legitimately reads > 100%")
        XCTAssertEqual(StatusPanelFormat.statsPercent(-0.1), 0, "a negative never prints below zero")
    }

    func testStatsNumericCells() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsBasic) else {
            return XCTFail("expected a StatsWire document")
        }
        let work = try XCTUnwrap(wire.summary.accounts["work"])
        XCTAssertEqual(StatusPanelFormat.statsSessionMeanPeak(work), "50 / 90%")
        XCTAssertEqual(StatusPanelFormat.statsWeeklyPeak(work), "40%")
    }

    func testStatsDurationIsCoarseTwoUnit() {
        XCTAssertEqual(StatusPanelFormat.statsDuration(6000), "1h40m")
        XCTAssertEqual(StatusPanelFormat.statsDuration(3600), "1h")
        XCTAssertEqual(StatusPanelFormat.statsDuration(300), "5m")
        XCTAssertEqual(StatusPanelFormat.statsDuration(45), "45s")
        XCTAssertEqual(StatusPanelFormat.statsDuration(0), "0s")
        XCTAssertEqual(StatusPanelFormat.statsDuration(-10), "0s")
    }

    func testWindowPhraseAndHeaderSubtitle() {
        XCTAssertEqual(StatusPanelFormat.statsWindowPhrase(window(period: "day")), "last 24h")
        XCTAssertEqual(StatusPanelFormat.statsWindowPhrase(window(period: "week")), "last 7 days")
        XCTAssertEqual(StatusPanelFormat.statsWindowPhrase(window(period: "month")), "last 30 days")
        XCTAssertEqual(StatusPanelFormat.statsWindowPhrase(window(period: "lifetime")), "all time")
        // The mock's header for the panel's default `week` query.
        XCTAssertEqual(StatusPanelFormat.statsHeaderSubtitle(window(period: "week")),
                       "Usage stats · last 7 days")
    }

    // A non-preset window (a `--since` query the socket never sends, but decoded honestly if one arrived)
    // falls back to its raw `since` offset, and lacking even that, to the wire's own human `label` — never
    // an invented span.
    func testWindowPhraseFallsBackToSinceThenLabel() {
        XCTAssertEqual(
            StatusPanelFormat.statsWindowPhrase(
                StatsWindow(start: 0, end: 0, label: "custom span", period: nil, since: "2026-07-01")),
            "since 2026-07-01")
        XCTAssertEqual(
            StatusPanelFormat.statsWindowPhrase(
                StatsWindow(start: 0, end: 0, label: "custom span", period: nil, since: nil)),
            "custom span")
    }

    // The pre-load default header (shown while loading / on failure, before a window arrives) must render the
    // SAME string a loaded `week` window does — so the header never visibly changes shape once data lands.
    func testDefaultHeaderSubtitleMatchesTheWeekWindowHeader() {
        XCTAssertEqual(StatusPanelFormat.statsDefaultHeaderSubtitle,
                       StatusPanelFormat.statsHeaderSubtitle(window(period: "week")))
    }

    func testAggregateTextFromTheGoldenRoster() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsBasic) else {
            return XCTFail("expected a StatsWire document")
        }
        // The golden roster: 0 all-high episodes (0s), swap_count 1, over a `day` window.
        XCTAssertEqual(StatusPanelFormat.statsAggregateText(roster: wire.summary.roster, window: wire.window),
                       "All accounts ≥90% at once — 0 episodes (0s) · swaps 1 · last 24h")
    }

    func testAggregateTextSingularEpisode() {
        let roster = StatsRoster(swapCount: 28,
                                 swaps: StatsSwaps(session: 20, weekly: 4, manual: 3, forced: 1, emergency: 0),
                                 allHighEpisodes: 1, allHighSecs: 6000)
        XCTAssertEqual(StatusPanelFormat.statsAggregateText(roster: roster, window: window(period: "week")),
                       "All accounts ≥90% at once — 1 episode (1h40m) · swaps 28 · last 7 days")
    }

    // MARK: - Failure copy (StatsFailure → the honest one-line Stats-tab message)

    // Every failure maps to a plain, honest sentence — never a blank tab, never a fabricated number (the
    // crown-jewel honesty rule on the read-only Stats surface). Mirrors the swap/capture error-copy tests.
    // Any transport sub-kind collapses to the one "couldn't reach the daemon" line — stats, unlike capture,
    // does not distinguish them (a read either lands or it doesn't).
    func testStatsFailureTextMapsEveryFailureToHumanCopy() {
        XCTAssertEqual(StatusPanelFormat.statsFailureText(.unavailable),
                       "Usage stats unavailable — the daemon socket didn't resolve.")
        XCTAssertEqual(StatusPanelFormat.statsFailureText(.transport(.connectionRefused(reason: "x"))),
                       "Couldn't reach the daemon for usage stats.")
        XCTAssertEqual(StatusPanelFormat.statsFailureText(.transport(.timedOut)),
                       "Couldn't reach the daemon for usage stats.")
        XCTAssertEqual(StatusPanelFormat.statsFailureText(.daemonError("invalid period")),
                       "Usage stats error: invalid period.")
        XCTAssertEqual(StatusPanelFormat.statsFailureText(.undecodable),
                       "Usage stats came back in an unreadable form.")
    }

    // MARK: - Row ordering (join stats handles with the roster order)

    func testOrderedStatHandlesFollowsRosterThenAppendsExtras() {
        // Roster order wins for accounts present in both; a roster account with no reading is dropped.
        XCTAssertEqual(
            StatusPanelFormat.orderedStatHandles(summaryHandles: ["scratch", "work"],
                                                 rosterOrder: ["work", "personal", "scratch"]),
            ["work", "scratch"])
        // A stats-only handle (not in the roster — normally none) is appended alphabetically after the roster.
        XCTAssertEqual(
            StatusPanelFormat.orderedStatHandles(summaryHandles: ["work", "zzz", "aaa"],
                                                 rosterOrder: ["work"]),
            ["work", "aaa", "zzz"])
    }

    // MARK: - Color tokens (mock `--spark` + `--sig-*`, exact values)
    //
    // These pin the EXACT mock values the SwiftUI pill/sparkline are thin consumers of. Like the #388
    // neutral-fill test, this layer IS the fidelity gate: the real popover can't be screenshot-verified in
    // CI, so a wrong alpha or hue here is caught ONLY by these assertions (the opaque-fg / translucent-bg
    // invariant is guarded structurally by `testSignalTextIsOpaqueAndFillIsTranslucent` below).

    func testSparkColorMatchesTheMock() {
        // mock `--spark`: rgba(60,60,67,.55) light / rgba(235,235,245,.5) dark — the secondary-label tint.
        XCTAssertEqual(StatusPanelFormat.sparkColor(dark: false),
                       .init(red: 60.0 / 255, green: 60.0 / 255, blue: 67.0 / 255, alpha: 0.55))
        XCTAssertEqual(StatusPanelFormat.sparkColor(dark: true),
                       .init(red: 235.0 / 255, green: 235.0 / 255, blue: 245.0 / 255, alpha: 0.5))
    }

    func testSignalColorsMatchTheMockExactly() {
        // --sig-*-bg (translucent pill fill), each signal light then dark:
        XCTAssertEqual(StatusPanelFormat.statsSignalFill(.underused, dark: false),
                       .init(red: 0, green: 122.0 / 255, blue: 255.0 / 255, alpha: 0.12))
        XCTAssertEqual(StatusPanelFormat.statsSignalFill(.underused, dark: true),
                       .init(red: 64.0 / 255, green: 140.0 / 255, blue: 230.0 / 255, alpha: 0.20))
        XCTAssertEqual(StatusPanelFormat.statsSignalFill(.balanced, dark: false),
                       .init(red: 30.0 / 255, green: 150.0 / 255, blue: 105.0 / 255, alpha: 0.13))
        XCTAssertEqual(StatusPanelFormat.statsSignalFill(.balanced, dark: true),
                       .init(red: 50.0 / 255, green: 180.0 / 255, blue: 130.0 / 255, alpha: 0.18))
        XCTAssertEqual(StatusPanelFormat.statsSignalFill(.saturated, dark: false),
                       .init(red: 178.0 / 255, green: 120.0 / 255, blue: 20.0 / 255, alpha: 0.15))
        XCTAssertEqual(StatusPanelFormat.statsSignalFill(.saturated, dark: true),
                       .init(red: 210.0 / 255, green: 160.0 / 255, blue: 80.0 / 255, alpha: 0.20))
        // --sig-*-fg (opaque label + dot), each signal light then dark:
        XCTAssertEqual(StatusPanelFormat.statsSignalText(.underused, dark: false),
                       .init(red: 38.0 / 255, green: 104.0 / 255, blue: 189.0 / 255, alpha: 1))
        XCTAssertEqual(StatusPanelFormat.statsSignalText(.underused, dark: true),
                       .init(red: 130.0 / 255, green: 179.0 / 255, blue: 237.0 / 255, alpha: 1))
        XCTAssertEqual(StatusPanelFormat.statsSignalText(.balanced, dark: false),
                       .init(red: 28.0 / 255, green: 138.0 / 255, blue: 95.0 / 255, alpha: 1))
        XCTAssertEqual(StatusPanelFormat.statsSignalText(.balanced, dark: true),
                       .init(red: 96.0 / 255, green: 207.0 / 255, blue: 161.0 / 255, alpha: 1))
        XCTAssertEqual(StatusPanelFormat.statsSignalText(.saturated, dark: false),
                       .init(red: 150.0 / 255, green: 102.0 / 255, blue: 17.0 / 255, alpha: 1))
        XCTAssertEqual(StatusPanelFormat.statsSignalText(.saturated, dark: true),
                       .init(red: 224.0 / 255, green: 178.0 / 255, blue: 104.0 / 255, alpha: 1))
    }

    func testSignalTextIsOpaqueAndFillIsTranslucent() {
        // The pill's foreground carries text (opaque, the readable channel); the background is a translucent fill.
        XCTAssertEqual(StatusPanelFormat.statsSignalText(.saturated, dark: false).alpha, 1)
        XCTAssertLessThan(StatusPanelFormat.statsSignalFill(.saturated, dark: false).alpha, 1)
    }

    // MARK: - PanelStatsModel: the phase machine

    @MainActor
    func testLoadDecodesGoldenIntoLoaded() async {
        let model = PanelStatsModel(client: client(CommandFakeConnection(ackOnSend: Fixtures.statsBasic)))
        await model.load()
        guard case .loaded(let wire) = model.phase else {
            return XCTFail("expected .loaded, got \(model.phase)")
        }
        XCTAssertEqual(wire.schema, 1)
        XCTAssertEqual(wire.summary.accounts["work"]?.band, .high)
    }

    @MainActor
    func testDaemonErrorEnvelopeLandsInFailed() async {
        let model = PanelStatsModel(client: client(CommandFakeConnection(ackOnSend: #"{"error":"invalid period"}"#)))
        await model.load()
        XCTAssertEqual(model.phase, .failed(.daemonError("invalid period")))
    }

    @MainActor
    func testUndecodableReplyLandsInFailed() async {
        let model = PanelStatsModel(client: client(CommandFakeConnection(ackOnSend: #"{"nonsense":true}"#)))
        await model.load()
        XCTAssertEqual(model.phase, .failed(.undecodable))
    }

    @MainActor
    func testTransportFaultLandsInFailed() async {
        let model = PanelStatsModel(
            client: ControlCommandClient(connector: CommandFakeConnector(.fail("ECONNREFUSED")),
                                         timeout: .seconds(5)))
        await model.load()
        guard case .failed(.transport) = model.phase else {
            return XCTFail("expected .failed(.transport), got \(model.phase)")
        }
    }

    @MainActor
    func testNilClientIsUnavailable() async {
        let model = PanelStatsModel(client: nil)
        await model.load()
        XCTAssertEqual(model.phase, .failed(.unavailable))
    }

    // MARK: - PanelStatsModel: tab selection

    @MainActor
    func testSelectStatsSetsTabAndTriggersLoad() async throws {
        let model = PanelStatsModel(client: client(CommandFakeConnection(ackOnSend: Fixtures.statsBasic)))
        XCTAssertEqual(model.tab, .status, "the panel opens on the Status glance")
        model.select(.stats)
        XCTAssertEqual(model.tab, .stats, "selecting Stats switches the tab synchronously")
        // The selection fires a one-shot load off a detached task; wait for it to settle.
        try await waitUntil({ model.phase.wire != nil }, "Stats load to settle")
    }

    @MainActor
    func testSelectSameTabIsANoOp() {
        let model = PanelStatsModel(client: nil)
        model.select(.status)  // already on status
        XCTAssertEqual(model.tab, .status)
        XCTAssertEqual(model.phase, .idle, "a no-op selection must not kick off a load")
    }

    @MainActor
    func testResetReturnsToStatusGlance() async {
        let model = PanelStatsModel(client: client(CommandFakeConnection(ackOnSend: Fixtures.statsBasic)))
        await model.load()
        model.select(.stats)  // was .status; now .stats
        model.reset()
        XCTAssertEqual(model.tab, .status)
        XCTAssertEqual(model.phase, .idle, "reset drops any loaded series so the next open re-queries live")
    }

    // MARK: - Helpers

    private func encode(_ command: StatsCommand) throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        return String(decoding: try encoder.encode(command), as: UTF8.self)
    }

    @MainActor
    private func client(_ connection: CommandFakeConnection) -> ControlCommandClient {
        ControlCommandClient(connector: CommandFakeConnector(.succeed(connection)), timeout: .seconds(5))
    }

    private func window(period: String) -> StatsWindow {
        StatsWindow(start: 0, end: 0, label: "l", period: period, since: nil)
    }

    private func waitUntil(_ predicate: () -> Bool, _ label: String) async throws {
        // A WALL-CLOCK-bounded poll (≈5 s max at 1 ms/iter), NOT a fixed `Task.yield()` budget: `Task.yield()`
        // only reschedules — it grants no real time — so under CI scheduler contention the detached stats-load
        // task can be starved past a yield-count budget and time out, while passing locally (fast, low
        // contention) and only intermittently on CI. `Task.sleep` gives the awaited task real time to run
        // regardless of load; the poll still returns the instant the predicate holds, so the success path
        // stays fast — only the failure path is now time-bounded.
        for _ in 0..<5_000 {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(1))
        }
        XCTFail("timed out waiting for \(label)")
    }
}
