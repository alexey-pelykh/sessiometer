// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Pure presentation formatting for the menu-bar status panel (issue #326): every text/glyph the
// SwiftUI panel renders, extracted as PURE functions over the store's already-decoded view state so
// they mirror the `status` verb's renderers in `src/cli.rs` and are unit-testable WITHOUT launching
// the app — exactly the pure-core / thin-shell split `HonestStateMachine` + `WatchStatusStore` use.
// `StatusPanelView` (the SwiftUI layer) is a thin consumer of these; the app never renders a number
// this file did not format, so the parity tests in `StatusPanelFormatTests` gate the whole panel.
//
// Source of truth mirrored — do NOT re-derive (grep the symbols, they move):
//   * `src/cli.rs` `health_glyph`      → `healthGlyph`      (the 5+1-state emoji rollup)
//   * `src/cli.rs` `health_cell`       → `authCell`         (glyph + `claude /login` / `recovering` cue + `disabled`)
//   * `src/cli.rs` `legacy_health_tags`→ `legacyHealthTags` (the pre-#119 auth-nil fallback)
//   * `src/cli.rs` `reset_cell`        → `resetCell`        (one window's "resets in", or `n/a`)
//   * `src/cli.rs` `humanize_until`    → `humanizeUntil`    (two-largest-unit compact duration)
//   * `src/cli.rs` `pct`               → `pct`              (`N%` or `n/a`, never a fabricated 0)
//
// The panel's SINGLE per-row reset-in pick (weekly-exhausted → weekly, else session), the honest-state
// banners, and the `next_swap` footer wording are #326's OWN panel spec (the issue AC), not a
// `src/cli.rs` mirror — the CLI prints both reset columns and phrases the footer differently.

import Foundation

/// Pure formatting for the status panel. A caseless namespace of `static` functions — no state, no
/// I/O, no clock (the caller passes `now`), so every output is a deterministic function of its inputs.
enum StatusPanelFormat {

    /// The `sessiometer capture` CLI subcommand — retained as the equivalent terminal command an operator
    /// may prefer (and the parity anchor for `StatusPanelFormatTests`). As of #360 the panel's PRIMARY
    /// capture path is the in-app "Capture active account" affordance (a real daemon-routed action over the
    /// #358 transport), NOT a clipboard copy of this string — see the capture-affordance copy below.
    static let captureCommand = "sessiometer capture"

    // MARK: - Capture affordance copy (issue #360 — the in-app capture states)

    /// The pending label. Capture is now a REAL daemon-routed action (#360: command → daemon → redacted
    /// ack), so a pending state is HONEST — unlike the superseded copy-command, which never ran and so had
    /// no honest in-flight state (design-menubar's old "no fake spinner" scoped only the never-running
    /// copy-command; a real action earns a real pending).
    static let capturePendingText = "Capturing…"

    /// The success confirmation — "Captured '<label>'" under the label the daemon actually ASSIGNED (the
    /// UUID-derived handle when the operator left the field blank), echoed from the redacted ack so the
    /// operator sees the real handle, never a fabricated one. Curly quotes match the panel's typography.
    static func captureDoneText(label: String) -> String {
        "Captured \u{2018}\(label)\u{2019}"
    }

    /// Human copy for a capture failure — the redacted machine verdict mapped to ONE operator-facing
    /// sentence (never the raw kebab tag or transport jargon), actionable where there is an action. Pure:
    /// a deterministic function of the non-secret `CaptureFailure`, unit-tested in isolation.
    static func captureErrorText(_ failure: CaptureFailure) -> String {
        switch failure {
        case .rejected(let reason):
            switch reason {
            case .noActiveAccount: return "No active account — run claude /login, then capture."
            case .keychainLocked:  return "Keychain is locked — unlock it, then try again."
            case .swapLockBusy:    return "The daemon is busy — try again in a moment."
            case .failed:          return "Capture failed — try again."
            }
        case .daemonError(let reason):
            // The same-user local peer should never be unauthorized; surface it plainly if it ever happens.
            return reason == "unauthorized" ? "Not authorized to capture." : "Capture failed — try again."
        case .transport(let error):
            switch error {
            case .connectionRefused: return "The daemon isn’t running."
            case .timedOut:          return "The daemon didn’t respond — try again."
            case .closedBeforeAck:   return "The daemon closed the connection — try again."
            case .encodeFailed, .io: return "Capture failed — try again."
            }
        case .undecodable:
            return "Unexpected reply from the daemon."
        case .unavailable:
            return "The daemon socket is unreachable."
        }
    }

    // MARK: - Manual switch affordance (issue #169 — the per-row swap-on-click)

    /// Why a roster row cannot be manually switched to. These are exactly the CLIENT-VISIBLE subset of
    /// the daemon's OWN non-`force` policy gates (`swap_command_verdict`, `src/daemon.rs`), in the
    /// daemon's own order — so a row the panel disables is a row the daemon would refuse.
    ///
    /// The daemon's THIRD gate, `cooldown`, is deliberately absent: the post-swap cooldown is in-memory
    /// daemon state and never rides the wire, so the client cannot know it. A row the panel shows as
    /// viable can therefore still come back `rejected(.cooldown)` — that refusal is rendered inline
    /// (`swapErrorText`). This asymmetry is the honest design: the panel disables ONLY what the wire
    /// proves, and never sends a viability hint (the daemon re-validates every target regardless).
    ///
    /// `enabled` is NOT a gate: `swap_command_verdict` does not read it. A parked account (issue #36) is
    /// out of the AUTO rotation, not un-switchable — the CLI's `use <account>` reaches it too.
    enum SwitchBlock: Equatable {
        /// The credential is quarantined (issue #42) — its access token was rejected, so the
        /// daemon refuses without `force`. NOT proven dead: a `sessiometer poke` may refresh
        /// it (issue #427).
        case quarantined
        /// The weekly window is exhausted (issue #11/#37) — the daemon refuses without `force`.
        case weeklyExhausted
    }

    /// The wire-visible block on manually switching to a row, or `nil` when the row is viable as far as
    /// the wire can say. Mirrors `swap_command_verdict`'s gate ORDER (quarantined before weekly), so the
    /// reason the panel shows is the reason the daemon would give.
    static func switchBlock(quarantined: Bool, weeklyExhausted: Bool) -> SwitchBlock? {
        if quarantined { return .quarantined }
        if weeklyExhausted { return .weeklyExhausted }
        return nil
    }

    /// A roster row's manual-switch state (issue #169), as a pure verdict the panel's `RosterView` maps to
    /// its affordance:
    ///   * `notATarget` — the ACTIVE row (a disabled button reads as "broken", so it stays a plain
    ///     display row).
    ///   * `available` — a viable switch target: an enabled button carrying a persistent, quiet swap chip
    ///     (visible at rest, brightening on hover — #448).
    ///   * `blocked(reason)` — a wire-visibly non-viable target: a disabled button carrying its reason.
    ///
    /// `isEnabled` is accepted and DELIBERATELY IGNORED — pinned as a parameter (rather than simply not
    /// consulted) so the "a parked account is still switchable" invariant is TESTABLE: a caller passing
    /// `isEnabled: false` on an otherwise-viable row must still get `.available`. This mirrors the daemon:
    /// `swap_command_verdict` (`src/daemon.rs`) takes no `enabled` input, so a parked account (issue #36,
    /// out of the AUTO rotation) is reachable by a manual `use <account>` / panel switch. If a future edit
    /// ever gates on `enabled` here, the parity test breaks loudly.
    static func rowSwitchState(
        isActive: Bool,
        isQuarantined: Bool,
        weeklyExhausted: Bool,
        isEnabled: Bool
    ) -> RowSwitchState {
        _ = isEnabled   // intentionally not a gate — see the daemon-parity note above.
        if isActive { return .notATarget }
        if let block = switchBlock(quarantined: isQuarantined, weeklyExhausted: weeklyExhausted) {
            return .blocked(block)
        }
        return .available
    }

    /// The pure verdict `rowSwitchState` returns — the panel's `RosterView` renders each case.
    enum RowSwitchState: Equatable {
        case notATarget
        case available
        case blocked(SwitchBlock)
    }

    /// Why a non-viable row cannot be switched to — shown as its hover tooltip and spoken by VoiceOver
    /// (a `dimmed` trait alone never tells the operator WHY).
    static func switchBlockedText(_ block: SwitchBlock) -> String {
        switch block {
        case .quarantined:     return "Can’t switch — credential is quarantined. Run sessiometer poke to refresh it."
        case .weeklyExhausted: return "Can’t switch — weekly limit reached."
        }
    }

    /// The viable row's (and the footer Swap button's) hover tooltip / accessibility hint.
    static func switchHelpText(label: String) -> String {
        "Switch to \(label)"
    }

    /// A row's spoken label, plus — for a non-viable switch target — the reason it is disabled.
    static func rowSwitchAccessibilityLabel(base: String, block: SwitchBlock?) -> String {
        guard let block else { return base }
        return "\(base). \(switchBlockedText(block))"
    }

    // MARK: - Switch-affordance layout budget (issue #169 watch-out: never truncate to something uninformative)

