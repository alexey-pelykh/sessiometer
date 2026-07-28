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

    // MARK: - Capture CARD copy + geometry (issue #765 — the first thing a new operator ever sees)

    // WHY THESE MOVED HERE. Every string and number below was a bare literal inside `StatusPanelCapture`
    // / `StatusPanelView`. Both files now compile into `MenubarTests` (#754), so the strings were
    // *reachable* — but reachable is not the same as PINNED: a gate that re-types the copy it measures is
    // measuring its own copy, and the two drift apart the first time someone edits one of them. This is the
    // same hoist `capturePendingText` above already made for the affordance's in-flight copy, and the same
    // one issue #750 made for the meter-cell widths. Nothing here changed a rendered byte — the panel
    // goldens are untouched by the move.
    //
    // WHAT NEEDED PINNING, and why this card specifically. SwiftUI `ImageRenderer` cannot rasterize the
    // AppKit-backed `TextField` this card hosts — it draws a blank placeholder box — so the panel golden
    // gate (#754) is structurally blind to exactly the surface a first-run operator meets. Issue #765
    // closes that with two lanes that need no raster at all: the accessibility tree (#758) proves the field
    // is REACHABLE and correctly typed, and CoreText metrics (#750/#762 `TextMetrics`) prove the copy FITS.
    // Both need the shipped string and the shipped budget, from one place, which is here.

    /// The empty-roster / first-run onboarding card's title (#326/#360) — the visually distinct "you have
    /// no accounts yet" state, deliberately not the daemon-down banner.
    static let captureCardOnboardingTitle = "Capture your first account"

    /// The status-item "Add account…" surface's title (#394) — the populated-panel capture path, reached
    /// from the right-click menu now that the persistent capture bar is gone. Same card, same mechanics;
    /// the title is the ONLY difference between the two entry points.
    static let captureCardAddAccountTitle = "Add account"

    /// The card's explanatory line, under the title.
    static let captureCardExplainer =
        "Capture the account you\u{2019}re signed into — the daemon adds it to the roster and starts "
        + "tracking it here."

    /// The affordance's secondary hint — the honest scope boundary: capture snapshots the account
    /// currently logged into Claude Code, so adding a DIFFERENT one is a `claude /login` first.
    static let captureScopeHint = "To add a different account, run claude /login first, then capture."

    /// The label field's placeholder. It invites an OPTIONAL label — blank means the daemon derives the
    /// handle from the account UUID, never from the email (#15).
    static let captureFieldPlaceholder = "e.g. Work, Personal"

    /// The primary button's title at rest. (`capturePendingText` above is its in-flight replacement.)
    static let captureButtonTitle = "Capture active account"

    /// The label field's accessibility label — what VoiceOver announces, and what the #758 tree walk finds
    /// the field by. Pinned here so the a11y gate and the view read ONE string.
    static let captureFieldAccessibilityLabel = "Account label, optional"

    /// The primary button's accessibility label, which differs in flight so a VoiceOver user hears that the
    /// action is running rather than that it is still offered.
    static func captureButtonAccessibilityLabel(pending: Bool) -> String {
        pending ? "Capturing the active account" : "Capture the active account"
    }

    /// The capture card's own internal padding, in points — LINKED (`CaptureCard` lays out with this exact
    /// constant on all four edges).
    static let captureCardPadding: Double = 12

    /// The horizontal inset between the panel edge and the capture card, in points — LINKED (both call
    /// sites in `StatusPanelView` apply this exact `.padding(.horizontal,)`).
    static let captureCardHorizontalInset: Double = 12

    /// The vertical spacing between the card's stacked elements, in points — LINKED (`CaptureCard` and
    /// `CaptureAffordance` both use this exact `VStack` spacing).
    static let captureCardSpacing: Double = 9

    /// The width available to the capture card's TEXT, in points.
    ///
    /// Derived, never hand-tuned: the panel's fixed width less the card's horizontal inset on each side,
    /// less the card's own padding on each side. A LINKED budget — every input is a constant a view lays
    /// out with — so unlike `rosterLabelBudget` (which folds in two ALLOWANCES) this one is exact rather
    /// than ±10 pt, and `PanelCaptureCardTests` measures against it directly.
    ///
    /// The card's own children are full-width: the title, explainer and hint are plain `Text` with no
    /// trailing element, and the field spans the card. So there is no column arithmetic to subtract, which
    /// is why this is a two-term derivation and not a six-term one.
    static var captureCardTextBudget: Double {
        panelContentWidth - 2 * captureCardHorizontalInset - 2 * captureCardPadding
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

    // MARK: - Stats card geometry (issue #700 — the panel↔mock pin for the full-width chart row)

    /// The Stats row card's own horizontal padding (mock `.stat { padding:10px 8px }`). Held here rather
    /// than inline in the view so `statsChartWidth` below is DERIVED from the same number the view lays
    /// out with, never a second copy that can drift from it — and so the caveat strip above the rows, which
    /// insets by this padding to line its dot up with a row's status dot, tracks the same one number.
    static let statsCardHorizontalPadding: Double = 8

    /// The Stats card's chart + metrics leading inset (mock `.stat-body`/`.stat-chart { margin-left:17px }`),
    /// which starts the series where the numbers describing it start.
    static let statsChartLeadingInset: Double = 17

    /// The Stats card's chart-row width on the shipped fixed-width panel (issue #700) — the box the
    /// full-width sparkline is actually laid out in.
    ///
    /// The build-reference mock authors its `.spark` viewBox at exactly this width (`viewBox="0 0 331 28"`
    /// in `design/menubar-preview.html`), so both surfaces plot the SAME vertices instead of one stretching
    /// a 96-wide viewBox against the other's fixed 3 pt stroke inset. That agreement is the whole point, and
    /// it is only as good as this derivation: `testStatsChartWidthMatchesTheMockAuthoredViewBox` pins the value
    /// against the mock's authored literal, so a change to any panel-geometry constant above goes RED here
    /// rather than silently diverging the panel from its own design reference. The Stats tab now DOES render
    /// (`RenderPanelTool` seeds one loaded `stats` fixture for `build-comparison.py`, #704), but that pairing is
    /// a manual side-by-side review — so this test stays the only MECHANICAL parity net the chart has.
    static var statsChartWidth: Double {
        panelContentWidth - 2 * rosterHorizontalInset - 2 * statsCardHorizontalPadding - statsChartLeadingInset
    }

    // MARK: - Text-cell layout budgets (issue #750 — the widths the text-metrics gate measures against)

    // WHY THESE MOVED HERE. Each was a bare literal in a view file the headless `MenubarTests` bundle
    // deliberately excludes — so the truncation policy they enforce was unmeasurable, and
    // `PanelTextMetricsTests` had nothing to assert against. Hoisting them into this Foundation-only layer
    // is the SAME move `switchAffordanceSlotWidth` / `panelContentWidth` / `statsCardHorizontalPadding`
    // already made, and for the same stated reason: a number the view lays out with must not be a second
    // copy that can drift from the number a test checks.
    //
    // TWO KINDS LIVE HERE, and the difference is load-bearing — do not read the second kind as the first:
    //
    //   * LINKED (the view reads this exact constant, so there is no second copy):
    //     `meterLabelCellWidth`, `meterPercentCellWidth`, `meterResetCellWidth` (the three `UsageMeter`
    //     cells, also reused by `BlindMeter`), `rowHorizontalPadding`, `rowInterElementSpacing`,
    //     `rowSpacerMinLength`, `statusDotWidth`, `monogramBadgeWidth`.
    //
    //   * ALLOWANCE (no view site exists to link — the element has no fixed frame and sizes to its own
    //     content, so this is a RESERVED budget, not a pin): `authColumnAllowance`,
    //     `statsSignalPillAllowance`. These are the two inputs `rosterLabelBudget` / `statsHandleBudget`
    //     cannot verify, which is why both budgets are documented as ±10 pt and why the issue #445
    //     invariant is asserted across a RANGE rather than at one derived number.
    //
    // NOTE these are BUDGETS, not measurements — what fits in them is a font-metric question the gate
    // answers, and answers differently as the system font changes.

    /// The usage meter's leading window-name cell (`SESSION` / `WEEKLY`), uppercased at 10 pt semibold.
    /// Fixed-width so Session and Weekly rows align down the panel; `.leading`-aligned.
    static let meterLabelCellWidth: Double = 52

    /// The usage meter's percent cell (`N%` / `n/a`), 12 pt semibold with monospaced digits, `.trailing`.
    /// The widest reachable content is `255%` — `WireModel` decodes `session_pct` / `weekly_pct` as a bare
    /// `UInt8` with NO clamp, so a daemon sending 255 renders `255%` here (measured: 35.56 pt, fits).
    static let meterPercentCellWidth: Double = 40

    /// The usage meter's trailing reset-in cell (`humanizeUntil` output), 11 pt monospaced digits,
    /// `.trailing`, `.lineLimit(1)`.
    ///
    /// MEASURED BOUNDARY (issue #750): the day form is the unbounded one — the hour form maxes at `23h59m`
    /// (44.15 pt) because hours roll into days. Three-digit days still fit (`999d23h` = 48.32 pt); overflow
    /// begins at FOUR digits (`1000d23h` = 55.32 pt). `Int64.max` seconds renders `106751991167300d15h`
    /// (132.24 pt). So this cell is safe for every plausible reset instant and clips only on a wire value
    /// that is already nonsense — which is exactly what the gate reports rather than assumes.
    static let meterResetCellWidth: Double = 52

    /// The leading status dot's diameter (`StatusDot`), on a roster row and on a Stats card's head row alike.
    static let statusDotWidth: Double = 8

    /// The monogram badge's side (`MonogramBadge`, a rounded square), on both row kinds.
    static let monogramBadgeWidth: Double = 30

    /// A roster row's inner horizontal padding per side (`AccountRowView`'s horizontal padding).
    static let rowHorizontalPadding: Double = 8

    /// The `HStack` spacing charged between each adjacent pair of children on BOTH identity-bearing rows —
    /// `AccountRowView`'s six (so five gaps, the count `rosterLabelBudget` charges) and `StatStripRow`'s
    /// four (three gaps, the count `statsHandleBudget` charges).
    static let rowInterElementSpacing: Double = 9

    /// The minimum width `AccountRowView`'s trailing `Spacer` collapses to once the label wants the slack.
    static let rowSpacerMinLength: Double = 6

    /// The auth column's reserved width allowance on a roster row, in points — the glyph plus its longest
    /// action cue (`claude /login`, `recovering`, `disabled`).
    ///
    /// An ALLOWANCE, not a pin: the column has no `.frame(width:)`, and its true width depends on SF-Symbol
    /// metrics this Foundation-only layer cannot measure. The 60 pt figure is not new — it is the number
    /// `switchAffordanceMinRowWidth` above has always folded into its own published derivation ("60 (auth
    /// glyph + its longest cue)"); naming it here just stops the two derivations owning separate copies.
    static let authColumnAllowance: Double = 60

    /// The width available to a roster row's ACCOUNT LABEL on the shipped fixed-width panel — the budget the
    /// `.lineLimit(1)` + `.truncationMode(.middle)` policy (issue #445) actually elides against.
    ///
    /// Derived, never hand-tuned, from the row's fixed columns at the label's tightest: `defaultRowWidth`
    /// minus both row paddings, the 8 pt status dot, the 30 pt monogram badge, the swap slot, the five
    /// inter-element gaps the six-child `HStack` charges, the collapsed spacer, and the auth allowance. On the
    /// shipped 380 pt panel that is 364 − 16 − 8 − 30 − 28 − 45 − 6 − 60 = **171 pt**.
    ///
    /// Its weakest input is `authColumnAllowance` (see there), so treat this as ±10 pt rather than exact —
    /// `PanelTextMetricsTests` therefore asserts the issue #445 invariant across a RANGE of budgets, not only
    /// at this one value, so the invariant does not rest on the allowance being precisely right.
    static var rosterLabelBudget: Double {
        defaultRowWidth
            - 2 * rowHorizontalPadding
            - statusDotWidth
            - monogramBadgeWidth
            - switchAffordanceSlotWidth
            - 5 * rowInterElementSpacing
            - rowSpacerMinLength
            - authColumnAllowance
    }

    /// The Stats head row's trailing signal-pill allowance, in points — like `authColumnAllowance`, a
    /// reserved budget rather than a `.frame(width:)` pin (the pill `.fixedSize()`s to its own label).
    static let statsSignalPillAllowance: Double = 85

    /// The width available to a Stats card's HANDLE label on the shipped fixed-width panel — the budget
    /// issue #700 bought by moving the sparkline onto its own row, and the one its "enough for a
    /// 28-character handle untruncated" claim is about.
    ///
    /// Derived from the card's own geometry: `panelContentWidth` minus both roster insets and both card
    /// paddings gives the 348 pt card content width; the head row then spends the status dot, the monogram
    /// badge, its three `HStack` gaps, and the trailing signal pill. 348 − 8 − 30 − 27 − 85 = **198 pt**,
    /// matching the figure `StatStripRow`'s own comment records.
    ///
    /// MEASURED (issue #750): a realistic 23-character address (`oleksii@company-one.com`) needs 170.03 pt
    /// and clears it; 28 characters of the WIDE glyph `x` needs 201.70 pt and does not. So #700's claim holds
    /// for representative handles and is marginal at the wide-glyph extreme — recorded, not silently rounded.
    static var statsHandleBudget: Double {
        panelContentWidth
            - 2 * rosterHorizontalInset
            - 2 * statsCardHorizontalPadding
            - statusDotWidth
            - monogramBadgeWidth
            - 3 * rowInterElementSpacing
            - statsSignalPillAllowance
    }

    // MARK: - Identity elision policy (issue #445 — the truncation MODE, hoisted so it can be asserted)

    /// Which end(s) of an over-long identity-bearing label the panel elides.
    enum IdentityElision: Equatable {
        /// Keep the head AND the tail, elide the middle — so a same-local-part address's distinguishing
        /// DOMAIN survives.
        case middle
        /// Keep the head, elide the tail.
        case tail
    }

    /// The elision policy for every label that CARRIES AN IDENTITY — the roster account label, the Stats
    /// handle, the next-swap target, and the capture/swap result lines that name an account.
    ///
    /// Hoisted out of the views (issue #750) because it is the whole substance of issue #445 and was
    /// previously five separate `.truncationMode(.middle)` literals in files the headless test bundle
    /// excludes — so "the panel middle-truncates identities" was an ASSUMPTION no test could reach. It is
    /// now one value, asserted by `PanelTextMetricsTests`, and a change to `.tail` reddens the gate instead
    /// of silently undoing #445 across five sites.
    ///
    /// Deliberately NOT applied to non-identity text (the next-swap REASON line, the stats caveat strip):
    /// those tail-truncate, because their information is front-loaded and they name no account.
    static let identityElision: IdentityElision = .middle

    // MARK: - Usage-bar fill (issue #750 — the clamp, hoisted so it can be locked)

    /// The usage bar's fill width for `fraction` within a track of `full` points.
    ///
    /// Hoisted out of `UsageBar` so the CLAMP is assertable. It matters because nothing upstream clamps: the
    /// wire decodes `session_pct` / `weekly_pct` as a bare `UInt8`, so a daemon sending 255 yields a fraction
    /// of 2.55 — which without this `min(1,…)` would paint a capsule 2.55× its own track. A live-but-tiny
    /// percent keeps a 5 pt sliver (mock `.m-fill { min-width: 5px }`) so it never reads as empty, and a zero
    /// or failed reading shows a BARE track (#137: never a fabricated fill). The NUMBER beside the bar still
    /// reports the real value — clamping is a drawing bound, not a truth edit.
    static func meterFillWidth(fraction: Double, full: Double) -> Double {
        let clamped = min(1, max(0, fraction))
        guard clamped > 0 else { return 0 }
        return max(5, full * clamped)
    }

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
        case .neutral: return .secondary            // mock --text-2 (unknown)
        }
    }

    // MARK: - Active-account bounded-blindness row (issues #479/#485)
    //
    // The panel's per-medium render of the SAME daemon `BlindActive` the `status` CLI narrates as a line
    // (`src/cli.rs`: "active {label}: blind for {dur} — last-known session {pct}% — auto-protection {OK |
    // DEGRADED (acting on a stale anchor)}"). R-2 STATE-parity: the CLI prints one sentence; the panel
    // composes a held-meter row + verdict from these pure verdicts, each unit-asserted (the panel cannot be
    // screenshot-verified in CI). The blind row REPLACES the bare `n/a … 🟡` a failed poll would show — a
    // SEMANTIC held state, never a false-healthy row (#137) — and reflects daemon state only (#169).

    /// The three auto-protection body verdicts for a blind ACTIVE account: OK / DEGRADED (#485), plus
    /// CORNERED (#572). **Cornered = blind + DEGRADED + no viable target** — the ONE bounded-blindness
    /// state the daemon CANNOT self-resolve, so the operator must act. The panel composes the SAME two
    /// daemon verdicts the CLI's `cornered_state` does (`src/cli.rs`): `auto_protection_degraded` AND
    /// `next_swap == no_viable_target` — no new wire field. DEGRADED-but-with-a-target is NOT cornered
    /// (the daemon can still swap), and an OK (interim-window) blind is never cornered.
    enum BlindSeverity: Equatable { case ok, degraded, cornered }

    static func blindSeverity(degraded: Bool, nextSwap: NextSwap?) -> BlindSeverity {
        guard degraded else { return .ok }
        if case .noViableTarget = nextSwap { return .cornered }
        return .degraded
    }

    /// The `nextSwap` the switchable roster may compose blind verdicts from — gated on a VOUCHED connection.
    /// Only `.connected` stands behind the retained `nextSwap`; under `.stale` (the valid-frame watchdog has
    /// elapsed — the daemon has gone quiet past the liveness window, but the last-good snapshot is still
    /// shown) that `nextSwap` is unvouched, so it is WITHHELD (`nil`). Withholding degrades a would-be
    /// CORNERED row to DEGRADED via `blindSeverity` above — a retained `noViableTarget` must never raise the
    /// loud red "cannot act" alarm off data the connection no longer vouches for — keeping the panel body's
    /// severity in step with the `.stale` `!` glance rather than inverting past it (#137, #572). Every other
    /// (non-switchable) roster already passes no `nextSwap`; this is the one switchable-path gate.
    static func rosterNextSwap(for state: ConnectionState, nextSwap: NextSwap?) -> NextSwap? {
        state == .connected ? nextSwap : nil
    }

    /// The eye-slash health glyph + its tint, keyed off `BlindSeverity`. OK is calm (`.neutral`), DEGRADED
    /// at-risk `.orange`. NOTE on the DEGRADED colour (#485): the CLI emphasizes its DEGRADED blind line in
    /// RED (`Severity::Red`, `src/cli.rs`); the panel deliberately uses ORANGE — the blind-DEGRADED GLANCE
    /// is `.attention`, one rung below `.noRunway`, so red would over-signal (a per-medium COLOUR choice
    /// under R-2 STATE-parity). CORNERED, however, IS `.red` (#572): its glance IS `.noRunway` (⊘), the
    /// worst rung, so red MATCHES the glance severity rather than over-signalling.
    static func blindSymbol(_ severity: BlindSeverity) -> (name: String, tint: HealthTint) {
        switch severity {
        case .ok:       return ("eye.slash", .neutral)
        case .degraded: return ("eye.slash", .orange)
        case .cornered: return ("eye.slash", .red)
        }
    }

    /// Whether a blind row should ALSO show its credential's own auth warning glyph beside the `eye.slash`.
    /// Usage-blindness and credential-health are ORTHOGONAL axes (a 429'd `/usage` poll says nothing about
    /// the refresh token), so a blind account whose credential is itself in a WARNING state
    /// (stale / at-risk / degraded / dead) must not have that warning SUPPRESSED just because the eye-slash
    /// took the slot — the CLI keeps both (its health cell is untouched by the blind override), and hiding a
    /// real credential signal is the #137 honest-state failure one axis over. Healthy / unknown add no
    /// warning, so the eye-slash stands alone (the common, ratified case). Reachable pair today: blind +
    /// `stale`/`atRisk` (the daemon suppresses `blind_active` only for a QUARANTINED account, and `degraded`/
    /// `dead` imply quarantined — so those two never co-occur in practice, but are covered defensively).
    static func blindCoShowsAuthWarning(_ auth: CredentialHealth?) -> Bool {
        switch auth {
        case .stale, .atRisk, .degraded, .dead: return true
        case .healthy, .unknown, nil:           return false
        }
    }

    /// The blind row's duration chip — `blind {dur}`, using the SAME `humanizeUntil` the CLI's
    /// `blind for {dur}` uses (`blind_secs` is a DURATION, rendered against nothing — no client clock).
    /// Replaces the reset-in cell, which is meaningless while the poll is blind.
    static func blindDurationChip(_ blindSecs: UInt64) -> String {
        "blind \(humanizeUntil(Int64(blindSecs)))"
    }

    /// The blind row's under-bar caption — WHY the meter is HELD: the value is the LAST-KNOWN reading and
    /// the poll is RATE-LIMITED (ADR-0017 bounded blindness is entered on a 429). A constant, so the held
    /// bar is never mistaken for a live one (the #137 never-false-healthy tell, carried onto the caption).
    static let blindLastKnownCaption = "LAST-KNOWN · RATE-LIMITED"

    /// One auto-protection body verdict: the shield glyph, the spoken verdict text, its tint, and — for
    /// CORNERED only — a second `remedy` sub-line. OK/DEGRADED carry `remedy == nil` (single-line, as #485
    /// shipped).
    struct BlindVerdict: Equatable {
        let symbol: String
        let text: String
        let tint: HealthTint
        /// The cornered "Out of capacity … · add or free an account" sub-line; `nil` for OK/DEGRADED.
        let remedy: String?
    }

    /// The auto-protection verdict line(s) for a blind active account, keyed off `BlindSeverity`
    /// (issue #479 surface 1; #485 OK/DEGRADED, #572 CORNERED) — the panel's render of the CLI's
    /// `auto-protection {OK | DEGRADED (acting on a stale anchor) | cannot act — …}`. OK is calm
    /// (`.neutral` — the CLI leaves OK un-emphasized); DEGRADED is the at-risk `.orange` fault
    /// ("acting on a stale anchor" mirrors the CLI parenthetical verbatim); CORNERED is the loudest,
    /// `.red` "Auto-protection CANNOT ACT" + the operator remedy (`corneredRemedy`), the panel half of
    /// the CLI's `render_cornered`. `nextSwap`/`now` are read only in the cornered branch (to fold the
    /// reset into the remedy); OK/DEGRADED ignore them.
    static func blindVerdict(_ severity: BlindSeverity, nextSwap: NextSwap?, now: Int64) -> BlindVerdict {
        switch severity {
        case .ok:
            return BlindVerdict(symbol: "checkmark.shield.fill",
                                text: "Auto-protection OK — daemon self-resolving", tint: .neutral, remedy: nil)
        case .degraded:
            return BlindVerdict(symbol: "exclamationmark.shield.fill",
                                text: "Auto-protection DEGRADED — acting on a stale anchor", tint: .orange, remedy: nil)
        case .cornered:
            return BlindVerdict(symbol: "xmark.shield.fill",
                                text: "Auto-protection CANNOT ACT", tint: .red,
                                remedy: corneredRemedy(nextSwap, now: now))
        }
    }

    /// The cornered remedy sub-line: `Out of capacity[, resets in {dur}] · add or free an account`. The
    /// remedy is **UNCONDITIONAL** — cornered is always unresolvable, so the CLI's `render_cornered`
    /// appends "add or free an account" unconditionally (issue #666), UNLIKE the general all-exhausted
    /// `nextSwapFooter` whose "add an account" nudge is gated on a structural-vs-transient wait. The
    /// wording is the **unified** "Out of capacity" — NOT a weekly/session split: on a mixed fleet the
    /// daemon's `cause` names the soonest spare's gating dimension, not a fleet-wide property (#665/#666),
    /// so the panel says only what the wire substantiates. `resetsAt` folds in via the same `humanizeUntil`
    /// the reset cells use; a daemon that sent no reset (or a non-`noViableTarget` next-swap, which the
    /// cornered branch never reaches) yields the bare remedy.
    static func corneredRemedy(_ nextSwap: NextSwap?, now: Int64) -> String {
        "Out of capacity\(corneredReliefClause(nextSwap, now: now)) · add or free an account"
    }

    /// The optional ", resets in {dur}" clause folded into BOTH the visual cornered remedy (`corneredRemedy`)
    /// and its VoiceOver phrasing (`rowAccessibilityLabel`) — ONE source so the two surfaces never drift on
    /// the reset wording. Empty unless the `noViableTarget` next-swap carries a reset instant; `humanizeUntil`
    /// clamps a passed reset (`<= 0` → "now") exactly as the reset cells do. The `cause` is deliberately
    /// ignored (the `_`): the panel's cornered wording is cause-INDEPENDENT — always "Out of capacity" + this
    /// clause, the ratified #666 unified framing. The CLI's `render_cornered` (`src/cli.rs`) instead keeps a
    /// `cause == nil → "no viable target"` fallback (dropping this clause). The CURRENT daemon always pairs a
    /// `cause` (and `resetsAt`) with a `noViableTarget`, but a pre-#405 daemon omits both (`WireModel` tolerates
    /// it via `decodeIfPresent`), reaching that arm — so against such a daemon the two surfaces diverge in
    /// WORDING ("Out of capacity" vs "no viable target"). Under R-2 STATE-parity that is an accepted per-medium
    /// choice, NOT a parity break: both convey the cornered state and the identical "add or free an account"
    /// remedy; the panel keeps its unified #666 wording rather than replicating the CLI's legacy fallback.
    static func corneredReliefClause(_ nextSwap: NextSwap?, now: Int64) -> String {
        guard case .noViableTarget(_, let resetsAt) = nextSwap, let at = resetsAt else { return "" }
        return ", resets in \(humanizeUntil(at - now))"
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

    // MARK: - AUTH cell, cont. (mirror `src/cli.rs` `health_cell` / `legacy_health_tags`)

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
        case .reconnecting:
            // The warm-dwell transient banner (#526): a routine drop still WITHIN the dwell — calmer than the
            // escalated `.disconnected` (`.warning`, not `.error`) with self-resolving copy, so the panel matches
            // the calm "…" glance the glyph shows during the dwell. Retains the last-known reading's age, like
            // `.disconnected` / `.stale`, so the dimmed roster is honestly dated. The title already carries the
            // "reconnecting" fact, so the detail complements it with the reading's provenance rather than
            // echoing it (the sibling banners split title/detail the same way).
            let base = "Showing last-known"
            return Banner(title: "Reconnecting…",
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
            // The not-running banner (#499 / #170): the daemon is absent, so numbers are not trustworthy
            // (`.error`, like `.disconnected` / `.unsupported`). #170 wires the Start-daemon affordance
            // (`StartDaemonCard`) beside this copy; the detail matches the design mock's not-running card.
            // The card shows a "Start daemon" button only where it can act (`LoginItemModel.canStartDaemon`
            // — #171 ships the bundled agent); until then it degrades honestly to this line alone.
            return Banner(title: "Daemon not running",
                          detail: "The background service isn’t running. Start it to resume live status.",
                          kind: .error)
        }
    }

    // MARK: - Start-daemon affordance copy (issue #170, beside the not-running banner)

    /// The "Start daemon" button title — mirrors the design mock's not-running card.
    static let startDaemonButtonTitle = "Start daemon"
    /// The in-flight label while the SMAppService agent registration is pending (the mock's transient beat).
    static let startDaemonPendingText = "Starting…"
    /// The reassurance hint beneath the button: starting the daemon is a runtime/lifecycle action that
    /// touches no credential (issue #15 redaction discipline) — the product half of the mock's `msg-hint`.
    static let startDaemonHint = "Start is a runtime action — it touches no credentials."

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
    /// `>= 90` Red (at/near the ~95% session swap-away ceiling, #41), `>= 75` Yellow (worth watching),
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
    /// week-blocked account is never painted "healthy", even under a lowered `weekly_ceiling`. `nil`
    /// when the weekly poll failed. Mirrors the CLI's `weekly_cell_severity`.
    static func weeklySeverity(weeklyPct: UInt8?, weeklyExhausted: Bool) -> UsageSeverity? {
        weeklyPct.map { weeklyExhausted ? .red : utilSeverity($0) }
    }

    // MARK: - `next_swap` footer (issue #326 AC — renders the FORWARD candidate, not swap history)

    /// The wait beyond which `nextSwapFooter`'s all-exhausted footer nudges "· add an account" — ONE
    /// session window (issue #666). Capacity returning within a session window is a TRANSIENT block the
    /// operator waits out; a longer — or unknown-duration — wait is a STRUCTURAL shortage where adding
    /// capacity is the remedy. Replaces the pre-#666 `NoTargetCause`-label proxy (`weekly` ⇒ nudge,
    /// `session` ⇒ silent), which mis-fired on a MIXED fleet where a `weekly` cause can name a sub-hour
    /// weekly reset (issue #665). Lockstep twin of the CLI `ADD_ACCOUNT_NUDGE_WAIT_SECS` (`src/cli.rs`)
    /// — both clients must render the SAME nudge decision (R-2 STATE-parity).
    static let addAccountNudgeWaitSecs: Int64 = 5 * 60 * 60

    /// The footer line for the daemon's `next_swap` candidate, or `nil` when there is no active anchor
    /// to swap from (the footer is then absent). Renders the FORWARD candidate the `watch` wire carries
    /// — NOT swap history (a true last-swap needs a new daemon source; issue #326 note).
    ///
    /// A `noViableTarget` carrying the #405 fleet-capacity relief renders it the panel's own concise
    /// way (R-2 STATE-parity — the SAME facts as the CLI's `next swap: none …` footer, not the same
    /// bytes): "Out of capacity" — never the pre-#666 false universal "every account is weekly-exhausted
    /// / over its session limit" (on a MIXED fleet the daemon's `cause` names the gating dimension of the
    /// soonest-returning spare, issue #665, NOT a fleet-wide property) — with the reset when the daemon
    /// knew it, and the "· add an account" nudge ONLY when the wait is structural (longer than one
    /// session window, or unknown) rather than transient (issue #666, gated on the WAIT not the `cause`
    /// label). A pre-#405 daemon (no `cause`) falls back to the bare "No viable target".
    static func nextSwapFooter(_ nextSwap: NextSwap?, now: Int64) -> String? {
        switch nextSwap {
        case .target(let to, _):
            return "Next swap → \(to)"
        case .noViableTarget(let cause, let resetsAt):
            switch cause {
            case nil:
                return "No viable target"
            case .session, .weekly:
                let relief = resetsAt.map { " — resets in \(humanizeUntil($0 - now))" } ?? ""
                // Nudge unless capacity is KNOWN to return within one session window: a sub-window
                // wait is transient (no nudge); a longer OR unknown-duration wait is structural.
                let structuralShortage = resetsAt.map { $0 - now > addAccountNudgeWaitSecs } ?? true
                let nudge = structuralShortage ? " · add an account" : ""
                return "Out of capacity\(relief)\(nudge)"
            }
        case .awaitingData:
            return "Awaiting data"
        case nil:
            return nil
        }
    }

    // MARK: - `canonical_scrub` banner (issue #469 — the fleet-wide scrubbed-canonical signal)

    /// The honest-state BANNER for the daemon's `canonical_scrub` rollup (`WireModel.swift`
    /// `CanonicalScrub`, wire #516), or `nil` when the shared canonical is healthy (the wire key is
    /// absent → no banner, same single-cardinality as `nextSwapFooter(nil)`). The shared
    /// `Claude Code-credentials` canonical item has been SCRUBBED — every `claude` session is logged
    /// out — the fleet-wide lockout NO per-account `auth` cell reflects (each row can read perfectly
    /// healthy while the shared item sits emptied), so no roster glyph carries it; only this daemon-level
    /// banner does. The View renders it ABOVE the roster in the `.connected` / `.stale` body, so a
    /// connected-but-scrubbed panel reads visibly DEGRADED (never healthy) while the live rows still show.
    ///
    /// Content-parity with the CLI's `shared login: scrubbed …` line (`src/cli.rs` `render_status`): the
    /// SAME state and the SAME `claude /login` remedy, each medium phrasing it its own way (R-2
    /// STATE-parity, as ADR-0016 did for `ActiveDeadNoTarget` / `nextSwapFooter`). `.exhausted` → an
    /// `.error` banner naming the state AND the actionable remedy (the un-recoverable residual that needs
    /// a re-login); `.recovering` → a calm `.info` banner with NO remedy (the daemon may self-heal by
    /// adopting a live account, so a re-login prompt would cry wolf). A fleet-wide STATE discriminant
    /// only — never per-account, never a token or email (issue #15). The remedy verb is the established
    /// `claude /login` cue the dead-credential `authCell` already uses — deliberately, so the operator
    /// meets ONE re-login verb.
    static func canonicalScrubBanner(_ scrub: CanonicalScrub?) -> Banner? {
        switch scrub {
        case .exhausted:
            return Banner(title: "Shared login scrubbed",
                          detail: "Every session is logged out — run claude /login.",
                          kind: .error)
        case .recovering:
            return Banner(title: "Shared login scrubbed",
                          detail: "Recovering automatically — no action needed.",
                          kind: .info)
        case nil:
            return nil
        }
    }

    // MARK: - `keychain_locked` banner (issue #498 — the fleet-wide unreadable-credential signal)

    /// The honest-state BANNER for the daemon's `keychain_locked` flag (`WireModel.swift`
    /// `keychainLocked`, wire #521), or `nil` when the login keychain is unlocked (the wire key is absent
    /// → no banner, same single-cardinality as `canonicalScrubBanner(nil)`). The macOS login keychain is
    /// LOCKED, so the daemon cannot READ the shared `Claude Code-credentials` item at ALL (access denied)
    /// — the fleet-wide unreadable-credential lockout NO per-account `auth` cell reflects (each row can
    /// read perfectly healthy while the shared item sits unreadable), so no roster glyph carries it; only
    /// this daemon-level banner does. The View renders it ABOVE the roster in the `.connected` / `.stale`
    /// body, so a connected-but-locked panel reads visibly DEGRADED (never healthy) while the live rows
    /// still show.
    ///
    /// The daemon-level SIBLING of `canonicalScrubBanner`, but for an UNREADABLE item rather than a
    /// readable-but-scrubbed one — so the REMEDY DIFFERS: UNLOCK THE KEYCHAIN, never `claude /login` (a
    /// re-login cannot help while the keychain that STORES the credential is locked). The design SSOT
    /// (`design-menubar.md`, the 9-state map) calls this the "actionable shape, waiting for unlock".
    /// Always an `.error` banner — a bare binary state with no calm/self-heal variant like the scrub's
    /// `.recovering` (the daemon stays blocked until the operator unlocks). Content-parity with the CLI's
    /// `shared login: unreadable …` line (`src/cli.rs` `render_status`): the SAME state and the SAME
    /// unlock remedy, each medium phrasing it its own way (R-2 STATE-parity, as ADR-0016 did for
    /// `ActiveDeadNoTarget`). A fleet-wide STATE discriminant only — never per-account, never a token or
    /// email (issue #15).
    static func keychainLockedBanner(_ locked: Bool) -> Banner? {
        guard locked else { return nil }
        return Banner(title: "Keychain locked",
                      detail: "The login keychain is locked — unlock it to read the shared login.",
                      kind: .error)
    }

    // MARK: - `systemic_refresh_failure` banner (issue #523 — the refresh-MECHANISM-down signal)

    /// The honest-state BANNER for the daemon's `systemic_refresh_failure` count (`WireModel.swift`
    /// `systemicRefreshFailure`, wire #378), or `nil` when the refresh mechanism is healthy (the wire key is
    /// absent → no banner, same single-cardinality as `canonicalScrubBanner(nil)`). `consecutive` refresh
    /// SWEEPS in a row have failed with `outcome=error` for EVERY eligible account — the refresh MECHANISM
    /// is down (a stale pinned `claude` path #375, a wedged spawn), not one account's credentials.
    ///
    /// The third daemon-level payload fault, and the one no per-account `auth` cell reflects even in
    /// PRINCIPLE: the other two are lockouts the rows merely fail to mention, but this one is visible
    /// BEFORE any account dies — that is the entire point of #378 (the #375 incident kept a total refresh
    /// outage invisible for ~4.5 h, until a token finally expired and the account was quarantined 🔴). So a
    /// connected panel with a full green roster is EXACTLY the state this banner exists to contradict.
    ///
    /// `.warning`, not `.error` — the deliberate severity split from its two `.error` siblings: a scrubbed
    /// or unreadable vault means the operator is blocked NOW, while a down refresh mechanism is PRE-DEATH
    /// (every account still works; they will lapse later if it stays down). It cannot self-heal either, so
    /// it is a real next-break task — never dismissible chrome. The same "act at your next break" rung the
    /// menu-bar glyph gives it (`!` `.attention`, issue #520), the two vault faults getting `⊘` `.noRunway`.
    ///
    /// Content-parity with the CLI's `refresh mechanism: DOWN — …` line (`src/cli.rs` `render_status`): the
    /// SAME state, the SAME count, and the SAME diagnostic remedy, each medium phrasing it its own way (R-2
    /// STATE-parity, as ADR-0016 did for `ActiveDeadNoTarget` / `nextSwapFooter`) — the CLI spells the
    /// remedy out for a terminal reader; the panel keeps it to the one line a popover affords. The noun
    /// agreement matches the CLI's at the `n=1` floor (a threshold of 1 fires on the first all-error sweep
    /// → "1 consecutive sweep"). Carries only the COUNT and a FIXED-TOKEN provenance class — never a
    /// token, path, or email (issue #15).
    ///
    /// The DOWN verdict is one state, but its EVIDENCE has three shapes, and issue #813 stopped this banner
    /// from citing the wrong one. `source` says which of the episode's opening brackets opened it — what
    /// each arm SAYS is below; the posture behind the split lives with `SystemicRefreshSource`:
    ///
    /// - `.sweep` — and `nil`, a pre-#813 daemon that sends no provenance at all: nothing better is
    ///   available there, it is what that daemon's own client always showed, and changing it would regress
    ///   an old daemon for no gain. The count IS a sweep count and reads exactly as before.
    /// - `.preflight` — ZERO sweeps have run; the count is a seeded floor of one kept only for pre-#813
    ///   grammar, so it is not cited at all and the preflight is named instead.
    /// - `.unrecognized` — a NEWER daemon opened the episode with a bracket this build has never heard of,
    ///   so the banner claims NO evidence: verdict and remedy only. Reusing the sweep phrasing there would
    ///   re-create #813's defect in a build too old to know better.
    ///
    /// The `.preflight` and `.unrecognized` arms make NO claim about sweeps having or not having run since
    /// — such an episode still clears only on a working sweep, so all-error sweeps may have run meanwhile;
    /// "no sweep has run" would swap one fabrication for another. The CLI's line splits on the same seam
    /// for the two arms a daemon of this vintage can send, so the two surfaces stay parallel.
    ///
    /// The CLI has no `.unrecognized` counterpart, and NOT because it cannot be older than the daemon it
    /// reads — it can (`status` dials a fixed, version-agnostic socket, and since #171/#269 the daemon
    /// ships embedded in the app while the CLI can come from Homebrew, so an updated app beside an older
    /// `brew` CLI is a real topology). It is a deliberate POSTURE split: serde rejects an unknown variant,
    /// so an unreadable bracket costs the Rust reader the whole frame — the refuse-don't-mis-render stance
    /// every wire enum there takes (`canonical_scrub`, `next_swap.state`, `auth`; none carry
    /// `#[serde(other)]`). A terminal reader gets an error and re-runs; a menu-bar panel would just go
    /// blank, which is why this client tolerates and degrades instead. Same goal, opposite mechanism.
    static func systemicRefreshFailureBanner(_ consecutive: UInt32?,
                                             source: SystemicRefreshSource? = nil) -> Banner? {
        guard let consecutive else { return nil }
        let detail: String
        switch source {
        case .preflight:
            detail = "The startup preflight could not resolve the claude binary — check the daemon log."
        case .unrecognized:
            // "cannot read the cause", NOT "this app is older than the daemon": the skew is the
            // overwhelmingly likely cause but it is an INFERENCE, and the a11y label states the
            // weaker claim — two surfaces documented as reading off one seam must not assert
            // different-strength things. The likely cause lives in the type's doc, not the string.
            detail = "This app cannot read the cause — check the daemon log."
        case .sweep, nil:
            let sweeps = consecutive == 1 ? "sweep" : "sweeps"
            detail = "\(consecutive) consecutive \(sweeps) failed for every eligible account — check the daemon log."
        }
        return Banner(title: "Refresh mechanism down", detail: detail, kind: .warning)
    }

    // MARK: - `canary` banner (issue #714 — the behavioral-canary identity-drift signal)

    /// The honest-state BANNER for the daemon's behavioral-canary verdict (`WireModel.swift` `CanaryStatus`,
    /// wire #714), or `nil` for the quiet verdicts / no verdict. The keychain-derivation identity check found
    /// the resolved credential no longer uniquely-and-correctly points at the displayed active account, so the
    /// daemon refuses credential writes (swaps AND auto-protection) — a fault NO per-account `auth` cell
    /// reflects (each row can read perfectly healthy while the shared credential's IDENTITY has drifted), so no
    /// roster glyph carries it; only this daemon-level banner does. The View renders it ABOVE the roster in the
    /// `.connected` / `.stale` body, so a connected-but-drifted panel reads visibly DEGRADED (never healthy)
    /// while the live rows still show.
    ///
    /// Content-parity with the CLI's `keychain canary: …` line (`src/cli.rs` `render_canary`): the SAME state,
    /// the SAME labels/count, and the SAME override remedy (each verdict naming its OWN tunable), each medium
    /// phrasing it its own way (R-2 STATE-parity, as ADR-0016 did for the other daemon-payload faults).
    /// Four ALARM shapes:
    ///   * `drift` NOT overridden → `.error`: the resolved credential belongs to `matched`, not the
    ///     named-active `displayed`; writes are REFUSED. The act-now severity of the vault pair.
    ///   * `ambiguous` → `.error`: more than one keychain item matches, so there is no unique write target;
    ///     writes are REFUSED. Also act-now.
    ///   * `refused_unparseable_canonical` (#730/#738) → `.error`: the resolved item matches no stash and is
    ///     not in Claude Code's format, so it is probably an unrelated secret; writes are REFUSED rather than
    ///     clobber it. Also act-now. Its remedy names `canary_nostashmatch_override` — a SEPARATE switch from
    ///     the drift one, so the two must never be cross-quoted.
    ///   * `drift` overridden → `.warning`: the drift stands, but `canary_drift_override` lets writes proceed
    ///     (each logged) — a standing, operator-acknowledged alarm, not a block. Next-break severity.
    /// The quiet verdicts (`ok` / `inconclusive` / `not_found`) and no verdict (`nil`) → no banner: `ok` /
    /// `inconclusive` are the quiet normal, and `not_found` is already voiced by the `canonical_scrub` /
    /// `keychain_locked` machinery (a second banner would double-report the same absent credential — the same
    /// reason the CLI's `render_canary` prints nothing for it). Operator LABELS and a COUNT only — never a
    /// token, email, or account-uuid (issue #15).
    ///
    /// Split across `daemonFaultBanner`'s rank arms because the drift variants are NOT one severity (like the
    /// scrub's `exhausted` / `recovering` split): the REFUSAL trio sits at ranks 3-5 (act-now `.error`) while
    /// an OVERRIDDEN drift sits at rank 7 (next-break `.warning`), SEPARATED by systemic-refresh — severity
    /// ranks by (fault, VARIANT), never by fault identity (#575).
    static func canaryBanner(_ canary: CanaryStatus?) -> Banner? {
        switch canary {
        case .drift(let displayed, let matched, let overridden):
            if overridden {
                return Banner(title: "Keychain identity drift",
                              detail: "The active credential belongs to \(matched), not \(displayed) — canary_drift_override is set, so writes proceed and are logged.",
                              kind: .warning)
            }
            return Banner(title: "Keychain identity drift",
                          detail: "The active credential belongs to \(matched), not \(displayed) — credential writes are refused (false alarm? set canary_drift_override and restart the daemon).",
                          kind: .error)
        case .ambiguous(let count):
            return Banner(title: "Keychain identity ambiguous",
                          detail: "\(count) duplicate keychain items found (expected one) — credential writes are refused until the extras are removed.",
                          kind: .error)
        case .refusedUnparseableCanonical:
            // #738: the #730 fail-CLOSED refuse, given the banner it never had. `.error` — the SAME
            // act-now rank as `CanaryDriftRefusing`, because it blocks credential writes identically;
            // there is no lower-severity sibling to confuse it with, since the operator's override
            // makes the daemon send `inconclusive` instead of this verdict. Content-parity with the
            // CLI's `keychain canary: unrecognized credential …` line: same evidence, same refusal,
            // same remedy — and the remedy names `canary_nostashmatch_override`, NOT the drift
            // override, which cannot clear this case.
            return Banner(title: "Unrecognized keychain credential",
                          detail: "The keychain item matches no stashed account and is not in Claude Code's own format — it is probably an unrelated secret, so credential writes are refused rather than overwrite it (vetted it as safe? set canary_nostashmatch_override and restart the daemon).",
                          kind: .error)
        case .ok, .inconclusive, .notFound, nil:
            return nil
        }
    }

    /// The single worst-first daemon-level fault banner for the `.connected` / `.stale` body — the panel
    /// shows ONE banner even when multiple daemon-level faults are set. EIGHT ranks over FOUR faults, because
    /// canonical-scrub AND the canary each split by VARIANT rather than occupying one slot (the canary alone
    /// spans four of the eight):
    ///
    ///   1. **keychain-locked** (#498) — `.error`, act now
    ///   2. **canonical-scrub `exhausted`** (#469) — `.error`, act now
    ///   3. **canary `drift` refusing** (#714) — `.error`, act now
    ///   4. **canary `ambiguous`** (#714) — `.error`, act now
    ///   5. **canary `refused_unparseable_canonical`** (#730/#738) — `.error`, act now
    ///   6. **systemic-refresh-failure** (#523) — `.warning`, next break
    ///   7. **canary `drift` overridden** (#714) — `.warning`, next break
    ///   8. **canonical-scrub `recovering`** (#469) — `.info`, calm; no action needed
    ///
    /// This order is pinned to the CLI's single canonical rank (`src/cli.rs` `DaemonPayloadFault::severity` +
    /// its enum declaration order): each surface renders in its own medium — an SGR line vs a banner tint —
    /// but the RANK must agree (R-2 rank-parity), and #575 caught the two surfaces ranking in OPPOSITE order
    /// precisely because each re-derived the rank independently.
    ///
    /// Ranks 1-5 are the "act now" band: the vault pair (an UNREADABLE shared item, ordered first because
    /// unlock-the-keychain must precede the scrub's `claude /login`, which cannot help while the keychain is
    /// locked; then the readable-but-SCRUBBED `exhausted`) PLUS the canary REFUSAL TRIO (a refusing drift, an
    /// ambiguous resolution, and the #730/#738 unparseable canonical — credential writes, swaps AND
    /// auto-protection, are blocked NOW, the same operator urgency; the unparseable refusal sorts last of the
    /// three because the other two are POSITIVE identity failures while it is precautionary).
    /// Systemic-refresh ranks under all five because it is PRE-DEATH — the act-now band
    /// blocks the operator now, while a down refresh mechanism leaves every account still working (a next-break
    /// task, `.warning`). It genuinely arbitrates rather than tie-breaks: it can coincide with a canary or scrub
    /// fault (the refresh mechanism spawns `claude` while the vault/identity live in the keychain).
    ///
    /// **Why the OVERRIDDEN drift and `recovering` rank BELOW systemic — the load-bearing subtlety.** Neither
    /// the canary's nor the scrub's two variants are one severity: a refusing drift / `exhausted` is an act-now
    /// block, but an OVERRIDDEN drift is a standing acknowledged alarm (writes proceed) and `recovering` is the
    /// calm self-healing state whose whole message is "no action needed". Ranking a fault as ONE slot by its
    /// identity would silently promote its calm variant above systemic — and #575 showed exactly that failure:
    /// a `recovering` scrub coinciding with a down refresh mechanism made the surfaces CONTRADICT each other
    /// (the glance shouted `!` at systemic while a fault-identity rank answered the click with a grey "no action
    /// needed"). Severity must therefore rank by (fault, VARIANT), never by fault identity — a self-healing or
    /// operator-overridden state can never outrank one that cannot self-heal / is refusing NOW.
    ///
    /// `nil` when all four are healthy (no banner). Keeps the worst-first order a testable pure function rather
    /// than a `??` chain buried in the View.
    static func daemonFaultBanner(keychainLocked: Bool,
                                  scrub: CanonicalScrub?,
                                  systemicRefreshFailure: UInt32? = nil,
                                  systemicRefreshSource: SystemicRefreshSource? = nil,
                                  canary: CanaryStatus? = nil) -> Banner? {
        // Ranks 1-2 — the "act now" vault pair.
        if let locked = keychainLockedBanner(keychainLocked) { return locked }
        if case .exhausted = scrub { return canonicalScrubBanner(scrub) }
        // Ranks 3-4 — the #714 canary REFUSAL pair (act-now `.error`): a refusing (non-overridden) drift and
        // an ambiguous resolution both block credential writes NOW, the same operator urgency as the vault pair.
        if case .drift(_, _, let overridden) = canary, !overridden { return canaryBanner(canary) }
        if case .ambiguous = canary { return canaryBanner(canary) }
        // Rank 5 — the #730/#738 unparseable-canonical refusal, closing the act-now canary TRIO: it
        // blocks credential writes exactly as ranks 3-4 do. Its position AFTER the two drift/ambiguous
        // arms is a reading-order convention mirroring the CLI's `DaemonPayloadFault` declaration, not
        // runtime arbitration — `canary` holds ONE verdict, so this arm and those can never both match.
        // The real arbitration is against the OTHER faults (the vault pair above, systemic below),
        // which is what the rank tests exercise. It needs no overridden twin below systemic: the
        // operator's canary_nostashmatch_override makes the daemon send `inconclusive`, reaching no arm.
        if case .refusedUnparseableCanonical = canary { return canaryBanner(canary) }
        // Rank 6 — the "next break" mechanism fault, ABOVE the overridden-drift and calm-scrub arms below.
        // The provenance (issue #813) only picks the banner's EVIDENCE clause — it never moves this
        // rank. A down mechanism is the same next-break fault however its episode opened.
        if let systemic = systemicRefreshFailureBanner(systemicRefreshFailure,
                                                       source: systemicRefreshSource) {
            return systemic
        }
        // Rank 7 — an OVERRIDDEN drift (next-break `.warning`): the identity alarm stands, but the operator's
        // canary_drift_override lets writes proceed (each logged), so it ranks BELOW systemic and ABOVE the
        // calm recovering scrub. Only the overridden variant moved down — the refusal trio stays at ranks 3-5.
        if case .drift(_, _, let overridden) = canary, overridden { return canaryBanner(canary) }
        // Rank 8 — `recovering` (or nothing): the calm self-healing state has the lowest claim on the one
        // banner slot, precisely because it is the one that says no action is needed.
        return canonicalScrubBanner(scrub)
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
        case .disconnected, .reconnecting: return "\(count) · last-known"   // #526: both warm drops show the retained roster
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
    /// the client could not honestly re-derive anyway (the daemon-only session ceiling / floor never
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

    /// The swap-callout's spoken VoiceOver label — the card's ONE accessible sentence: identity plus the
    /// daemon's "why" (when present). Independent of the card's VISUAL lead by design (#698): the visual
    /// shows a bare `→ <target>` — the adjacent Swap button names the verb and the arrow saves the width the
    /// target needs — but VoiceOver reads this text element on its own, with none of that adjacent context,
    /// so it must speak the whole `"Next swap to <target>"` sentence. Omits the reason clause for a pre-#393
    /// daemon (`reason == nil`) so VoiceOver never speaks a dangling `". ."` where the "why" is absent.
    /// Lifted out of `SwapCalloutCard`'s `private var` (#702) so the #698 spoken-label invariant is guarded
    /// by a direct unit test rather than resting on code review.
    static func swapCalloutAccessibilityLabel(target: String, reason: String?) -> String {
        if let reason {
            return "Next swap to \(target). \(reason)."
        }
        return "Next swap to \(target)."
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
        weeklyReset: String,
        blind: BlindActive? = nil,
        nextSwap: NextSwap? = nil,
        now: Int64 = 0
    ) -> String {
        var parts: [String] = [label]
        if isActive { parts.append("active") }
        parts.append(authSpoken(auth: auth, recovering: recovering, enabled: enabled, quarantined: quarantined))
        if let blind = blind {
            // Blind active row (#485): speak the SEMANTIC held state the row shows — blind duration,
            // last-known session %, and the auto-protection verdict — in place of the two `n/a` meters the
            // row no longer draws. Mirrors the CLI's spoken facts (blind for {dur} · last-known {pct} · OK/
            // DEGRADED/CANNOT-ACT); never a fabricated live reading (#137).
            parts.append("blind for \(humanizeUntil(Int64(blind.blindSecs)))")
            parts.append("last-known session \(blind.lastKnownSessionPct) percent")
            switch blindSeverity(degraded: blind.autoProtectionDegraded, nextSwap: nextSwap) {
            case .ok:
                parts.append("auto-protection okay, daemon self-resolving")
            case .degraded:
                parts.append("auto-protection degraded, acting on a stale anchor")
            case .cornered:
                // #572: speak the cornered verdict AND the remedy — a VoiceOver user must HEAR "add or free
                // an account", not the understated "degraded" the pre-#572 label spoke for this state. The
                // reset clause shares `corneredReliefClause` with the visual remedy so the two never drift.
                let relief = corneredReliefClause(nextSwap, now: now)
                parts.append("auto-protection cannot act, out of capacity\(relief), add or free an account")
            }
        } else {
            // Both windows, each with its reset — matching the row's two meters and the CLI's two columns.
            parts.append("session \(pct(sessionPct)) resets in \(sessionReset)")
            parts.append("weekly \(pct(weeklyPct)) resets in \(weeklyReset)")
        }
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
    ///
    /// The rounding RULE is part of the mirror, not an incidental detail: Swift's default `.rounded()`
    /// (to-nearest, ties away from zero) is exactly Rust's `f64::round`, so a fraction landing on a half
    /// percent cannot read one lower here than it does in the CLI. Load-bearing wherever both surfaces
    /// render the SAME wire figure — above all the `≥N%` census water, which the CLI's `roster_line` prints
    /// through this very `pct` (see `statsAllHighLabel`, issue #805).
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

    /// The honest caveat shown above the Stats readout when the daemon reports `config_unreadable`
    /// (issue #642): the numbers below were computed against DEFAULT tunables because `config.toml`
    /// exists but could not be parsed, so every ceiling-dependent figure (cap-hits, the band, the
    /// sparkline scale) may be well off the operator's own thresholds.
    ///
    /// Leads with the CONSEQUENCE — "computed against default tunables" — because that is what the
    /// operator must know to read the numbers correctly; "the config failed to load" alone would
    /// state a fault without saying what it costs. `reason` is the daemon's own classification (see
    /// `StatsWire.configUnreadable`), naming the failure class and the command that prints the full
    /// detail, so the caveat routes the operator onward instead of dead-ending in an apology.
    static func statsConfigUnreadableNote(_ reason: String) -> String {
        "Computed against default tunables — \(reason)."
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

    /// The aggregate callout under the Stats rows — mock `.agg` "All accounts ≥95% at once — 3 episodes
    /// (1h40m) · swaps 28 · last 7 days", built from the summary `roster` (`StatsRoster`) + the window phrase.
    /// Facts only (magnitudes + the neutral span), never a recommendation.
    ///
    /// The water is READ FROM THE WIRE (`allHighThreshold`), never assumed: it is `session_ceiling`,
    /// which the operator can retune, so a literal here would silently lie the moment it is retuned
    /// (issue #805 — the label had been pinned at `≥90%` while the aggregator censused at 95).
    static func statsAggregateText(roster: StatsRoster, window: StatsWindow) -> String {
        let episodes = roster.allHighEpisodes
        let epWord = episodes == 1 ? "episode" : "episodes"
        return "\(statsAllHighLabel(roster.allHighThreshold)) — \(episodes) \(epWord)"
            + " (\(statsDuration(roster.allHighSecs)))"
            + " · swaps \(roster.swapCount) · \(statsWindowPhrase(window))"
    }

    /// The aggregate callout's leading clause, stating the water the census actually used.
    ///
    /// A `nil` water (a pre-#804 daemon that never sent `all_high_threshold` — see `StatsRoster`)
    /// DROPS the qualifier rather than substituting a number: naming a threshold the daemon never
    /// reported would fabricate exactly the fact this issue exists to stop fabricating. The metric's
    /// own identity survives the drop — "all-accounts-high" is what the census is called — so the
    /// degraded line still says WHAT was counted, only not the water it was counted at. This is the
    /// panel's standing honesty rule on the read-only Stats surface (never a fabricated number),
    /// applied to a label rather than to a magnitude.
    static func statsAllHighLabel(_ threshold: Double?) -> String {
        guard let threshold else { return "All accounts high at once" }
        return "All accounts ≥\(statsPercent(threshold))% at once"
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
    /// over-cap reading clamps to the top. `inset` keeps the stroke off the edges. `x` is evenly spaced
    /// across the plot; a single-point series centres. An empty series yields no points.
    ///
    /// The box is a PARAMETER, not the old fixed 96 × 28: issue #700 moved the chart to its own full-width
    /// card row, so the shipping call site passes the size its `Canvas` was actually laid out at (which the
    /// panel's geometry makes `statsChartWidth`, 331) and the mock authors its `.spark` viewBox to match —
    /// no third copy of the number in between. Widening only re-spreads `x` — the `y` mapping depends solely on
    /// `height`/`inset`, so the series semantics are width-invariant. A box too narrow to hold its own
    /// insets (`width <= 2 * inset`) has no plot to speak of and yields no points, rather than folding the
    /// series backwards onto itself.
    static func sparkPoints(
        _ values: [Double],
        width: Double,
        height: Double,
        inset: Double
    ) -> [SparkPoint] {
        guard !values.isEmpty, width > 2 * inset else { return [] }
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
