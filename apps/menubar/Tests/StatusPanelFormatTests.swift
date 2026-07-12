// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Parity + behavior tests for `StatusPanelFormat` (issue #326): the pure formatting the SwiftUI panel
// renders. Because the panel draws NOTHING it did not format here, these tests are the panel's
// acceptance gate — they pin every AC:
//
//   * rows render all wire fields (pct, reset-in, auth glyph/cue) — mirrored BYTE-FOR-BYTE against
//     `src/cli.rs` `pct` / `reset_cell` / `humanize_until` / `health_glyph` / `health_cell` /
//     `legacy_health_tags`, the same cases the Rust unit tests pin;
//   * the single reset-in picks the WEEKLY reset when weekly-exhausted, else the SESSION reset;
//   * the auth glyph == `health_glyph`, with `dead` → `claude /login`, `recovering` distinct, disabled
//     tagged;
//   * each honest state shows its banner;
//   * the footer renders `next_swap` (forward candidate), not history;
//   * `captureCommand` is the exact `sessiometer capture` CLI subcommand (the equivalent terminal command;
//     the in-app capture affordance is #360, tested in `AccountCaptureTests`);
//   * every row is VoiceOver-navigable (one spoken label).
//
// The wire → row → panel integration cases decode the shared golden fixtures through `parseWatchFrame`
// + `AccountRow.rows(from:)`, proving the panel formatting is fed by the real store projection (and
// that `recovering` survives it — the field #326 added to `AccountRow`).

import XCTest

final class StatusPanelFormatTests: XCTestCase {

    // MARK: - pct (mirror `src/cli.rs` `pct`)

    func testPctRendersPercentOrNA() {
        XCTAssertEqual(StatusPanelFormat.pct(60), "60%")
        XCTAssertEqual(StatusPanelFormat.pct(0), "0%")     // never fabricated away
        XCTAssertEqual(StatusPanelFormat.pct(100), "100%")
        XCTAssertEqual(StatusPanelFormat.pct(nil), "n/a")  // failed poll, not a fake 0
    }

    // MARK: - humanizeUntil (mirror `src/cli.rs` `humanize_until`)