    /// The trailing swap-chip slot's own width in points — wide enough for the swap glyph and for the small
    /// `ProgressView` that replaces it while the swap is in flight. This EXCLUDES the row `HStack`'s 9 pt
    /// spacing that precedes it, so the slot's total trailing cost is `switchAffordanceSlotWidth + 9`.
    ///
    /// #448 widened this 18 → 28: the chip is no longer hover-REVEALED but PERSISTENT — a quiet, low-emphasis
    /// mark shown at rest on every switch target so a first-time operator sees the row is actionable on a
    /// transient popover — so the slot now carries a visible glyph in the steady state and earns a little
    /// more room to sit comfortably (still far under the row's spare width; see `switchAffordanceMinRowWidth`).
    ///
    /// The slot is laid out on EVERY roster row — empty on the active row, the quiet chip at rest on the
    /// others. Two consequences, both load-bearing: the auth column stays aligned across active and
    /// non-active rows, and, decisively, NEITHER the chip's resting presence NOR its hover-brighten can
    /// REFLOW the row (the slot width is identical hidden / resting / armed). The label's available width is
    /// constant, so its truncation is too.
    static let switchAffordanceSlotWidth: Double = 28

    /// The minimum row width, in points, at which the manual-switch affordance is offered at all.
    ///
    /// Derived from the row's fixed columns at their tightest: 16 (row insets) + 8 (status dot) + 9 +
    /// 30 (monogram) + 9 + 64 (a label floor worth reading) + 6 (min spacer) + 60 (auth glyph + its
    /// longest cue) + 37 (the #448-widened 28 pt slot plus its 9 pt spacing) ≈ 239 — kept at the round 240
    /// floor (the shipped `defaultRowWidth` ≈ 364 clears it with ~125 pt to spare, so the +10 slot bump does
    /// not press it). Below this, the affordance is not merely hidden — the row is not interactive AT ALL, so
    /// a too-narrow row can never degrade into an invisible whole-row hot-zone (the mis-click hazard the
    /// arm-on-hover guard exists to prevent: the chip is quiet and cursor-less at rest, armed only on hover).
    static let switchAffordanceMinRowWidth: Double = 240

    /// Whether a row of `rowWidth` points can host the manual-switch affordance without squeezing the
    /// label into an uninformative truncation. The panel is fixed-width today, so its caller derives
    /// `rowWidth` from `defaultRowWidth` rather than measuring — see `StatusPanelView`.
    static func rowFitsSwitchAffordance(rowWidth: Double) -> Bool {
        rowWidth >= switchAffordanceMinRowWidth
    }

    /// The panel's fixed content width in points — the source of truth for the `.frame(width:)` the SwiftUI
    /// `StatusPanelView` pins, kept HERE (in the testable, Foundation-only layer) alongside the width gate
    /// it feeds so a test can assert the shipped geometry clears `switchAffordanceMinRowWidth`.
    static let panelContentWidth: Double = 380

    /// The roster's horizontal inset per side — each row sits inside it, so a row is this much narrower
    /// than the panel on each edge.
    static let rosterHorizontalInset: Double = 8

    /// The width available to one roster row on the shipped fixed-width panel.
    static var defaultRowWidth: Double { panelContentWidth - 2 * rosterHorizontalInset }

    // MARK: - Swap-chip emphasis (issue #448 — persistent-quiet, brightens when armed)

    /// The per-row swap chip's emphasis level. #169 revealed the trailing swap glyph ONLY on hover, so on a
    /// transient popover a first-time operator never saw a row was actionable. #448 makes it PERSISTENT: a
    /// quiet, low-emphasis mark shown AT REST on every switch target, that BRIGHTENS when the row is armed
    /// (hover / focus). The view maps each level to a neutral SYSTEM tint — `.resting` → `.tertiary`
    /// (≈ the mock's `--text-3` decorative token), `.armed` → `.secondary` (≈ `--text-2`) — a SEMANTIC tint
    /// step, never a hardcoded opacity (the #388 "tints/opacities live in the testable layer" discipline).
    /// Neutral at every level, never `.tint`: the one accent action is the footer Swap (Von Restorff).
    enum SwitchChipEmphasis: Equatable {
        /// No chip — the active row / a dropped connection (the row is not a switch target), left pure data.
        case hidden
        /// Visible but quiet — the steady state on a switch target (viable OR wire-blocked; the glyph SHAPE,
        /// arrow vs `nosign`, carries the block, not the emphasis).
        case resting
        /// Brightened — the row is armed (hovered / focused), inviting the press.
        case armed
    }

    /// The chip emphasis for a row (issue #448). Kept HERE (not decided inline in the view) so the
    /// resting-visible-vs-armed-brighten distinction is unit-asserted against the design intent rather than
    /// buried in SwiftUI. `offersSwitch` is the view's own gate (a non-active row that fits the width);
    /// `armed` is whether the row is currently hovered/focused. A non-target row is `.hidden`; a switch
    /// target is `.resting` at rest and `.armed` once armed — the persistent-quiet → brighten behavior.
    static func switchChipEmphasis(offersSwitch: Bool, armed: Bool) -> SwitchChipEmphasis {
        guard offersSwitch else { return .hidden }
        return armed ? .armed : .resting
    }

    // MARK: - Swap phase copy (issue #169 — the in-flight / settled swap states)

    /// The in-flight label, shown on the clicked row (or the footer Swap button) while the daemon runs
    /// the swap. A swap is a REAL daemon-routed write, so a pending state is honest.
    static let swapPendingText = "Switching…"

    /// The success confirmation, named from the redacted ack's OWN labels — never a client guess about
    /// what the daemon did. A no-op (`already_active`) says so plainly rather than claiming a switch.
    static func swapDoneText(_ success: SwapSuccess) -> String {
        switch success {
        case .swapped(let from, let to): return "Switched \(from) → \(to)"
        case .alreadyActive(let to):     return "\(to) is already active"
        }
    }

    /// Human copy for a failed swap — the redacted machine verdict mapped to ONE operator-facing
    /// sentence (never the raw kebab tag or transport jargon), actionable where there is an action. Pure:
    /// a deterministic function of the non-secret `SwapFailure`, unit-tested in isolation.
    ///
    /// The two AMBIGUOUS transport outcomes — a timeout and an EOF before the ack — deliberately do NOT
    /// say "the switch failed": the daemon writes the ack only AFTER the swap runs, so a lost ack means
    /// the swap may well have COMMITTED. Claiming failure there would be a false negative; the copy sends
    /// the operator to the roster (which the next `watch` snapshot settles authoritatively) instead.
    static func swapErrorText(_ failure: SwapFailure) -> String {
        switch failure {
        case .rejected(let reason):
            switch reason {
            case .unknownTarget:    return "That account is no longer in the roster."
            case .ambiguousTarget:  return "Two accounts share that label — rename one, then switch."
            case .quarantined:      return "Credential is quarantined — run sessiometer poke to refresh, then switch."
            case .weeklyExhausted:  return "Weekly limit reached — that account can’t take the session yet."
            case .cooldown:         return "Swapped too recently — try again in a moment."
            case .noActiveAccount:  return "No active account to switch away from."
            case .keychainLocked:   return "Keychain is locked — unlock it, then try again."
            case .swapLockBusy:     return "The daemon is busy — try again in a moment."
            case .failed:           return "Switch failed — try again."
            }
        case .daemonError(let reason):
            // The same-user local peer should never be unauthorized; surface it plainly if it ever happens.
            return reason == "unauthorized" ? "Not authorized to switch accounts." : "Switch failed — try again."
        case .transport(let error):
            switch error {
            case .connectionRefused: return "The daemon isn’t running."
            case .timedOut:          return "The daemon didn’t answer — check the roster before retrying."
            case .closedBeforeAck:   return "The daemon closed the connection — check the roster before retrying."
            case .encodeFailed, .io: return "Switch failed — try again."
            }
        case .undecodable:
            return "Unexpected reply from the daemon."
        case .unavailable:
            return "The daemon socket is unreachable."
        }
    }

    // MARK: - Percentage cell (mirror `src/cli.rs` `pct`)

    /// A `0...100` percent as `N%`, or `n/a` when the last poll failed — never a fabricated `0`
    /// (mirrors `src/cli.rs` `pct`).
    static func pct(_ percent: UInt8?) -> String {
        percent.map { "\($0)%" } ?? "n/a"
    }

    // MARK: - Reset-in cell (mirror `src/cli.rs` `humanize_until` / `reset_cell`)

    /// A whole-second remaining time as a compact "resets in" — the two largest non-zero units, e.g.
    /// `12m` / `4h` / `3d4h` — mirroring `src/cli.rs` `humanize_until` EXACTLY: a reset already reached
    /// (`<= 0`) is `now`, and under a minute is `<1m`.
    static func humanizeUntil(_ secs: Int64) -> String {
        if secs <= 0 { return "now" }
        let minute: Int64 = 60
        let hour: Int64 = 60 * minute
        let day: Int64 = 24 * hour
        let days = secs / day
        let hours = (secs % day) / hour
        let mins = (secs % hour) / minute
        if days > 0 {
            return hours > 0 ? "\(days)d\(hours)h" : "\(days)d"
        } else if hours > 0 {
            return mins > 0 ? "\(hours)h\(mins)m" : "\(hours)h"
        } else if mins > 0 {
            return "\(mins)m"
        } else {
            return "<1m"
        }
    }

    /// One window's "resets in" against the client's own clock `now`, or `n/a` when the instant is
    /// unknown (mirrors `src/cli.rs` `reset_cell`) — never a fabricated duration.
    static func resetCell(_ resetAt: Int64?, now: Int64) -> String {
        guard let at = resetAt else { return "n/a" }
        return humanizeUntil(at - now)
    }

    /// The panel's SINGLE per-row reset-in (issue #326 AC): a `weekly_exhausted` account keys off its
    /// WEEKLY reset — it is blocked for the week regardless of the session window — otherwise the
    /// SESSION reset, the sooner and more-actionable window. Humanized like `resetCell`, against the
    /// client's own clock `now`.
    static func resetIn(
        weeklyExhausted: Bool,
        sessionResetsAt: Int64?,
        weeklyResetsAt: Int64?,
        now: Int64
    ) -> String {
        let instant = weeklyExhausted ? weeklyResetsAt : sessionResetsAt
        return resetCell(instant, now: now)
    }

