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

/// Seven days in seconds — the span `window(period:)` builds, and the denominator every coverage
/// assertion in this file divides by (issue #1029).
private let weekSecs: Int64 = 604_800

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
        // The census water the daemon actually used (issue #804), as a FRACTION — the label reads it
        // from here rather than hardcoding one (issue #805).
        XCTAssertEqual(try XCTUnwrap(wire.summary.roster.allHighThreshold), 0.95, accuracy: 1e-9)

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
        // verbatim into a fixed-width popover, where an over-long reason costs vertical room the rest
        // of the tab needs (issue #818 bounds the panel, so the cost is a scroll rather than a clip).
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
    // The Stats tab now renders (`RenderPanelTool` seeds a loaded `stats` fixture, #704), so `build-comparison.py`
    // can diff the two surfaces visually — but that is a manual review, so this stays the only thing that
    // MECHANICALLY checks they agree. Asserting the DERIVED width against the mock's authored literal is what
    // turns a panel-geometry change into a red test instead of a silent divergence from the design reference.
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
        // Ties mirror the CLI's `pct` (`src/stats.rs`) — away from zero, as Rust's `f64::round` does — so the
        // two surfaces cannot state percents differing by one for the same wire fraction. Load-bearing since
        // both render the `≥N%` census water through this helper (issue #805).
        //
        // 0.845 is the case that actually DISCRIMINATES the rounding mode: `0.845 * 100` is exactly 84.5 in
        // IEEE-754, and 84 is even, so ties-away-from-zero gives 85 where banker's rounding (ties-to-even)
        // would give 84. A tie whose integer part is ODD — 0.855 → 86 — agrees under both modes and would
        // pass even against the wrong rule, which is why it is not the assertion relied on here.
        XCTAssertEqual(StatusPanelFormat.statsPercent(0.845), 85, "ties round AWAY from zero, as Rust does")
        XCTAssertEqual(StatusPanelFormat.statsPercent(0.855), 86)
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
        // The golden roster: 0 all-high episodes (0s), swap_count 1, over a `day` window, censused
        // at the golden's own 0.95 water — and jointly covered for 21600 s of that window's 43200,
        // so it is a PARTLY-measured report and says so (issue #1029). The share is derived from
        // the golden's own numbers rather than restated, so a regenerated fixture cannot leave this
        // expectation asserting a stale percent.
        let covered = try XCTUnwrap(wire.summary.roster.allHighCoveredSecs,
                                    "the current daemon always sends all_high_covered_secs (issue #804)")
        let share = Int((Double(covered) / Double(wire.window.end - wire.window.start) * 100).rounded())
        XCTAssertEqual(StatusPanelFormat.statsAggregateText(roster: wire.summary.roster, window: wire.window),
                       "All accounts ≥95% at once — 0 episodes (0s, all in view \(share)% of the window)"
                       + " · swaps 1 · last 24h")
    }

    func testAggregateTextSingularEpisode() {
        let roster = StatsRoster(swapCount: 28,
                                 swaps: StatsSwaps(session: 20, weekly: 4, manual: 3, forced: 1, emergency: 0),
                                 allHighEpisodes: 1, allHighSecs: 6000, allHighThreshold: 0.95,
                                 allHighCoveredSecs: weekSecs, censusOverRoster: true)
        XCTAssertEqual(StatusPanelFormat.statsAggregateText(roster: roster, window: window(period: "week")),
                       "All accounts ≥95% at once — 1 episode (1h40m) · swaps 28 · last 7 days")
    }

    // MARK: - The census water is READ, never assumed (issue #805)

    /// THE CROSS-LANGUAGE DRIFT GATE. The rendered label's threshold must equal the AGGREGATOR's,
    /// and this asserts that across the language boundary rather than against a second hand-typed
    /// literal — by deriving the expectation from the same bytes the Rust encoder emitted.
    ///
    /// The chain each link of which is already enforced elsewhere, so this closes it at near-zero cost:
    ///   1. `params_from` (`src/stats.rs`) derives the water from `session_ceiling` and hands it to
    ///      `RosterWire.all_high_threshold`;
    ///   2. Rust byte-pins its own encoder output into `build/fixtures/wire-stats-basic.json`
    ///      (`the_committed_stats_wire_golden_still_matches_the_socket_encoder`);
    ///   3. `WireGoldenTests.testStatsFixtureMatchesRustGolden` byte-pins `Fixtures.statsBasic` to
    ///      that golden;
    ///   4. THIS test pins the rendered label to the water decoded from that fixture.
    ///
    /// So retuning the aggregator's water forces the golden to be regenerated, which breaks link 3
    /// until the fixture is updated, at which point THIS test fails unless the label tracked it. A
    /// literal re-hardcoded into the label cannot survive that, which is precisely the regression
    /// issue #805 fixed and this gate exists to prevent recurring.
    func testRenderedLabelStatesTheAggregatorsOwnWater() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsBasic) else {
            return XCTFail("expected a StatsWire document")
        }
        let water = try XCTUnwrap(wire.summary.roster.allHighThreshold,
                                  "the current daemon always sends all_high_threshold (issue #804)")
        // Derived from the wire, NOT restated: `95` never appears as a literal expectation here.
        let expected = "All accounts ≥\(Int((water * 100).rounded()))% at once"
        XCTAssertTrue(
            StatusPanelFormat.statsAggregateText(roster: wire.summary.roster, window: wire.window)
                .hasPrefix(expected),
            "the aggregate label must state the aggregator's own census water, not a hardcoded one"
        )
    }

    /// The label must be DERIVED, not merely correct-by-coincidence at the default. A hardcoded
    /// literal passes any single-value assertion; it cannot pass two different waters. This is the
    /// test that actually kills the hardcode class rather than re-pinning today's value.
    func testLabelTracksARetunedWaterRatherThanAFixedLiteral() {
        func label(_ water: Double) -> String {
            StatusPanelFormat.statsAggregateText(
                roster: StatsRoster(swapCount: 0,
                                    swaps: StatsSwaps(session: 0, weekly: 0, manual: 0, forced: 0, emergency: 0),
                                    allHighEpisodes: 0, allHighSecs: 0, allHighThreshold: water,
                                    allHighCoveredSecs: weekSecs, censusOverRoster: true),
                window: window(period: "week"))
        }
        XCTAssertTrue(label(0.95).hasPrefix("All accounts ≥95% at once"))
        XCTAssertTrue(label(0.80).hasPrefix("All accounts ≥80% at once"), "an operator-retuned water must show")
        XCTAssertTrue(label(0.90).hasPrefix("All accounts ≥90% at once"))
    }

    /// A pre-#804 daemon never sent the water. The panel must then DROP the qualifier — it must not
    /// invent one, and it must not fail to decode: an absent additive key is the `decodeIfPresent`
    /// forward-compat path, and a fabricated threshold is the defect issue #805 exists to end.
    func testAbsentWaterDropsTheQualifierInsteadOfFabricatingOne() {
        let roster = StatsRoster(swapCount: 28,
                                 swaps: StatsSwaps(session: 20, weekly: 4, manual: 3, forced: 1, emergency: 0),
                                 allHighEpisodes: 1, allHighSecs: 6000, allHighThreshold: nil,
                                 allHighCoveredSecs: weekSecs, censusOverRoster: true)
        let text = StatusPanelFormat.statsAggregateText(roster: roster, window: window(period: "week"))
        XCTAssertEqual(text, "All accounts high at once — 1 episode (1h40m) · swaps 28 · last 7 days")
        // The counted fact survives; only the unknown water is withheld. No digit-then-% may appear
        // before the episode count, which is what a fabricated threshold would look like.
        XCTAssertFalse(text.hasPrefix("All accounts ≥"), "no water may be stated when none was reported")
    }

    /// The pre-#804 wire shape decodes rather than throwing — the compat half of the above.
    func testPre804RosterWithoutTheWaterStillDecodes() throws {
        let line = #"{"swap_count":2,"swaps":{"session":1,"weekly":1,"manual":0,"forced":0,"emergency":0},"# +
                   #""all_high_episodes":3,"all_high_secs":600}"#
        let roster = try JSONDecoder().decode(StatsRoster.self, from: Data(line.utf8))
        XCTAssertNil(roster.allHighThreshold, "an absent additive key is nil, never a decode error")
        XCTAssertEqual(roster.allHighEpisodes, 3, "the rest of the roster decodes unchanged")
    }

    // MARK: - The census set is READ, never assumed (issue #866)

    /// The rendered callout over a roster differing ONLY in the census regime — and, where a test
    /// asks, its water — so every assertion below isolates the one axis it names.
    private func censusText(_ censusOverRoster: Bool?,
                            threshold: Double? = 0.95,
                            coveredSecs: Int64? = weekSecs) -> String {
        let roster = StatsRoster(swapCount: 28,
                                 swaps: StatsSwaps(session: 20, weekly: 4, manual: 3, forced: 1, emergency: 0),
                                 allHighEpisodes: 3, allHighSecs: 6000, allHighThreshold: threshold,
                                 allHighCoveredSecs: coveredSecs, censusOverRoster: censusOverRoster)
        return StatusPanelFormat.statsAggregateText(roster: roster, window: window(period: "week"))
    }

    /// THE REPORTED DEFECT, asserted directly: the two regimes used to render IDENTICAL bytes over
    /// identical data, so a reader could not tell whether the daemon intersected the configured
    /// roster or degraded to whoever held samples. Everything else in this section pins the copy;
    /// this pins the property the copy exists to provide, and would fail for any wording that
    /// happened to collapse the two again.
    func testTheTwoCensusRegimesDoNotRenderIdentically() {
        XCTAssertNotEqual(censusText(true), censusText(false),
                          "the configured and degraded censuses must be distinguishable on the panel")
    }

    /// The degraded census names its set by narrowing the SUBJECT — in that regime "All accounts" is
    /// not merely unqualified, it is false: the census demonstrably did not see them all.
    func testDegradedCensusNamesItsSampledSet() {
        XCTAssertEqual(censusText(false),
                       "All sampled accounts ≥95% at once — 3 episodes (1h40m) · swaps 28 · last 7 days")
    }

    /// The configured regime is the unqualified sentence — it already states that the census covered
    /// the roster, so a qualifier there would be noise on the overwhelmingly common render.
    func testConfiguredCensusRendersUnqualified() {
        XCTAssertEqual(censusText(true),
                       "All accounts ≥95% at once — 3 episodes (1h40m) · swaps 28 · last 7 days")
        XCTAssertFalse(censusText(true).contains("sampled"),
                       "a census that DID cover the roster must not be annotated as sampled")
    }

    /// A pre-#866 daemon never sent the set. The panel must then DROP the qualifier — the same rule
    /// the `nil` water follows. Asserting equality with the CONFIGURED render (rather than against a
    /// restated literal) is what makes this a drop rather than a coincidence: it fails both if the
    /// absent key were read as degraded — fabricating a regime the daemon never reported, the very
    /// defect issue #866 exists to end — and if it grew any other qualifier of its own.
    func testAbsentCensusSetDropsTheQualifierInsteadOfAssumingARegime() {
        XCTAssertEqual(censusText(nil), censusText(true),
                       "an unreported set must render exactly as the unqualified sentence")
        XCTAssertFalse(censusText(nil).contains("sampled"),
                       "no set may be named when none was reported")
    }

    /// The two honesty rules compose rather than compete: an unknown WATER and a degraded SET are
    /// independent facts, and a daemon old enough to send neither still renders both drops.
    func testTheSetAndTheWaterDegradeIndependently() {
        XCTAssertEqual(censusText(false, threshold: nil),
                       "All sampled accounts high at once — 3 episodes (1h40m) · swaps 28 · last 7 days")
        XCTAssertEqual(censusText(nil, threshold: nil),
                       "All accounts high at once — 3 episodes (1h40m) · swaps 28 · last 7 days")
    }

    /// THE CROSS-LANGUAGE KEY GATE, the `census_over_roster` sibling of
    /// `testRenderedLabelStatesTheAggregatorsOwnWater`, riding that test's chain rather than
    /// restating it: `Fixtures.statsBasic` is byte-pinned to the Rust-emitted golden
    /// (`WireGoldenTests`), so decoding the regime out of it proves the Swift `CodingKey` matches
    /// the name the Rust `RosterWire` actually serializes. A misspelled key would decode as `nil`
    /// and silently drop the qualifier on EVERY payload, which is indistinguishable from the
    /// pre-#866 daemon path and therefore invisible to every other test in this section.
    func testTheGoldenFixtureCarriesTheCensusSetItWasBuiltUnder() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsBasic) else {
            return XCTFail("expected a StatsWire document")
        }
        XCTAssertEqual(wire.summary.roster.censusOverRoster, true,
                       "the current daemon always sends census_over_roster (issue #866), and the "
                       + "golden report is built under the CONFIGURED regime")
        // Series buckets carry the same regime — one report, one census, one set.
        XCTAssertEqual(wire.series.first?.roster.censusOverRoster, true)
    }

    /// The pre-#866 wire shape decodes rather than throwing — the compat half of the drop above.
    func testPre866RosterWithoutTheCensusSetStillDecodes() throws {
        let line = #"{"swap_count":2,"swaps":{"session":1,"weekly":1,"manual":0,"forced":0,"emergency":0},"# +
                   #""all_high_episodes":3,"all_high_secs":600,"all_high_threshold":0.95}"#
        let roster = try JSONDecoder().decode(StatsRoster.self, from: Data(line.utf8))
        XCTAssertNil(roster.censusOverRoster, "an absent additive key is nil, never a decode error")
        XCTAssertEqual(roster.allHighThreshold, 0.95, "the rest of the roster decodes unchanged")
    }

    // MARK: - The census's DENOMINATOR is READ, never assumed (issue #1029)

    /// The rendered callout over a roster differing ONLY in how much of the window the census could
    /// see the whole set at one moment. `episodes` / `secs` are parameters because the three states
    /// this section pins are not all reachable from one pair: an unmeasurable census necessarily
    /// counts zero, and a genuinely quiet week counts zero too — which is the entire problem.
    private func coverageText(_ coveredSecs: Int64?, episodes: UInt32 = 0, secs: Int64 = 0) -> String {
        let roster = StatsRoster(swapCount: 39,
                                 swaps: StatsSwaps(session: 30, weekly: 5, manual: 3, forced: 1, emergency: 0),
                                 allHighEpisodes: episodes, allHighSecs: secs, allHighThreshold: 0.95,
                                 allHighCoveredSecs: coveredSecs, censusOverRoster: true)
        return StatusPanelFormat.statsAggregateText(roster: roster, window: window(period: "week"))
    }

    /// THE REPORTED DEFECT, asserted directly. A week in which no instant existed with the whole
    /// roster observable rendered as `0 episodes (0s)` — a confident calm, on a week when no account
    /// was usable for days. `RosterWire`'s own doc states the contract this violated (*"Read
    /// `all_high_episodes` ONLY against `all_high_covered_secs`"*), and the producer states it twice
    /// in the imperative (`src/usage_stats.rs`).
    func testAnUnmeasurableCensusIsNotRenderedAsACalmZero() {
        let unmeasurable = coverageText(0)
        XCTAssertEqual(unmeasurable,
                       "All accounts ≥95% at once — not measurable: never all in view at the same moment"
                       + " · swaps 39 · last 7 days")
        XCTAssertFalse(unmeasurable.contains("0 episodes"),
                       "a bare `0` reads as a genuinely quiet week — the reading REQ-STA-B-008 forbids")
    }

    /// A NEGATIVE denominator is that same unmeasurable state, not a fourth one — which is why the
    /// gate is `<= 0` and not the `== 0` its prose shorthand names, mirroring the CLI's own `> 0`.
    /// Nothing should put one on the wire, but this surface does not take "the daemon would never
    /// send that" as a standard (the reason `statsWindowSeconds` overflow-checks), and the failure
    /// mode is far from inert: tightened to `== 0`, a negative falls through to the count branch,
    /// where `statsPercent` clamps its negative share to `0` and the `<1` case then renders
    /// `0 episodes (0s, all in view <1% of the window)` — a FABRICATED coverage claim on a payload
    /// that measured nothing, which is worse than the bare zero this issue set out to fix.
    func testANegativeDenominatorIsUnmeasurableRatherThanATraceOfCoverage() {
        XCTAssertEqual(coverageText(-1), coverageText(0),
                       "a negative denominator is no more measurable than a zero one")
    }

    /// THE PROPERTY THE COPY EXISTS TO PROVIDE, pinned independently of the copy: unmeasurable,
    /// genuinely quiet, and barely-measured must be THREE distinguishable renders. Every one of them
    /// carried `0` episodes before this change and so rendered identical bytes. This test would fail
    /// for any rewording that collapsed any pair of them again, which the equality assertions above
    /// and below would not.
    func testTheThreeCensusStatesRenderDistinguishably() {
        let unmeasurable = coverageText(0)
        let quiet = coverageText(weekSecs)
        let barely = coverageText(weekSecs / 100)
        XCTAssertNotEqual(unmeasurable, quiet, "an unmeasurable week must not read as a quiet one")
        XCTAssertNotEqual(unmeasurable, barely, "no coverage must not read as a trace of coverage")
        XCTAssertNotEqual(quiet, barely, "a wholly-measured week must not read as a barely-measured one")
    }

    /// A WHOLLY measured week is the terse, unannotated form — the common case stays clean, and the
    /// annotation's mere presence is itself the low-coverage signal.
    func testAWhollyMeasuredWeekCarriesNoCoverageClause() {
        XCTAssertEqual(coverageText(weekSecs, episodes: 3, secs: 6000),
                       "All accounts ≥95% at once — 3 episodes (1h40m) · swaps 39 · last 7 days")
        XCTAssertFalse(coverageText(weekSecs).contains("in view"),
                       "a fully-measured census is not annotated")
    }

    /// A PARTLY measured week states the share it was measured over — the conservative
    /// no-covered-second bar alone would still print a confident calm for a week the metric barely
    /// saw (REQ-STA-B-008: "low-coverage periods SHALL be annotated").
    func testAPartlyMeasuredCensusStatesTheShareItWasMeasuredOver() {
        XCTAssertEqual(coverageText(weekSecs / 2, episodes: 3, secs: 6000),
                       "All accounts ≥95% at once — 3 episodes (1h40m, all in view 50% of the window)"
                       + " · swaps 39 · last 7 days")
    }

    /// Rounding must not manufacture a whole the share is NOT, at EITHER end — both of these windows
    /// are strictly partly measured, which is the one thing the annotation exists to say, so a `0%`
    /// or a `100%` would deny it. The clamp is COPIED from the CLI (`src/stats.rs` `roster_line`)
    /// rather than re-decided, so the two surfaces cannot disagree by a percent.
    func testTheShareNeverRoundsToAFalseZeroOrAFalseHundred() {
        XCTAssertTrue(coverageText(60).contains("all in view <1% of the window"),
                      "a trace of coverage renders `<1%`, never a false `0%`: \(coverageText(60))")
        let nearly = coverageText(weekSecs - 60)
        XCTAssertTrue(nearly.contains("all in view >99% of the window"),
                      "near-total coverage renders `>99%`, never a false `100%`: \(nearly)")
        // `100%` is reachable ONLY by the wholly-measured branch, which prints no clause at all —
        // so `>99%` can never be mistaken for it.
        XCTAssertFalse(nearly.contains("100%"))
    }

    /// The annotation says what the share MEASURES, not the field that carries it. `covered` names
    /// `all_high_covered_secs` and answers *covered by what?* with nothing — the second defect issue
    /// #1029 reports, and one the CLI had too.
    func testTheCoverageClauseDoesNotLeakTheFieldName() {
        XCTAssertFalse(coverageText(weekSecs / 2, episodes: 3, secs: 6000).contains("covered"),
                       "no field name may reach an operator-facing string")
        XCTAssertFalse(coverageText(0).contains("covered"))
    }

    /// A pre-#804 daemon never sent the denominator. The panel must then DROP the coverage clause —
    /// it must not read the silence as unmeasurability, which would assert a fact the daemon never
    /// reported: the same fabrication class issue #805 ended one field over. Asserting equality with
    /// the WHOLLY-measured render is what makes this a drop rather than a coincidence.
    func testAnAbsentDenominatorDropsTheClauseInsteadOfClaimingUnmeasurable() {
        XCTAssertEqual(coverageText(nil, episodes: 3, secs: 6000),
                       coverageText(weekSecs, episodes: 3, secs: 6000),
                       "an unreported denominator must render exactly as the unannotated sentence")
        XCTAssertFalse(coverageText(nil).contains("not measurable"),
                       "unmeasurability may not be claimed from a silence")
        XCTAssertFalse(coverageText(nil).contains("in view"),
                       "no share may be stated when no denominator was reported")
    }

    /// The UNMEASURABLE census also drops the SET qualifier — naming the set a census intersected
    /// over describes a measurement that never happened. This is the second suppression
    /// `statsAllHighLabel`'s own doc said #1029 would add, and it restores agreement with the CLI's
    /// `roster_line`, which suppresses on `census_over_roster || all_high_covered_secs == 0`.
    func testAnUnmeasurableCensusAlsoDropsTheSetQualifier() {
        XCTAssertEqual(censusText(false, coveredSecs: 0),
                       "All accounts ≥95% at once — not measurable: never all in view at the same moment"
                       + " · swaps 28 · last 7 days")
        XCTAssertFalse(censusText(false, coveredSecs: 0).contains("sampled"),
                       "a census that was never taken has no set to name")
        // The WATER survives the drop, matching the CLI exactly: it is a parameter the daemon
        // carried regardless of whether the census could be taken, so `roster_line` states it on `—`
        // too. One parameter of the label degrades and the other does not, deliberately.
        XCTAssertTrue(censusText(false, coveredSecs: 0).hasPrefix("All accounts ≥95% at once"))
        // A nil denominator keeps the pre-#1029 behaviour: an older daemon's silence is not
        // unmeasurability, so a degraded census it DID report still names its set.
        XCTAssertTrue(censusText(false, coveredSecs: nil).hasPrefix("All sampled accounts"))
    }

    /// THE CROSS-LANGUAGE KEY GATE for the denominator — the `all_high_covered_secs` sibling of
    /// `testTheGoldenFixtureCarriesTheCensusSetItWasBuiltUnder`, and load-bearing for the same
    /// reason: a misspelled `CodingKey` decodes as `nil` and silently drops the gate on EVERY
    /// payload, which is indistinguishable from the pre-#804 daemon path and therefore invisible to
    /// every other test in this section. `Fixtures.statsBasic` is byte-pinned to the Rust-emitted
    /// golden (`WireGoldenTests`), so reading the key out of it proves the name matches what
    /// `RosterWire` actually serializes.
    func testTheGoldenFixtureCarriesTheDenominatorItWasMeasuredOver() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsBasic) else {
            return XCTFail("expected a StatsWire document")
        }
        let covered = try XCTUnwrap(wire.summary.roster.allHighCoveredSecs,
                                    "the current daemon always sends all_high_covered_secs (issue #804)")
        XCTAssertGreaterThan(covered, 0)
        XCTAssertLessThan(covered, wire.window.end - wire.window.start,
                          "the golden report is PARTLY measured — it exercises the annotated branch")
        XCTAssertNotNil(wire.series.first?.roster.allHighCoveredSecs,
                        "series buckets carry the denominator too — one report, one census")
    }

    /// A WIRE-HOSTILE window neither crashes nor invents a share. The span is the annotation's
    /// denominator and is subtracted from two `Int64`s decoded straight off the socket — and Swift's
    /// `-` TRAPS on overflow in release as well as debug, where the Rust original merely wraps. So
    /// `statsWindowSeconds` clamps and overflow-checks, and this pins both halves: an inverted or
    /// degenerate window takes the same no-annotation branch the CLI's `(end - start).max(0)` gives
    /// it, and an `Int64`-overflowing one degrades the same way instead of terminating the app.
    func testAHostileWindowSpanNeitherTrapsNorInventsAShare() {
        XCTAssertEqual(StatusPanelFormat.statsWindowSeconds(window(period: "week")), weekSecs)
        XCTAssertEqual(StatusPanelFormat.statsWindowSeconds(
            StatsWindow(start: 500, end: 100, label: "l", period: "week", since: nil)), 0,
            "an inverted window clamps to zero, mirroring Rust's `.max(0)`")
        XCTAssertEqual(StatusPanelFormat.statsWindowSeconds(
            StatsWindow(start: .min, end: .max, label: "l", period: "week", since: nil)), 0,
            "an overflowing span is not a window — it degrades, it does not trap")
        // And the render that divides by it stays honest: a partly-covered roster over a degenerate
        // window states its count with NO share, because no share was measurable.
        let roster = StatsRoster(swapCount: 39,
                                 swaps: StatsSwaps(session: 30, weekly: 5, manual: 3, forced: 1, emergency: 0),
                                 allHighEpisodes: 3, allHighSecs: 6000, allHighThreshold: 0.95,
                                 allHighCoveredSecs: 600, censusOverRoster: true)
        let degenerate = StatusPanelFormat.statsAggregateText(
            roster: roster, window: StatsWindow(start: 0, end: 0, label: "l", period: "week", since: nil))
        XCTAssertEqual(degenerate,
                       "All accounts ≥95% at once — 3 episodes (1h40m) · swaps 39 · last 7 days")
        XCTAssertFalse(degenerate.contains("in view"), "no share may be stated over a zero-length window")
    }

    /// THE EMPTY ROSTER, end to end from the wire — `stats-census-coverage-gate.feature.md` Rule 4.
    /// It earns a test of its own rather than riding the `coverageText(0)` assertion above because a
    /// gate that passes on a cardinality-zero subject is not evidence the gate works: an empty roster
    /// trivially has no instant at which "all accounts" were high, and the whole question is whether
    /// that renders as UNKNOWN or as a measured calm. The CLI already answers it — `—`, pinned in
    /// `build/fixtures/cli-renders/stats-empty-roster.txt` — and this is the panel's half.
    ///
    /// Driven through `decodeStatsReply` rather than a hand-built `StatsRoster` so it also proves the
    /// gate survives the DECODE: a misnamed `CodingKey` yields `nil`, which takes the drop path and
    /// would render this exact payload as a confident `0 episodes (0s)`.
    func testAnEmptyRosterReportsUnknownRatherThanAMeasuredZero() throws {
        let line = #"{"schema":1,"window":{"start":0,"end":604800,"label":"last 7 days","period":"week"},"# +
                   #""accounts":[],"series":[],"summary":{"roster":{"swap_count":0,"# +
                   #""swaps":{"session":0,"weekly":0,"manual":0,"forced":0,"emergency":0},"# +
                   #""all_high_episodes":0,"all_high_secs":0,"all_high_threshold":0.95,"# +
                   #""all_high_covered_secs":0,"census_over_roster":true},"accounts":{}}}"#
        guard case .ok(let wire) = try decodeStatsReply(line) else {
            return XCTFail("expected a StatsWire document")
        }
        XCTAssertTrue(wire.summary.accounts.isEmpty, "the subject really is empty — cardinality zero")
        let text = StatusPanelFormat.statsAggregateText(roster: wire.summary.roster, window: wire.window)
        XCTAssertEqual(text,
                       "All accounts ≥95% at once — not measurable: never all in view at the same moment"
                       + " · swaps 0 · last 7 days")
        XCTAssertFalse(text.contains("0 episodes"),
                       "an empty roster is unmeasurable, not calm")
    }

    /// The pre-#804 wire shape decodes rather than throwing — the compat half of the drop above, and
    /// the shape a daemon predating BOTH the water and the denominator sends.
    func testPre804RosterWithoutTheDenominatorStillDecodes() throws {
        let line = #"{"swap_count":2,"swaps":{"session":1,"weekly":1,"manual":0,"forced":0,"emergency":0},"# +
                   #""all_high_episodes":3,"all_high_secs":600}"#
        let roster = try JSONDecoder().decode(StatsRoster.self, from: Data(line.utf8))
        XCTAssertNil(roster.allHighCoveredSecs, "an absent additive key is nil, never a decode error")
        XCTAssertNil(roster.allHighThreshold)
        XCTAssertEqual(roster.allHighEpisodes, 3, "the rest of the roster decodes unchanged")
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

    // MARK: - PanelStatsModel: render-preview fixture (#704 — the `--render-panel` Stats oracle)

    /// The `loadedPreview` factory `RenderPanelTool` uses seeds the Stats tab straight to `.loaded` WITHOUT a
    /// query — the socket-free property the #704 render fixture rests on. Driven off `statsBasic` (a golden
    /// fixture) so it exercises only the factory, independent of the 3-card fixture asserted below.
    @MainActor
    func testLoadedPreviewSeedsStatsTabWithoutAQuery() throws {
        guard case .ok(let wire) = try decodeStatsReply(Fixtures.statsBasic) else {
            return XCTFail("statsBasic did not decode to a StatsWire")
        }
        let model = PanelStatsModel.loadedPreview(wire)
        XCTAssertEqual(model.tab, .stats, "the render fixture opens directly on the Stats tab")
        XCTAssertEqual(model.phase, .loaded(wire),
                       "seeded straight to .loaded — a nil-client load() would have landed .failed(.unavailable)")
    }

    /// The 3-card render fixture decodes to the mock's `healthy-stats-*` cards — the guard that keeps
    /// `loadedPreviewFixture` from silently drifting off the wire contract (and its `fatalError` from ever
    /// firing at render time). Decoded from the raw JSON so a broken fixture fails cleanly here.
    @MainActor
    func testStatsPreviewFixtureDecodesToTheMockCards() throws {
        guard case .ok(let wire) = try decodeStatsReply(PanelStatsModel.statsPreviewFixtureJSON) else {
            return XCTFail("the stats preview fixture did not decode to a StatsWire")
        }
        // The three cards, keyed by the CAPITALISED roster labels RenderPanelTool's rows carry — so the
        // case-sensitive `orderedStatHandles` join lands them in Work / Personal / Temp order.
        XCTAssertEqual(Set(wire.summary.accounts.keys), ["Work", "Personal", "Temp"])
        // Bands → the mock's three signal pills.
        XCTAssertEqual(StatusPanelFormat.statsSignal(try XCTUnwrap(wire.summary.accounts["Work"]).band), .saturated)
        XCTAssertEqual(StatusPanelFormat.statsSignal(try XCTUnwrap(wire.summary.accounts["Personal"]).band), .balanced)
        XCTAssertEqual(StatusPanelFormat.statsSignal(try XCTUnwrap(wire.summary.accounts["Temp"]).band), .underused)
        // The displayed numeric body matches the mock's active Work card verbatim.
        let work = try XCTUnwrap(wire.summary.accounts["Work"])
        XCTAssertEqual(StatusPanelFormat.statsSessionMeanPeak(work), "42 / 100%")
        XCTAssertEqual(StatusPanelFormat.statsWeeklyPeak(work), "88%")
        XCTAssertEqual(work.capHits, 42)
        // The aggregate callout matches the mock — whose water is the shipping default (95) the
        // fixture also carries, ILLUSTRATED there rather than pinned (issue #805).
        XCTAssertEqual(StatusPanelFormat.statsAggregateText(roster: wire.summary.roster, window: wire.window),
                       "All accounts ≥95% at once — 3 episodes (1h40m) · swaps 28 · last 7 days")
        // Seven daily buckets → a per-bucket sparkline point, on the fixed [0, 1] scale (peak reaches the top).
        let series = StatusPanelFormat.sparkSeries(wire.series, handle: "Work")
        XCTAssertEqual(series.count, 7)
        XCTAssertEqual(series.max(), 1.0)
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

    /// A window whose SPAN is real, not just its `period` label. The span is the denominator the
    /// census's coverage annotation divides by (issue #1029), so a zero-length one — what this
    /// helper used to build, back when only the phrase was ever read off it — would make every
    /// coverage assertion in this file vacuously unannotated. It is always a WEEK, whatever
    /// `period` says (that argument drives only the rendered phrase), so a roster carrying
    /// `allHighCoveredSecs: weekSecs` reads as WHOLLY measured; a test needing a span of its own
    /// builds `StatsWindow` directly, as the hostile-span one does.
    private func window(period: String) -> StatsWindow {
        StatsWindow(start: 0, end: weekSecs, label: "l", period: period, since: nil)
    }

    /// Poll ON THE MAIN ACTOR, bounded by WALL CLOCK. This copy already reasoned its way to the wall-clock
    /// half; `@MainActor` is the half #1078 adds — `PanelStatsModel` is `@MainActor`, and `StatsTests` is
    /// not, so this helper was nonisolated and read `phase` from the global cooperative executor (SE-0338).
    ///
    /// The deadline stays load-bearing here for a reason that does NOT hold in the capture suites: `select`
    /// kicks off `load()` on a `Task`, and `phase` only carries a wire AFTER `await client.send(…)` returns
    /// — strictly after a suspension. There is no already-queued transition for a yield to release, so a
    /// `Task.yield()` budget grants the load no real time and starves under scheduler contention: measured
    /// on this suite (issue #1078), swapping this poll for a `0..<10_000` yield budget failed 5 of 15
    /// isolated runs under 8x CPU oversubscription, where a wall-clock-bounded poll failed 0 of 15
    /// interleaved against it.
    ///
    /// Sleeping is what grants that real time, and because it SUSPENDS rather than blocks, the main actor
    /// stays free to run the very load being awaited. The poll still returns the instant the predicate
    /// holds, so only the failure path is time-bounded.
    @MainActor
    private func waitUntil(_ predicate: () -> Bool, _ label: String) async throws {
        let budget: Duration = .seconds(5)
        let deadline = ContinuousClock.now.advanced(by: budget)
        while !predicate() {
            guard ContinuousClock.now < deadline else {
                return XCTFail("timed out waiting for \(label) after \(budget)")
            }
            try await Task.sleep(for: .milliseconds(1))
        }
    }
}