    func testHumanizeUntilMatchesCliTwoLargestUnits() {
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(0), "now")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(-5), "now")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(30), "<1m")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(59), "<1m")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(600), "10m")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(2 * 3600), "2h")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(3600 + 5 * 60), "1h5m")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(3 * 86400), "3d")
        XCTAssertEqual(StatusPanelFormat.humanizeUntil(86400 + 4 * 3600), "1d4h")
    }

    // MARK: - resetCell (mirror `src/cli.rs` `reset_cell`)

    func testResetCellRendersEachWindowDirectlyOrNA() {
        let now: Int64 = 1_000_000
        XCTAssertEqual(StatusPanelFormat.resetCell(now + 600, now: now), "10m")
        XCTAssertEqual(StatusPanelFormat.resetCell(now + 2 * 3600, now: now), "2h")
        XCTAssertEqual(StatusPanelFormat.resetCell(now + 3 * 86400, now: now), "3d")
        XCTAssertEqual(StatusPanelFormat.resetCell(nil, now: now), "n/a")
    }

    // MARK: - resetIn (issue #326 AC: weekly-exhausted → weekly, else session)

    func testResetInPicksWeeklyWhenExhaustedElseSession() {
        let now: Int64 = 1_000_000
        let session: Int64 = now + 3600          // 1h
        let weekly: Int64 = now + 3 * 86400       // 3d

        // Not exhausted → the SESSION reset governs.
        XCTAssertEqual(
            StatusPanelFormat.resetIn(weeklyExhausted: false, sessionResetsAt: session, weeklyResetsAt: weekly, now: now),
            "1h")
        // Exhausted → the WEEKLY reset governs, regardless of the (sooner) session window.
        XCTAssertEqual(
            StatusPanelFormat.resetIn(weeklyExhausted: true, sessionResetsAt: session, weeklyResetsAt: weekly, now: now),
            "3d")
        // Unknown chosen instant → n/a (never a fabricated duration).
        XCTAssertEqual(
            StatusPanelFormat.resetIn(weeklyExhausted: false, sessionResetsAt: nil, weeklyResetsAt: weekly, now: now),
            "n/a")
        XCTAssertEqual(
            StatusPanelFormat.resetIn(weeklyExhausted: true, sessionResetsAt: session, weeklyResetsAt: nil, now: now),
            "n/a")
    }

    // MARK: - healthGlyph (mirror `src/cli.rs` `health_glyph`)

    func testHealthGlyphMapsEachRollupState() {
        XCTAssertEqual(StatusPanelFormat.healthGlyph(.healthy), "🟢")
        XCTAssertEqual(StatusPanelFormat.healthGlyph(.unknown), "⚪")
        XCTAssertEqual(StatusPanelFormat.healthGlyph(.stale), "🟡")
        XCTAssertEqual(StatusPanelFormat.healthGlyph(.atRisk), "🟠")
        // #427: a quarantined-but-refreshable credential shares the warm 🟠 band with atRisk,
        // reserving 🔴 for a PROVEN refresh-token death (told apart by the needs-refresh cue).
        XCTAssertEqual(StatusPanelFormat.healthGlyph(.degraded), "🟠")
        XCTAssertEqual(StatusPanelFormat.healthGlyph(.dead), "🔴")
    }

    // MARK: - healthSymbol (panel-native SF Symbol per state — distinct SHAPES, not color-alone)

    func testHealthSymbolMapsEachStateToADistinctShape() {
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.healthy).name, "checkmark.circle.fill")
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.unknown).name, "questionmark.circle")
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.stale).name, "clock.badge.exclamationmark")
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.atRisk).name, "exclamationmark.triangle.fill")
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.degraded).name, "arrow.clockwise.circle.fill")
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.dead).name, "xmark.octagon.fill")
        // Tints are semantic roles (the view maps them to system colors); unknown stays neutral (#137).
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.healthy).tint, .green)
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.unknown).tint, .neutral)
        // #427: degraded shares atRisk's warm .orange tint but a DISTINCT shape (refresh-arrow) — a
        // recoverable warning, not the red death; the shape carries the distinction, not the color.
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.degraded).tint, .orange)
        XCTAssertEqual(StatusPanelFormat.healthSymbol(.dead).tint, .red)
        // Every symbol name is DISTINCT → health is shape-encoded, not color-alone (WCAG 1.4.1 — the fix
        // the shape-identical emoji ramp lacked). Degraded and atRisk share 🟠 yet stay distinct SHAPES.
        let names = Set([CredentialHealth.healthy, .unknown, .stale, .atRisk, .degraded, .dead]
            .map { StatusPanelFormat.healthSymbol($0).name })
        XCTAssertEqual(names.count, 6)
    }

    // MARK: - Tint tokens (#388 — role → contrast-safe asset-catalog token; the load-bearing warning fix)

    func testHealthTintMapsEachRoleToItsContrastSafeToken() {
        // The #388 token table: the healthy check + the warm warning tints move to asset-catalog color sets
        // (the view resolves `.asset(name)` → `Color(name)`). Unknown stays `.secondary` — the #137 no-false-green.
        XCTAssertEqual(StatusPanelFormat.healthTint(.green),   .asset("HealthOK"))
        XCTAssertEqual(StatusPanelFormat.healthTint(.yellow),  .asset("UtilAmber"))
        XCTAssertEqual(StatusPanelFormat.healthTint(.orange),  .asset("UtilOrange"))
        XCTAssertEqual(StatusPanelFormat.healthTint(.red),     .asset("UtilRed"))
        XCTAssertEqual(StatusPanelFormat.healthTint(.neutral), .secondary)
    }

    func testStaleAndAtRiskGlyphTintsStayDistinct() {
        // #388: severity-by-warmth is a second channel on top of the distinct shapes; the two warning states
        // must NOT collapse to one amber (the `status` CLI keeps 🟡 / 🟠 apart too — cross-surface parity).
        XCTAssertNotEqual(StatusPanelFormat.healthTint(.yellow), StatusPanelFormat.healthTint(.orange))
    }

    func testUsageTextTintUsesTheDarkerTextTokenFamily() {
        // The %-TEXT (small text, WCAG 4.5:1) takes the darker `--ut-*` tokens; a failed poll stays
        // `.primary` — an uncolored metric, never a false "healthy" green (#137).
        XCTAssertEqual(StatusPanelFormat.usageTextTint(.green),  .asset("UtilGreen"))
        XCTAssertEqual(StatusPanelFormat.usageTextTint(.yellow), .asset("UtilAmber"))
        XCTAssertEqual(StatusPanelFormat.usageTextTint(.red),    .asset("UtilRed"))
        XCTAssertEqual(StatusPanelFormat.usageTextTint(nil),     .primary)
    }

    func testWarningTextAndGlyphShareOneTokenSource() {
        // #388 widened charter — the %-text warning and the stale/dead glyph express the SAME warning
        // semantic, so they resolve to the SAME token: one semantic source, not two ambers/reds that drift.
        XCTAssertEqual(StatusPanelFormat.usageTextTint(.yellow), StatusPanelFormat.healthTint(.yellow))
        XCTAssertEqual(StatusPanelFormat.usageTextTint(.red),    StatusPanelFormat.healthTint(.red))
    }

    // MARK: - authCell (mirror `src/cli.rs` `health_cell` — byte parity)

    func testAuthCellMirrorsHealthCell() {
        // A current daemon: glyph, with the DEAD `claude /login` cue softened to `recovering`.
        XCTAssertEqual(cell(.healthy), "🟢")
        XCTAssertEqual(cell(.unknown), "⚪")
        XCTAssertEqual(cell(.stale), "🟡")
        XCTAssertEqual(cell(.atRisk), "🟠")
        // #427: a DEGRADED (quarantined-but-refreshable) credential is 🟠 with a needs-REFRESH cue,
        // NEVER the 🔴 "claude /login" of a proven death — byte-parity with `src/cli.rs` `health_cell`.
        XCTAssertEqual(cell(.degraded), "🟠 degraded — run 'sessiometer poke'")
        XCTAssertEqual(cell(.degraded, recovering: true), "🟠 recovering")
        XCTAssertEqual(cell(.dead), "🔴 claude /login")
        XCTAssertEqual(cell(.dead, recovering: true), "🔴 recovering")
        // `disabled` (rotation #36) trails the glyph, independent of credential health.
        XCTAssertEqual(cell(.healthy, enabled: false), "🟢 disabled")
        XCTAssertEqual(cell(.degraded, enabled: false), "🟠 degraded — run 'sessiometer poke' disabled")
        XCTAssertEqual(cell(.dead, enabled: false), "🔴 claude /login disabled")
        XCTAssertEqual(cell(.dead, recovering: true, enabled: false), "🔴 recovering disabled")
    }

    func testAuthCellFallsBackToLegacyTagsWhenAuthNil() {
        // Pre-#119 daemon (auth nil) → the comma-joined legacy tags, never a defaulted glyph.
        XCTAssertEqual(StatusPanelFormat.authCell(auth: nil, recovering: false, enabled: true, quarantined: false), "")
        XCTAssertEqual(StatusPanelFormat.authCell(auth: nil, recovering: false, enabled: false, quarantined: false), "disabled")
        XCTAssertEqual(StatusPanelFormat.authCell(auth: nil, recovering: false, enabled: true, quarantined: true), "needs re-login")
        XCTAssertEqual(StatusPanelFormat.authCell(auth: nil, recovering: true, enabled: true, quarantined: true), "recovering")
        XCTAssertEqual(StatusPanelFormat.authCell(auth: nil, recovering: false, enabled: false, quarantined: true), "disabled, needs re-login")
    }

    // MARK: - authCue (glyphless trailing cue for the modern path)

    func testAuthCueSplitsTheTrailingCueFromTheGlyph() {
        XCTAssertNil(StatusPanelFormat.authCue(auth: .healthy, recovering: false, enabled: true))
        XCTAssertNil(StatusPanelFormat.authCue(auth: .stale, recovering: false, enabled: true))
        XCTAssertNil(StatusPanelFormat.authCue(auth: .atRisk, recovering: false, enabled: true))
        // #427: the degraded cue is needs-refresh, softened to `recovering` while healing (#109).
        XCTAssertEqual(StatusPanelFormat.authCue(auth: .degraded, recovering: false, enabled: true), "degraded — run 'sessiometer poke'")
        XCTAssertEqual(StatusPanelFormat.authCue(auth: .degraded, recovering: true, enabled: true), "recovering")
        XCTAssertEqual(StatusPanelFormat.authCue(auth: .dead, recovering: false, enabled: true), "claude /login")
        XCTAssertEqual(StatusPanelFormat.authCue(auth: .dead, recovering: true, enabled: true), "recovering")
        XCTAssertEqual(StatusPanelFormat.authCue(auth: .healthy, recovering: false, enabled: false), "disabled")
        XCTAssertEqual(StatusPanelFormat.authCue(auth: .degraded, recovering: false, enabled: false), "degraded — run 'sessiometer poke' disabled")
        XCTAssertEqual(StatusPanelFormat.authCue(auth: .dead, recovering: false, enabled: false), "claude /login disabled")
    }

    // MARK: - legacyHealthTags (mirror `src/cli.rs` `legacy_health_tags`)

    func testLegacyHealthTagsMirrorCli() {
        XCTAssertEqual(StatusPanelFormat.legacyHealthTags(enabled: true, quarantined: false, recovering: false), "")
        XCTAssertEqual(StatusPanelFormat.legacyHealthTags(enabled: false, quarantined: false, recovering: false), "disabled")
        XCTAssertEqual(StatusPanelFormat.legacyHealthTags(enabled: true, quarantined: true, recovering: false), "needs re-login")
        XCTAssertEqual(StatusPanelFormat.legacyHealthTags(enabled: true, quarantined: true, recovering: true), "recovering")
        XCTAssertEqual(StatusPanelFormat.legacyHealthTags(enabled: false, quarantined: true, recovering: false), "disabled, needs re-login")
    }

    // MARK: - banner (issue #326 AC: each honest state shows its banner)

    func testBannerCoversEveryHonestState() {
        XCTAssertEqual(StatusPanelFormat.banner(for: .connecting, accountCount: 0).kind, .info)
        XCTAssertEqual(StatusPanelFormat.banner(for: .connecting, accountCount: 0).title, "Connecting…")

        let connected = StatusPanelFormat.banner(for: .connected, accountCount: 3)
        XCTAssertEqual(connected.kind, .healthy)          // the ONLY healthy banner
        XCTAssertEqual(connected.title, "Live")
        XCTAssertEqual(connected.detail, "3 accounts.")
        XCTAssertEqual(StatusPanelFormat.banner(for: .connected, accountCount: 1).detail, "1 account.")  // singular

        XCTAssertEqual(StatusPanelFormat.banner(for: .emptyRoster, accountCount: 0).kind, .info)
        XCTAssertEqual(StatusPanelFormat.banner(for: .stale, accountCount: 2).kind, .warning)
        XCTAssertEqual(StatusPanelFormat.banner(for: .disconnected(reason: "EOF"), accountCount: 2).kind, .error)
        XCTAssertEqual(StatusPanelFormat.banner(for: .unsupported, accountCount: 0).kind, .error)

        // Crash-looping (#169): a fault banner, never healthy; the held snapshot's numbers are refused.
        let crash = StatusPanelFormat.banner(for: .crashLooping, accountCount: 3)
        XCTAssertEqual(crash.kind, .error)
        XCTAssertEqual(crash.title, "Daemon crash-looping")
        XCTAssertEqual(crash.detail, "Restarting repeatedly; holding status until it stays up.")

        // Only `.connected` is ever the healthy kind (the never-healthy-when-dead invariant).
        for state in Self.allNonConnectedStates {
            XCTAssertNotEqual(StatusPanelFormat.banner(for: state, accountCount: 1).kind, .healthy,
                              "state \(state) must not render a healthy banner")
        }
    }

    // MARK: - snapshot age (council: the panel↔CLI parity render of the wire `generated_at`)

    func testSnapshotAgeTextRendersUpdatedAgoOrNilWhenNoInstant() {
        let now: Int64 = 1_000_000
        // No generation instant (the wire's `0` sentinel for a never-generated snapshot) → no age line.
        XCTAssertNil(StatusPanelFormat.snapshotAgeText(generatedAt: 0, now: now))
        XCTAssertNil(StatusPanelFormat.snapshotAgeText(generatedAt: -5, now: now))
        // A same-instant snapshot reads "just now"; older ones humanize with the reset-in vocabulary
        // (the same `humanizeUntil` two-largest-unit format, so the panel↔CLI parity is inherited).
        XCTAssertEqual(StatusPanelFormat.snapshotAgeText(generatedAt: now, now: now), "updated just now")
        XCTAssertEqual(StatusPanelFormat.snapshotAgeText(generatedAt: now - 45, now: now), "updated <1m ago")
        XCTAssertEqual(StatusPanelFormat.snapshotAgeText(generatedAt: now - 600, now: now), "updated 10m ago")
        XCTAssertEqual(StatusPanelFormat.snapshotAgeText(generatedAt: now - 2 * 3600, now: now), "updated 2h ago")
        // Client-ahead clock skew clamps to "just now" — never a negative age.
        XCTAssertEqual(StatusPanelFormat.snapshotAgeText(generatedAt: now + 30, now: now), "updated just now")
    }

    func testSnapshotIsStaleBeyondMaxPollCadence() {
        let now: Int64 = 1_000_000
        // Absent freshness is unknown, not stale.
        XCTAssertFalse(StatusPanelFormat.snapshotIsStale(generatedAt: 0, now: now))
        // Within the max poll cadence (3600 s = POLL_SECS_HI) → fresh, even AT the boundary.
        XCTAssertFalse(StatusPanelFormat.snapshotIsStale(generatedAt: now - 3600, now: now))
        // One second past it → unambiguously stale (outlived any legitimate poll cadence).
        XCTAssertTrue(StatusPanelFormat.snapshotIsStale(generatedAt: now - 3601, now: now))
    }

    func testBannerFoldsSnapshotAgeIntoRetainingStates() {
        // The three RETAINING states (connected / stale / disconnected) surface the age in the detail…
        XCTAssertEqual(
            StatusPanelFormat.banner(for: .connected, accountCount: 3, ageText: "updated 12s ago").detail,
            "3 accounts · updated 12s ago.")
        XCTAssertTrue(
            StatusPanelFormat.banner(for: .stale, accountCount: 2, ageText: "updated 4m ago")
                .detail.contains("· updated 4m ago."))
        XCTAssertTrue(
            StatusPanelFormat.banner(for: .disconnected(reason: "EOF"), accountCount: 2, ageText: "updated 4m ago")
                .detail.contains("· updated 4m ago."))
        // …while transient / refused states never do (no retained reading to age).
        for state in [ConnectionState.connecting, .emptyRoster, .unsupported] {
            XCTAssertFalse(
                StatusPanelFormat.banner(for: state, accountCount: 0, ageText: "updated 12s ago")
                    .detail.contains("updated"),
                "state \(state) must not fold in a snapshot age")
        }
        // A Live daemon whose data is stale escalates healthy → warning (the connected-but-stale cell).
        XCTAssertEqual(
            StatusPanelFormat.banner(for: .connected, accountCount: 3, ageText: "updated 2h ago", ageStale: true).kind,
            .warning)
        // A fresh Live daemon stays healthy.
        XCTAssertEqual(
            StatusPanelFormat.banner(for: .connected, accountCount: 3, ageText: "updated 12s ago", ageStale: false).kind,
            .healthy)
        // The no-age path reproduces the original detail exactly (existing callers unaffected).
        XCTAssertEqual(StatusPanelFormat.banner(for: .connected, accountCount: 3).detail, "3 accounts.")
    }

    // MARK: - usage severity + swap-trigger (mirror `src/cli.rs` `util_severity` / `weekly_cell_severity`)

    func testUtilSeverityBandsMirrorTheCli() {
        // Bands: >= 90 Red, >= 75 Yellow, else Green (RED_UTIL_PCT / YELLOW_UTIL_PCT in src/cli.rs).
        XCTAssertEqual(StatusPanelFormat.utilSeverity(0), .green)
        XCTAssertEqual(StatusPanelFormat.utilSeverity(74), .green)
        XCTAssertEqual(StatusPanelFormat.utilSeverity(75), .yellow)   // Yellow boundary
        XCTAssertEqual(StatusPanelFormat.utilSeverity(89), .yellow)
        XCTAssertEqual(StatusPanelFormat.utilSeverity(90), .red)      // Red boundary (≈ the swap trigger)
        XCTAssertEqual(StatusPanelFormat.utilSeverity(100), .red)
    }

    func testSessionSeverityMapsPercentOrNil() {
        XCTAssertEqual(StatusPanelFormat.sessionSeverity(20), .green)
        XCTAssertEqual(StatusPanelFormat.sessionSeverity(92), .red)
        XCTAssertNil(StatusPanelFormat.sessionSeverity(nil))          // failed poll → no color, not a fake green
    }

    func testWeeklySeverityRedWhenExhaustedRegardlessOfPercent() {
        // A weekly-EXHAUSTED account is Red whatever its rounded percent (the week-blocked verdict).
        XCTAssertEqual(StatusPanelFormat.weeklySeverity(weeklyPct: 3, weeklyExhausted: true), .red)
        XCTAssertEqual(StatusPanelFormat.weeklySeverity(weeklyPct: 100, weeklyExhausted: true), .red)
        // Not exhausted → the raw bands.
        XCTAssertEqual(StatusPanelFormat.weeklySeverity(weeklyPct: 10, weeklyExhausted: false), .green)
        XCTAssertEqual(StatusPanelFormat.weeklySeverity(weeklyPct: 80, weeklyExhausted: false), .yellow)
        // Failed poll → nil even when flagged exhausted (no present reading to color, mirrors the CLI).
        XCTAssertNil(StatusPanelFormat.weeklySeverity(weeklyPct: nil, weeklyExhausted: true))
    }

    // MARK: - nextSwapFooter (issue #326 AC: forward candidate, not history)

    func testNextSwapFooterWording() {
        XCTAssertEqual(StatusPanelFormat.nextSwapFooter(.target(to: "personal", reason: .onlyCandidate), now: 0), "Next swap → personal")
        // A pre-#405 daemon (no cause) → the bare fallback, unchanged.
        XCTAssertEqual(StatusPanelFormat.nextSwapFooter(.noViableTarget(cause: nil, resetsAt: nil), now: 0), "No viable target")
        XCTAssertEqual(StatusPanelFormat.nextSwapFooter(.awaitingData, now: 0), "Awaiting data")
        XCTAssertNil(StatusPanelFormat.nextSwapFooter(nil, now: 0))   // no active anchor → no footer
    }

    // #405: a `noViableTarget` carrying fleet-capacity relief renders the composite the panel's own
    // way — STATE-parity with the CLI's `next swap: none …` footer (same facts, not the same bytes).
    func testNextSwapFooterOutOfCapacityRelief() {
        // Weekly-exhausted fleet: a week-long block → name the reset AND nudge to add an account.
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(
                .noViableTarget(cause: .weekly, resetsAt: 1_000_000 + 2 * 86_400 + 4 * 3_600), now: 1_000_000),
            "Out of capacity — resets in 2d4h · add an account")
        // Weekly cause but the daemon did not know the reset → the nudge without a duration.
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(.noViableTarget(cause: .weekly, resetsAt: nil), now: 1_000_000),
            "Out of capacity · add an account")
        // Over-session fleet: a transient block that resets soon → name the reset, NO add-account nudge.
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(
                .noViableTarget(cause: .session, resetsAt: 1_000_000 + 47 * 60), now: 1_000_000),
            "Every account over its session limit — resets in 47m")
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(.noViableTarget(cause: .session, resetsAt: nil), now: 1_000_000),
            "Every account over its session limit")
    }

    // MARK: - captureCommand (the CLI-equivalent subcommand; in-app capture affordance is #360)

    func testCaptureCommandIsTheExactSubcommand() {
        XCTAssertEqual(StatusPanelFormat.captureCommand, "sessiometer capture")
    }

    // MARK: - rowAccessibilityLabel (issue #326 AC: VoiceOver-navigable rows)

    func testRowAccessibilityLabelSpeaksTheRow() {
        let active = StatusPanelFormat.rowAccessibilityLabel(
            label: "work", isActive: true, auth: .healthy, recovering: false, enabled: true,
            quarantined: false, sessionPct: 60, weeklyPct: 10, sessionReset: "10m", weeklyReset: "5d")
        XCTAssertEqual(active, "work, active, auth healthy, session 60% resets in 10m, weekly 10% resets in 5d")

        let dead = StatusPanelFormat.rowAccessibilityLabel(
            label: "old", isActive: false, auth: .dead, recovering: false, enabled: true,
            quarantined: true, sessionPct: nil, weeklyPct: nil, sessionReset: "n/a", weeklyReset: "n/a")
        XCTAssertEqual(dead, "old, credential dead, run claude /login, session n/a resets in n/a, weekly n/a resets in n/a")

        // #427: a degraded (quarantined-but-refreshable) account speaks needs-REFRESH, never re-login.
        let degraded = StatusPanelFormat.rowAccessibilityLabel(
            label: "parked", isActive: false, auth: .degraded, recovering: false, enabled: true,
            quarantined: true, sessionPct: nil, weeklyPct: nil, sessionReset: "n/a", weeklyReset: "n/a")
        XCTAssertEqual(degraded, "parked, credential degraded, run sessiometer poke to refresh, session n/a resets in n/a, weekly n/a resets in n/a")

        // A healthy pre-#119 legacy account speaks no auth verdict (empty phrase dropped).
        let legacy = StatusPanelFormat.rowAccessibilityLabel(
            label: "leg", isActive: false, auth: nil, recovering: false, enabled: true,
            quarantined: false, sessionPct: 5, weeklyPct: 5, sessionReset: "2h", weeklyReset: "6d")
        XCTAssertEqual(legacy, "leg, session 5% resets in 2h, weekly 5% resets in 6d")

        // A parked (disabled) account speaks the `parked` tag.
        let parked = StatusPanelFormat.rowAccessibilityLabel(
            label: "p", isActive: false, auth: .healthy, recovering: false, enabled: false,
            quarantined: false, sessionPct: 1, weeklyPct: 1, sessionReset: "1h", weeklyReset: "3d")
        XCTAssertEqual(parked, "p, auth healthy, parked, session 1% resets in 1h, weekly 1% resets in 3d")
    }

    // MARK: - Integration: wire → AccountRow → panel format (recovering distinct from dead)

    func testDeadVersusRecoveringSurviveTheStoreProjection() throws {
        // A dead, NOT-recovering account (shared golden) → the actionable re-login cue.
        let deadRows = try rows(from: Fixtures.snapshotAwaitingDead)
        let dead = try XCTUnwrap(deadRows.first)
        XCTAssertEqual(dead.auth, .dead)
        XCTAssertFalse(dead.isRecovering)
        XCTAssertEqual(StatusPanelFormat.authCell(auth: dead.auth, recovering: dead.isRecovering,
                                                  enabled: dead.isEnabled, quarantined: dead.isQuarantined),
                       "🔴 claude /login")

        // The SAME dead rollup but mid-recovery (#109) → held, not re-logged: "recovering", NOT the
        // command. This is the AC's "recovering distinct from dead", proven through the projection.
        let healRows = try rows(from: Self.snapshotDeadRecovering)
        let heal = try XCTUnwrap(healRows.first)
        XCTAssertEqual(heal.auth, .dead)
        XCTAssertTrue(heal.isRecovering)
        XCTAssertEqual(StatusPanelFormat.authCell(auth: heal.auth, recovering: heal.isRecovering,
                                                  enabled: heal.isEnabled, quarantined: heal.isQuarantined),
                       "🔴 recovering")
    }

    func testResetInBindingWindowThroughTheProjection() throws {
        // A weekly-exhausted account (shared golden) → the single reset-in keys off the WEEKLY reset,
        // never the sooner session window.
        let exhaustedRows = try rows(from: Fixtures.snapshotNoViable)
        let exhausted = try XCTUnwrap(exhaustedRows.first)
        XCTAssertTrue(exhausted.weeklyExhausted)
        let now: Int64 = 1_893_456_100   // == the fixture's generated_at
        let picked = StatusPanelFormat.resetIn(weeklyExhausted: exhausted.weeklyExhausted,
                                               sessionResetsAt: exhausted.sessionResetsAt,
                                               weeklyResetsAt: exhausted.weeklyResetsAt, now: now)
        XCTAssertEqual(picked, StatusPanelFormat.resetCell(exhausted.weeklyResetsAt, now: now))
        XCTAssertNotEqual(picked, StatusPanelFormat.resetCell(exhausted.sessionResetsAt, now: now))

        // A non-exhausted account → the SESSION reset governs.
        let liveRows = try rows(from: Fixtures.snapshotRichTarget)
        let live = try XCTUnwrap(liveRows.first)            // "work": weekly_exhausted false
        XCTAssertFalse(live.weeklyExhausted)
        let picked2 = StatusPanelFormat.resetIn(weeklyExhausted: live.weeklyExhausted,
                                                sessionResetsAt: live.sessionResetsAt,
                                                weeklyResetsAt: live.weeklyResetsAt, now: now)
        XCTAssertEqual(picked2, StatusPanelFormat.resetCell(live.sessionResetsAt, now: now))
    }

    func testNextSwapTargetMarkerSurvivesTheProjection() throws {
        // The store resolves the `next_swap` target label onto the matching row.
        let rows = try rows(from: Fixtures.snapshotRichTarget)   // next_swap → "personal"
        let target = try XCTUnwrap(rows.first { $0.label == "personal" })
        XCTAssertTrue(target.isNextSwapTarget)
        let other = try XCTUnwrap(rows.first { $0.label == "work" })
        XCTAssertFalse(other.isNextSwapTarget)
        XCTAssertEqual(StatusPanelFormat.nextSwapFooter(.target(to: "personal", reason: .onlyCandidate), now: 0), "Next swap → personal")
    }

    // MARK: - Header subtitle (issue #355 — design-reference parity)

    func testHeaderSubtitleSpeaksTheHonestStatePerConnection() {
        // Connected: identity — "N accounts · {active} active".
        XCTAssertEqual(
            StatusPanelFormat.headerSubtitle(state: .connected, accountCount: 3,
                                             activeLabel: "work", ageStale: false),
            "3 accounts · work active")
        // Singular account, no active anchor → just the count (correct pluralization).
        XCTAssertEqual(
            StatusPanelFormat.headerSubtitle(state: .connected, accountCount: 1,
                                             activeLabel: nil, ageStale: false),
            "1 account")
        // Connected but the snapshot has outlived any poll cadence → "· stale", never a false "fresh".
        XCTAssertEqual(
            StatusPanelFormat.headerSubtitle(state: .connected, accountCount: 3,
                                             activeLabel: "work", ageStale: true),
            "3 accounts · work active · stale")
        // The gone-quiet `.stale` connection is always marked stale, regardless of age.
        XCTAssertEqual(
            StatusPanelFormat.headerSubtitle(state: .stale, accountCount: 2,
                                             activeLabel: "work", ageStale: false),
            "2 accounts · work active · stale")
        // Dropped connection → last-known, never "active" (honest-state discipline in the header).
        XCTAssertEqual(
            StatusPanelFormat.headerSubtitle(state: .disconnected(reason: "EOF"), accountCount: 3,
                                             activeLabel: "work", ageStale: false),
            "3 accounts · last-known")
        // Absent / transitional states speak their status, not a roster count.
        XCTAssertEqual(StatusPanelFormat.headerSubtitle(state: .connecting, accountCount: 0,
                                                        activeLabel: nil, ageStale: false),
                       "Connecting to the daemon…")
        XCTAssertEqual(StatusPanelFormat.headerSubtitle(state: .emptyRoster, accountCount: 0,
                                                        activeLabel: nil, ageStale: false),
                       "Welcome")
        XCTAssertEqual(StatusPanelFormat.headerSubtitle(state: .unsupported, accountCount: 3,
                                                        activeLabel: "work", ageStale: false),
                       "Version mismatch")
        // Crash-looping (#169): a fault sub-line, never a false "N accounts · active" roster claim.
        XCTAssertEqual(StatusPanelFormat.headerSubtitle(state: .crashLooping, accountCount: 3,
                                                        activeLabel: "work", ageStale: false),
                       "Daemon fault")
    }

    // MARK: - Swap callout (issue #355 — design-reference parity)

    func testSwapCalloutTargetIsPresentOnlyForAViableForwardCandidate() {
        XCTAssertEqual(StatusPanelFormat.swapCalloutTarget(.target(to: "personal", reason: .onlyCandidate)), "personal")
        XCTAssertNil(StatusPanelFormat.swapCalloutTarget(.noViableTarget(cause: nil, resetsAt: nil)))
        XCTAssertNil(StatusPanelFormat.swapCalloutTarget(.awaitingData))
        XCTAssertNil(StatusPanelFormat.swapCalloutTarget(nil))
    }

    func testSwapCalloutReasonRendersTheDaemonSelectionAxis() {
        // #393: the "why" line is now the daemon's OWN reason read off the wire — the #37
        // soonest-reset axis, the sole-candidate default, or the no-tiebreak roster-order fallback —
        // each rendered concisely (state-parity with the CLI's parenthetical). It is NO LONGER a
        // client-derived "lowest weekly · most headroom" claim, which asserted a rationale on the
        // SUPERSEDED selection axis.
        XCTAssertEqual(
            StatusPanelFormat.swapCalloutReason(
                .target(to: "spare", reason: .soonestReset(resetsAt: 1_893_800_000))),
            "weekly resets soonest")
        XCTAssertEqual(
            StatusPanelFormat.swapCalloutReason(.target(to: "spare", reason: .onlyCandidate)),
            "only viable target")
        // ≥2 accounts qualified but none reported a reset → the card must NOT say "only viable
        // target"; the others were viable too. It names the axis that actually decided.
        XCTAssertEqual(
            StatusPanelFormat.swapCalloutReason(.target(to: "spare", reason: .rosterOrder)),
            "first eligible · no reset times known")
        // A pre-#393 daemon sent a target with no reason → no "why" line (the card shows just the
        // label — strictly more honest than the old superseded-rule story).
        XCTAssertNil(StatusPanelFormat.swapCalloutReason(.target(to: "spare", reason: nil)))
        // A non-target candidate (or no anchor) has no reason to render.
        XCTAssertNil(StatusPanelFormat.swapCalloutReason(.noViableTarget(cause: nil, resetsAt: nil)))
        XCTAssertNil(StatusPanelFormat.swapCalloutReason(.awaitingData))
        XCTAssertNil(StatusPanelFormat.swapCalloutReason(nil))
    }

    // MARK: - Helpers

    private func cell(_ auth: CredentialHealth, recovering: Bool = false, enabled: Bool = true) -> String {
        StatusPanelFormat.authCell(auth: auth, recovering: recovering, enabled: enabled, quarantined: false)
    }

    private func rows(from fixture: String) throws -> [AccountRow] {
        let frame = try parseWatchFrame(fixture)
        guard case .snapshot(let status) = frame else {
            XCTFail("expected a snapshot frame")
            return []
        }
        return AccountRow.rows(from: status)
    }

    private static let allNonConnectedStates: [ConnectionState] = [
        .connecting, .emptyRoster, .stale, .disconnected(reason: "EOF"), .unsupported, .crashLooping,
    ]

    /// A DEAD account that is mid-recovery (#109) — the current daemon's `snapshotAwaitingDead` golden
    /// has `recovering:false`, so this hand-built frame is the only way to exercise the recovering
    /// branch through the real decoder. Same contract, `recovering:true`.
    private static let snapshotDeadRecovering = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"accounts":[{"label":"heal","active":false,"enabled":true,"quarantined":true,"recovering":true,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"dead"}],"next_swap":null,"refresh_enabled":false}
    """#
}