    // MARK: - AUTH cell (mirror `src/cli.rs` `health_glyph` / `health_cell` / `legacy_health_tags`)

    /// The needs-REFRESH cue for a `degraded` (bare-quarantine) credential — byte-identical to the
    /// CLI's `DEGRADED_CUE` (`src/cli.rs`, issue #427): the honest counterpart to `dead`'s
    /// `claude /login`. Deliberately NOT "re-login" — a quarantined-but-refreshable account needs a
    /// `poke`, not a re-authentication (the false-🔴 the honest verdict prevents).
    static let degradedCue = "degraded — run 'sessiometer poke'"

    /// The emoji glyph for a credential rollup — self-coloring content, not an overlay — mirroring
    /// `src/cli.rs` `health_glyph` (issue #119, #427; the neutral `⚪` for `unknown` is the anti-#137
    /// "no false green" verdict).
    static func healthGlyph(_ health: CredentialHealth) -> String {
        switch health {
        case .healthy:  return "🟢"
        case .unknown:  return "⚪"
        case .stale:    return "🟡"
        case .atRisk:   return "🟠"
        // #427: a quarantined-but-refreshable credential shares the warm 🟠 band with `atRisk`
        // (both "act soon, recoverable"), reserving 🔴 for a PROVEN refresh-token death. The two
        // are told apart by the needs-refresh cue (`authCue`) and, in the panel, by DISTINCT
        // SHAPES (`healthSymbol`); the load-bearing 🟠-poke vs 🔴-re-login split is carried by color.
        case .degraded: return "🟠"
        case .dead:     return "🔴"
        }
    }

    /// The native SF Symbol + semantic tint for a health state — the PANEL's per-medium render of the
    /// SAME `CredentialHealth` the CLI (and `healthGlyph`, the byte-parity mirror) shows as an emoji. R-2
    /// was re-ratified (2026-07-09) as STATE-parity — the enum + `authSpoken` rendered per-medium — so
    /// the panel draws a native symbol while the CLI keeps its emoji. DISTINCT SHAPES per state (checkmark
    /// / question / clock / triangle / octagon), so health is legible WITHOUT color — the WCAG 1.4.1 fix
    /// the shape-identical emoji ramp lacked. `unknown` stays neutral (the #137 "no false green").
    static func healthSymbol(_ health: CredentialHealth) -> (name: String, tint: HealthTint) {
        switch health {
        case .healthy:  return ("checkmark.circle.fill", .green)
        case .unknown:  return ("questionmark.circle", .neutral)
        case .stale:    return ("clock.badge.exclamationmark", .yellow)
        case .atRisk:   return ("exclamationmark.triangle.fill", .orange)
        // #427: DISTINCT shape from `atRisk` (a refresh-arrow vs a warning-triangle) so a
        // quarantined-but-refreshable credential is legible WITHOUT color — WCAG 1.4.1 — while
        // sharing the `.orange` warm-warning tint, honest that it is recoverable, not the red death.
        case .degraded: return ("arrow.clockwise.circle.fill", .orange)
        case .dead:     return ("xmark.octagon.fill", .red)
        }
    }

    /// The semantic tint ROLE for a health symbol. This Foundation-only namespace cannot name a SwiftUI
    /// `Color`, so it names the ROLE; the view maps it (via `healthTint`) to a concrete tint — never
    /// `Color.accentColor` (the AUTH glyph is never app-tinted, #84).
    enum HealthTint: Equatable { case green, yellow, orange, red, neutral }

    /// The RESOLVED tint target for a panel role — the Foundation-only handle the SwiftUI view turns into a
    /// concrete `Color`. `.asset` names an asset-catalog color set (#388: a theme-adaptive, contrast-safe
    /// token carrying Any/Dark + Increased-Contrast variants, because a raw system `Color` fails WCAG
    /// non-text/text contrast on the translucent vibrancy — system yellow ≈ 1.2:1 there); `.secondary` /
    /// `.primary` keep the system semantic colors where contrast already passes (neutral / no-data — the
    /// #137 "no false green").
    enum PanelTint: Equatable {
        case asset(String)
        case secondary
        case primary
    }

    /// The AUTH glyph's tint token (#388 token table). The healthy check and the warm warning tints move to
    /// contrast-safe asset tokens (`--ok` / `--ut-a` / `--ut-o` / `--ut-r` from the design mock); `.neutral`
    /// (unknown) stays `Color.secondary` — the #137 "no false green". `.yellow` (stale) and `.orange` (atRisk)
    /// map to DISTINCT tokens (amber vs orange), never one collapsed amber: severity-by-warmth is a second
    /// channel over the distinct shapes, and the `status` CLI keeps its 🟡 / 🟠 apart too (state-parity).
    static func healthTint(_ tint: HealthTint) -> PanelTint {
        switch tint {
        case .green:   return .asset("HealthOK")    // mock --ok  (healthy)
        case .yellow:  return .asset("UtilAmber")   // mock --ut-a (stale)
        case .orange:  return .asset("UtilOrange")  // mock --ut-o (atRisk)
        case .red:     return .asset("UtilRed")     // mock --ut-r (dead)
        case .neutral: return .secondary            // mock --text-3 (unknown)
        }
    }

    // MARK: - Panel chrome fidelity tokens (#388 — theme-aware accent emphasis + neutral fills)
    //
    // The design mock (`apps/menubar/design/menubar-preview.html`) hand-tunes its accent-emphasis opacities
    // and its neutral chrome fills PER THEME; the SwiftUI panel had them hardcoded to the LIGHT values,
    // theme-invariant. Two washouts fell out of that:
    //   * DARK accent emphasis (active row / dot halo / swap callout) rendered ~1.5–1.8× too faint — the
    //     mock bumps these opacities in dark, the panel did not.
    //   * Neutral fills routed through `Color.secondary.opacity(k)` washed out in BOTH themes: `.secondary`
    //     is the LABEL family (base ~(60,60,67), already ~0.5 alpha), so `secondary.opacity(k)` renders at
    //     ≈ half the mock's intended alpha AND over the wrong base hue (the mock's neutral fills are the
    //     systemGray/white FILL family, base (120,120,128)/white).
    // These pure, theme-parameterized tokens carry the mock's EXACT values into the testable layer, so the
    // view stays a thin `@Environment(\.colorScheme)` consumer and every number is unit-asserted against the
    // oracle (the panel cannot be screenshot-verified in CI; the `StatusPanelFormatTests` assertion is the
    // gate). The accent HUE itself is NOT here — it stays `Color.accentColor`, pinned to the brand-blue
    // `AccentColor` asset (#391, #007aff/#0a84ff), which already equals the mock's `--accent`; only the
    // theme-variant ALPHA lives here.
    //
    // GUARDRAIL: never `Color.secondary.opacity(k)` a FILL. `.secondary` is a label-family (text) tint; a
    // translucent neutral FILL must use `neutralFill` below — that mis-use IS the washout this fixes.
    // `.secondary` stays correct for secondary TEXT and for the neutral (`.neutral`/unknown) tint role.

    /// An accent-tinted emphasis SURFACE whose opacity the mock raises in dark mode. The accent hue is
    /// `Color.accentColor` (brand-blue asset, #391); these cases name only the theme-variant alpha.
    enum AccentEmphasis: Equatable {
        /// The active row's accent-tint card fill — mock `--active-bg` (.08 light / .15 dark).
        case activeRowFill
        /// The active status dot's soft accent halo — mock `--accent-halo` (.20 light / .30 dark).
        case activeDotHalo
        /// The swap-callout hero card's accent-tint fill — mock `--accent-tint` (.10 light / .16 dark).
        case swapCalloutFill
        /// The swap-callout hero card's accent-tint hairline border — mock `--accent-tint-border`
        /// (.20 light / .30 dark).
        case swapCalloutBorder
    }

    /// The theme-aware opacity for an accent-emphasis surface, applied over `Color.accentColor`. `light` is
    /// the mock's light-theme value (unchanged from what shipped — the panel was already correct in light);
    /// `dark` raises it to the mock's dark value so the active row / swap callout read at the mock's intended
    /// dark emphasis instead of the too-faint light value. Values are the mock's `--active-bg` /
    /// `--accent-halo` / `--accent-tint` / `--accent-tint-border` alphas
    /// (`apps/menubar/design/menubar-preview.html`).
    static func accentOpacity(_ emphasis: AccentEmphasis, dark: Bool) -> Double {
        switch emphasis {
        case .activeRowFill:     return dark ? 0.15 : 0.08
        case .activeDotHalo:     return dark ? 0.30 : 0.20
        case .swapCalloutFill:   return dark ? 0.16 : 0.10
        case .swapCalloutBorder: return dark ? 0.30 : 0.20
        }
    }

    /// A translucent NEUTRAL fill role — the mock's gray-in-light / white-in-dark chrome fills, formerly
    /// (mis-)rendered via `Color.secondary.opacity(k)` (the #388 washout). Distinct from the health/usage
    /// TINT roles (`PanelTint`): those are semantic FOREGROUND tints on contrast-safe asset colorsets
    /// (#406, Increase-Contrast-adaptive); these are DECORATIVE background fills (no text / WCAG 1.4.11
    /// role — the glyph or content on top carries meaning), carried as exact sRGB values so they are
    /// unit-testable in the asset-catalog-free logic-test bundle (`MenubarTests` compiles no `.xcassets`).
    enum NeutralFillRole: Equatable {
        /// The monogram badge + the header app-glyph badge — mock `--badge-bg`
        /// (gray(120,120,128) .16 light / white .10 dark).
        case badge
        /// The usage-meter track — mock `--track` (gray(120,120,128) .22 light / white .14 dark).
        case track
        /// The capture card's background — mock `--card-bg` (gray(120,120,128) .08 light / white .05 dark).
        case card
    }

