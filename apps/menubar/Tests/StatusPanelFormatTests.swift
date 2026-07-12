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

    // MARK: - Active-row tag (issue #501 — neutral sentence-case capsule, mock `.tag`)

    func testActiveTagLabelIsNeutralSentenceCase() {
        // The active tag is a calm SENTENCE-CASE capsule (mock `.tag`), NOT the old letter-spaced accent
        // uppercase pill. Guard the exact label so a regression back to "ACTIVE" — which read as an accent
        // web badge and re-inflated the active over-signalling #387 M5 reduced — fails loudly.
        XCTAssertEqual(StatusPanelFormat.activeTagLabel, "Active")
        XCTAssertNotEqual(StatusPanelFormat.activeTagLabel, "ACTIVE")
        // Sentence-case: leading capital, remainder lowercase — never all-caps.
        XCTAssertNotEqual(StatusPanelFormat.activeTagLabel, StatusPanelFormat.activeTagLabel.uppercased(),
                          "the tag must be sentence-case, not letter-spaced uppercase (mock `.tag`)")
        XCTAssertEqual(StatusPanelFormat.activeTagLabel.prefix(1), "A")
        XCTAssertEqual(String(StatusPanelFormat.activeTagLabel.dropFirst()),
                       StatusPanelFormat.activeTagLabel.dropFirst().lowercased())
    }

    func testActiveTagLabelClearsContrastOnTheBadgeCapsuleBothThemes() {
        // The tag reuses the neutral `.badge` fill (`neutralFill(.badge, …)`) + `--text-2` text; on the
        // capsule the label must clear the WCAG 1.4.11 3:1 floor (the tag is a DECORATIVE non-text
        // redundancy cue — `accessibilityHidden`, the row's spoken label already carries ", active"). This
        // reproduces the mock's design-token math over a representative opaque chrome base and guards the
        // tokens from drifting below the floor; the mock claims ~3.9:1 light / ~3.8:1 dark. Reuses the
        // shared `RGB` / `contrast` WCAG helpers below; only the capsule's translucent compositing is local.

        // LIGHT — `--text-2` rgba(60,60,67,.72) on the `.badge` capsule over #f5f5f7 (the mock's stated
        // opaque light base, `menubar-preview.html:155`). Observed ≈4.1:1.
        let lightText2 = StatusPanelFormat.FillRGBA(red: 60/255, green: 60/255, blue: 67/255, alpha: 0.72)
        let lightCapsule = composite(StatusPanelFormat.neutralFill(.badge, dark: false), over: RGB(245, 245, 247))
        let lightRatio = contrast(composite(lightText2, over: lightCapsule), lightCapsule)
        XCTAssertGreaterThanOrEqual(lightRatio, 3.0,
                                    "light tag label must clear WCAG 1.4.11 3:1 (observed ≈4.1:1)")

        // DARK — `--text-2` rgba(235,235,245,.6) on the `.badge` capsule over #3a3a3c (the standard macOS
        // dark control tone the mock's ~3.8:1 claim corresponds to). Observed ≈3.7:1.
        let darkText2 = StatusPanelFormat.FillRGBA(red: 235/255, green: 235/255, blue: 245/255, alpha: 0.6)
        let darkCapsule = composite(StatusPanelFormat.neutralFill(.badge, dark: true), over: RGB(58, 58, 60))
        let darkRatio = contrast(composite(darkText2, over: darkCapsule), darkCapsule)
        XCTAssertGreaterThanOrEqual(darkRatio, 3.0,
                                    "dark tag label must clear WCAG 1.4.11 3:1 (observed ≈3.7:1)")
    }

    /// Source-over composite of a translucent `FillRGBA` over an opaque `RGB` base → the opaque rendered
    /// colour. The shared palette helpers assume opaque fills; the #501 tag capsule (and its `--text-2`
    /// label over it) is translucent, so flatten each layer before measuring contrast.
    private func composite(_ top: StatusPanelFormat.FillRGBA, over base: RGB) -> RGB {
        let a = top.alpha
        return RGB(a * top.red   + (1 - a) * base.red,
                   a * top.green + (1 - a) * base.green,
                   a * top.blue  + (1 - a) * base.blue)
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
        let lightBase = RGB(247, 247, 250)
        let darkBase = RGB(38, 38, 43)
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

private struct RGB {
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
