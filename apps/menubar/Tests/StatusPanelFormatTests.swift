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

import AppKit
import SwiftUI
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

    // MARK: - Chrome fidelity tokens (#388 — theme-aware accent emphasis + neutral fills)
    //
    // These assert the EXACT mock values (`apps/menubar/design/menubar-preview.html`) the SwiftUI view is a
    // thin `@Environment(\.colorScheme)` consumer of. This layer IS the fidelity gate: the real popover can't
    // be screenshot-verified in CI, so a wrong number here (a typo, a dropped dark bump, a base-hue slip) is
    // caught ONLY by these assertions — never by an eyeball.

    func testAccentEmphasisOpacityIsThemeAwareAtTheMockValues() {
        // Light (dark:false) is what already shipped; dark is the bump the panel was MISSING (the dark active
        // row / swap callout read ~1.5–1.8× too faint when hardcoded to the light values).
        // LIGHT: --active-bg .08 · --accent-halo .20 · --accent-tint .10 · --accent-tint-border .20
        XCTAssertEqual(StatusPanelFormat.accentOpacity(.activeRowFill,     dark: false), 0.08)
        XCTAssertEqual(StatusPanelFormat.accentOpacity(.activeDotHalo,     dark: false), 0.20)
        XCTAssertEqual(StatusPanelFormat.accentOpacity(.swapCalloutFill,   dark: false), 0.10)
        XCTAssertEqual(StatusPanelFormat.accentOpacity(.swapCalloutBorder, dark: false), 0.20)
        // DARK: --active-bg .15 · --accent-halo .30 · --accent-tint .16 · --accent-tint-border .30
        XCTAssertEqual(StatusPanelFormat.accentOpacity(.activeRowFill,     dark: true),  0.15)
        XCTAssertEqual(StatusPanelFormat.accentOpacity(.activeDotHalo,     dark: true),  0.30)
        XCTAssertEqual(StatusPanelFormat.accentOpacity(.swapCalloutFill,   dark: true),  0.16)
        XCTAssertEqual(StatusPanelFormat.accentOpacity(.swapCalloutBorder, dark: true),  0.30)
    }

    func testEveryAccentEmphasisIsHeavierInDark() {
        // The point of I3: dark is STRICTLY heavier than light for every accent surface. An equal pair would
        // mean a site was left theme-invariant — exactly the bug this fixes — so the loop guards all four.
        for emphasis in [StatusPanelFormat.AccentEmphasis.activeRowFill, .activeDotHalo,
                         .swapCalloutFill, .swapCalloutBorder] {
            XCTAssertGreaterThan(StatusPanelFormat.accentOpacity(emphasis, dark: true),
                                 StatusPanelFormat.accentOpacity(emphasis, dark: false),
                                 "\(emphasis) must be heavier in dark (the mock raises every accent-emphasis alpha)")
        }
    }

    func testNeutralFillMatchesTheMockGrayInLightWhiteInDark() {
        // Mock neutral FILL family: systemGray (120,120,128) in light, white in dark — replacing the washed
        // `Color.secondary.opacity(k)` (label base ~60,60,67 already ~0.5 alpha → ≈half the intended fill).
        let g = 120.0 / 255, b = 128.0 / 255
        // LIGHT over systemGray: --badge-bg .16 · --track .22 · --card-bg .08
        XCTAssertEqual(StatusPanelFormat.neutralFill(.badge, dark: false), .init(red: g, green: g, blue: b, alpha: 0.16))
        XCTAssertEqual(StatusPanelFormat.neutralFill(.track, dark: false), .init(red: g, green: g, blue: b, alpha: 0.22))
        XCTAssertEqual(StatusPanelFormat.neutralFill(.card,  dark: false), .init(red: g, green: g, blue: b, alpha: 0.08))
        // DARK over white: --badge-bg .10 · --track .14 · --card-bg .05
        XCTAssertEqual(StatusPanelFormat.neutralFill(.badge, dark: true), .init(red: 1, green: 1, blue: 1, alpha: 0.10))
        XCTAssertEqual(StatusPanelFormat.neutralFill(.track, dark: true), .init(red: 1, green: 1, blue: 1, alpha: 0.14))
        XCTAssertEqual(StatusPanelFormat.neutralFill(.card,  dark: true), .init(red: 1, green: 1, blue: 1, alpha: 0.05))
    }

    func testNeutralFillBaseHueMatchesTheMockNotTheWashedLabelColor() {
        // The washout wasn't only alpha — the base HUE was wrong too. Guard the base so a regression back to a
        // label-derived neutral, a flat gray, or a white-in-light slip fails loudly (not just a subtle shade).
        let light = StatusPanelFormat.neutralFill(.badge, dark: false)
        XCTAssertEqual(light.red,   120.0 / 255)
        XCTAssertEqual(light.green, 120.0 / 255)
        XCTAssertEqual(light.blue,  128.0 / 255)   // a hair bluer than R/G — the mock's systemGray, not flat gray
        XCTAssertEqual(StatusPanelFormat.neutralFill(.badge, dark: true).red, 1.0)  // dark base is pure white
    }

    // #699 removed the active-row text capsule, and with it the two tests that pinned its label string
    // and its on-capsule contrast — plus the local `composite` helper that only they used.
    //
    // The `neutralFill(.badge)` TOKEN stays guarded by the two `neutralFill` tests above; its one
    // surviving consumer is the header app-glyph badge (`StatusPanelChrome`), NOT the monogram badge —
    // that has filled with the per-account `Color.accountBadge` since #445. The WCAG 1.4.11 contrast
    // assertion is retired rather than re-homed because that fill no longer hosts TEXT anywhere (the
    // header badge carries a glyph). Active's non-colour cue is now the leading dot's fill-vs-ring
    // SHAPE, and the row's spoken ", active" is asserted in `testRowAccessibilityLabelSpeaksTheRow`.

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

        // #499: daemon-starting is a transient info banner; not-running is an absent-daemon error banner —
        // distinct titles, so both read distinct from EACH OTHER and from the socket-dropped banner.
        let starting = StatusPanelFormat.banner(for: .starting, accountCount: 0)
        XCTAssertEqual(starting.kind, .info)
        XCTAssertEqual(starting.title, "Starting…")
        let notRunning = StatusPanelFormat.banner(for: .notRunning, accountCount: 0)
        XCTAssertEqual(notRunning.kind, .error)
        XCTAssertEqual(notRunning.title, "Daemon not running")
        let dropped = StatusPanelFormat.banner(for: .disconnected(reason: "EOF"), accountCount: 0)
        let staleBanner = StatusPanelFormat.banner(for: .stale, accountCount: 0)
        XCTAssertNotEqual(notRunning.title, dropped.title, "not-running must not read as the socket-dropped banner")

        // #526: the warm-dwell transient banner is CALMER than the escalated drop — a self-resolving
        // `.warning` "Reconnecting…", not the loud `.error` "Daemon not responding" the escalation shows.
        // This is the panel-side of the same calm-"…"-then-loud-"!" split the glyph makes.
        let reconnecting = StatusPanelFormat.banner(for: .reconnecting(reason: "EOF"), accountCount: 2)
        XCTAssertEqual(reconnecting.kind, .warning, "reconnecting is a calm warning, never the disconnected error")
        XCTAssertEqual(reconnecting.title, "Reconnecting…")
        XCTAssertNotEqual(reconnecting.kind, dropped.kind, "the transient must not read as loud as the escalation")
        XCTAssertNotEqual(reconnecting.title, dropped.title, "reconnecting must not read as the escalated drop")
        XCTAssertNotEqual(starting.title, dropped.title, "starting must not read as the socket-dropped banner")
        XCTAssertNotEqual(starting.title, staleBanner.title, "starting must not read as the stale banner")
        XCTAssertNotEqual(notRunning.title, staleBanner.title, "not-running must not read as the stale banner")
        XCTAssertNotEqual(starting.title, notRunning.title)

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
        // …while transient / refused states never do (no retained reading to age) — including the #499
        // cold-refused daemon-absent states, which never held a reading.
        for state in [ConnectionState.connecting, .emptyRoster, .unsupported, .starting, .notRunning] {
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

    // #405/#666: a `noViableTarget` carrying fleet-capacity relief renders it the panel's own way —
    // STATE-parity with the CLI's `next swap: none …` footer (same facts, not the same bytes), WITHOUT
    // the false universal and with the "· add an account" nudge gated on the WAIT, not the `cause` label.
    func testNextSwapFooterOutOfCapacityRelief() {
        // A LONG wait (days) is a structural shortage → name the reset AND nudge to add an account.
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(
                .noViableTarget(cause: .weekly, resetsAt: 1_000_000 + 2 * 86_400 + 4 * 3_600), now: 1_000_000),
            "Out of capacity — resets in 2d4h · add an account")
        // #665/#666 regression — the live mixed-fleet miscalibration: a `weekly` cause naming a
        // SUB-SESSION-WINDOW weekly reset (soonest spare returns in 59m). The pre-#666 panel keyed the
        // nudge off the `weekly` LABEL and read "Out of capacity · add an account" for a one-HOUR wait.
        // Now the label is irrelevant: a sub-window wait is transient → NO nudge.
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(
                .noViableTarget(cause: .weekly, resetsAt: 1_000_000 + 59 * 60), now: 1_000_000),
            "Out of capacity — resets in 59m")
        // Just OVER one session window (6h > 5h) → structural again, the nudge returns — proving the
        // gate keys off the wait, not the cause (a `weekly` cause both times).
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(
                .noViableTarget(cause: .weekly, resetsAt: 1_000_000 + 6 * 3_600), now: 1_000_000),
            "Out of capacity — resets in 6h · add an account")
        // The boundary is STRICT: exactly one session window still counts as within the window —
        // the nudge needs MORE than a session window (lockstep with the CLI's constant-derived pin).
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(
                .noViableTarget(cause: .weekly, resetsAt: 1_000_000 + StatusPanelFormat.addAccountNudgeWaitSecs),
                now: 1_000_000),
            "Out of capacity — resets in 5h")
        // Cause present but the daemon did not know the reset → wait UNKNOWN, treated as structural → nudge.
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(.noViableTarget(cause: .weekly, resetsAt: nil), now: 1_000_000),
            "Out of capacity · add an account")
        // …and identically under a SESSION label (label-independent unknown-wait handling).
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(.noViableTarget(cause: .session, resetsAt: nil), now: 1_000_000),
            "Out of capacity · add an account")
        // A SESSION cause with a soon reset (47m ≪ one session window) → transient, no nudge, no false
        // universal — the SAME honest render as any short-wait cause (label-independent).
        XCTAssertEqual(
            StatusPanelFormat.nextSwapFooter(
                .noViableTarget(cause: .session, resetsAt: 1_000_000 + 47 * 60), now: 1_000_000),
            "Out of capacity — resets in 47m")
    }

    // MARK: - swapCalloutAccessibilityLabel (issue #702 — the swap-callout's spoken label, guarded directly)

    // #698's headline invariant: the spoken label keeps the full "Next swap to <target>" sentence — VoiceOver
    // users have NONE of the visual context (the card's bare "→ <target>" lead, #698) to supply the missing
    // words. This gap was invisible: an adversarial trim to "<target>. <reason>." passed 434/434 because the
    // label lived in a `private var` on the SwiftUI card, outside this headless test bundle (#702). Now the
    // logic is a pure `StatusPanelFormat` helper, pinned here on BOTH arms. The spoken strings are asserted
    // LITERALLY — never derived from the visual "→" lead — so this test cannot re-couple the two.
    func testSwapCalloutAccessibilityLabelSpeaksFullSentence() {
        // reason present (post-#393 daemon) → identity + the daemon's "why", each its own sentence.
        XCTAssertEqual(
            StatusPanelFormat.swapCalloutAccessibilityLabel(target: "personal", reason: "weekly resets soonest"),
            "Next swap to personal. weekly resets soonest.")
        // reason absent (pre-#393 daemon) → identity only, with no dangling ". ." where the "why" is absent.
        XCTAssertEqual(
            StatusPanelFormat.swapCalloutAccessibilityLabel(target: "personal", reason: nil),
            "Next swap to personal.")
        // The spoken label never borrows the card's visual "→ <target>" lead (#698) — the two stay independent.
        XCTAssertFalse(
            StatusPanelFormat.swapCalloutAccessibilityLabel(target: "personal", reason: "weekly resets soonest").contains("→"),
            "spoken label must not re-couple to the visual arrow lead")
    }

    // MARK: - canonicalScrubBanner (issue #469 — the fleet-wide scrubbed-canonical signal)

    // #469: the daemon's `canonical_scrub` rollup renders a distinct HONEST BANNER (title + detail +
    // kind) naming the state and, for the un-recoverable residual, the `claude /login` remedy. The View
    // renders it ABOVE the roster in `.connected` / `.stale`, so a connected-but-scrubbed panel reads
    // visibly degraded (never healthy). Absent (nil) when the shared canonical is healthy.
    func testCanonicalScrubBannerNamesTheStateAndRemedy() throws {
        // Exhausted → an `.error` banner: the state (title) + the actionable re-login remedy (detail).
        let exhausted = try XCTUnwrap(StatusPanelFormat.canonicalScrubBanner(.exhausted))
        XCTAssertEqual(exhausted.title, "Shared login scrubbed")
        XCTAssertEqual(exhausted.kind, .error, "the un-recoverable residual reads as an error")
        XCTAssertTrue(exhausted.detail.contains("claude /login"), "detail names the remedy: \(exhausted.detail)")

        // Recovering → a calm `.info` banner; the fleet may self-heal, so NO re-login prompt.
        let recovering = try XCTUnwrap(StatusPanelFormat.canonicalScrubBanner(.recovering))
        XCTAssertEqual(recovering.title, "Shared login scrubbed")
        XCTAssertEqual(recovering.kind, .info, "the self-healing state is calm, not an error")
        XCTAssertFalse(recovering.detail.contains("claude /login"),
                       "recovering carries no re-login remedy — it may self-heal")

        // Healthy (nil) → no banner (same single-cardinality as `nextSwapFooter(nil)`).
        XCTAssertNil(StatusPanelFormat.canonicalScrubBanner(nil))
    }

    // #469 content-parity with the CLI (`src/cli.rs` `render_status`): both surfaces name the SAME state
    // ("scrubbed") and, on the exhausted case, the SAME `claude /login` remedy; the recovering case names
    // "recovering automatically" and carries NO re-login remedy on BOTH surfaces (R-2 STATE-parity — the
    // same facts, each medium phrasing its own way, so the panel checks its own rendered title + detail).
    func testCanonicalScrubBannerIsContentParityWithTheCLI() throws {
        let exhausted = try XCTUnwrap(StatusPanelFormat.canonicalScrubBanner(.exhausted))
        let exhaustedText = "\(exhausted.title) \(exhausted.detail)"
        XCTAssertTrue(exhaustedText.contains("scrubbed"), "names the state: \(exhaustedText)")
        XCTAssertTrue(exhaustedText.contains("claude /login"), "names the shared remedy: \(exhaustedText)")

        let recovering = try XCTUnwrap(StatusPanelFormat.canonicalScrubBanner(.recovering))
        let recoveringText = "\(recovering.title) \(recovering.detail)"
        XCTAssertTrue(recoveringText.contains("scrubbed"), "names the state: \(recoveringText)")
        XCTAssertTrue(recoveringText.lowercased().contains("recovering automatically"),
                      "names the calm self-heal cue: \(recoveringText)")
        XCTAssertFalse(recoveringText.contains("claude /login"),
                       "recovering carries no re-login remedy — parity with the CLI")
    }

    // #469 / #15: no secret in the canonical-scrub banner — a bare state discriminant, never a token or
    // email. The wire rollup carries no handle at all today (even a future additive handle would be a
    // non-secret roster label, #516), so the banner is trivially redaction-clean.
    func testCanonicalScrubBannerCarriesNoSecret() throws {
        for scrub in [CanonicalScrub.exhausted, .recovering] {
            let banner = try XCTUnwrap(StatusPanelFormat.canonicalScrubBanner(scrub))
            let text = "\(banner.title) \(banner.detail)"
            XCTAssertFalse(text.lowercased().contains("token"), "no token in the scrub banner: \(text)")
            XCTAssertFalse(text.contains("@"), "no email in the scrub banner: \(text)")
        }
    }

    // MARK: - keychainLockedBanner (issue #498 — the fleet-wide unreadable-credential signal)

    // #498: the daemon's `keychain_locked` rollup renders a distinct HONEST BANNER (title + detail + kind)
    // naming the state and the UNLOCK-THE-KEYCHAIN remedy. The View renders it ABOVE the roster in
    // `.connected` / `.stale`, so a connected-but-locked panel reads visibly degraded (never healthy).
    // Absent (nil) when the login keychain is unlocked.
    func testKeychainLockedBannerNamesTheStateAndRemedy() throws {
        // Locked → an `.error` banner: the state (title) + the actionable unlock remedy (detail).
        let locked = try XCTUnwrap(StatusPanelFormat.keychainLockedBanner(true))
        XCTAssertEqual(locked.title, "Keychain locked")
        XCTAssertEqual(locked.kind, .error, "a locked keychain is an unresolved error until the operator unlocks")
        XCTAssertTrue(locked.detail.lowercased().contains("unlock"), "detail names the remedy: \(locked.detail)")

        // The unlock remedy is DISTINCT from the scrub's `claude /login` (#498-vs-#469): a re-login cannot
        // help while the keychain that STORES the credential is locked.
        XCTAssertFalse(locked.detail.contains("claude /login"),
                       "keychain-locked never prompts re-login — unlock the keychain: \(locked.detail)")

        // Unlocked (false) → no banner (same single-cardinality as `canonicalScrubBanner(nil)`).
        XCTAssertNil(StatusPanelFormat.keychainLockedBanner(false))
    }

    // #498 content-parity with the CLI (`src/cli.rs` `render_status` — the `shared login: unreadable …`
    // line): both surfaces name the SAME state (keychain "locked") and the SAME "unlock" remedy, and
    // NEITHER names `claude /login` (R-2 STATE-parity — the same facts, each medium phrasing its own way,
    // so the panel checks its own rendered title + detail).
    func testKeychainLockedBannerIsContentParityWithTheCLI() throws {
        let locked = try XCTUnwrap(StatusPanelFormat.keychainLockedBanner(true))
        let text = "\(locked.title) \(locked.detail)".lowercased()
        XCTAssertTrue(text.contains("keychain"), "names the subject: \(text)")
        XCTAssertTrue(text.contains("locked"), "names the state: \(text)")
        XCTAssertTrue(text.contains("unlock"), "names the shared remedy: \(text)")
        XCTAssertFalse(text.contains("claude /login"),
                       "keychain-locked carries no re-login remedy — parity with the CLI: \(text)")
    }

    // #498 / #15: no secret in the keychain-locked banner — a bare fleet-wide state discriminant, never a
    // token or email. The wire flag is a bare `Bool` carrying no handle at all, so the banner is trivially
    // redaction-clean.
    func testKeychainLockedBannerCarriesNoSecret() throws {
        let banner = try XCTUnwrap(StatusPanelFormat.keychainLockedBanner(true))
        let text = "\(banner.title) \(banner.detail)"
        XCTAssertFalse(text.lowercased().contains("token"), "no token in the keychain-locked banner: \(text)")
        XCTAssertFalse(text.contains("@"), "no email in the keychain-locked banner: \(text)")
    }

    // MARK: - daemonFaultBanner (issue #498 — worst-first single daemon-level fault banner)

    // The panel shows ONE daemon-level fault banner even when multiple faults are set. Worst-first:
    // keychain-locked (#498) OUTRANKS canonical-scrub (#469) — an UNREADABLE shared item is at least as
    // severe as a readable-but-scrubbed one, and its unlock remedy must reach the operator before the
    // scrub's `claude /login` (which cannot help while the keychain is locked). In practice the two are
    // daemon-mutually-exclusive; this pins the deterministic tiebreak as a tested invariant.
    func testDaemonFaultBannerIsWorstFirstKeychainOverScrub() throws {
        // BOTH present → keychain-locked wins (the sole banner names the keychain state, not the scrub).
        let both = try XCTUnwrap(StatusPanelFormat.daemonFaultBanner(keychainLocked: true, scrub: .exhausted))
        XCTAssertEqual(both.title, "Keychain locked", "keychain-locked outranks canonical-scrub: \(both.title)")

        // Keychain-only → the keychain banner.
        let keychainOnly = try XCTUnwrap(StatusPanelFormat.daemonFaultBanner(keychainLocked: true, scrub: nil))
        XCTAssertEqual(keychainOnly.title, "Keychain locked")

        // Scrub-only → the scrub banner (keychain healthy, so it falls through to the scrub).
        let scrubOnly = try XCTUnwrap(StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .exhausted))
        XCTAssertEqual(scrubOnly.title, "Shared login scrubbed")

        // Neither → no banner.
        XCTAssertNil(StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: nil))
    }

    // MARK: - systemicRefreshFailureBanner (issue #523 — the refresh-MECHANISM-down signal)

    // The panel-half of #520/#523: the refresh mechanism is down (#378) while every account is still ALIVE,
    // so nothing in the roster carries it — only this banner does. `.warning`, not `.error`: it is
    // pre-death (the vault pair blocks NOW; this lapses later), the same next-break rung the glyph gives it.
    func testSystemicRefreshFailureBannerNamesTheCountAndTheDiagnosticRemedy() throws {
        let banner = try XCTUnwrap(StatusPanelFormat.systemicRefreshFailureBanner(3))
        XCTAssertEqual(banner.title, "Refresh mechanism down")
        XCTAssertEqual(banner.detail, "3 consecutive sweeps failed for every eligible account — check the daemon log.")
        XCTAssertEqual(banner.kind, .warning, "pre-death → next-break .warning, not the vault pair's .error")
    }

    // Healthy mechanism → no banner (the wire key is absent), the same single-cardinality as its siblings.
    func testSystemicRefreshFailureBannerIsAbsentWhenTheMechanismIsHealthy() {
        XCTAssertNil(StatusPanelFormat.systemicRefreshFailureBanner(nil))
    }

    // Noun agreement at the n=1 floor — a configured threshold of 1 fires on the FIRST all-error sweep, so
    // "1 consecutive sweep" is reachable. Matches the CLI line's own agreement (`src/cli.rs` render_status).
    func testSystemicRefreshFailureBannerAgreesAtTheSingleSweepFloor() throws {
        let one = try XCTUnwrap(StatusPanelFormat.systemicRefreshFailureBanner(1))
        XCTAssertEqual(one.detail, "1 consecutive sweep failed for every eligible account — check the daemon log.")
    }

    // AC1/AC2 (#813): the banner phrases the episode's EVIDENCE from its provenance instead of always
    // asserting a sweep. On the `.preflight` arm ZERO sweeps have run — the count is a seeded floor of 1
    // kept only so a pre-#813 client stays grammatical — so citing it would state a fabricated observation
    // in the one signal whose whole purpose is diagnosability.
    func testSystemicRefreshFailureBannerDoesNotInventASweepOnThePreflightArm() throws {
        let pre = try XCTUnwrap(StatusPanelFormat.systemicRefreshFailureBanner(1, source: .preflight))
        XCTAssertEqual(pre.title, "Refresh mechanism down", "the VERDICT is unchanged — only its evidence")
        XCTAssertEqual(pre.detail,
                       "The startup preflight could not resolve the claude binary — check the daemon log.")
        XCTAssertFalse(pre.detail.contains("sweep"), "#813 AC1: no sweep is asserted")
        XCTAssertFalse(pre.detail.contains("1"), "#813 AC1: the seeded count is not cited as evidence")
        XCTAssertEqual(pre.kind, .warning, "provenance picks the evidence clause, never the severity")

        // AC2 — the SWEEP arm is unchanged from the pre-#813 render, byte for byte. An explicit `.sweep`
        // and an absent provenance (a pre-#813 daemon) must both produce exactly what shipped before.
        let legacy = try XCTUnwrap(StatusPanelFormat.systemicRefreshFailureBanner(3))
        let swept = try XCTUnwrap(StatusPanelFormat.systemicRefreshFailureBanner(3, source: .sweep))
        XCTAssertEqual(swept.detail, legacy.detail, "#813 AC2: the sweep arm did not move")
        XCTAssertEqual(swept.detail,
                       "3 consecutive sweeps failed for every eligible account — check the daemon log.")

        // The CLI's line splits on the SAME seam and names the same observation, each medium phrasing it
        // its own way (R-2 STATE-parity). That parity is asserted where BOTH strings are reachable —
        // `src/cli.rs` `render_status_surfaces_the_systemic_refresh_failure_when_the_mechanism_is_down`;
        // from here it could only be re-read off the literal already pinned above.
    }

    // Issue #15: the banner carries only the COUNT — never a token, path, or email. The CLI line names the
    // `[refresh] claude binary` because a terminal reader can act on it; the panel keeps to the daemon log.
    func testSystemicRefreshFailureBannerCarriesNoSecret() throws {
        // #813 AC4: swept across EVERY arm. The preflight arm is where a path would leak — its subject IS a
        // binary location — so it is asserted here rather than trusted to the discriminant's fixed shape.
        for banner in [try XCTUnwrap(StatusPanelFormat.systemicRefreshFailureBanner(7)),
                       try XCTUnwrap(StatusPanelFormat.systemicRefreshFailureBanner(1, source: .preflight)),
                       try XCTUnwrap(StatusPanelFormat.systemicRefreshFailureBanner(2, source: .unrecognized))] {
            let text = banner.title + " " + banner.detail
            for forbidden in ["@", "sk-", "token", "Bearer", "/Users/", ".json", "/"] {
                XCTAssertFalse(text.contains(forbidden), "#15: the banner must not carry \(forbidden): \(text)")
            }
        }
    }

    // The 3-way worst-first rank: the two "act now" vault faults outrank the pre-death mechanism fault.
    // Unlike the vault pair (daemon-mutually-exclusive — a locked keychain can't be read to know
    // scrubbed-ness), systemic-refresh CAN genuinely coincide with either, so this arm really arbitrates.
    func testDaemonFaultBannerRanksTheVaultPairOverSystemicRefresh() throws {
        // Each vault fault + systemic → the vault fault wins (the operator is blocked NOW).
        let keychainAndSystemic = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: true, scrub: nil, systemicRefreshFailure: 3))
        XCTAssertEqual(keychainAndSystemic.title, "Keychain locked", "act-now keychain outranks systemic")

        let scrubAndSystemic = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .exhausted, systemicRefreshFailure: 3))
        XCTAssertEqual(scrubAndSystemic.title, "Shared login scrubbed", "act-now scrub outranks systemic")

        // Systemic alone → it finally surfaces (the vault is healthy, so it falls through).
        let systemicOnly = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: nil, systemicRefreshFailure: 3))
        XCTAssertEqual(systemicOnly.title, "Refresh mechanism down")

        // All healthy → no banner.
        XCTAssertNil(StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: nil, systemicRefreshFailure: nil))
    }

    // The scrub's two variants are NOT one severity — `recovering` ranks LAST, BELOW systemic. Ranking
    // canonical-scrub as one slot made the surfaces contradict each other: `PresentationState.make` ignores
    // `recovering` (only `exhausted` is a ⊘ input), so the glance shouts `!` at the systemic fault, while a
    // fault-identity rank answered the resulting click with a calm "no action needed" over a green roster —
    // during a total refresh outage. That is strictly worse than the false-healthy it replaced: it does not
    // merely fail to explain the `!`, it CONTRADICTS it. A self-healing state can never outrank one that
    // cannot self-heal.
    func testCalmRecoveringScrubNeverOutranksTheSystemicFaultTheGlyphIsShouting() throws {
        let recoveringAndSystemic = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .recovering, systemicRefreshFailure: 3))
        XCTAssertEqual(recoveringAndSystemic.title, "Refresh mechanism down",
                       "the calm self-healing scrub must not bury the fault that cannot self-heal")
        XCTAssertEqual(recoveringAndSystemic.kind, .warning)

        // The exact cross-surface invariant, asserted end-to-end rather than per-resolver: whenever the
        // glance shows a fault glyph, the panel's one banner must EXPLAIN that fault — never contradict it.
        let glance = PresentationState.make(for: .connected, accountCount: 3,
                                            canonicalScrub: .recovering, systemicRefreshFailure: 3)
        XCTAssertEqual(glance.glyph, .attention, "the glance shouts at the systemic fault")
        XCTAssertFalse(recoveringAndSystemic.detail.contains("no action needed"),
                       "the panel must not answer a shouting glyph with 'no action needed'")

        // `recovering` alone still surfaces its calm banner — it is ranked last, not dropped.
        let recoveringAlone = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .recovering, systemicRefreshFailure: nil))
        XCTAssertEqual(recoveringAlone.title, "Shared login scrubbed")
        XCTAssertEqual(recoveringAlone.kind, .info)

        // And an `exhausted` scrub still outranks systemic — only the CALM variant moved.
        let exhaustedAndSystemic = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .exhausted, systemicRefreshFailure: 3))
        XCTAssertEqual(exhaustedAndSystemic.title, "Shared login scrubbed")
        XCTAssertEqual(exhaustedAndSystemic.kind, .error)
    }

    // MARK: - canaryBanner (issue #714/#728 — the behavioral-canary identity-drift signal)

    // The refusing (non-overridden) drift → an act-now `.error` banner naming BOTH labels (which account the
    // credential actually belongs to vs which is named active) AND the `canary_drift_override` remedy. Content-
    // parity with the CLI `keychain canary: drift — … credential writes are refused …` line (`src/cli.rs`
    // render_canary): same state, same labels, same remedy, phrased for the popover.
    func testCanaryBannerNamesTheDriftStateLabelsAndRemedy() throws {
        let refusing = try XCTUnwrap(StatusPanelFormat.canaryBanner(.drift(displayed: "work", matched: "personal", overridden: false)))
        XCTAssertEqual(refusing.title, "Keychain identity drift")
        XCTAssertEqual(refusing.detail,
                       "The active credential belongs to personal, not work — credential writes are refused (false alarm? set canary_drift_override and restart the daemon).")
        XCTAssertEqual(refusing.kind, .error, "a refusing drift blocks writes NOW → act-now .error")

        // The OTHER variant — overridden: the drift stands but writes proceed → next-break `.warning`, the
        // (fault, VARIANT) split from the refusing drift. Same labels, the override-is-set message.
        let overridden = try XCTUnwrap(StatusPanelFormat.canaryBanner(.drift(displayed: "work", matched: "personal", overridden: true)))
        XCTAssertEqual(overridden.title, "Keychain identity drift")
        XCTAssertEqual(overridden.detail,
                       "The active credential belongs to personal, not work — canary_drift_override is set, so writes proceed and are logged.")
        XCTAssertEqual(overridden.kind, .warning, "an overridden drift is a standing acknowledged alarm → .warning, not .error")
    }

    // The `ambiguous` verdict → an act-now `.error` banner naming the COUNT and the remove-duplicates remedy.
    // Content-parity with the CLI `keychain canary: ambiguous — {count} … items found …` line.
    func testCanaryBannerNamesTheAmbiguousCountAndRemedy() throws {
        let banner = try XCTUnwrap(StatusPanelFormat.canaryBanner(.ambiguous(count: 2)))
        XCTAssertEqual(banner.title, "Keychain identity ambiguous")
        XCTAssertEqual(banner.detail,
                       "2 duplicate keychain items found (expected one) — credential writes are refused until the extras are removed.")
        XCTAssertEqual(banner.kind, .error, "no unique write target → writes refused NOW → act-now .error")
    }

    // The `refused_unparseable_canonical` verdict (#730/#738) → an act-now `.error` banner naming the
    // EVIDENCE (matches no stash, not Claude Code's format), the REFUSAL, and its OWN override. Content-parity
    // with the CLI's `keychain canary: unrecognized credential — …` line. The remedy must name
    // `canary_nostashmatch_override` and NOT `canary_drift_override`: they are deliberately separate switches
    // (`src/config.rs`), so quoting the drift one would send the operator to a lever that cannot clear this.
    func testCanaryBannerNamesTheUnparseableCanonicalRefusalAndItsOwnOverride() throws {
        let banner = try XCTUnwrap(StatusPanelFormat.canaryBanner(.refusedUnparseableCanonical))
        XCTAssertEqual(banner.title, "Unrecognized keychain credential")
        XCTAssertEqual(banner.detail,
                       "The keychain item matches no stashed account and is not in Claude Code's own format — it is probably an unrelated secret, so credential writes are refused rather than overwrite it (vetted it as safe? set canary_nostashmatch_override and restart the daemon).")
        XCTAssertEqual(banner.kind, .error,
                       "writes refused NOW → act-now .error, the same rank as a refusing drift")
        XCTAssertFalse(banner.detail.contains("canary_drift_override"),
                       "the unparseable refusal must not quote the DRIFT override — a separate switch that cannot clear it")
    }

    // The quiet verdicts (and no verdict) → NO banner: `ok` / `inconclusive` are the quiet normal, and
    // `not_found` is already voiced by the scrub / keychain machinery (a second banner would double-report
    // the same absent credential — the same reason the CLI's render_canary prints nothing for it).
    func testCanaryBannerIsAbsentForQuietVerdicts() {
        XCTAssertNil(StatusPanelFormat.canaryBanner(.ok))
        XCTAssertNil(StatusPanelFormat.canaryBanner(.inconclusive))
        XCTAssertNil(StatusPanelFormat.canaryBanner(.notFound))
        XCTAssertNil(StatusPanelFormat.canaryBanner(nil))
    }

    // Issue #15: the banner carries only operator LABELS and a COUNT — never a token, email, or account-uuid.
    func testCanaryBannerCarriesNoSecret() throws {
        let banners = [
            try XCTUnwrap(StatusPanelFormat.canaryBanner(.drift(displayed: "work", matched: "personal", overridden: false))),
            try XCTUnwrap(StatusPanelFormat.canaryBanner(.drift(displayed: "work", matched: "personal", overridden: true))),
            try XCTUnwrap(StatusPanelFormat.canaryBanner(.ambiguous(count: 2))),
            try XCTUnwrap(StatusPanelFormat.canaryBanner(.refusedUnparseableCanonical)),
        ]
        for banner in banners {
            let text = banner.title + " " + banner.detail
            for forbidden in ["@", "sk-", "Bearer", "/Users/", ".json", "-credentials"] {
                XCTAssertFalse(text.contains(forbidden), "#15: the canary banner must not carry \(forbidden): \(text)")
            }
        }
    }

    // The 8-rank worst-first order (pinned to the CLI's `DaemonPayloadFault`): the canary REFUSAL TRIO
    // joins the ACT-NOW band (ranks 3-5, under the vault pair, OVER systemic), while an OVERRIDDEN drift is
    // NEXT-BREAK (rank 7, UNDER systemic, OVER the calm recovering scrub). Severity by (fault, VARIANT), never
    // fault identity (#575) — the same split the scrub's exhausted/recovering pair already proved load-bearing.
    func testDaemonFaultBannerRanksTheCanaryRefusalTrioInTheActNowBand() throws {
        let refusing = CanaryStatus.drift(displayed: "work", matched: "personal", overridden: false)
        let overridden = CanaryStatus.drift(displayed: "work", matched: "personal", overridden: true)

        // The vault pair still outranks a refusing drift (ranks 1-2 over rank 3).
        let keychainVsRefusing = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: true, scrub: nil, systemicRefreshFailure: nil, canary: refusing))
        XCTAssertEqual(keychainVsRefusing.title, "Keychain locked", "keychain (rank 1) outranks a refusing drift (rank 3)")
        let scrubVsRefusing = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .exhausted, systemicRefreshFailure: nil, canary: refusing))
        XCTAssertEqual(scrubVsRefusing.title, "Shared login scrubbed", "scrub-exhausted (rank 2) outranks a refusing drift (rank 3)")

        // A refusing drift and an ambiguous resolution BOTH outrank systemic-refresh (act-now over next-break).
        let refusingVsSystemic = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: nil, systemicRefreshFailure: 3, canary: refusing))
        XCTAssertEqual(refusingVsSystemic.title, "Keychain identity drift", "a refusing drift (rank 3) outranks systemic (rank 6)")
        XCTAssertEqual(refusingVsSystemic.kind, .error)
        let ambiguousVsSystemic = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: nil, systemicRefreshFailure: 3, canary: .ambiguous(count: 2)))
        XCTAssertEqual(ambiguousVsSystemic.title, "Keychain identity ambiguous", "ambiguous (rank 4) outranks systemic (rank 6)")

        // #738 — the third act-now refusal completes the band. It outranks systemic (it BLOCKS writes now,
        // systemic only foreshadows), and it is outranked by the vault pair and by its two #714 siblings.
        let unparseableVsSystemic = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: nil, systemicRefreshFailure: 3,
                                                canary: .refusedUnparseableCanonical))
        XCTAssertEqual(unparseableVsSystemic.title, "Unrecognized keychain credential",
                       "an unparseable-canonical refusal (rank 5) outranks systemic (rank 6)")
        XCTAssertEqual(unparseableVsSystemic.kind, .error)
        let scrubVsUnparseable = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .exhausted, systemicRefreshFailure: nil,
                                                canary: .refusedUnparseableCanonical))
        XCTAssertEqual(scrubVsUnparseable.title, "Shared login scrubbed",
                       "scrub-exhausted (rank 2) outranks an unparseable-canonical refusal (rank 5)")
        // No assertion for "refusing drift (rank 3) beats this (rank 5)" — deliberately. `canary` is ONE
        // optional verdict, so a drift and an unparseable refusal can never co-occur; such a test would
        // pass without exercising anything and would keep passing if the arms were reordered. The canary's
        // intra-band order is a reading-order convention (mirroring `DaemonPayloadFault`), and only the
        // cross-FAULT edges below/above are real arbitration.
        // It does beat the calm recovering scrub (rank 5 > rank 8) — a genuinely independent field.
        let unparseableVsRecovering = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .recovering, systemicRefreshFailure: nil,
                                                canary: .refusedUnparseableCanonical))
        XCTAssertEqual(unparseableVsRecovering.title, "Unrecognized keychain credential",
                       "an unparseable-canonical refusal (rank 5) outranks recovering scrub (rank 8)")

        // But an OVERRIDDEN drift ranks UNDER systemic (rank 7 > rank 6) — the writes-proceed variant is
        // next-break, so a coincident down mechanism (the harder-to-recover fault) must surface first.
        let overriddenVsSystemic = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: nil, systemicRefreshFailure: 3, canary: overridden))
        XCTAssertEqual(overriddenVsSystemic.title, "Refresh mechanism down", "systemic (rank 6) outranks an overridden drift (rank 7)")

        // And an OVERRIDDEN drift ranks OVER the calm recovering scrub (rank 7 > rank 8): an acknowledged
        // identity alarm still beats a self-healing state.
        let overriddenVsRecovering = try XCTUnwrap(
            StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: .recovering, systemicRefreshFailure: nil, canary: overridden))
        XCTAssertEqual(overriddenVsRecovering.title, "Keychain identity drift", "an overridden drift (rank 7) outranks recovering scrub (rank 8)")
        XCTAssertEqual(overriddenVsRecovering.kind, .warning)

        // A quiet verdict alongside a healthy fleet → no banner (canary never fabricates one).
        XCTAssertNil(StatusPanelFormat.daemonFaultBanner(keychainLocked: false, scrub: nil, systemicRefreshFailure: nil, canary: .ok))
    }

    // MARK: - captureCommand (the CLI-equivalent subcommand; in-app capture affordance is #360)

    func testCaptureCommandIsTheExactSubcommand() {
        XCTAssertEqual(StatusPanelFormat.captureCommand, "sessiometer capture")
    }

    // MARK: - Start-daemon card copy, attributed to its writer (issue #820)
    //
    // `startPhase` has had TWO writers since issue #788 — the Start button and the launch-time registration
    // repair — and until now both rendered identical copy, so a repair no press stood behind was
    // indistinguishable from a press that failed. These pin the distinction ON THE STRINGS. Whether the
    // strings actually reach the screen is `StartDaemonCardTests`' job.

    /// The pending beat says WHICH writer is running. The operator's wording is unchanged from issue #170.
    func testPendingTextNamesTheWriterThatPaintedTheBeat() {
        XCTAssertEqual(StatusPanelFormat.startDaemonPendingText(for: .operatorStart), "Starting…")
        XCTAssertEqual(StatusPanelFormat.startDaemonPendingText(for: .launchRepair), "Repairing…")
        XCTAssertNotEqual(StatusPanelFormat.startDaemonPendingText(for: .operatorStart),
                          StatusPanelFormat.startDaemonPendingText(for: .launchRepair),
                          "identical copy is exactly the defect issue #820 fixes — assert they DIFFER, not "
                          + "merely that each has some value")
    }

    /// The failure line: verbatim for a press, attributed for the launch repair. Driven with the ONE reason
    /// both writers emit byte-identically (`notStartedReason`'s wording), because that is the case where the
    /// reason itself carries no signal and the attribution is doing all the work.
    func testFailureTextAttributesOnlyTheUnpromptedLaunchRepair() {
        // DERIVED, not transcribed (issue #779). This used to be a hand-copied literal, which silently stops
        // testing the shipped sentence the moment the model's copy changes. It is now composed by the model —
        // and pinned against the literal issue #745 shipped, so the equality below is doing two jobs: it keeps
        // this attribution test honest, AND it is the assertion that issue #779 did not disturb the no-log
        // rendering. `offeringLogAffordance: false` is that no-log arm.
        let shared = LoginItemModel.StartFailureReason.notStarted.text(offeringLogAffordance: false)
        XCTAssertEqual(shared, "The daemon was registered but didn’t start. Check Console for details.",
                       "with no log to open, the card must still say exactly what issue #745 shipped — the "
                       + "issue #169 honest-affordance rule withholds the BUTTON, never the information")

        XCTAssertEqual(StatusPanelFormat.startDaemonFailureText(reason: shared, origin: .operatorStart),
                       shared,
                       "the operator pressed the button a moment ago — a prefix telling them so is noise, "
                       + "and issue #170/#745's shipped copy stays byte-unchanged on that path")

        let repaired = StatusPanelFormat.startDaemonFailureText(reason: shared, origin: .launchRepair)
        XCTAssertNotEqual(repaired, shared, "the same reason must not read the same from both writers")
        XCTAssertTrue(repaired.hasPrefix(StatusPanelFormat.startDaemonRepairAttribution),
                      "the attribution LEADS — the operator's first question about a card they never "
                      + "summoned is who did this, not what went wrong")
        XCTAssertTrue(repaired.hasSuffix(shared), "and the reason itself survives the prefix intact")
    }

    /// The attribution says both halves out loud — that it was automatic (so: not you) and what triggered
    /// it. A prefix that named only one would leave the operator to guess the other.
    func testTheRepairAttributionSaysAutomaticAndWhy() {
        let attribution = StatusPanelFormat.startDaemonRepairAttribution
        XCTAssertTrue(attribution.lowercased().contains("automatic"),
                      "no press stands behind this; the copy has to say so")
        XCTAssertTrue(attribution.lowercased().contains("update"), "and name what triggered it")
    }

    /// Redaction (issue #15) is not weakened by the prefix: the attribution is a fixed literal that
    /// interpolates nothing, so whatever redaction the reason arrived with is what the card shows.
    func testAttributionAddsNothingBeyondTheReasonItWasGiven() {
        let reason = "Operation not permitted"
        let attributed = StatusPanelFormat.startDaemonFailureText(reason: reason, origin: .launchRepair)
        XCTAssertEqual(attributed,
                       "\(StatusPanelFormat.startDaemonRepairAttribution) — \(reason)",
                       "the whole output is the fixed attribution plus the reason as given — no second "
                       + "source of text that could smuggle in something un-redacted")
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

    // MARK: - Active-account bounded-blindness row (issues #479/#485)

    // The eye-slash blind glyph is DISTINCT from every auth `healthSymbol` shape (so blindness is legible
    // without color, WCAG 1.4.1); OK is calm neutral, DEGRADED the at-risk orange rung, CORNERED red (#572 —
    // its glance IS no-runway ⊘, so red matches rather than over-signals).
    func testBlindSymbolIsAnEyeSlashDistinctFromAuthGlyphs() {
        XCTAssertEqual(StatusPanelFormat.blindSymbol(.ok).name, "eye.slash")
        XCTAssertEqual(StatusPanelFormat.blindSymbol(.degraded).name, "eye.slash")
        XCTAssertEqual(StatusPanelFormat.blindSymbol(.cornered).name, "eye.slash")
        XCTAssertEqual(StatusPanelFormat.blindSymbol(.ok).tint, .neutral)
        XCTAssertEqual(StatusPanelFormat.blindSymbol(.degraded).tint, .orange)
        XCTAssertEqual(StatusPanelFormat.blindSymbol(.cornered).tint, .red)
        // The blind glyph must not collide with any auth glyph shape (a distinct state needs a distinct shape).
        let authGlyphs: [CredentialHealth] = [.healthy, .unknown, .stale, .atRisk, .degraded, .dead]
        let authNames = Set(authGlyphs.map { StatusPanelFormat.healthSymbol($0).name })
        XCTAssertFalse(authNames.contains("eye.slash"), "the blind glyph must be shape-distinct from every auth glyph")
    }

    // The severity composition mirrors the CLI's `cornered_state`: cornered iff DEGRADED *and* the snapshot
    // carries no viable swap target. Not-degraded is always OK; degraded-with-a-target is DEGRADED (the daemon
    // can still swap), only degraded + noViableTarget is CORNERED.
    func testBlindSeverityComposesCornered() {
        // Not degraded → OK regardless of next-swap.
        XCTAssertEqual(StatusPanelFormat.blindSeverity(degraded: false, nextSwap: nil), .ok)
        XCTAssertEqual(StatusPanelFormat.blindSeverity(
            degraded: false, nextSwap: .noViableTarget(cause: .weekly, resetsAt: 1)), .ok)
        // Degraded but a target exists → DEGRADED, not cornered (the daemon can still act).
        XCTAssertEqual(StatusPanelFormat.blindSeverity(
            degraded: true, nextSwap: .target(to: "personal", reason: .onlyCandidate)), .degraded)
        XCTAssertEqual(StatusPanelFormat.blindSeverity(degraded: true, nextSwap: nil), .degraded)
        XCTAssertEqual(StatusPanelFormat.blindSeverity(degraded: true, nextSwap: .awaitingData), .degraded)
        // Degraded + no viable target → CORNERED.
        XCTAssertEqual(StatusPanelFormat.blindSeverity(
            degraded: true, nextSwap: .noViableTarget(cause: .weekly, resetsAt: 1)), .cornered)
        XCTAssertEqual(StatusPanelFormat.blindSeverity(
            degraded: true, nextSwap: .noViableTarget(cause: nil, resetsAt: nil)), .cornered)
    }

    // #572 honest-state gate (#137): the switchable roster composes the CORNERED verdict ONLY from a VOUCHED
    // connection. `.connected` stands behind the retained next-swap → passes it through; `.stale` (last-good
    // snapshot still shown, but the valid-frame watchdog has elapsed — the daemon has gone quiet) WITHHOLDS
    // it, so a retained `noViableTarget` degrades cornered → DEGRADED, matching the `.stale` `!` glance
    // instead of inverting past it into a loud red "cannot act" alarm off unvouched data.
    func testRosterNextSwapWithholdsUnvouchedCornered() {
        let cornered = NextSwap.noViableTarget(cause: .weekly, resetsAt: 1)
        let target = NextSwap.target(to: "personal", reason: .onlyCandidate)
        // Vouched: `.connected` passes the retained next-swap through unchanged.
        XCTAssertEqual(StatusPanelFormat.rosterNextSwap(for: .connected, nextSwap: cornered), cornered)
        XCTAssertEqual(StatusPanelFormat.rosterNextSwap(for: .connected, nextSwap: target), target)
        XCTAssertNil(StatusPanelFormat.rosterNextSwap(for: .connected, nextSwap: nil))
        // Unvouched: `.stale` withholds it (→ nil) regardless of what was retained.
        XCTAssertNil(StatusPanelFormat.rosterNextSwap(for: .stale, nextSwap: cornered))
        XCTAssertNil(StatusPanelFormat.rosterNextSwap(for: .stale, nextSwap: target))
        // End-to-end invariant: a stale + would-be-cornered row composes as DEGRADED, not cornered…
        XCTAssertEqual(
            StatusPanelFormat.blindSeverity(
                degraded: true,
                nextSwap: StatusPanelFormat.rosterNextSwap(for: .stale, nextSwap: cornered)),
            .degraded)
        // …while the SAME row on a vouched `.connected` connection is genuinely CORNERED.
        XCTAssertEqual(
            StatusPanelFormat.blindSeverity(
                degraded: true,
                nextSwap: StatusPanelFormat.rosterNextSwap(for: .connected, nextSwap: cornered)),
            .cornered)
    }

    // The duration chip reuses `humanizeUntil` — the SAME format as the CLI's `blind for {dur}`.
    func testBlindDurationChipHumanizesTheSeconds() {
        XCTAssertEqual(StatusPanelFormat.blindDurationChip(240), "blind 4m")
        XCTAssertEqual(StatusPanelFormat.blindDurationChip(1380), "blind 23m")
        XCTAssertEqual(StatusPanelFormat.blindDurationChip(3600 + 5 * 60), "blind 1h5m")
        XCTAssertEqual(StatusPanelFormat.blindDurationChip(30), "blind <1m")
    }

    // The verdict mirrors the CLI: OK calm (`.neutral`, un-emphasized), DEGRADED the at-risk orange fault, the
    // "acting on a stale anchor" parenthetical carried verbatim; distinct shield SHAPES per state (not color-alone).
    // OK/DEGRADED are single-line (`remedy == nil`, as #485 shipped) and ignore nextSwap/now.
    func testBlindVerdictMirrorsTheCliOkVsDegraded() {
        let ok = StatusPanelFormat.blindVerdict(.ok, nextSwap: nil, now: 0)
        XCTAssertEqual(ok.symbol, "checkmark.shield.fill")
        XCTAssertEqual(ok.text, "Auto-protection OK — daemon self-resolving")
        XCTAssertEqual(ok.tint, .neutral)
        XCTAssertNil(ok.remedy, "OK is single-line — no remedy sub-line")

        let degraded = StatusPanelFormat.blindVerdict(.degraded, nextSwap: nil, now: 0)
        XCTAssertEqual(degraded.symbol, "exclamationmark.shield.fill")
        XCTAssertEqual(degraded.text, "Auto-protection DEGRADED — acting on a stale anchor")
        XCTAssertEqual(degraded.tint, .orange)
        XCTAssertNil(degraded.remedy, "DEGRADED is single-line — no remedy sub-line")
        XCTAssertNotEqual(ok.symbol, degraded.symbol, "OK and DEGRADED must be shape-distinct, not color-alone")

        XCTAssertEqual(StatusPanelFormat.blindLastKnownCaption, "LAST-KNOWN · RATE-LIMITED")
    }

    // The CORNERED verdict (#572) is the panel half of the CLI's `render_cornered`: the loudest `.red`
    // "Auto-protection CANNOT ACT", a shield SHAPE distinct from OK/DEGRADED, PLUS the operator remedy
    // sub-line. The remedy is UNCONDITIONAL ("add or free an account" always, per #666) and folds the
    // soonest reset in via the SAME `humanizeUntil` the reset cells use.
    func testBlindVerdictCorneredSpeaksCannotActPlusRemedy() {
        // With a reset: the remedy folds "resets in {dur}" in, compact `humanizeUntil` format (no space).
        let cornered = StatusPanelFormat.blindVerdict(
            .cornered, nextSwap: .noViableTarget(cause: .weekly, resetsAt: 2 * 86400 + 4 * 3600), now: 0)
        XCTAssertEqual(cornered.symbol, "xmark.shield.fill")
        XCTAssertEqual(cornered.text, "Auto-protection CANNOT ACT")
        XCTAssertEqual(cornered.tint, .red)
        XCTAssertEqual(cornered.remedy, "Out of capacity, resets in 2d4h · add or free an account")

        // No reset on the wire → the bare unconditional remedy (still "add or free an account").
        let noReset = StatusPanelFormat.blindVerdict(
            .cornered, nextSwap: .noViableTarget(cause: nil, resetsAt: nil), now: 0)
        XCTAssertEqual(noReset.remedy, "Out of capacity · add or free an account")

        // Shape-distinct from BOTH lighter verdicts (not color-alone — WCAG 1.4.1).
        let ok = StatusPanelFormat.blindVerdict(.ok, nextSwap: nil, now: 0)
        let degraded = StatusPanelFormat.blindVerdict(.degraded, nextSwap: nil, now: 0)
        XCTAssertNotEqual(cornered.symbol, ok.symbol)
        XCTAssertNotEqual(cornered.symbol, degraded.symbol)
    }

    // A blind row keeps its credential's OWN warning glyph beside the eye-slash when the credential is itself
    // in a warning state — usage-blindness and credential-health are orthogonal, so a stale/at-risk credential
    // must NOT be visually suppressed just because the eye-slash took the health slot (#137 honest-state, and
    // the CLI keeps both). Healthy/unknown/absent add no warning → the eye-slash stands alone.
    func testBlindCoShowsAuthWarningOnlyForWarningCredentials() {
        XCTAssertTrue(StatusPanelFormat.blindCoShowsAuthWarning(.stale))
        XCTAssertTrue(StatusPanelFormat.blindCoShowsAuthWarning(.atRisk))
        XCTAssertTrue(StatusPanelFormat.blindCoShowsAuthWarning(.degraded))
        XCTAssertTrue(StatusPanelFormat.blindCoShowsAuthWarning(.dead))
        XCTAssertFalse(StatusPanelFormat.blindCoShowsAuthWarning(.healthy))
        XCTAssertFalse(StatusPanelFormat.blindCoShowsAuthWarning(.unknown))
        XCTAssertFalse(StatusPanelFormat.blindCoShowsAuthWarning(nil))
    }

    // The a11y label speaks the blind state (duration, last-known %, verdict) IN PLACE of the two meters —
    // matching what the blind row draws; never a fabricated live reading (#137).
    func testRowAccessibilityLabelSpeaksTheBlindState() {
        let degraded = StatusPanelFormat.rowAccessibilityLabel(
            label: "work", isActive: true, auth: .healthy, recovering: false, enabled: true,
            quarantined: false, sessionPct: nil, weeklyPct: nil, sessionReset: "n/a", weeklyReset: "n/a",
            blind: BlindActive(blindSecs: 1380, lastKnownSessionPct: 87, autoProtectionDegraded: true))
        XCTAssertEqual(degraded,
            "work, active, auth healthy, blind for 23m, last-known session 87 percent, auto-protection degraded, acting on a stale anchor")

        let ok = StatusPanelFormat.rowAccessibilityLabel(
            label: "work", isActive: true, auth: .healthy, recovering: false, enabled: true,
            quarantined: false, sessionPct: nil, weeklyPct: nil, sessionReset: "n/a", weeklyReset: "n/a",
            blind: BlindActive(blindSecs: 240, lastKnownSessionPct: 64, autoProtectionDegraded: false))
        XCTAssertEqual(ok,
            "work, active, auth healthy, blind for 4m, last-known session 64 percent, auto-protection okay, daemon self-resolving")
    }

    // A blind row whose credential is ALSO in a warning state speaks BOTH — the at-risk auth verdict is not
    // dropped because the poll went blind (the a11y half of #485's orthogonal-axes fix; the visual half rides
    // `blindCoShowsAuthWarning`). Orthogonal facts, both voiced.
    func testRowAccessibilityLabelSpeaksAuthWarningAlongsideBlind() {
        let label = StatusPanelFormat.rowAccessibilityLabel(
            label: "work", isActive: true, auth: .atRisk, recovering: false, enabled: true,
            quarantined: false, sessionPct: nil, weeklyPct: nil, sessionReset: "n/a", weeklyReset: "n/a",
            blind: BlindActive(blindSecs: 240, lastKnownSessionPct: 64, autoProtectionDegraded: false))
        XCTAssertEqual(label,
            "work, active, auth at risk, blind for 4m, last-known session 64 percent, auto-protection okay, daemon self-resolving")
    }

    // A CORNERED blind row (#572) speaks the cannot-act verdict AND the remedy — a VoiceOver user must HEAR
    // "add or free an account", not the understated "degraded" the pre-#572 label spoke. Composes from
    // `blind.autoProtectionDegraded` + a `noViableTarget` next-swap; the reset folds in via `humanizeUntil`.
    func testRowAccessibilityLabelSpeaksTheCorneredState() {
        let cornered = StatusPanelFormat.rowAccessibilityLabel(
            label: "work", isActive: true, auth: .healthy, recovering: false, enabled: true,
            quarantined: false, sessionPct: nil, weeklyPct: nil, sessionReset: "n/a", weeklyReset: "n/a",
            blind: BlindActive(blindSecs: 1080, lastKnownSessionPct: 92, autoProtectionDegraded: true),
            nextSwap: .noViableTarget(cause: .weekly, resetsAt: 2 * 86400 + 4 * 3600), now: 0)
        XCTAssertEqual(cornered,
            "work, active, auth healthy, blind for 18m, last-known session 92 percent, "
                + "auto-protection cannot act, out of capacity, resets in 2d4h, add or free an account")

        // Degraded + a viable target is NOT cornered — it stays the DEGRADED "stale anchor" line (the daemon
        // can still swap), proving the a11y label keys off the SAME composition as the visual, not `degraded`
        // alone.
        let stillDegraded = StatusPanelFormat.rowAccessibilityLabel(
            label: "work", isActive: true, auth: .healthy, recovering: false, enabled: true,
            quarantined: false, sessionPct: nil, weeklyPct: nil, sessionReset: "n/a", weeklyReset: "n/a",
            blind: BlindActive(blindSecs: 1080, lastKnownSessionPct: 92, autoProtectionDegraded: true),
            nextSwap: .target(to: "personal", reason: .onlyCandidate), now: 0)
        XCTAssertEqual(stillDegraded,
            "work, active, auth healthy, blind for 18m, last-known session 92 percent, "
                + "auto-protection degraded, acting on a stale anchor")
    }

    // MARK: - Per-account refresh-token EXPIRY modifier (issues #878/#882/#884)
    //
    // R-2 STATE-parity with the `status` CLI's `EXPIRY` column (#883): the SAME cases the Rust
    // `expiry_cell_classifies_each_horizon_and_reports_an_unobserved_deadline_as_a_gap` /
    // `the_expiry_cell_is_tinted_by_its_own_horizon_band` tests pin, asserted here against the Swift
    // mirrors. The panel cannot be screenshot-verified in CI, so these pure verdicts ARE the acceptance
    // gate for AC-1/3/4.

    /// The panel test clock — an arbitrary fixed epoch, matching the Rust tests' `NOW` discipline.
    private static let expiryNow: Int64 = 1_893_456_000

    func testExpiryCellMirrorsTheCLIForEveryHorizon() {
        let now = Self.expiryNow

        // An OBSERVED deadline renders the compact time-until the RESET cells already use — the same
        // `humanizeUntil` both surfaces share, so `3d` / `29d4h` are byte-identical to the CLI's.
        XCTAssertEqual(
            StatusPanelFormat.expiryCell(
                AccountExpiry(expiresAt: now + 3 * 86_400, horizonState: .within), now: now),
            "3d")
        XCTAssertEqual(
            StatusPanelFormat.expiryCell(
                AccountExpiry(expiresAt: now + 29 * 86_400 + 4 * 3_600, horizonState: .beyond), now: now),
            "29d4h")

        // A passed deadline is a STATE word, never `now` — `humanizeUntil`'s zero case would read as a
        // reset ARRIVING, the exact opposite of what has happened.
        XCTAssertEqual(
            StatusPanelFormat.expiryCell(
                AccountExpiry(expiresAt: now - 86_400, horizonState: .lapsed), now: now),
            "lapsed")
    }

    func testExpiryCellIsGapHonestForEveryFlavourOfAbsence() {
        let now = Self.expiryNow

        // GAP HONESTY (#137) — all three absences render the gap, never a silent "fine".
        // (a) polled, but the credential carried no parseable deadline …
        XCTAssertEqual(
            StatusPanelFormat.expiryCell(
                AccountExpiry(expiresAt: nil, horizonState: .unknown), now: now),
            StatusPanelFormat.expiryGap)
        // … (b) no modifier on the wire at all (a pre-#882 daemon, or an unpolled account) …
        XCTAssertEqual(StatusPanelFormat.expiryCell(nil, now: now), StatusPanelFormat.expiryGap)
        // … and (c) a MALFORMED frame claiming a FORWARD-LOOKING state with no deadline: `within` is a
        // claim ABOUT a deadline, so without one there is nothing to say. Unreachable from the daemon's
        // `account_expiry`, but the wire decodes each field independently, so it degrades honestly.
        XCTAssertEqual(
            StatusPanelFormat.expiryCell(
                AccountExpiry(expiresAt: nil, horizonState: .within), now: now),
            StatusPanelFormat.expiryGap)

        // The GAP IS NOT `n/a`: this panel already spends `n/a` on a FAILED poll, and filing an OBSERVED
        // absence under transient failure would misreport it.
        XCTAssertNotEqual(StatusPanelFormat.expiryGap, StatusPanelFormat.pct(nil))

        // Every absence is UNCOLOURED too, not merely gap-TEXTED — the tint half of the same rule, pinned
        // for each flavour so a future edit cannot leave a `within`-with-no-deadline showing an amber `—`.
        for absent in [nil,
                       AccountExpiry(expiresAt: nil, horizonState: .unknown),
                       AccountExpiry(expiresAt: nil, horizonState: .within),
                       AccountExpiry(expiresAt: nil, horizonState: .beyond),
                       AccountExpiry(expiresAt: now + 604_800, horizonState: .unknown)] {
            XCTAssertNil(StatusPanelFormat.expirySeverity(absent, now: now),
                         "absence of a reading must never carry colour")
        }
    }

    /// The arm-ordering pin, mirroring the Rust test's: a DECLARED `lapsed` with no deadline is NOT a
    /// data-insufficiency case — `lapsed` is a bare state word that never reads `expiresAt`. Were it to
    /// fall through to the gap, the roster would hide the whole line whenever the lapse was its only
    /// expiry datum (`rosterShowsExpiry` materializes it only once some row is non-gap) — a dead login
    /// rendered as no login problem at all, the one outcome this surface exists to prevent.
    func testDeclaredLapseWithNoDeadlineOutranksTheGapAndStillShowsTheLine() {
        let now = Self.expiryNow
        let declared = AccountExpiry(expiresAt: nil, horizonState: .lapsed)

        XCTAssertEqual(StatusPanelFormat.expiryCell(declared, now: now), "lapsed")
        XCTAssertEqual(StatusPanelFormat.expirySeverity(declared, now: now), .red)
        XCTAssertTrue(StatusPanelFormat.rosterShowsExpiry([declared], now: now),
                      "a lapse must never be swallowed by the roster-level gap elision")
    }

    /// The render-time comparison is AUTHORITATIVE (the CLI's `expiry_view` rule). The panel draws a
    /// snapshot built at the daemon's last tick — up to `poll_interval_secs` old — so a deadline that
    /// passes inside that window still arrives classified `within`/`beyond`. Trusting the cached class
    /// would render `now` (this panel's word for a BENIGN reset arriving) at exactly the moment the login
    /// died, and every token lapse crosses that window exactly once.
    func testAPassedDeadlineReadsLapsedEvenWhenTheCachedClassSaysOtherwise() {
        let now = Self.expiryNow
        for stale: ExpiryHorizon in [.within, .beyond] {
            let expiry = AccountExpiry(expiresAt: now - 60, horizonState: stale)
            XCTAssertEqual(StatusPanelFormat.expiryCell(expiry, now: now), "lapsed",
                           "a \(stale) class over a passed deadline must not render a duration")
            XCTAssertEqual(StatusPanelFormat.expirySeverity(expiry, now: now), .red)
        }

        // The BOUNDARY: `at == now` is already gone, matching the CLI's `at <= now`.
        XCTAssertEqual(
            StatusPanelFormat.expiryCell(
                AccountExpiry(expiresAt: now, horizonState: .within), now: now),
            "lapsed")

        // MONOTONE in the safe direction: a declared `lapsed` stays lapsed even should the client clock
        // read EARLIER than the daemon's (backwards skew, NTP). Once either clock has seen the deadline
        // pass, the line does not un-lapse.
        XCTAssertEqual(
            StatusPanelFormat.expiryCell(
                AccountExpiry(expiresAt: now + 86_400, horizonState: .lapsed), now: now),
            "lapsed")
    }

    func testExpirySeverityMirrorsTheCLIColourBands() {
        let now = Self.expiryNow

        // Red for a deadline already past (only an operator `claude /login` recovers it) …
        XCTAssertEqual(
            StatusPanelFormat.expirySeverity(
                AccountExpiry(expiresAt: now - 1, horizonState: .lapsed), now: now), .red)
        // … amber for one WITHIN the configured horizon (the foresight the feature exists to give) …
        XCTAssertEqual(
            StatusPanelFormat.expirySeverity(
                AccountExpiry(expiresAt: now + 3 * 86_400, horizonState: .within), now: now), .yellow)
        // … de-emphasised for one BEYOND it (the CLI's `Dim`): nothing to act on …
        XCTAssertEqual(
            StatusPanelFormat.expirySeverity(
                AccountExpiry(expiresAt: now + 90 * 86_400, horizonState: .beyond), now: now), .neutral)
        // … and UNCOLOURED for an unobserved deadline: absence of colour is not a false "healthy".
        XCTAssertNil(StatusPanelFormat.expirySeverity(nil, now: now))
        XCTAssertNil(StatusPanelFormat.expirySeverity(
            AccountExpiry(expiresAt: nil, horizonState: .unknown), now: now))

        // The amber/red pair must stay DISTINCT from each other — the whole point of the band is that
        // "re-login soon" and "re-login now" do not read alike.
        XCTAssertNotEqual(StatusPanelFormat.healthTint(.yellow), StatusPanelFormat.healthTint(.red))
    }

    /// AC-3 STATE-parity, stated as a relation rather than trusted to prose: for every input the two
    /// surfaces could see, the panel's cell string IS the CLI's cell string. The Rust side pins the same
    /// table (`src/cli.rs` `expiry_cell` tests); this asserts the Swift mirror lands on it case-for-case,
    /// so a future edit to one surface that forgets the other fails HERE rather than in an operator's eye.
    func testExpiryStateParityTableMatchesTheCLIVocabularyExactly() {
        let now = Self.expiryNow
        let table: [(AccountExpiry?, String, StatusPanelFormat.HealthTint?)] = [
            (nil,                                                             "—",      nil),
            (AccountExpiry(expiresAt: nil, horizonState: .unknown),           "—",      nil),
            (AccountExpiry(expiresAt: now + 604_800, horizonState: .unknown), "—",      nil),
            (AccountExpiry(expiresAt: nil, horizonState: .lapsed),            "lapsed", .red),
            (AccountExpiry(expiresAt: now - 86_400, horizonState: .lapsed),   "lapsed", .red),
            (AccountExpiry(expiresAt: now + 6 * 86_400 + 21 * 3_600, horizonState: .within), "6d21h", .yellow),
            (AccountExpiry(expiresAt: now + 45 * 60, horizonState: .within),  "45m",    .yellow),
            (AccountExpiry(expiresAt: now + 29 * 86_400, horizonState: .beyond), "29d", .neutral),
        ]
        for (expiry, cell, severity) in table {
            XCTAssertEqual(StatusPanelFormat.expiryCell(expiry, now: now), cell)
            XCTAssertEqual(StatusPanelFormat.expirySeverity(expiry, now: now), severity,
                           "severity drifted for the cell rendering '\(cell)'")
        }

        // An `unknown` class is authoritative in the OTHER direction: a stray `expiresAt` beside it is
        // NOT trusted into a rendered duration (row 3 above) — the daemon saw no parseable deadline.
        XCTAssertNil(StatusPanelFormat.expirySeverity(
            AccountExpiry(expiresAt: now + 604_800, horizonState: .unknown), now: now))
    }

    /// The within-horizon MARK (issue #935), mirroring the CLI's `expiry_table_cell` (issue #934) —
    /// what the panel actually DRAWS, as opposed to the byte-pinned cross-surface fact above.
    ///
    /// The mark exists because the tint was the ONLY thing between `5d18h` and `29d`: two durations of
    /// the same shape, distinguished by a colour that Increase Contrast, a greyscale filter, and
    /// colour-blindness each take away. Unlike the gap (`—` versus a duration) there is no text
    /// difference underneath to fall back on, so losing the colour lost the whole per-account signal.
    func testExpiryLineCellBracketsOnlyAWithinHorizonDeadline() {
        let now = Self.expiryNow
        let table: [(AccountExpiry?, String)] = [
            // WITHIN — marked, and the mark wraps whatever `expiryCell` produced.
            (AccountExpiry(expiresAt: now + 6 * 86_400 + 21 * 3_600, horizonState: .within), "[6d21h]"),
            (AccountExpiry(expiresAt: now + 45 * 60, horizonState: .within),                 "[45m]"),
            // BEYOND, the gap, and `lapsed` — all bare, matching the CLI arm for arm.
            (AccountExpiry(expiresAt: now + 29 * 86_400, horizonState: .beyond),             "29d"),
            (AccountExpiry(expiresAt: now - 86_400, horizonState: .lapsed),                   "lapsed"),
            (AccountExpiry(expiresAt: nil, horizonState: .lapsed),                            "lapsed"),
            (AccountExpiry(expiresAt: nil, horizonState: .unknown),                           "—"),
            (AccountExpiry(expiresAt: now + 604_800, horizonState: .unknown),                 "—"),
            (nil,                                                                             "—"),
            // The render-time staleness rule reaches the mark: a `within` class whose deadline has
            // already passed renders `lapsed`, so it must lose the bracket in the SAME step it loses
            // the amber. A cell reading `[lapsed]` would claim a deadline is inside a forward window.
            (AccountExpiry(expiresAt: now - 60, horizonState: .within),                       "lapsed"),
            (AccountExpiry(expiresAt: now, horizonState: .within),                            "lapsed"),
        ]
        for (expiry, drawn) in table {
            XCTAssertEqual(StatusPanelFormat.expiryLineCell(expiry, now: now), drawn)
        }
    }

    /// The mark and the tint agree on which cells are inside the horizon — written as separate matches
    /// (as the CLI writes them) precisely so this relation is assertable rather than tautological. If
    /// they could disagree, an operator without colour and an operator with it would be reading two
    /// different fleets.
    func testTheExpiryMarkAndTheTintAgreeOnWhichCellsAreWithin() {
        let now = Self.expiryNow
        let candidates: [AccountExpiry?] = [
            nil,
            AccountExpiry(expiresAt: nil, horizonState: .unknown),
            AccountExpiry(expiresAt: now + 604_800, horizonState: .unknown),
            AccountExpiry(expiresAt: nil, horizonState: .lapsed),
            AccountExpiry(expiresAt: now - 86_400, horizonState: .lapsed),
            AccountExpiry(expiresAt: now - 60, horizonState: .within),
            AccountExpiry(expiresAt: now, horizonState: .within),
            AccountExpiry(expiresAt: now + 1, horizonState: .within),
            AccountExpiry(expiresAt: now + 3 * 86_400, horizonState: .within),
            AccountExpiry(expiresAt: now + 29 * 86_400, horizonState: .beyond),
        ]
        var marked = 0
        for expiry in candidates {
            let isMarked = StatusPanelFormat.expiryWithinHorizon(expiry, now: now)
            XCTAssertEqual(isMarked, StatusPanelFormat.expirySeverity(expiry, now: now) == .yellow,
                           "the mark and the amber band disagree about \(String(describing: expiry))")
            // And the predicate really is what the drawn cell brackets on.
            let drawn = StatusPanelFormat.expiryLineCell(expiry, now: now)
            XCTAssertEqual(isMarked, drawn.hasPrefix("[") && drawn.hasSuffix("]"))
            if isMarked { marked += 1 }
        }
        // Degenerate-subject guard: an all-unmarked sweep would satisfy every assertion above.
        XCTAssertEqual(marked, 2, "expected exactly the two live within-horizon rows to be marked")
    }

    /// The mark costs EXACTLY the two bracket characters, whatever the cell underneath is — the claim
    /// that actually bounds the line's width, and the one that survives a re-configured horizon.
    ///
    /// An earlier draft asserted only that `[6d23h]` is 7 characters, which reads as a width ceiling
    /// but is not one: `expiry_horizon_secs` is operator-configurable, so a 30-day horizon admits
    /// `[29d23h]` and a 400-day one `[1y2d]`-shaped cells. The bracket overhead is the invariant; the
    /// widest DURATION is a property of the operator's configuration, not of this function. (Nothing
    /// truncates either way — the line has no fixed frame and ends in a `Spacer`.)
    func testTheMarkCostsExactlyTwoCharactersOverTheUnmarkedCell() {
        let now = Self.expiryNow
        let withins: [Int64] = [45 * 60, 86_400, 6 * 86_400 + 23 * 3_600, 29 * 86_400, 400 * 86_400]
        var marked = 0
        for offset in withins {
            let expiry = AccountExpiry(expiresAt: now + offset, horizonState: .within)
            let bare = StatusPanelFormat.expiryCell(expiry, now: now)
            let drawn = StatusPanelFormat.expiryLineCell(expiry, now: now)
            XCTAssertEqual(drawn, "[\(bare)]", "the mark must wrap the fact, never restate it")
            XCTAssertEqual(drawn.count, bare.count + 2)
            marked += 1
        }
        XCTAssertEqual(marked, withins.count, "the sweep skipped a case")

        // At the 7-day DEFAULT horizon the widest cell is `[6d23h]` — 7 columns against the 6 that
        // `lapsed` and the `EXPIRY` label already require, so the default costs the line one column.
        XCTAssertEqual(StatusPanelFormat.expiryLineCell(
            AccountExpiry(expiresAt: now + 6 * 86_400 + 23 * 3_600, horizonState: .within), now: now),
            "[6d23h]")
        XCTAssertEqual("lapsed".count, StatusPanelFormat.expiryRowLabel.count)
    }

    /// The roster-level materialization rule, mirroring the CLI's `status_columns` (`EXPIRY` is built
    /// only `if rows.iter().any(|row| row.expiry != EXPIRY_GAP)`). An all-gap roster shows NO line on
    /// either surface — gap-honest, because it CLAIMS nothing — but a single observed deadline turns the
    /// line on for EVERY row, so a sibling's `—` reads as the pointed absence it is.
    func testRosterShowsExpiryOnlyOnceSomeAccountHasAnObservedDeadline() {
        let now = Self.expiryNow

        XCTAssertFalse(StatusPanelFormat.rosterShowsExpiry([], now: now),
                       "an empty roster claims nothing")
        XCTAssertFalse(StatusPanelFormat.rosterShowsExpiry([nil, nil], now: now),
                       "an unpolled / pre-#882 roster shows no line rather than a column of dashes")
        XCTAssertFalse(
            StatusPanelFormat.rosterShowsExpiry(
                [nil, AccountExpiry(expiresAt: nil, horizonState: .unknown)], now: now),
            "polled-but-no-deadline is still a gap — an all-unknown roster stays silent, exactly as the CLI elides its column")

        XCTAssertTrue(
            StatusPanelFormat.rosterShowsExpiry(
                [nil, AccountExpiry(expiresAt: now + 86_400, horizonState: .within)], now: now),
            "one observed deadline turns the line on for the whole roster")
        XCTAssertTrue(
            StatusPanelFormat.rosterShowsExpiry(
                [AccountExpiry(expiresAt: now - 1, horizonState: .lapsed), nil], now: now),
            "a lapse alone must materialize the line")
    }

    /// AC-4: the spoken row carries the expiry too. The visual line is `accessibilityHidden` (the row is
    /// ONE VoiceOver element), so this label is the ONLY place a screen-reader user learns their login
    /// has a deadline — and the gap is spoken in WORDS, never as the bare `—` dash VoiceOver would skip
    /// or pronounce as punctuation. A silently-dropped absence is a false calm.
    func testRowAccessibilityLabelSpeaksTheExpiryIncludingTheGap() {
        let now = Self.expiryNow
        func spoken(_ expiry: AccountExpiry?, shows: Bool = true) -> String {
            StatusPanelFormat.rowAccessibilityLabel(
                label: "work", isActive: false, auth: .healthy, recovering: false, enabled: true,
                quarantined: false, sessionPct: 30, weeklyPct: 20, sessionReset: "2h", weeklyReset: "3d",
                now: now, expiry: expiry, showsExpiry: shows)
        }

        // WITHIN carries the horizon band in words (issue #935) — the spoken twin of the brackets the
        // line now draws. A screen-reader user gets no colour at all, so before the mark they had
        // strictly less than a sighted operator squinting at a tint: `3d` and `29d` were one sentence.
        XCTAssertTrue(spoken(AccountExpiry(expiresAt: now + 3 * 86_400, horizonState: .within))
            .hasSuffix("login expires in 3d, inside the expiry horizon"))
        // BEYOND stays a bare duration, exactly as the line stays unbracketed — spoken and visual are
        // marked together or neither, so a VoiceOver user and a sighted one read the same fleet.
        XCTAssertTrue(spoken(AccountExpiry(expiresAt: now + 29 * 86_400, horizonState: .beyond))
            .hasSuffix("login expires in 29d"))
        XCTAssertFalse(spoken(AccountExpiry(expiresAt: now + 29 * 86_400, horizonState: .beyond))
            .contains("horizon"), "a beyond-horizon deadline must not be spoken as if it were inside")
        // PARITY WITH THE VISUAL LINE, not a richer spoken variant: the cell renders a bare `lapsed`, so
        // the label speaks a bare "login expired" — see `rowAccessibilityLabel` for why a spoken-only
        // remedy is the defect this pins against.
        XCTAssertTrue(spoken(AccountExpiry(expiresAt: now - 1, horizonState: .lapsed))
            .hasSuffix("login expired"))
        XCTAssertFalse(spoken(AccountExpiry(expiresAt: now - 1, horizonState: .lapsed)).contains("claude"),
                       "the spoken line must not carry a remedy the visual cell withholds")
        XCTAssertTrue(spoken(nil).hasSuffix("login expiry not observed"),
                      "the gap is spoken as an absence, never as a dash and never dropped")

        // When the roster hides the line, the label stays silent about expiry too — the spoken and visual
        // surfaces materialize together, so VoiceOver never announces a fact the panel does not show.
        XCTAssertFalse(spoken(nil, shows: false).contains("login expir"))
        XCTAssertEqual(spoken(nil, shows: false),
                       "work, auth healthy, session 30% resets in 2h, weekly 20% resets in 3d")
    }

    /// The composition invariant (#884), pinned as an executable claim rather than a comment: expiry does
    /// NOT escalate the menu-bar glance, therefore it ships NO panel banner — "both or neither", resolved
    /// as NEITHER. `PresentationState.make` takes no expiry input at all, so the strongest possible expiry
    /// signal (a LAPSED login on the active account) leaves a healthy fleet's glyph healthy.
    ///
    /// What this can and cannot catch: it pins both SIGNATURES, so a later item that feeds expiry in as a
    /// REQUIRED input breaks here at compile time and owes its sibling banner in the same change. An input
    /// added with a default would slip past — so the invariant's real home is the rationale block in
    /// `StatusPanelFormat`, and this is its cheapest executable half.
    func testALapsedExpiryNeitherEscalatesTheGlanceNorRaisesADaemonBanner() {
        let healthy = PresentationState.make(for: .connected, accountCount: 2)
        XCTAssertEqual(healthy.glyph, .healthy)

        // The daemon-level banner set is unchanged: expiry is a ROW-axis modifier — it HAS a row to live
        // on, which is exactly why it is not a fourth daemon-level payload fault (the vault and the
        // refresh mechanism are faults precisely because neither has a row).
        XCTAssertNil(StatusPanelFormat.daemonFaultBanner(
            keychainLocked: false, scrub: nil, systemicRefreshFailure: nil,
            systemicRefreshSource: nil, canary: nil))
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
        // #526: a warm drop still WITHIN the dwell shows the SAME retained roster header as the escalation —
        // the dimmed last-known roster, never a false "active".
        XCTAssertEqual(
            StatusPanelFormat.headerSubtitle(state: .reconnecting(reason: "EOF"), accountCount: 3,
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

    // MARK: - Account identity color (issue #445 — deterministic label→palette hash, WCAG-AA, accent-excluded)

    func testAccountColorIndexIsStableAndBounded() {
        // Deterministic: the same label maps to the same slot on EVERY call (FNV-1a — never the per-process
        // randomized `Hasher`, which would reshuffle every account's color each launch).
        for label in ["work-alice", "work-bob", "acme.gmail.com", "", "Personal", "  spaced  "] {
            let a = StatusPanelFormat.accountColorIndex(for: label)
            let b = StatusPanelFormat.accountColorIndex(for: label)
            XCTAssertEqual(a, b, "index for '\(label)' must be stable across calls")
            XCTAssertTrue((0..<StatusPanelFormat.accountColorCount).contains(a),
                          "index \(a) for '\(label)' must be within the palette")
        }
        // Leading/trailing whitespace is trimmed before hashing, so a padded label keeps its color.
        XCTAssertEqual(StatusPanelFormat.accountColorIndex(for: "work"),
                       StatusPanelFormat.accountColorIndex(for: "  work  "))
        // A 6-account same-local-part roster spreads across several slots, not one collapsed color.
        let indices = ["work-alice", "work-bob", "work-carol", "work-dave", "work-erin", "work-frank"]
            .map(StatusPanelFormat.accountColorIndex(for:))
        XCTAssertGreaterThanOrEqual(Set(indices).count, 3,
                                    "a 6-account same-local-part roster should not collapse to < 3 colors")
    }

    func testEveryPaletteSlotIsReachable() {
        // Each of the N slots is hit by some label — no dead palette entry (also proves the probe helper
        // works). Keyed by the fill's components (FillRGBA is Equatable, not Hashable) → distinct per slot.
        let keys = (0..<StatusPanelFormat.accountColorCount).map { slot -> String in
            let fill = paletteFill(slot, dark: false)
            return "\(fill.red),\(fill.green),\(fill.blue)"
        }
        XCTAssertEqual(Set(keys).count, StatusPanelFormat.accountColorCount,
                       "every palette slot must be reachable and its fill distinct")
    }

    func testAccountPaletteMeetsWcagAAAgainstThePanelReferenceBase() {
        // The panel floats on live vibrancy — NOT headlessly measurable (the owner-eyeball residual, same class
        // as #326/#388/#446/#504). We assert against the mock's OPAQUE popover reference base, the same
        // convention the #388 `--text-2` comment uses ("4.53:1 over #f5f5f7"): light #f7f7fa / dark #26262b.
        let lightBase = Self.lightBase
        let darkBase = Self.darkBase
        let lightText = StatusPanelFormat.accountMonogramColor(dark: false)
        let darkText = StatusPanelFormat.accountMonogramColor(dark: true)
        for slot in 0..<StatusPanelFormat.accountColorCount {
            let lightFill = paletteFill(slot, dark: false)
            let darkFill = paletteFill(slot, dark: true)
            // Badge FILL vs the panel base — WCAG 1.4.11 non-text ≥ 3:1 (a perceptible color region).
            XCTAssertGreaterThanOrEqual(contrast(lightFill, lightBase), 3.0,
                                        "light fill \(slot) must clear 3:1 on the panel base")
            XCTAssertGreaterThanOrEqual(contrast(darkFill, darkBase), 3.0,
                                        "dark fill \(slot) must clear 3:1 on the panel base")
            // Monogram GLYPH vs its actual background (the opaque fill) — WCAG 1.4.3 text ≥ 4.5:1.
            XCTAssertGreaterThanOrEqual(contrast(lightText, lightFill), 4.5,
                                        "light monogram \(slot) must clear 4.5:1 on its fill")
            XCTAssertGreaterThanOrEqual(contrast(darkText, darkFill), 4.5,
                                        "dark monogram \(slot) must clear 4.5:1 on its fill")
        }
    }

    func testAccountPaletteExcludesTheAccentHue() {
        // Accent = brand blue (#007aff light / #0a84ff dark), hue ≈ 211°. Every palette hue sits ≥ 25° away so
        // the identity color never reads as the one accent action (#445 AC "excluding the active/accent hue").
        let accentHue = hue(RGB(0, 122, 255))
        for slot in 0..<StatusPanelFormat.accountColorCount {
            let h = hue(paletteFill(slot, dark: false))
            let delta = min(abs(h - accentHue), 360 - abs(h - accentHue))
            XCTAssertGreaterThanOrEqual(delta, 25,
                                        "palette hue \(slot) (\(Int(h))°) is too close to the accent (\(Int(accentHue))°)")
        }
    }

    // MARK: - Smart monogram (issue #445 — distinguishing token, collision-escalating, never label.first)

    func testMonogramUsesTheDistinguishingTokenNotLabelFirst() {
        // `label.first` would collapse a same-local-part roster to one letter; the smart monogram pairs the
        // first token's initial with the distinguishing suffix token's initial.
        let m = StatusPanelFormat.accountMonograms(["work-alice", "work-bob", "work-carol"])
        XCTAssertEqual(m["work-alice"], "WA")
        XCTAssertEqual(m["work-bob"], "WB")
        XCTAssertEqual(m["work-carol"], "WC")
        XCTAssertFalse(Set(m.values).contains("W"), "must not collapse to label.first")
    }

    func testMonogramsAreDistinctAcrossSimilarRosters() {
        // The core AC property: two similar labels never collapse to the same pair — the resolved set is fully
        // distinct, each ≤ 2 chars, non-empty.
        let rosters: [[String]] = [
            ["work-alice", "work-bob", "work-carol", "work-dave", "work-erin", "work-frank"],
            // Shared prefix AND suffix — the distinguishing token is in the MIDDLE, so first⋅last collapses
            // (all → "WX") and the ladder must escalate to first⋅second to stay distinct.
            ["work-alpha-x", "work-beta-x", "work-gamma-x"],
            ["acme.gmail.com", "acme.work.com", "acme.proton.me"],
            ["proj-1", "proj-2", "proj-10", "proj-11"],
            ["work", "works", "working", "workflow"],
            ["a", "b", "c"],
            ["team/alpha", "team/beta", "team/gamma"],
        ]
        for roster in rosters {
            let m = StatusPanelFormat.accountMonograms(roster)
            XCTAssertEqual(Set(m.values).count, roster.count, "monograms must be distinct for \(roster)")
            for (label, mono) in m {
                XCTAssertFalse(mono.isEmpty, "monogram for '\(label)' must be non-empty")
                XCTAssertLessThanOrEqual(mono.count, 2, "monogram '\(mono)' must be ≤ 2 chars")
            }
        }
    }

    func testMonogramDerivationIsDeterministic() {
        let roster = ["work-alice", "work-bob", "acme.gmail.com"]
        XCTAssertEqual(StatusPanelFormat.accountMonograms(roster), StatusPanelFormat.accountMonograms(roster))
    }

    func testMonogramSingleTokenAndDegenerateLabels() {
        XCTAssertEqual(StatusPanelFormat.accountMonograms(["Work"])["Work"], "WO")   // 2 chars from one token
        XCTAssertEqual(StatusPanelFormat.accountMonograms(["camelCase"])["camelCase"], "CC")  // camelCase split
        XCTAssertEqual(StatusPanelFormat.accountMonograms(["x"])["x"], "X")          // lone char → itself
        XCTAssertEqual(StatusPanelFormat.accountMonograms([""])[""], "?")            // empty → sentinel
        XCTAssertEqual(StatusPanelFormat.accountMonograms(["  "])["  "], "?")        // whitespace → sentinel
    }

    // MARK: - #388 tint-token CONTRAST (issue #759 — the token VALUES, not just the role→token mapping)
    //
    // The tests at :123-152 guard WHICH token each role maps to. These guard what the tokens ARE: the
    // shipped `Assets.xcassets` colour sets, resolved through the REAL `Color.panelAssets` seam under each
    // of the four appearances the panel can be drawn in (aqua / darkAqua × normal / Increase-Contrast), and
    // measured with the same sRGB helper the #445 palette test uses (:1414).
    //
    // TWO DIFFERENT BARS, and getting them backwards would manufacture a false failure. A token's bar is set
    // by the SURFACE it paints, which was established by censusing every consumer site rather than assumed:
    //
    //   TEXT (WCAG 1.4.3 → 4.5:1)
    //     UtilGreen/Amber/Red  `usageTextTint`      → the percent text        (Roster :556, :628)
    //     UtilOrange/Red       `healthTint`         → the auth cue text       (Roster :462-463)
    //     UtilOrange/Red       `healthTint`         → blind verdict + remedy  (Roster :655, :666)
    //     UtilAmber            `healthTint(.yellow)`→ footer stale-age text   (Chrome :404)
    //   NON-TEXT (WCAG 1.4.11 → 3:1)
    //     HealthOK + the above `healthTint`         → the health SF Symbol    (Roster :472)
    //                                               → blind leading rule      (Roster :338)
    //                                               → verdict shield glyph    (Roster :652)
    //
    // `HealthOK` is the one GLYPH-ONLY token — it never paints text, so holding it to 4.5:1 would be a false
    // failure (it measures 3.08:1 in light). What keeps it off text is `blindVerdict` emitting `.neutral`
    // rather than `.green` for the OK verdict; that is load-bearing, so it is pinned below rather than left
    // to a reader's goodwill.

    /// The panel reference base — the mock's OPAQUE popover colour. Same convention, and the same two
    /// values, the #445 palette test uses at :1263 (the live panel floats on vibrancy, which is not
    /// headlessly measurable; this is the agreed stand-in).
    private static let lightBase = RGB(247, 247, 250)
    private static let darkBase = RGB(38, 38, 43)

    /// The surfaces the panel can be drawn on — light and dark, each with the base it sits on.
    ///
    /// The Increase-Contrast variants are deliberately ABSENT, and that is a measured decision rather than
    /// an oversight. Each colour set ships four variants (Any / Dark / high-contrast / dark high-contrast),
    /// so a four-surface sweep looks obviously right — but AppKit selects the `contrast: high` variant from
    /// the SYSTEM Increase-Contrast setting, not from the `NSAppearance` name, so
    /// `.accessibilityHighContrastAqua` resolves BYTE-IDENTICALLY to `.aqua` here. A four-surface sweep
    /// would therefore be two real measurements and two duplicates presenting as four — a degenerate gate
    /// that reports broader coverage than it has. `testTheHighContrastVariantsAreNotReachableByAppearance`
    /// pins that so the gap stays visible; closing it is issue #832.
    private static let surfaces: [(name: String, appearance: NSAppearance.Name, base: RGB)] = [
        ("light", .aqua, lightBase),
        ("dark", .darkAqua, darkBase),
    ]

    /// The #388 tokens that paint TEXT somewhere in the panel (censused above) — the 4.5:1 set.
    private static let textTintAssets = ["UtilGreen", "UtilAmber", "UtilOrange", "UtilRed"]
    /// Every #388 token reachable through `healthTint`, i.e. every token that paints a GLYPH — the 3:1 set.
    private static let nonTextTintAssets = ["HealthOK", "UtilAmber", "UtilOrange", "UtilRed"]

    /// Max-channel separation (0…1) at or under which two deuteranope-simulated tints count as NOT told
    /// apart by colour, so the SHAPE channel has to carry the distinction. A judgment threshold rather than
    /// a standard, set with headroom: every collapse this palette actually exhibits measures ≤ 5.65/255
    /// (quoted in `testSeverityPairsThatCollapseUnderDeuteranopiaAreSeparatedByShape`), so a small
    /// platform drift in the simulated values cannot quietly walk a pair across the line.
    private static let deuteranopeCollapse = 12 / 255.0

    /// The ONE contrast predicate every assertion in this section runs through, so the canary
    /// (`testTheContrastGateCanFail`) proves THE GATE can fail rather than proving some parallel arithmetic
    /// can. Mutation-driven, never inspection — the #437 lesson `BarGlyphParityTests` records.
    private func clearsBar(_ foreground: RGB, on background: RGB, bar: Double) -> Bool {
        contrast(foreground, background) >= bar
    }

    /// Every (token, surface) cell that does NOT clear `bar`, named `token@surface`. `resolve` is injected
    /// so the canary can feed the SAME audit a deliberately degraded token and watch the cell appear.
    private func failingCells(
        _ assets: [String],
        bar: Double,
        resolve: (String, NSAppearance.Name) throws -> RGB
    ) rethrows -> Set<String> {
        var failures: Set<String> = []
        for asset in assets {
            for surface in Self.surfaces {
                let resolved = try resolve(asset, surface.appearance)
                if !clearsBar(resolved, on: surface.base, bar: bar) {
                    failures.insert("\(asset)@\(surface.name)")
                }
            }
        }
        return failures
    }

    /// Every pair of `assets` that resolves to the SAME colour on some surface, named `a~b@surface`. The
    /// distinctness counterpart of `failingCells`, carrying the same injected `resolve` for the same reason:
    /// it lets the canary drive THIS sweep by mutation instead of re-doing its arithmetic on itself.
    private func collapsedPairs(
        _ assets: [String],
        resolve: (String, NSAppearance.Name) throws -> RGB
    ) rethrows -> Set<String> {
        var collapsed: Set<String> = []
        for surface in Self.surfaces {
            let resolved = try assets.map { try resolve($0, surface.appearance) }
            for i in resolved.indices {
                for j in resolved.indices where j > i && channelDistance(resolved[i], resolved[j]) == 0 {
                    collapsed.insert("\(assets[i])~\(assets[j])@\(surface.name)")
                }
            }
        }
        return collapsed
    }

    // MARK: AC-1 — every TEXT tint token clears WCAG AA (4.5:1), in both themes

    func testEveryTextTintTokenClearsWcagAAExceptTheOneDocumentedMiss() throws {
        // MEASURED, not assumed — and the measurement contradicted the issue's premise, so this gates at the
        // measured boundary rather than fabricating a pass. 7 of the 8 (token × surface) cells clear 4.5:1.
        // One does not:
        //
        //   UtilGreen (mock `--ut-g` #268a3f) in LIGHT measures 4.10:1 — a REAL WCAG 1.4.3 miss on the
        //   healthy percent text. It is base-insensitive: 4.10 over the #f7f7fa panel base, 4.03 over the
        //   mock page #f5f5f7, 4.38 even over pure white. The high-contrast variant (#1f7134) clears at 5.66.
        //
        // NOT fixed here, deliberately. Per #388 the panel tokens are DERIVED from the mock's CSS custom
        // properties, so the fix belongs in `design/menubar-preview.html` — the ratified visual build
        // reference — and darkening `--ut-g` reddens the 34 byte-compared panel goldens (#754), whose
        // re-baseline is gated on a `Panel-Goldens-Rebaselined:` trailer. That is a palette change the
        // operator has not approved, so this pins CURRENT behaviour and tracks the fix in issue #830.
        //
        // The assertion is set EQUALITY, not "ignore the known one": a second token dropping below AA grows
        // the set and fails loudly, and FIXING `--ut-g` shrinks it and also fails loudly — telling whoever
        // fixed it to drop the exception and close the issue. A one-sided allowlist would silently swallow
        // both.
        let documentedMisses: Set<String> = ["UtilGreen@light"]
        let measured = try failingCells(Self.textTintAssets, bar: 4.5, resolve: assetRGB)
        XCTAssertEqual(measured, documentedMisses,
                       "the set of text tints below WCAG AA 4.5:1 changed. Grew → a token regressed below "
                       + "AA. Shrank → the palette was fixed: delete the stale entry and close issue "
                       + "#830. Measured failures: \(measured.sorted())")

        // Pin the miss's MAGNITUDE too, so a drift that keeps it failing but moves it materially still trips.
        let green = try assetRGB("UtilGreen", .aqua)
        XCTAssertEqual(contrast(green, Self.lightBase), 4.10, accuracy: 0.05,
                       "UtilGreen/light drifted from its pinned 4.10:1 — re-derive the exception in issue "
                       + "#830 before adjusting this number")
    }

    func testTheHighContrastVariantsAreNotReachableByAppearance() throws {
        // Discovered by measurement while building the sweep above, and pinned here because the NEXT person
        // to read those colour sets will reach the same obvious-looking conclusion I did: four variants ship,
        // therefore sweep four appearances. They do not resolve. Every asset returns its NORMAL-contrast
        // value under the high-contrast appearance name, so adding those surfaces would silently double the
        // apparent coverage without measuring anything new — the degenerate-gate failure mode.
        //
        // This asserts the CURRENT platform behaviour, so if a future macOS starts honouring the appearance
        // name this test goes red and points at issue #832 — which is the wanted outcome: that is the
        // day the high-contrast variants become guardable and the surface list should grow.
        for asset in Self.nonTextTintAssets + Self.textTintAssets {
            XCTAssertEqual(try assetRGB(asset, .accessibilityHighContrastAqua),
                           try assetRGB(asset, .aqua),
                           "\(asset) now differs under the high-contrast appearance — AppKit began honouring "
                           + "it. Add the contrast surfaces to `surfaces` and close issue #832.")
            XCTAssertEqual(try assetRGB(asset, .accessibilityHighContrastDarkAqua),
                           try assetRGB(asset, .darkAqua),
                           "\(asset) now differs under the dark high-contrast appearance — see issue #832.")
        }
        // The colour sets DO ship distinct high-contrast variants; this is a resolution gap, not a design
        // gap. Left un-asserted here on purpose — reading the .colorset JSON off disk from a test bundle
        // would guard the file rather than the seam the panel actually draws through.
    }

    // MARK: AC-2 — non-text fills take the DIFFERENT, lower bar (3:1), and text took the darker family

    func testEveryGlyphTintClearsTheWcagNonTextBar() throws {
        // The 3:1 bar — the "correct, different bar" the `StatusPanelRoster` exemption comment describes.
        // Every token that paints a health glyph / leading rule / shield clears it on all four surfaces
        // (the tightest cell is HealthOK in light at 3.08:1).
        let failures = try failingCells(Self.nonTextTintAssets, bar: 3.0, resolve: assetRGB)
        XCTAssertEqual(failures, [],
                       "glyph tints below the WCAG 1.4.11 non-text bar of 3:1: \(failures.sorted())")
    }

    // MARK: AC-3 — the RESTING swap chip clears the non-text bar AS RENDERED (issue #956)
    //
    // Every assertion above measures a TOKEN. This one measures PIXELS, and it has to, for a reason that is
    // specific to this chip rather than a stylistic preference.
    //
    // The resting chip was `.foregroundStyle(.tertiary)` — a hierarchical style, which is unresolvable BY
    // CONSTRUCTION: `HierarchicalShapeStyle.resolve(in:)` returns `Never` on every OS version, and the macOS
    // 13 deployment target has no `resolve` API at all. So no `assetRGB`-shaped test could ever read its
    // value, and the gate that did exist (`PanelInteractionStateTests`) could only assert the RELATION
    // `armedInk > restingInk` plus a 0.001 magnitude floor. A resting chip at 1.91:1 satisfies both and
    // reports green — which is exactly what happened: issue #949 measured a BUILT panel and found resting
    // FAILING the WCAG 1.4.11 non-text floor of 3:1 in BOTH appearances (1.91:1 light, 2.70:1 dark), with
    // ZERO of 243 light / 433 dark chip pixels clearing it, while every gate in the tree stayed green.
    //
    // Reading the shipped raster sidesteps the whole problem: pixels resolve whatever the style would not.
    // Issue #956 moved the token to an asset colour set, which IS readable — but a token assertion alone
    // would re-introduce the same blind spot one indirection later (it proves the TOKEN's value, not that
    // the chip is PAINTED with it), so the acceptance gate is the render and the token check below is only
    // corroboration.
    //
    // NAMED HONESTLY: this is contrast IN THE REFERENCE RENDER, and it is never a claim about shipped
    // appearance. WCAG 1.4.11 against a translucent vibrancy backdrop over arbitrary wallpaper is not a
    // well-posed question — there is no single adjacent colour — so the repo measures against representative
    // solids instead (`BarGlyphRenderer.swift:67`, and `surfaces` above). Same convention here, except that
    // the background is not even assumed: it is read out of the ROI.

    /// The swap chip's pixel box in a `healthy` render (760 × 898), pinned by measurement in issue #949
    /// against `design/renders/panel-goldens/panel-healthy-{light,dark}.png`. `x` is the chip's own 32 px
    /// column; the two `y` bands are the two switch-target rows — the ACTIVE row is not a switch target and
    /// carries no chip, so including it would dilute the sample with plain background.
    private static let swapChipColumn = 698..<730
    private static let swapChipRows = [349..<375, 541..<567]

    /// The chip's rendered INK and the surface it actually sits on, both read out of the ROI rather than
    /// assumed: the BACKGROUND is the modal colour there (the row fill), and the INK is the colour furthest
    /// from it in luminance — the glyph's opaque core, since antialiasing only ever blends BETWEEN the two
    /// and therefore cannot overshoot either end.
    ///
    /// Deriving both ends from the render is what makes this an absolute measurement of shipped pixels
    /// rather than a restatement of the token: it stays correct if the row fill moves, and it collapses to
    /// a ratio of 1.0 (a loud fail) if the chip stops being drawn at all.
    private func renderedChipInk(
        _ raster: PanelRaster, file: StaticString = #filePath, line: UInt = #line
    ) -> (ink: RGB, background: RGB, samples: Int)? {
        guard raster.width >= Self.swapChipColumn.upperBound,
              let lastRow = Self.swapChipRows.last, raster.height >= lastRow.upperBound else {
            XCTFail("""
                render is \(raster.width)×\(raster.height), too small for the chip ROI pinned in issue #949 \
                (x \(Self.swapChipColumn), y \(Self.swapChipRows)). The panel's LAYOUT moved — re-derive the \
                ROI against a fresh render before trusting any number from this gate.
                """, file: file, line: line)
            return nil
        }
        var counts: [UInt32: Int] = [:]
        for band in Self.swapChipRows {
            for y in band {
                for x in Self.swapChipColumn {
                    let o = (y * raster.width + x) * 4
                    counts[UInt32(raster.bytes[o]) << 16
                           | UInt32(raster.bytes[o + 1]) << 8
                           | UInt32(raster.bytes[o + 2]), default: 0] += 1
                }
            }
        }
        guard let background = counts.max(by: { $0.value < $1.value }).map({ Self.srgb($0.key) }) else {
            XCTFail("the chip ROI sampled no pixels at all", file: file, line: line)
            return nil
        }
        let backgroundLuminance = relativeLuminance(background)
        let ink = counts.keys
            .map(Self.srgb)
            .max { abs(relativeLuminance($0) - backgroundLuminance)
                 < abs(relativeLuminance($1) - backgroundLuminance) }
        guard let ink else { return nil }
        return (ink, background, counts.values.reduce(0, +))
    }

    private static func srgb(_ packed: UInt32) -> RGB {
        RGB(Int((packed >> 16) & 0xFF), Int((packed >> 8) & 0xFF), Int(packed & 0xFF))
    }

    /// Rasterize the `healthy` fixture through the SAME harness the panel goldens use, so this gate and the
    /// drift gate are looking at the same pixels.
    @MainActor
    private func renderedHealthyPanel(
        _ scheme: ColorScheme, file: StaticString = #filePath, line: UInt = #line
    ) -> PanelRaster? {
        let now = Int64(Date().timeIntervalSince1970)
        guard let fixture = PanelRenderHarness.fixtures(now: now).first(where: { $0.name == "healthy" }) else {
            XCTFail("no `healthy` fixture in the harness catalog", file: file, line: line)
            return nil
        }
        guard let cg = PanelRenderHarness.render(fixture, scheme: scheme) else {
            XCTFail("""
                ImageRenderer returned nil for healthy/\(scheme == .light ? "light" : "dark"). If \
                ImageRendererHeadlessProbeTests is ALSO red, a platform revision withdrew headless SwiftUI \
                rasterization (issue #749) and this gate has lost its foundation.
                """, file: file, line: line)
            return nil
        }
        return PanelRaster.normalize(cg)
    }

    @MainActor
    func testTheRestingSwapChipClearsTheWcagNonTextBarAsRendered() throws {
        // 3.34:1 is the DESIGNED value, not a floor scraped: resting spans 1.93–2.07:1 across rows in a live
        // panel as the backdrop varies, so aiming at a nominal 3.0 would leave the worst row near 2.8. The
        // tokens (`#828282` light / `#808080` dark) were solved against the backgrounds this ROI actually
        // measures — rgb(239,239,239) and rgb(48,48,48).
        for (name, scheme) in [("light", ColorScheme.light), ("dark", ColorScheme.dark)] {
            let raster = try XCTUnwrap(renderedHealthyPanel(scheme),
                                       "could not rasterize healthy/\(name) into an sRGB RGBA8 buffer")
            let measured = try XCTUnwrap(renderedChipInk(raster), "chip ROI unreadable in healthy/\(name)")
            let ratio = contrast(measured.ink, measured.background)

            XCTAssertNotEqual(measured.ink, measured.background,
                              "the chip ROI in healthy/\(name) is a flat \(measured.background) — the chip "
                              + "is not being drawn at all, so the ratio below is meaningless")
            XCTAssertGreaterThanOrEqual(
                ratio, 3.0,
                "the RESTING swap chip measures \(String(format: "%.2f", ratio)):1 in healthy/\(name) "
                + "(ink \(measured.ink) on \(measured.background), \(measured.samples) px sampled) — below "
                + "the WCAG 1.4.11 non-text floor of 3:1. This is the regression issue #956 fixed: do NOT "
                + "relax the bar, move `StatusPanelFormat.switchChipRestingTint` back over it.")
            XCTAssertEqual(
                ratio, 3.34, accuracy: 0.05,
                "the resting chip drifted from its designed 3.34:1 in healthy/\(name) (now "
                + "\(String(format: "%.4f", ratio)):1, ink \(measured.ink) on \(measured.background)). Still "
                + "over 3:1 is not sufficient — re-derive the token against issue #949's measurements before "
                + "moving this number, and leave `.armed` (4.73:1) alone: shrinking the rest→armed step is "
                + "how the rejected candidate in #956 failed.")
        }
    }

    func testTheRestingSwapChipTokenShipsAndResolves() throws {
        // Corroboration for the render gate, and the precise error message when the colour set is the thing
        // that broke: a missing/misnamed `SwapChipResting` would otherwise surface as an inscrutable
        // low-contrast render. Deliberately NOT a substitute for the measurement above.
        for surface in Self.surfaces {
            let token = try assetRGB("SwapChipResting", surface.appearance)
            XCTAssertGreaterThanOrEqual(
                contrast(token, surface.base), 3.0,
                "SwapChipResting@\(surface.name) is below the 3:1 non-text bar against the reference base")
        }
    }

    @MainActor
    func testTheRenderedChipContrastGateCanFail() throws {
        // MUTATION, never inspection — the #437 lesson. Repaint the ROI with the exact pre-#956 pair issue
        // #949 measured on a built panel, and watch THE GATE'S OWN predicate reject it. Without this, a
        // green above could equally mean the ROI is mis-pinned onto flat background, which is the failure
        // mode a pixel gate is most prone to.
        for (name, scheme, ink, background) in [
            ("light", ColorScheme.light, RGB(175, 175, 175), RGB(239, 239, 239)),
            ("dark", ColorScheme.dark, RGB(113, 113, 113), RGB(48, 48, 48)),
        ] {
            let raster = try XCTUnwrap(renderedHealthyPanel(scheme))
            let regressed = try XCTUnwrap(renderedChipInk(Self.repaintingChipROI(raster, ink: ink,
                                                                                 background: background)))
            XCTAssertEqual(regressed.ink, ink, "the ROI repaint did not land where the reader looks")
            XCTAssertLessThan(
                contrast(regressed.ink, regressed.background), 3.0,
                "the pre-#956 \(name) chip (\(ink) on \(background), measured at "
                + "\(name == "light" ? "1.91" : "2.70"):1 in issue #949) PASSES the 3:1 predicate — the gate "
                + "cannot fail and proves nothing about the shipped chip.")
        }
    }

    /// A copy of `raster` with the chip ROI repainted as `ink` strokes on `background` — the mutation the
    /// canary drives the gate with. The stroke is a centred vertical band so it is unambiguously the
    /// luminance extreme while `background` stays modal, mirroring how a real glyph sits in the box.
    private static func repaintingChipROI(_ raster: PanelRaster, ink: RGB, background: RGB) -> PanelRaster {
        var bytes = raster.bytes
        let strokes = swapChipColumn.dropFirst(12).prefix(6)
        for band in swapChipRows {
            for y in band {
                for x in swapChipColumn {
                    let c = strokes.contains(x) ? ink : background
                    let o = (y * raster.width + x) * 4
                    bytes[o] = UInt8((c.red * 255).rounded())
                    bytes[o + 1] = UInt8((c.green * 255).rounded())
                    bytes[o + 2] = UInt8((c.blue * 255).rounded())
                    bytes[o + 3] = 255
                }
            }
        }
        return PanelRaster(width: raster.width, height: raster.height, bytes: bytes)
    }

    func testTheGlyphOnlyTintNeverReachesATextSite() {
        // HealthOK sits at 3.08:1 in light — fine for a glyph, BELOW AA for text. The only TINT-ROLE path
        // that could put it on a text run is the blind verdict, whose calm case is `.neutral`, not
        // `.green`. Flipping that one enum (an easy, plausible "make OK look positive" edit) would silently
        // paint sub-AA text, and no other test in this file would notice. So pin it at the source of truth.
        //
        // This is NOT a total guarantee, and the gap is worth naming: converting any of the `healthColor`
        // call sites (Roster :417 / :424 / :433) from `Image` to `Text` would also put HealthOK on text,
        // and nothing here would catch that. Those are structurally glyph sites, so the risk is low — but
        // "low" is the honest word, not "impossible".
        for severity in [StatusPanelFormat.BlindSeverity.ok, .degraded, .cornered] {
            let tint = StatusPanelFormat.blindVerdict(severity, nextSwap: nil, now: 0).tint
            XCTAssertNotEqual(tint, .green,
                              "blindVerdict(\(severity)) resolves to .green → HealthOK (3.08:1 in light) "
                              + "would paint the verdict TEXT at Roster :655, below WCAG AA. Keep OK "
                              + "`.neutral`, or move HealthOK to a 4.5:1-clearing value first.")
        }
    }

    func testTextTintsAreStrictlyDarkerThanTheBrightFillFamilyTheyReplaced() throws {
        // This is the #388 distinction the `StatusPanelRoster` comment (`barColor`) has carried in prose:
        // the small percent TEXT took the darker `--ut-*` tokens while the BAR kept the bright system fills.
        // Encoding it as a relation — text contrast strictly greater than the fill it sits beside — makes
        // the exemption a gate instead of a claim, and it holds regardless of the exact system-colour values
        // (which Apple revises between macOS releases, so pinning their hexes would be brittle).
        let bands: [(StatusPanelFormat.UsageSeverity, NSColor)] = [
            (.green, .systemGreen), (.yellow, .systemOrange), (.red, .systemRed),
        ]
        for (band, fill) in bands {
            guard case .asset(let tokenName) = StatusPanelFormat.usageTextTint(band) else {
                return XCTFail("usageTextTint(\(band)) is no longer an asset token")
            }
            for surface in Self.surfaces {
                let text = try assetRGB(tokenName, surface.appearance)
                let bar = try systemRGB(fill, surface.appearance)
                XCTAssertGreaterThan(contrast(text, surface.base), contrast(bar, surface.base),
                                     "\(band) on \(surface.name): the TEXT tint \(tokenName) "
                                     + "(\(contrast(text, surface.base))) is not higher-contrast than the "
                                     + "bright bar fill (\(contrast(bar, surface.base))) — the two families "
                                     + "collapsed, so #388's darker-text split is gone")
            }
        }
    }

    func testTheMeterBarFillIsCarriedByTheAdjacentPercentTextNotByItsOwnContrast() throws {
        // The issue's AC-2 premise — that the meter-bar fills clear 3:1 — is FALSE when measured, and the
        // MOCK'S OWN `--u-*` values fail the same way, so this is a design property rather than a Swift
        // defect. Against the `--track` neutral the fill sits on (and against the panel base), in LIGHT:
        //
        //     systemGreen  1.61 vs track / 2.08 vs base       (mock --u-g #34c759: 1.61 / 2.08)
        //     systemOrange 1.67 vs track / 2.16 vs base       (mock --u-a #ff9f0a: 1.49 / 1.92)
        //     systemRed    2.59 vs track / 3.34 vs base       (mock --u-r #ff3b30: 2.57 / 3.32)
        //
        // (The Swift bar takes AppKit's system colours, which are NOT byte-identical to the mock's `--u-*`
        // — systemOrange is #FF8D28 vs `--u-a` #ff9f0a, systemRed #FF383C vs `--u-r` #ff3b30. Hence the
        // "≈" in the `barColor` comment. Both families fail 3:1, so the drift changes no verdict here.)
        //
        // Asserting >= 3:1 here would redden the build against the ratified build reference to satisfy a
        // premise that measurement refutes — the #437 failure mode in reverse. Tracked as issue #831.
        //
        // What IS assertable is the compensating control, and it is the reason the bar is defensible under
        // WCAG 1.4.11 at all: the bar is `.accessibilityHidden(true)` (Roster :583) and the exact value sits
        // beside it as TEXT. The bar reinforces a number; it never carries it alone.
        //
        // SCOPE OF THIS TEST, stated precisely because the obvious reading overclaims it. It pins the
        // FORMATTER — that a percent still renders as a percent string — and the tint's contrast. It does
        // NOT pin that the view still draws that string: deleting `Text(StatusPanelFormat.pct(...))` from
        // `UsageMeter` leaves this green. What would catch that is `PanelGoldenParityTests`' raster
        // goldens, which is a different gate in a different file.
        for pct in [UInt8(0), 7, 60, 95, 100] {
            XCTAssertEqual(StatusPanelFormat.pct(pct), "\(pct)%",
                           "the percent value must stay renderable as text beside the bar — it is the bar's "
                           + "non-colour channel (issue #831)")
        }
        // …and that text's tint clears AA. `UtilGreen` is NOT in this list, and the omission is the whole
        // problem rather than a convenience: on the HEALTHY band neither channel clears its bar — the fill
        // is 2.08 (issue #831) and the text is 4.10 (issue #830). Each issue documents one half, so the
        // green band is the one place the compensating-control argument does not actually hold. Add
        // "UtilGreen" here the moment #830 lands; that is the assertion proving the hole closed.
        XCTAssertEqual(try failingCells(["UtilAmber", "UtilRed"], bar: 4.5, resolve: assetRGB), [],
                       "the amber/red percent text no longer clears AA, so the meter bar's compensating "
                       + "control is gone on those bands too (issues #830, #831)")
    }

    // MARK: AC-3 — the severity families stay mutually distinguishable

    func testEverySeverityRoleResolvesToADistinctTokenWithinItsFamily() throws {
        // `testStaleAndAtRiskGlyphTintsStayDistinct` (:133) checks ONE pair. This extends that bar to the
        // whole tint set — within each family, because the two families deliberately SHARE tokens across
        // families (`testWarningTextAndGlyphShareOneTokenSource`, :148), so a cross-family sweep would fail
        // by design.
        // `PanelTint` is Equatable, not Hashable (it names a role, and is never a dictionary key), so the
        // sweep is pairwise — the same shape `testStaleAndAtRiskGlyphTintsStayDistinct` uses for its one pair.
        let glyphRoles: [StatusPanelFormat.HealthTint] = [.green, .yellow, .orange, .red]
        for i in glyphRoles.indices {
            for j in glyphRoles.indices where j > i {
                XCTAssertNotEqual(StatusPanelFormat.healthTint(glyphRoles[i]),
                                  StatusPanelFormat.healthTint(glyphRoles[j]),
                                  "glyph severity roles \(glyphRoles[i]) and \(glyphRoles[j]) collapsed "
                                  + "onto one token")
            }
        }
        let textRoles: [StatusPanelFormat.UsageSeverity] = [.green, .yellow, .red]
        for i in textRoles.indices {
            for j in textRoles.indices where j > i {
                XCTAssertNotEqual(StatusPanelFormat.usageTextTint(textRoles[i]),
                                  StatusPanelFormat.usageTextTint(textRoles[j]),
                                  "percent-text severity bands \(textRoles[i]) and \(textRoles[j]) "
                                  + "collapsed onto one token")
            }
        }

        // Distinct token NAMES could still resolve to one colour (the failure `PanelGoldenParityTests`
        // :290 guards for the asset lookup). Assert the resolved sRGB values are pairwise distinct too, on
        // every surface — a numeric bar, not just an enum-identity one. Routed through the same injected-
        // resolver audit shape AC-1 and AC-2 use, so the canary can drive this sweep by mutation as well.
        let collapsed = try collapsedPairs(Self.nonTextTintAssets, resolve: assetRGB)
        XCTAssertEqual(collapsed, [],
                       "tint tokens that resolved to the SAME colour on some surface: \(collapsed.sorted())")
    }

    func testSeverityPairsThatCollapseUnderDeuteranopiaAreSeparatedByShape() throws {
        // The governing principle from the (locked) brand record is WCAG 1.4.1: colour is never the SOLE
        // differentiator; state is carried by SHAPE with colour redundant on top. The art-direction record
        // flags a green↔red colour-blind risk specifically, so this asserts the redundancy where it is
        // actually needed rather than trusting the hue numbers.
        //
        // MEASURED (Viénot-Brettel-Mollon 1999 deuteranopia simulation, max-channel separation of the
        // simulated colours, quoted on a 0-255 scale) — the warm family collapses almost completely in
        // LIGHT. Six of the ten state pairs land under the 12/255 threshold and therefore execute the
        // inner assertion:
        //
        //     stale↔atRisk 5.65   stale↔degraded 5.65   stale↔dead 1.20
        //     atRisk↔degraded 0.00   atRisk↔dead 4.45   degraded↔dead 4.45
        //
        // So hue does essentially NO work separating stale / at-risk / dead for a deuteranope. That is
        // acceptable ONLY because every one of those states carries a distinct SF Symbol — and this test is
        // what keeps that true, driven by the measured collapse rather than by a standing assumption.
        //
        // The test cannot go vacuously green: atRisk and degraded share ONE token (`UtilOrange`, the #427
        // decision that degraded is a recoverable warning distinguished by SHAPE), so their separation is
        // exactly 0 by construction on every platform and the assertion always fires. Note also that the
        // simulation must run in LINEAR light — the widespread gamma-encoded variants of this matrix report
        // stale↔dead as ≈29 rather than 1.20, which would drop every pair above the threshold and quietly
        // turn this whole test into a no-op.
        let states: [CredentialHealth] = [.healthy, .stale, .atRisk, .degraded, .dead]
        for surface in Self.surfaces {
            var simulated: [(state: CredentialHealth, symbol: String, colour: RGB)] = []
            for state in states {
                let symbol = StatusPanelFormat.healthSymbol(state)
                guard case .asset(let token) = StatusPanelFormat.healthTint(symbol.tint) else { continue }
                simulated.append((state, symbol.name, deuteranope(try assetRGB(token, surface.appearance))))
            }
            for i in simulated.indices {
                for j in simulated.indices where j > i {
                    let separation = channelDistance(simulated[i].colour, simulated[j].colour)
                    guard separation <= Self.deuteranopeCollapse else { continue }
                    XCTAssertNotEqual(simulated[i].symbol, simulated[j].symbol,
                                      "\(simulated[i].state) and \(simulated[j].state) are indistinguishable "
                                      + "by colour under deuteranopia on \(surface.name) AND share the "
                                      + "symbol '\(simulated[i].symbol)' — colour is then the sole "
                                      + "differentiator, which WCAG 1.4.1 forbids")
                }
            }
        }
    }

    // MARK: AC-4 — CONSTRAINT-A canary: the gate PROVES it can fail

    func testTheContrastGateCanFail() throws {
        // The gate above is only evidence if it can go red. Proven by MUTATION through the SAME `clearsBar`
        // predicate and the SAME `failingCells` audit the real assertions use — never by inspecting the
        // arithmetic. (The #437 precedent: three real render bugs were misread five times as a DESIGN
        // failure, and a golden authored then would have defended them. `BarGlyphParityTests`
        // :250 is this repo's working pattern; this is its colour-space analogue.)

        // 1. A real token, unmutated, clears its bar — so a green canary is not vacuous.
        let amber = try assetRGB("UtilAmber", .aqua)
        XCTAssertTrue(clearsBar(amber, on: Self.lightBase, bar: 4.5),
                      "UtilAmber no longer clears AA unmutated — the canary's control is broken")

        // 2. Mutate that SAME token 90 % of the way toward the background it is measured against. Nothing
        //    else changes: same asset, same surface, same predicate.
        let washedOut = blend(amber, toward: Self.lightBase, 0.9)
        XCTAssertFalse(clearsBar(washedOut, on: Self.lightBase, bar: 4.5),
                       "a token washed 90 % into the panel base still cleared 4.5:1 — the text bar cannot "
                       + "fail, so it is not evidence")
        XCTAssertFalse(clearsBar(washedOut, on: Self.lightBase, bar: 3.0),
                       "the same washed-out token cleared even the 3:1 non-text bar — neither bar can fail")

        // 3. Drive it through the real audit: injecting the mutation as the resolver must make the cell
        //    surface in exactly the place AC-1 and AC-2 read their verdicts from.
        let mutatingResolve: (String, NSAppearance.Name) throws -> RGB = { asset, appearance in
            let real = try self.assetRGB(asset, appearance)
            return asset == "UtilAmber" ? blend(real, toward: Self.lightBase, 0.9) : real
        }
        let textFailures = try failingCells(Self.textTintAssets, bar: 4.5, resolve: mutatingResolve)
        XCTAssertTrue(textFailures.contains("UtilAmber@light"),
                      "the mutated token did not surface in the AC-1 audit — the audit is blind to it, so a "
                      + "real regression would pass too. Saw: \(textFailures.sorted())")
        let glyphFailures = try failingCells(Self.nonTextTintAssets, bar: 3.0, resolve: mutatingResolve)
        XCTAssertTrue(glyphFailures.contains("UtilAmber@light"),
                      "the mutated token did not surface in the AC-2 non-text audit. Saw: \(glyphFailures.sorted())")

        // 4. The distinctness bar (AC-3) must be able to fail too, and that needs its own mutation —
        //    comparing a colour with ITSELF would be 0 by construction and would prove nothing about the
        //    sweep. Force UtilRed to resolve to UtilAmber's value and require `collapsedPairs` — the audit
        //    AC-3 reads its verdict from — to report the pair.
        let red = try assetRGB("UtilRed", .aqua)
        XCTAssertGreaterThan(channelDistance(amber, red), 0,
                             "UtilAmber and UtilRed are already identical — the canary's control is broken")
        let collapsingResolve: (String, NSAppearance.Name) throws -> RGB = { asset, appearance in
            try self.assetRGB(asset == "UtilRed" ? "UtilAmber" : asset, appearance)
        }
        let collapsed = try collapsedPairs(Self.nonTextTintAssets, resolve: collapsingResolve)
        XCTAssertTrue(collapsed.contains("UtilAmber~UtilRed@light"),
                      "two tokens forced onto one colour did not surface in the AC-3 distinctness sweep, so "
                      + "testEverySeverityRoleResolvesToADistinctTokenWithinItsFamily cannot fail. Saw: "
                      + "\(collapsed.sorted())")

        // 5. And the deuteranopia bar (AC-3's second half): a pair that collapses under simulation must be
        //    detected as collapsing. `UtilOrange` and `UtilRed` are visibly different in sRGB yet land
        //    4.45/255 apart once simulated — under `deuteranopeCollapse`, so the shape assertion fires.
        let orange = try assetRGB("UtilOrange", .aqua)
        XCTAssertGreaterThan(channelDistance(orange, red), Self.deuteranopeCollapse,
                             "UtilOrange and UtilRed are NOT distinguishable in plain sRGB — the "
                             + "deuteranopia canary has nothing to demonstrate")
        XCTAssertLessThanOrEqual(channelDistance(deuteranope(orange), deuteranope(red)), Self.deuteranopeCollapse,
                                 "UtilOrange and UtilRed no longer collapse under deuteranopia, so the "
                                 + "collapse-detection branch may never execute — re-check that "
                                 + "testSeverityPairsThatCollapseUnderDeuteranopiaAreSeparatedByShape "
                                 + "still asserts anything")
    }

    // MARK: - Helpers

    /// A shipped asset colour set, resolved through the REAL panel seam (`Color.panelAssets`, the bundle
    /// fix #754 made) under `appearance`, as sRGB components. Resolution happens INSIDE
    /// `performAsCurrentDrawingAppearance` because an asset colour is dynamic — read outside it, every
    /// surface would silently return the host process's appearance and the four-surface sweep would be
    /// four copies of one measurement (a degenerate pass).
    private func assetRGB(_ name: String, _ appearance: NSAppearance.Name) throws -> RGB {
        let bundle = Color.panelAssets
        let dynamic = try XCTUnwrap(NSColor(named: name, bundle: bundle),
                                    "colour set \(name) did not resolve from "
                                    + "\(bundle.bundleURL.lastPathComponent) — the compiled Assets.xcassets "
                                    + "is missing from the MenubarTests bundle (project.yml)")
        return try resolve(dynamic, appearance, label: name)
    }

    /// An AppKit system colour under `appearance` — the bright `--u-*`-family fills the bar keeps (#388).
    private func systemRGB(_ colour: NSColor, _ appearance: NSAppearance.Name) throws -> RGB {
        try resolve(colour, appearance, label: "\(colour)")
    }

    private func resolve(_ colour: NSColor, _ appearance: NSAppearance.Name, label: String) throws -> RGB {
        var resolved: NSColor?
        try XCTUnwrap(NSAppearance(named: appearance), "appearance \(appearance.rawValue) is unavailable")
            .performAsCurrentDrawingAppearance { resolved = colour.usingColorSpace(.sRGB) }
        let srgb = try XCTUnwrap(resolved, "\(label) did not convert to sRGB under \(appearance.rawValue)")
        return RGB(srgb.redComponent, srgb.greenComponent, srgb.blueComponent)
    }

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

    /// The badge fill for palette slot `index` — found via the REAL public API by probing for a label that
    /// hashes to that slot (so the test exercises `accountColorIndex` + `accountBadgeFill`, not a private peek).
    private func paletteFill(_ index: Int, dark: Bool) -> StatusPanelFormat.FillRGBA {
        StatusPanelFormat.accountBadgeFill(for: probeLabel(mappingTo: index), dark: dark)
    }

    /// A short label whose color hash lands on `index` — a deterministic search over the FNV-1a mapping.
    private func probeLabel(mappingTo index: Int) -> String {
        for n in 0..<100_000 {
            let candidate = "probe\(n)"
            if StatusPanelFormat.accountColorIndex(for: candidate) == index { return candidate }
        }
        XCTFail("no probe label mapped to palette slot \(index)")
        return ""
    }

    private static let allNonConnectedStates: [ConnectionState] = [
        .connecting, .emptyRoster, .stale, .disconnected(reason: "EOF"), .unsupported, .crashLooping,
        .starting, .notRunning,   // #499
        .reconnecting(reason: "EOF"),   // #526: the transient warm drop must never read healthy either
    ]

    /// A DEAD account that is mid-recovery (#109) — the current daemon's `snapshotAwaitingDead` golden
    /// has `recovering:false`, so this hand-built frame is the only way to exercise the recovering
    /// branch through the real decoder. Same contract, `recovering:true`.
    private static let snapshotDeadRecovering = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"accounts":[{"label":"heal","active":false,"enabled":true,"quarantined":true,"recovering":true,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"dead"}],"next_swap":null,"refresh_enabled":false}
    """#
}

// MARK: - #445 palette test helpers: WCAG contrast + hue over sRGB
//
// Pure color math for the palette assertions — the standard WCAG 2.x relative-luminance / contrast-ratio and
// an HSV hue, over sRGB. Kept in the test target (not shipped) so `StatusPanelFormat` stays a plain color
// vocabulary; the assertions do the verification. The palette fills are opaque (alpha 1), so a fill's own
// color IS its rendered color — no compositing needed here.

private struct RGB: Equatable {
    let red, green, blue: Double
    init(_ r: Int, _ g: Int, _ b: Int) {
        red = Double(r) / 255; green = Double(g) / 255; blue = Double(b) / 255
    }
    /// Raw sRGB components already in 0…1 (e.g. a composited result).
    init(_ r: Double, _ g: Double, _ b: Double) { red = r; green = g; blue = b }
    init(_ c: StatusPanelFormat.FillRGBA) { red = c.red; green = c.green; blue = c.blue }
}

private func srgbToLinear(_ c: Double) -> Double {
    c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
}

private func relativeLuminance(_ c: RGB) -> Double {
    0.2126 * srgbToLinear(c.red) + 0.7152 * srgbToLinear(c.green) + 0.0722 * srgbToLinear(c.blue)
}

private func contrast(_ a: RGB, _ b: RGB) -> Double {
    let hi = max(relativeLuminance(a), relativeLuminance(b))
    let lo = min(relativeLuminance(a), relativeLuminance(b))
    return (hi + 0.05) / (lo + 0.05)
}

private func contrast(_ a: StatusPanelFormat.FillRGBA, _ b: RGB) -> Double { contrast(RGB(a), b) }
private func contrast(_ a: StatusPanelFormat.FillRGBA, _ b: StatusPanelFormat.FillRGBA) -> Double {
    contrast(RGB(a), RGB(b))
}

// MARK: - #759 tint-contrast helpers: channel distance, alpha blend, deuteranopia simulation

/// The largest per-channel difference between two colors, in 0…1. The separation metric for "did these two
/// tokens collapse onto one color" — deliberately a max-channel delta, the same shape as the drift metric
/// `BarGlyphRenderer.diffFraction` / `PanelRaster.diffFraction` threshold on, so the numbers read alike.
private func channelDistance(_ a: RGB, _ b: RGB) -> Double {
    max(abs(a.red - b.red), max(abs(a.green - b.green), abs(a.blue - b.blue)))
}

/// `color` composited `amount` (0…1) of the way toward `background` — source-over with `1 - amount` alpha.
/// The canary's mutation: it degrades a REAL token along the one axis contrast measures, so the perturbed
/// value is a plausible token rather than an arbitrary constant.
private func blend(_ color: RGB, toward background: RGB, _ amount: Double) -> RGB {
    RGB(color.red + (background.red - color.red) * amount,
        color.green + (background.green - color.green) * amount,
        color.blue + (background.blue - color.blue) * amount)
}

/// A deuteranope's view of `color` — Viénot, Brettel & Mollon (1999): convert to LMS, discard the M cone by
/// reconstructing it from L and S, convert back. Operates in LINEAR light (the same `srgbToLinear` the
/// contrast math uses), because the cone transform is only valid on linear intensities.
///
/// Used to answer one question with a measurement instead of an assumption: for a viewer with the most
/// common form of colour-blindness, do two severity tints still differ? For this palette the warm family
/// does NOT (see `testSeverityPairsThatCollapseUnderDeuteranopiaAreSeparatedByShape`), which is precisely
/// why the shape channel is load-bearing rather than decorative.
private func deuteranope(_ color: RGB) -> RGB {
    let r = srgbToLinear(color.red), g = srgbToLinear(color.green), b = srgbToLinear(color.blue)
    let l = 17.8824 * r + 43.5161 * g + 4.11935 * b
    let m = 3.45565 * r + 27.1554 * g + 3.86714 * b
    let s = 0.0299566 * r + 0.184309 * g + 1.46709 * b
    let simulatedM = 0.494207 * l + 1.24827 * s        // the deuteranope's reconstructed M response
    let outR = 0.0809444479 * l - 0.130504409 * simulatedM + 0.116721066 * s
    let outG = -0.0102485335 * l + 0.0540193266 * simulatedM - 0.113614708 * s
    let outB = -0.000365296938 * l - 0.00412161469 * simulatedM + 0.693511405 * s
    return RGB(linearToSrgb(outR), linearToSrgb(outG), linearToSrgb(outB))
}

private func linearToSrgb(_ c: Double) -> Double {
    let clamped = min(max(c, 0), 1)
    return clamped <= 0.0031308 ? 12.92 * clamped : 1.055 * pow(clamped, 1 / 2.4) - 0.055
}

/// The HSV hue in degrees (0…360); 0 for an achromatic color (never expected in the palette).
private func hue(_ c: RGB) -> Double {
    let maxComponent = max(c.red, c.green, c.blue)
    let minComponent = min(c.red, c.green, c.blue)
    let delta = maxComponent - minComponent
    guard delta > 0 else { return 0 }
    var h: Double
    if maxComponent == c.red {
        h = (c.green - c.blue) / delta
    } else if maxComponent == c.green {
        h = 2 + (c.blue - c.red) / delta
    } else {
        h = 4 + (c.red - c.green) / delta
    }
    h *= 60
    return h < 0 ? h + 360 : h
}

private func hue(_ c: StatusPanelFormat.FillRGBA) -> Double { hue(RGB(c)) }