    /// A resolved sRGB fill as raw components — the Foundation-only handle the SwiftUI view turns into a
    /// `Color(.sRGB, …)`. Kept as NUMBERS (not a `Color`) so this layer stays AppKit/SwiftUI-free and the
    /// values are directly unit-assertable against the mock (component-wise `Equatable`).
    struct FillRGBA: Equatable {
        let red: Double
        let green: Double
        let blue: Double
        let alpha: Double
    }

    /// The theme-aware sRGB fill for a neutral role — the mock's exact `--badge-bg` / `--track` /
    /// `--card-bg` values (`apps/menubar/design/menubar-preview.html`). The base is the mock's neutral FILL
    /// family: systemGray (120,120,128) in light, white in dark, each at the mock's per-role alpha. The view
    /// renders this as a PLAIN translucent fill (NOT routed through the panel material), so the source-over
    /// composite matches the mock's rgba math.
    static func neutralFill(_ role: NeutralFillRole, dark: Bool) -> FillRGBA {
        // Mock neutral base: systemGray (120,120,128) in light, white in dark.
        let base: (r: Double, g: Double, b: Double) = dark ? (1, 1, 1) : (120.0 / 255, 120.0 / 255, 128.0 / 255)
        let alpha: Double
        switch role {
        case .badge: alpha = dark ? 0.10 : 0.16
        case .track: alpha = dark ? 0.14 : 0.22
        case .card:  alpha = dark ? 0.05 : 0.08
        }
        return FillRGBA(red: base.r, green: base.g, blue: base.b, alpha: alpha)
    }

    // MARK: - Active-row tag (issue #501 — neutral sentence-case capsule, mock `.tag`)

    /// The roster active-row tag label. A calm NEUTRAL sentence-case capsule (mock `.tag`,
    /// `apps/menubar/design/menubar-preview.html:243`) — `--badge-bg` fill + `--text-2` text, NO
    /// border — reusing the same neutral `.badge` fill token (`neutralFill(.badge, …)`) as the
    /// monogram badge. The tag is ONE of the row's THREE redundant "active" cues (filled accent dot +
    /// this tag + accent-tint row fill), so active never rides on colour alone (WCAG 1.4.1 / R-2). It
    /// is SENTENCE-CASE — NOT the letter-spaced uppercase "ACTIVE" that read as an accent web badge and,
    /// as a second accent element beside the dot, re-inflated the active over-signalling #387 M5 reduced
    /// (a same-hue accent-on-accent-tint pill also sank the label to ~3:1). On the neutral capsule the
    /// label clears the WCAG 1.4.11 3:1 floor (≈4.1:1 light / ≈3.7:1 dark — asserted in `StatusPanelFormatTests`).
    static let activeTagLabel = "Active"

    /// The full AUTH cell string, mirroring `src/cli.rs` `health_cell` BYTE-FOR-BYTE: the glyph, a
    /// PROVEN-DEAD account's `claude /login` cue and a `degraded` (quarantined-but-refreshable) one's
    /// needs-refresh `degradedCue` (issue #427) — each softened to `recovering` for a healing account
    /// (issue #109) — then the independent `disabled` rotation tag (#36). A pre-#119 daemon
    /// (`auth == nil`) falls back to the legacy comma-joined tags. Kept as the parity anchor for the
    /// tests and the row's VoiceOver label; the VIEW draws the glyph and cue as separate elements via
    /// `healthGlyph` + `authCue`.
    static func authCell(
        auth: CredentialHealth?,
        recovering: Bool,
        enabled: Bool,
        quarantined: Bool
    ) -> String {
        guard let health = auth else {
            return legacyHealthTags(enabled: enabled, quarantined: quarantined, recovering: recovering)
        }
        var cell = healthGlyph(health)
        if let cue = authActionCue(auth: health, recovering: recovering) {
            cell += " " + cue
        }
        if !enabled {
            cell += " disabled"
        }
        return cell
    }

    /// The trailing AUTH cue WITHOUT the glyph — the action a `dead` (`claude /login`) or `degraded`
    /// (needs-refresh) account needs, softened to `recovering` while healing (#109), plus a trailing
    /// `disabled` — or `nil` when there is no cue. For the modern (`auth != nil`) path where the view
    /// renders the glyph as its own element; the legacy (`auth == nil`) path uses `legacyHealthTags`.
    static func authCue(auth: CredentialHealth?, recovering: Bool, enabled: Bool) -> String? {
        var parts: [String] = []
        if let auth, let cue = authActionCue(auth: auth, recovering: recovering) {
            parts.append(cue)
        }
        if !enabled {
            parts.append("disabled")
        }
        return parts.isEmpty ? nil : parts.joined(separator: " ")
    }

    /// The per-verdict action word (issue #427) shared by `authCell` / `authCue` / `authSpoken` so the
    /// three never drift: a PROVEN-`dead` credential needs `claude /login`; a `degraded`
    /// (quarantined-but-refreshable) one needs a refresh (`degradedCue`); either softens to
    /// `recovering` while healing (#109). Every other state carries no action cue (`nil`).
    private static func authActionCue(auth: CredentialHealth, recovering: Bool) -> String? {
        switch auth {
        case .dead:     return recovering ? "recovering" : "claude /login"
        case .degraded: return recovering ? "recovering" : degradedCue
        default:        return nil
        }
    }

    /// The pre-#119 AUTH text for an account whose daemon sent no rollup (`auth == nil`), mirroring
    /// `src/cli.rs` `legacy_health_tags`: comma-joined `disabled` (#36) + `needs re-login` /
    /// `recovering` (#42/#109), so an old daemon degrades gracefully rather than showing a
    /// defaulted-healthy glyph over a dead account.
    static func legacyHealthTags(enabled: Bool, quarantined: Bool, recovering: Bool) -> String {
        var status = ""
        if !enabled {
            status += "disabled"
        }
        if quarantined {
            if !status.isEmpty {
                status += ", "
            }
            status += recovering ? "recovering" : "needs re-login"
        }
        return status
    }

    // MARK: - Honest-state banner (issue #326 AC — panel spec, not a `src/cli.rs` mirror)

    /// The visual weight of a banner — drives the panel's tint (semantic `Color`), never a literal hex.
    enum BannerKind: Equatable {
        /// The one healthy state — a live, schema-supported roster.
        case healthy
        /// A neutral, non-degraded transitional/info state (connecting, empty-roster).
        case info
        /// Degraded but recoverable, last-known data shown MARKED stale (stale).
        case warning
        /// Absent or unreadable — numbers are not trustworthy (disconnected, unsupported).
        case error
    }

    /// A rendered honest-state banner: a headline + one plain sentence + its tint. Every
    /// `ConnectionState` maps to exactly one banner (the AC's connecting / connected / dropped / empty /
    /// stale / unsupported), so the panel ALWAYS states its honest connection status and never renders
    /// healthy on a degraded or absent daemon.
    struct Banner: Equatable {
        let title: String
        let detail: String
        let kind: BannerKind
    }

    /// Derive the honest-state banner for a connection state (`accountCount` speaks the live count).
    /// Pure — the same state always yields the same banner. The `disconnected` reason is deliberately
    /// NOT surfaced verbatim (it is transport jargon, e.g. "connection closed (EOF)"); the banner is a
    /// plain operator-facing sentence.
    ///
    /// `ageText` (from `snapshotAgeText`) folds the snapshot's freshness into the detail for the three
    /// states that RETAIN a reading (connected / stale / disconnected) — so a persistent "Live" never
    /// silently implies the numbers are fresh (the council's "don't let Live imply fresh"). It is
    /// deliberately omitted for `connecting` (no snapshot yet), `emptyRoster` (no reading to age), and
    /// `unsupported` (numbers refused — the banner shows no freshness). `ageStale` (from
    /// `snapshotIsStale`) escalates a Live-but-stale daemon (transport up, data outlived any poll
    /// cadence) from `.healthy` to `.warning` — the connected-but-stale cell of the matrix.
    static func banner(for state: ConnectionState,
                       accountCount: Int,
                       ageText: String? = nil,
                       ageStale: Bool = false) -> Banner {
        switch state {
        case .connecting:
            return Banner(title: "Connecting…",
                          detail: "Reaching the daemon.",
                          kind: .info)
        case .connected:
            let plural = accountCount == 1 ? "" : "s"
            let base = "\(accountCount) account\(plural)"
            return Banner(title: "Live",
                          detail: ageText.map { "\(base) · \($0)." } ?? "\(base).",
                          kind: ageStale ? .warning : .healthy)
        case .emptyRoster:
            return Banner(title: "No accounts yet",
                          detail: "Connected to the daemon — no accounts configured.",
                          kind: .info)
        case .stale:
            let base = "Daemon quiet; showing last-known"
            return Banner(title: "Data may be stale",
                          detail: ageText.map { "\(base) · \($0)." } ?? "\(base).",
                          kind: .warning)
        case .disconnected:
            let base = "Reconnecting; showing last-known"
            return Banner(title: "Daemon not responding",
                          detail: ageText.map { "\(base) · \($0)." } ?? "\(base).",
                          kind: .error)
        case .unsupported:
            return Banner(title: "Update required",
                          detail: "The daemon speaks a newer version this app can't read.",
                          kind: .error)
        case .crashLooping:
            // The crash-loop FAULT banner (#169): a persistent fault shape that never renders healthy —
            // the held snapshot's numbers are refused until the daemon stays up (the healthy-flash is
            // debounced). Clock-free copy ("repeatedly", not "5× in the last minute") — the machine
            // counts consecutive unstable reconnects, not wall-clock restarts.
            return Banner(title: "Daemon crash-looping",
                          detail: "Restarting repeatedly; holding status until it stays up.",
                          kind: .error)
        case .starting:
            // The daemon-starting banner (#499): a transient, non-degraded "coming up" state — same weight
            // as `.connecting` (`.info`). A STATIC message; the app fakes no progress it isn't doing.
            return Banner(title: "Starting…",
                          detail: "Waiting for the daemon to come up.",
                          kind: .info)
        case .notRunning:
            // The not-running banner (#499): the daemon is absent, so numbers are not trustworthy
            // (`.error`, like `.disconnected` / `.unsupported`). The Start-daemon affordance is #170
            // (launch-at-login via SMAppService), deferred and signing-blocked — so the banner degrades
            // to this inert explanatory line, with no button yet.
            return Banner(title: "Daemon not running",
                          detail: "The daemon isn't running.",
                          kind: .error)
        }
    }

    // MARK: - Snapshot age (issue #326 / council — the CLI's parity render of the wire `generated_at`)

    /// The age (in seconds) past which a snapshot's data is UNAMBIGUOUSLY stale — the maximum possible
    /// poll cadence (`POLL_SECS_HI` = 3600 in `src/daemon.rs`). A snapshot older than this has outlived
    /// even the slowest legitimate poll interval, so it cannot be dismissed as "just a long cadence."
    /// Deliberately conservative: it NEVER false-alarms a healthy-but-slow daemon (the client does not
    /// know the configured cadence, so a lower bar would cry wolf), and the transport-liveness watchdog
    /// (#344, 32 s) already catches a DROPPED connection far sooner. This is the backstop for the one
    /// gap the watchdog misses — a daemon that keeps HEARTBEATING while its poll loop is wedged (frames
    /// still arrive, so the connection reads live, but `generated_at` stops advancing). Mirrors the Rust
    /// `STALE_AGE_SECS` (`src/cli.rs`); the two thresholds move together.
    static let staleAgeSecs: Int64 = 3600

    /// "updated Ns ago" for a snapshot's freshness, or `nil` when there is no generation instant
    /// (`generatedAt <= 0` — the wire's `0` sentinel for an all-defaults / never-generated snapshot).
    /// The age is `now - generatedAt` against the client's OWN clock, humanized with the SAME
    /// two-largest-unit `humanizeUntil` the reset-in uses (so the vocabulary matches and the panel↔CLI
    /// parity is inherited from that already-byte-mirrored humanizer). Clamped at 0 for a benign
    /// client-ahead clock skew. This is the field the wire contract itself earmarks for exactly this UX
    /// (`snapshot.rs`: "a client compares it against its own clock and greys out once the gap grows").
    static func snapshotAgeText(generatedAt: Int64, now: Int64) -> String? {
        guard generatedAt > 0 else { return nil }
        let age = max(0, now - generatedAt)
        return age == 0 ? "updated just now" : "updated \(humanizeUntil(age)) ago"
    }

    /// Whether a snapshot is unambiguously stale — older than `staleAgeSecs`. `false` for a snapshot
    /// with no generation instant (`generatedAt <= 0`): absent freshness is NOT stale (it is unknown).
    /// Drives the connected-but-stale banner escalation (a `Live` daemon whose data has outlived any
    /// poll cadence is flagged `.warning`).
    static func snapshotIsStale(generatedAt: Int64, now: Int64) -> Bool {
        generatedAt > 0 && (now - generatedAt) > staleAgeSecs
    }

    // MARK: - Usage severity + swap-trigger (mirror `src/cli.rs` `util_severity` / `weekly_cell_severity`)

    /// One utilization urgency band. Mirrors the subset of `src/cli.rs` `Severity` the per-cell
    /// utilization overlay uses — the reset-proximity `Dim` and the account-aggregate's reset-soon
    /// downgrade are CLI-table concerns (the `ACCOUNT` cell), NOT the per-metric panel color, so the
    /// panel mirror is the three utilization bands only.
    enum UsageSeverity: Equatable { case green, yellow, red }

    /// The usage %-TEXT tint token (#388). The meter percent is small text (WCAG 4.5:1), so it uses the
    /// darker `--ut-*` TEXT family — NOT the brighter `--u-*` BAR-FILL family the meter bar keeps (a bar
    /// is a non-text fill, 3:1, and the mock fills it with the system-bright colors). A failed poll (`nil`)
    /// stays `.primary`: an uncolored metric, never a false "healthy" green (#137).
    static func usageTextTint(_ severity: UsageSeverity?) -> PanelTint {
        switch severity {
        case .green:  return .asset("UtilGreen")   // mock --ut-g
        case .yellow: return .asset("UtilAmber")   // mock --ut-a
        case .red:    return .asset("UtilRed")     // mock --ut-r
        case .none:   return .primary
        }
    }

    /// The urgency band for a utilization percent — the panel's mirror of `src/cli.rs` `util_severity`:
    /// `>= 90` Red (at/near the ~95% session swap-away trigger, #41), `>= 75` Yellow (worth watching),
    /// else Green. One shared "how full is too full" definition (issue #84), so the panel's per-metric
    /// threshold color keys off the SAME bands as the CLI's per-cell overlay for the same reading.
    static func utilSeverity(_ pct: UInt8) -> UsageSeverity {
        if pct >= 90 { return .red }
        if pct >= 75 { return .yellow }
        return .green
    }

    /// The SESSION metric's severity — the raw `utilSeverity` of its percent, or `nil` when the poll
    /// failed (the `n/a` text carries the truth; an uncolored metric is not a false "healthy"). Mirrors
    /// the CLI's `session_severity` (`account.session_pct.map(util_severity)`).
    static func sessionSeverity(_ sessionPct: UInt8?) -> UsageSeverity? {
        sessionPct.map(utilSeverity)
    }

    /// The WEEKLY metric's severity — `utilSeverity` of its percent, EXCEPT a weekly-EXHAUSTED account
    /// (the daemon's blocked-for-the-week verdict, #11/#37) reads Red whatever the rounded percent — a
    /// week-blocked account is never painted "healthy", even under a lowered `weekly_trigger`. `nil`
    /// when the weekly poll failed. Mirrors the CLI's `weekly_cell_severity`.
    static func weeklySeverity(weeklyPct: UInt8?, weeklyExhausted: Bool) -> UsageSeverity? {
        weeklyPct.map { weeklyExhausted ? .red : utilSeverity($0) }
    }

    // MARK: - `next_swap` footer (issue #326 AC — renders the FORWARD candidate, not swap history)

    /// The footer line for the daemon's `next_swap` candidate, or `nil` when there is no active anchor
    /// to swap from (the footer is then absent). Renders the FORWARD candidate the `watch` wire carries
    /// — NOT swap history (a true last-swap needs a new daemon source; issue #326 note).
    ///
    /// A `noViableTarget` carrying the #405 fleet-capacity relief renders it the panel's own concise
    /// way (R-2 STATE-parity — the SAME facts as the CLI's `next swap: none …` footer, not the same
    /// bytes): a weekly-exhausted fleet reads "Out of capacity … · add an account" (a week-long block
    /// — adding an account is the remedy), an over-session fleet reads "every account over its session
    /// limit" WITHOUT the nudge (a transient block that resets soon), each naming the reset when the
    /// daemon knew it. A pre-#405 daemon (no `cause`) falls back to the bare "No viable target".
    static func nextSwapFooter(_ nextSwap: NextSwap?, now: Int64) -> String? {
        switch nextSwap {
        case .target(let to, _):
            return "Next swap → \(to)"
        case .noViableTarget(let cause, let resetsAt):
            let relief = resetsAt.map { " — resets in \(humanizeUntil($0 - now))" } ?? ""
            switch cause {
            case .weekly:  return "Out of capacity\(relief) · add an account"
            case .session: return "Every account over its session limit\(relief)"
            case nil:      return "No viable target"
            }
        case .awaitingData:
            return "Awaiting data"
        case nil:
            return nil
        }
    }

    // MARK: - `canonical_scrub` footer (issue #469 — the fleet-wide scrubbed-canonical signal)

    /// The footer line for the daemon's `canonical_scrub` rollup (`WireModel.swift` `CanonicalScrub`,
    /// issue #516), or `nil` when the shared canonical is healthy (the wire key is absent → the footer
    /// is then absent, same single-cardinality as `nextSwapFooter`). The shared `Claude Code-credentials`
    /// canonical item has been SCRUBBED — every `claude` session is logged out — the fleet-wide lockout
    /// NO per-account `auth` cell reflects (each row can read perfectly healthy while the shared item
    /// sits emptied), so no roster glyph carries it; only this daemon-level footer does.
    ///
    /// Content-parity with the CLI's `shared login: scrubbed …` footer (`src/cli.rs`): the SAME state
    /// and the SAME byte-shared `claude /login` remedy, each medium phrasing it its own way (R-2
    /// STATE-parity, as ADR-0016 did for `ActiveDeadNoTarget` / `nextSwapFooter`). `.exhausted` names
    /// the state AND the actionable remedy — the un-recoverable residual that needs a re-login;
    /// `.recovering` is the calm, no-action cue (the daemon may self-heal by adopting a live account,
    /// so a re-login prompt would cry wolf). A fleet-wide STATE discriminant only — never per-account,
    /// never a token or email (issue #15). The remedy string is the established `claude /login` cue the
    /// dead-credential `authCell` already uses — deliberately, so the operator meets ONE re-login verb.
    static func canonicalScrubFooter(_ scrub: CanonicalScrub?) -> String? {
        switch scrub {
        case .exhausted:  return "Shared login scrubbed · run claude /login"
        case .recovering: return "Shared login scrubbed · recovering automatically"
        case nil:         return nil
        }
    }

    // MARK: - Header identity + swap callout (issue #355 — design-reference parity)

    /// The header's identity sub-line — the design reference's `app-sub` ("N accounts · {active}
    /// active"). Honest per connection state: a degraded roster reads "last-known" and a Live-but-wedged
    /// or gone-quiet snapshot appends "· stale", so the always-present identity line NEVER implies the
    /// numbers are live/fresh (the never-healthy-on-degraded discipline, carried into the header).
    static func headerSubtitle(state: ConnectionState,
                               accountCount: Int,
                               activeLabel: String?,
                               ageStale: Bool) -> String {
        let plural = accountCount == 1 ? "" : "s"
        let count = "\(accountCount) account\(plural)"
        switch state {
        case .connecting:   return "Connecting to the daemon…"
        case .starting:     return "Connecting to the daemon…"   // #499: the "coming up" identity line (mock app-sub)
        case .notRunning:   return "Daemon not running"          // #499: no last-known reading to age (never connected)
        case .emptyRoster:  return "Welcome"
        case .unsupported:  return "Version mismatch"
        case .crashLooping: return "Daemon fault"
        case .disconnected: return "\(count) · last-known"
        case .connected, .stale:
            let base = activeLabel.map { "\(count) · \($0) active" } ?? count
            let isStale: Bool = { if case .stale = state { return true } else { return ageStale } }()
            return isStale ? "\(base) · stale" : base
        }
    }

    /// The swap-callout target label (the design reference's hero card), or `nil` when there is no
    /// forward candidate — the card is then absent (same single-cardinality as `nextSwapFooter`; a
    /// `noViableTarget` / `awaitingData` / absent anchor shows no card).
    static func swapCalloutTarget(_ nextSwap: NextSwap?) -> String? {
        if case .target(let to, _) = nextSwap { return to }
        return nil
    }

    /// The swap-callout's muted "why" line — the daemon's OWN selection reason (issue #393),
    /// rendered from the wire `NextSwap.target` discriminant. This REPLACES the former client-side
    /// derivation, which asserted "lowest weekly · most headroom" — a rationale on the SUPERSEDED
    /// selection axis (`pick_target` chooses by soonest weekly reset, #37, not headroom), and one
    /// the client could not honestly re-derive anyway (the daemon-only session trigger / floor never
    /// ride the wire). `nil` when the candidate is not a `target`, OR when a pre-#393 daemon carried
    /// a target with no reason — the card then shows just the label (strictly more honest than the
    /// old superseded-rule story). Each medium renders the shared discriminant its own way
    /// (state-parity): this concise phrase for the panel, a parenthetical for `sessiometer status`.
    static func swapCalloutReason(_ nextSwap: NextSwap?) -> String? {
        guard case .target(_, let reason) = nextSwap else { return nil }
        switch reason {
        case .soonestReset:  return "weekly resets soonest"
        case .onlyCandidate: return "only viable target"
        case .rosterOrder:   return "first eligible · no reset times known"
        case nil:            return nil
        }
    }

    // MARK: - Row VoiceOver label (issue #326 AC — VoiceOver-navigable rows)

    /// One spoken, comma-separated sentence for a row's VoiceOver label, so the whole row reads as a
    /// single accessible element rather than a scatter of unlabeled glyphs. Speaks identity, the active
    /// marker, the auth verdict + its cue, both usage percents each with its own reset-in — the same facts
    /// the row shows visually. Next-swap is NOT per-row (R-2 re-ratified 2026-07-09): it is a single-cardinality
    /// fact spoken once by the footer, mirroring the CLI (which has no per-row next marker).
    static func rowAccessibilityLabel(
        label: String,
        isActive: Bool,
        auth: CredentialHealth?,
        recovering: Bool,
        enabled: Bool,
        quarantined: Bool,
        sessionPct: UInt8?,
        weeklyPct: UInt8?,
        sessionReset: String,
        weeklyReset: String
    ) -> String {
        var parts: [String] = [label]
        if isActive { parts.append("active") }
        parts.append(authSpoken(auth: auth, recovering: recovering, enabled: enabled, quarantined: quarantined))
        // Both windows, each with its reset — matching the row's two meters and the CLI's two columns.
        parts.append("session \(pct(sessionPct)) resets in \(sessionReset)")
        parts.append("weekly \(pct(weeklyPct)) resets in \(weeklyReset)")
        // Drop any empty auth phrase (a healthy pre-#119 legacy account speaks no auth verdict).
        return parts.filter { !$0.isEmpty }.joined(separator: ", ")
    }

    /// A spoken auth verdict for VoiceOver — the glyph's meaning in words (the emoji alone is a
    /// color-only signal), plus the DEAD cue and `parked` tag. Empty for a healthy pre-#119 legacy
    /// account that carries no verdict and no tags.
    static func authSpoken(
        auth: CredentialHealth?,
        recovering: Bool,
        enabled: Bool,
        quarantined: Bool
    ) -> String {
        var phrase: String
        if let health = auth {
            switch health {
            case .healthy: phrase = "auth healthy"
            case .unknown: phrase = "auth unknown"
            case .stale:   phrase = "auth stale"
            case .atRisk:  phrase = "auth at risk"
            // #427: spoken needs-refresh, distinct from `dead`'s needs-re-login.
            case .degraded: phrase = recovering ? "recovering" : "credential degraded, run sessiometer poke to refresh"
            case .dead:    phrase = recovering ? "recovering" : "credential dead, run claude /login"
            }
        } else {
            // Legacy (auth nil): speak only the tags the CLI would show.
            if quarantined {
                phrase = recovering ? "recovering" : "needs re-login"
            } else {
                phrase = ""
            }
        }
        if !enabled {
            phrase = phrase.isEmpty ? "parked" : "\(phrase), parked"
        }
        return phrase
    }

    // MARK: - Stats tab (issue #446 — the mock's `.stats` view, fed by the #356 socket `stats` verb)
    //
    // Pure presentation over the decoded `StatsWire` (WireModel.swift), mirroring the design mock
    // (`apps/menubar/design/menubar-preview.html` `.stats`) — so the SwiftUI `StatsView` stays a thin
    // consumer and every number is unit-asserted against the oracle (the panel cannot be screenshot-verified
    // in CI, exactly like the #388 chrome tokens above; the `StatusPanelFormatTests` assertion is the gate).

    /// The Stats-tab header phrase for the resolved window — mock `.app-sub` "Usage stats · last 7 days" for
    /// the panel's default `week` window. Derived from the wire's OWN window (not hardcoded), so a different
    /// period reads honestly and the header never fabricates a phrase it did not query.
    static func statsHeaderSubtitle(_ window: StatsWindow) -> String {
        "Usage stats · \(statsWindowPhrase(window))"
    }

    /// The Stats-tab header shown BEFORE the wire's own window arrives (loading / failed / idle): the phrase
    /// for the panel's fixed `week` query (`StatsCommand.period`). A `week`-window `statsHeaderSubtitle`
    /// renders the identical string — `StatsTests.testDefaultHeaderSubtitleMatchesTheWeekWindowHeader` locks
    /// the two together so this pre-load constant can never drift from the loaded-window header.
    static let statsDefaultHeaderSubtitle = "Usage stats · last 7 days"

    /// The compact window phrase for the Stats header / aggregate callout. The preset periods read as the
    /// mock's spelled-out spans; a `--since` window falls back to its raw offset, and anything else to the
    /// wire's own human echo — never an invented span.
    static func statsWindowPhrase(_ window: StatsWindow) -> String {
        switch window.period {
        case "day": return "last 24h"
        case "week": return "last 7 days"
        case "month": return "last 30 days"
        case "lifetime": return "all time"
        default:
            if let since = window.since { return "since \(since)" }
            return window.label
        }
    }

    /// A quota fraction (0…1, the `StatsDim` wire scale) as a whole percent — the stats analogue of the CLI's
    /// `pct` (`src/stats.rs`), which rounds `fraction × 100`. Clamped at the floor so a tiny negative never
    /// prints; NOT clamped at the top (an over-cap peak legitimately reads > 100%).
    static func statsPercent(_ fraction: Double) -> Int {
        Int((max(0, fraction) * 100).rounded())
    }

    /// The Stats row's "Session m/pk" cell — mean then peak, mock `.sc-val` "42 / 100%" (the mean bare, the
    /// peak carrying the single trailing `%`).
    static func statsSessionMeanPeak(_ account: StatsAccountStats) -> String {
        "\(statsPercent(account.session.mean)) / \(statsPercent(account.session.peak))%"
    }

    /// The Stats row's "Weekly pk" cell — the weekly peak percent, mock `.sc-val` "88%".
    static func statsWeeklyPeak(_ account: StatsAccountStats) -> String {
        "\(statsPercent(account.weekly.peak))%"
    }

    /// The honest one-line message the Stats tab shows when the query did not yield a series — never a blank
    /// tab, never a fabricated number (the crown-jewel honesty rule, applied to the read-only Stats surface).
    static func statsFailureText(_ failure: StatsFailure) -> String {
        switch failure {
        case .unavailable:
            return "Usage stats unavailable — the daemon socket didn't resolve."
        case .transport:
            return "Couldn't reach the daemon for usage stats."
        case .daemonError(let reason):
            return "Usage stats error: \(reason)."
        case .undecodable:
            return "Usage stats came back in an unreadable form."
        }
    }

    /// The neutral three-way utilisation signal the mock's `.signal` pill shows, collapsed from the wire's
    /// finer `band` EXACTLY as the CLI does (`src/stats.rs` `SignalBand::of`): idle/low → underused,
    /// moderate → balanced, high/at-cap → saturated. A DESCRIPTOR (equal-weight departures from the balanced
    /// middle), never a recommendation — the Stats tab is read-only.
    enum StatSignal: Equatable {
        case underused
        case balanced
        case saturated

        /// The provisional descriptor word (mock `.signal` label; final copy pending #160's framing review).
        var label: String {
            switch self {
            case .underused: return "underused"
            case .balanced: return "balanced"
            case .saturated: return "saturated"
            }
        }
    }

    /// Collapse a wire `band` into the mock's three-way signal (see `StatSignal`).
    static func statsSignal(_ band: StatsBand) -> StatSignal {
        switch band {
        case .idle, .low: return .underused
        case .moderate: return .balanced
        case .high, .atCap: return .saturated
        }
    }

    /// The aggregate callout under the Stats rows — mock `.agg` "All accounts ≥90% at once — 3 episodes
    /// (1h40m) · swaps 28 · last 7 days", built from the summary `roster` (`StatsRoster`) + the window phrase.
    /// Facts only (magnitudes + the neutral span), never a recommendation.
    static func statsAggregateText(roster: StatsRoster, window: StatsWindow) -> String {
        let episodes = roster.allHighEpisodes
        let epWord = episodes == 1 ? "episode" : "episodes"
        return "All accounts ≥90% at once — \(episodes) \(epWord) (\(statsDuration(roster.allHighSecs)))"
            + " · swaps \(roster.swapCount) · \(statsWindowPhrase(window))"
    }

    /// A whole-second span as the compact coarse duration the aggregate callout uses — the two-largest-unit
    /// form mirroring the CLI's `fmt_dur` (`src/stats.rs`): `1h40m` / `1h` / `40m` / `30s`; a non-positive
    /// span is `0s`. Distinct from `humanizeUntil` (the reset-in cell, which reads `now` / `<1m`).
    static func statsDuration(_ secs: Int64) -> String {
        if secs <= 0 { return "0s" }
        let hour: Int64 = 3600
        let hours = secs / hour
        let mins = (secs % hour) / 60
        let s = secs % 60
        if hours > 0 {
            return mins > 0 ? "\(hours)h\(mins)m" : "\(hours)h"
        } else if mins > 0 {
            return "\(mins)m"
        } else {
            return "\(s)s"
        }
    }

    // MARK: - Stats sparkline geometry (issue #446 — R-2 parity with the CLI trend sparkline)

    /// One sparkline vertex in the SVG-style box, as raw `Double`s (Foundation-only, so it stays in the
    /// logic-test bundle and is component-wise `Equatable`-testable). The view maps these to `CGPoint`s.
    struct SparkPoint: Equatable {
        let x: Double
        let y: Double
    }

    /// The per-bucket session-peak series for `handle`, in bucket order — the CLI trend sparkline's pick
    /// (`src/stats.rs`: "the per-bucket session peak — the sparkline 'how hot did it get' pick"). A bucket
    /// with no reading for the handle plots at the floor (`0`), honestly — the aggregator never invents a
    /// reading, and neither does this: an unmeasured bucket is a real low, not a gap the sparkline hides.
    static func sparkSeries(_ series: [StatsBucket], handle: String) -> [Double] {
        series.map { $0.accounts[handle]?.session.peak ?? 0 }
    }

    /// Map a value series to sparkline vertices in a `width` × `height` box, on the FIXED [0, 1] (0–100% of
    /// the quota cap) scale — R-2 parity with the CLI sparkline (`src/stats.rs` `ramp_level`, which clamps to
    /// `[0, 1]`), NOT auto-normalised per account: a value of `1.0` reaches the top, `0.0` the floor, an
    /// over-cap reading clamps to the top. `inset` keeps the stroke off the edges; with the mock's box
    /// (96 × 28, inset 3) this reproduces the mock's `.spark` path vertices exactly. `x` is evenly spaced
    /// across the plot; a single-point series centres. An empty series yields no points.
    static func sparkPoints(
        _ values: [Double],
        width: Double,
        height: Double,
        inset: Double
    ) -> [SparkPoint] {
        guard !values.isEmpty else { return [] }
        let left = inset, right = width - inset
        let top = inset, bottom = height - inset
        let n = values.count
        return values.enumerated().map { index, value in
            let x = n == 1 ? (left + right) / 2 : left + Double(index) / Double(n - 1) * (right - left)
            let clamped = min(1, max(0, value))
            let y = bottom - clamped * (bottom - top)
            return SparkPoint(x: x, y: y)
        }
    }

    /// The Stats rows, ORDERED to match the Status roster (so the two tabs list accounts identically), with
    /// any stats-only handle (present in the window but not the live roster — normally none, the daemon splits
    /// orphans out) appended alphabetically. Pure over the two key sets, so the view's roster join is testable
    /// without SwiftUI. Handles NOT in `summaryHandles` (a roster account with no reading this window) are
    /// omitted — the Stats view shows what was MEASURED, matching the CLI summary.
    static func orderedStatHandles(summaryHandles: Set<String>, rosterOrder: [String]) -> [String] {
        var out: [String] = []
        var placed: Set<String> = []
        for label in rosterOrder where summaryHandles.contains(label) {
            out.append(label)
            placed.insert(label)
        }
        for handle in summaryHandles.sorted() where !placed.contains(handle) {
            out.append(handle)
        }
        return out
    }

    // MARK: - Stats color tokens (issue #446 — mock `--spark` + `--sig-*`, theme-aware, unit-testable)

    /// The sparkline stroke / area / end-dot color — mock `--spark` (`rgba(60,60,67,.55)` light /
    /// `rgba(235,235,245,.5)` dark), the secondary-label neutral graphic tint. Carried as an exact `FillRGBA`
    /// (like the #388 neutral fills) so it is unit-assertable in the asset-catalog-free logic bundle; the view
    /// renders the line/dot at this alpha and the area at a fraction of it (mock `.sp-area { fill-opacity:.2 }`).
    /// Its OWN label-family base (60,60,67)/(235,235,245) — distinct from the (120,120,128)/white chrome-fill
    /// family (`neutralFill`) — so it is a separate token, not a `NeutralFillRole` case.
    static func sparkColor(dark: Bool) -> FillRGBA {
        dark
            ? FillRGBA(red: 235.0 / 255, green: 235.0 / 255, blue: 245.0 / 255, alpha: 0.5)
            : FillRGBA(red: 60.0 / 255, green: 60.0 / 255, blue: 67.0 / 255, alpha: 0.55)
    }

    /// The signal pill's background FILL — mock `--sig-under-bg` / `--sig-bal-bg` / `--sig-sat-bg`, per theme.
    static func statsSignalFill(_ signal: StatSignal, dark: Bool) -> FillRGBA {
        switch (signal, dark) {
        case (.underused, false): return FillRGBA(red: 0, green: 122.0 / 255, blue: 255.0 / 255, alpha: 0.12)
        case (.underused, true): return FillRGBA(red: 64.0 / 255, green: 140.0 / 255, blue: 230.0 / 255, alpha: 0.20)
        case (.balanced, false): return FillRGBA(red: 30.0 / 255, green: 150.0 / 255, blue: 105.0 / 255, alpha: 0.13)
        case (.balanced, true): return FillRGBA(red: 50.0 / 255, green: 180.0 / 255, blue: 130.0 / 255, alpha: 0.18)
        case (.saturated, false): return FillRGBA(red: 178.0 / 255, green: 120.0 / 255, blue: 20.0 / 255, alpha: 0.15)
        case (.saturated, true): return FillRGBA(red: 210.0 / 255, green: 160.0 / 255, blue: 80.0 / 255, alpha: 0.20)
        }
    }

    /// The signal pill's foreground (label + dot) color — mock `--sig-under-fg` / `--sig-bal-fg` /
    /// `--sig-sat-fg`, per theme. Opaque (alpha 1); it carries text, so — unlike the decorative bg fill — it
    /// is the readable channel.
    static func statsSignalText(_ signal: StatSignal, dark: Bool) -> FillRGBA {
        switch (signal, dark) {
        case (.underused, false): return FillRGBA(red: 38.0 / 255, green: 104.0 / 255, blue: 189.0 / 255, alpha: 1)
        case (.underused, true): return FillRGBA(red: 130.0 / 255, green: 179.0 / 255, blue: 237.0 / 255, alpha: 1)
        case (.balanced, false): return FillRGBA(red: 28.0 / 255, green: 138.0 / 255, blue: 95.0 / 255, alpha: 1)
        case (.balanced, true): return FillRGBA(red: 96.0 / 255, green: 207.0 / 255, blue: 161.0 / 255, alpha: 1)
        case (.saturated, false): return FillRGBA(red: 150.0 / 255, green: 102.0 / 255, blue: 17.0 / 255, alpha: 1)
        case (.saturated, true): return FillRGBA(red: 224.0 / 255, green: 178.0 / 255, blue: 104.0 / 255, alpha: 1)
        }
    }

    // MARK: - Account identity disambiguation kit (issue #445 — per-account color + smart monogram)
    //
    // A roster of same-local-part accounts (`work-alice`, `work-bob`, …) collapses the panel's identity
    // cues: every MonogramBadge shows the same first letter and tail-truncation hides the one distinguishing
    // part of each label. This restores distinguishability with THREE cues, none alone sufficient (WCAG
    // 1.4.1 — color is NEVER the sole signal, always paired with the monogram + the label text): a per-account
    // COLOR, a smart 2-char MONOGRAM from the distinguishing token, and MIDDLE-truncation (the last is a view-
    // layer `.truncationMode` change; the two below are the testable pure core).
    //
    // IDENTITY HANDLE = `label` (issue #15 / R-2). The AC says "seed the color from the on-wire
    // `account_uuid`", but `account_uuid` is NOT on the status wire: `AccountStatusLine` (`snapshot.rs` /
    // `WireModel.swift`) carries `label` as the ONE identity handle and never a uuid, and no uuid rides any
    // wire golden. Seeding from `label` keeps the AC's "no wire change" TRUE and honors R-2 (one handle,
    // rendered per-medium — the handle IS `label`). Trade-off accepted: the color re-derives if the operator
    // renames the label — fine for a disambiguation AID (rename is rare; the color is never the sole cue).

    /// A resolved fill helper — an opaque sRGB `FillRGBA` from 0…255 components (like the mock's hex values).
    private static func accountRGB(_ red: Double, _ green: Double, _ blue: Double) -> FillRGBA {
        FillRGBA(red: red / 255, green: green / 255, blue: blue / 255, alpha: 1)
    }

    /// The per-account badge FILL palette (issue #445) — 8 LOW-CHROMA, colorblind-considerate hues (they vary
    /// in luminance as well as hue, so the cue survives color-vision deficiency), the active/accent blue hue
    /// EXCLUDED. Per theme the fill inverts to stay high-contrast on the panel: LIGHT is a muted mid-DARK tone
    /// (a near-white monogram reads on it, and it clears the near-white panel); DARK is a muted mid-LIGHT tone
    /// (a near-black monogram reads on it, and it clears the near-black panel). Exact sRGB so
    /// `StatusPanelFormatTests` can assert WCAG-AA against the panel reference base. NEUTRAL by construction
    /// (#173): a muted identity hue, never a vivid provider brand color.
    private static let accountFillPalette: [(light: FillRGBA, dark: FillRGBA)] = [
        (accountRGB(78,  64, 112), accountRGB(190, 176, 216)),  // violet
        (accountRGB(100, 60, 112), accountRGB(206, 172, 214)),  // purple
        (accountRGB(116, 58,  98), accountRGB(218, 168, 200)),  // magenta
        (accountRGB(122, 56,  66), accountRGB(226, 168, 174)),  // rose
        (accountRGB(122, 74,  46), accountRGB(226, 182, 150)),  // clay
        (accountRGB(98,  80,  38), accountRGB(210, 190, 138)),  // ochre
        (accountRGB(64,  92,  50), accountRGB(176, 202, 156)),  // moss
        (accountRGB(38,  96,  88), accountRGB(148, 204, 194)),  // teal
    ]

    /// The number of palette slots — the modulus of the color hash, exposed for the palette tests.
    static var accountColorCount: Int { accountFillPalette.count }

    /// The palette index for a label (issue #445) — a STABLE, deterministic FNV-1a hash of the trimmed label
    /// mod the palette size. Deliberately NOT Swift's `Hasher`/`hashValue`, which is per-process RANDOMIZED
    /// (it would reshuffle every account's color on each launch and defeat any test); FNV-1a is a fixed
    /// function, so an account keeps its color across launches and the mapping is unit-assertable.
    static func accountColorIndex(for label: String) -> Int {
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        var hash: UInt32 = 2_166_136_261
        for byte in trimmed.utf8 {
            hash = (hash ^ UInt32(byte)) &* 16_777_619
        }
        return Int(hash % UInt32(accountFillPalette.count))
    }

    /// The badge FILL for a label + theme (issue #445) — the label-seeded palette hue.
    static func accountBadgeFill(for label: String, dark: Bool) -> FillRGBA {
        let slot = accountFillPalette[accountColorIndex(for: label)]
        return dark ? slot.dark : slot.light
    }

    /// The account MONOGRAM glyph color for a theme (issue #445) — a high-contrast neutral (near-white in
    /// light, near-black in dark) that carries the 2-char monogram ON the badge fill (the opaque fill is the
    /// glyph's real background). Theme-uniform across the palette; the per-account HUE lives in the FILL, so
    /// the glyph itself stays neutral and legible on every slot in both themes (asserted ≥ 4.5:1 in tests).
    static func accountMonogramColor(dark: Bool) -> FillRGBA {
        dark ? accountRGB(28, 28, 30) : accountRGB(245, 245, 247)
    }

    /// A roster-aware map of `label` → 2-char MONOGRAM (issue #445). Derived from the label's DISTINGUISHING
    /// token — NOT `label.first`, which collapses a same-local-part roster (`work-alice`, `work-bob`, … all →
    /// "W"). Collision-ESCALATING: assigned greedily in roster order, each label taking its most-distinguishing
    /// FREE candidate, so two similar labels never collapse to the same pair — the resolved set is fully
    /// DISTINCT for distinct labels. A single-token short label degenerates to its first two chars ("Work" →
    /// "WO"); a lone character is itself ("x" → "X"); an empty/whitespace label is "?".
    static func accountMonograms(_ labels: [String]) -> [String: String] {
        var result: [String: String] = [:]
        var used: Set<String> = []
        for label in labels {
            if result[label] != nil { continue }   // a duplicate label resolves once to the same monogram
            let candidate = monogramCandidates(label).first { !used.contains($0) }
                ?? uniqueMonogramFallback(label, used: used)
            result[label] = candidate
            used.insert(candidate)
        }
        return result
    }

    /// The ordered candidate monograms for a label, most-distinguishing first (issue #445): the FIRST token's
    /// initial paired with the LAST token's initial (`work-alice` → "WA" — the same-local-part case the kit
    /// targets), then first⋅second, then the identity-initial paired with each later char of the collapsed
    /// string (keeps the leading letter while escalating), then each token's own leading pair, then the
    /// collapsed leading pair. All 2-char, uppercased, de-duplicated; a lone char / empty label falls to a
    /// 1-char / "?" tail so `accountMonograms` always has a non-empty seed.
    private static func monogramCandidates(_ label: String) -> [String] {
        let tokens = monogramTokens(label)
        let collapsed = tokens.joined()
        var out: [String] = []
        func push(_ s: String) {
            if s.count == 2 && !out.contains(s) { out.append(s) }
        }
        if tokens.count >= 2 {
            push(monogramInitial(tokens[0]) + monogramInitial(tokens[tokens.count - 1]))  // first ⋅ last
            push(monogramInitial(tokens[0]) + monogramInitial(tokens[1]))                 // first ⋅ second
        }
        if let first = collapsed.first {
            let lead = String(first).uppercased()
            for ch in collapsed.dropFirst() {                                             // first ⋅ each later char
                push(lead + String(ch).uppercased())
            }
        }
        for token in tokens.reversed() { push(monogramLeadingPair(token)) }               // each token's own pair
        push(monogramLeadingPair(collapsed))
        if out.isEmpty {
            out.append(collapsed.first.map { String($0).uppercased() } ?? "?")            // lone char / empty
        }
        return out
    }

    /// Split a label into alphanumeric tokens (issue #445) — separators are any non-alphanumeric PLUS the
    /// lowercase→uppercase and letter↔digit boundaries, so `work-alice`, `work.alice`, `workAlice`, and
    /// `work1` all tokenize to their parts. Empty runs are dropped.
    private static func monogramTokens(_ label: String) -> [String] {
        var tokens: [String] = []
        var current = ""
        var previous: Character?
        for ch in label {
            guard ch.isLetter || ch.isNumber else {
                if !current.isEmpty { tokens.append(current); current = "" }
                previous = nil
                continue
            }
            if let prev = previous, monogramIsBoundary(prev, ch), !current.isEmpty {
                tokens.append(current); current = ""
            }
            current.append(ch)
            previous = ch
        }
        if !current.isEmpty { tokens.append(current) }
        return tokens
    }

    /// A camelCase / letter↔digit split point — the boundaries `monogramTokens` cuts on (beyond punctuation).
    private static func monogramIsBoundary(_ a: Character, _ b: Character) -> Bool {
        if a.isLowercase && b.isUppercase { return true }
        if a.isLetter && b.isNumber { return true }
        if a.isNumber && b.isLetter { return true }
        return false
    }

    /// A token's first character, uppercased (empty for an empty token — never passed one).
    private static func monogramInitial(_ token: String) -> String {
        token.first.map { String($0).uppercased() } ?? ""
    }

    /// A token's leading two characters, uppercased — a 1-char token yields a 1-char string that
    /// `monogramCandidates` skips (candidates must be 2 chars), so it never emits a half-pair.
    private static func monogramLeadingPair(_ token: String) -> String {
        String(token.prefix(2)).uppercased()
    }

    /// A guaranteed-UNIQUE monogram when every derived candidate is already taken (issue #445) — the first
    /// alnum char paired with a digit, then the bare char, then a "?"-series. Rarely reached (the candidate
    /// walk resolves realistic rosters), it exists so full distinctness is an INVARIANT, not a hope.
    private static func uniqueMonogramFallback(_ label: String, used: Set<String>) -> String {
        let base = monogramTokens(label).joined().first.map { String($0).uppercased() } ?? "?"
        for n in 2...9 where !used.contains(base + String(n)) { return base + String(n) }
        if !used.contains(base) { return base }
        for n in 1...99 where !used.contains("?" + String(n)) { return "?" + String(n) }
        return "?"
    }
}
