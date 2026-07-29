// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The status panel's per-account roster (issue #326) and its row building blocks, split out of `StatusPanelView`
// by #640. The roster renders LIVE only on `.connected` and DIMMED-but-retained on every degraded state, never
// frozen-as-live (#137); that dimming is applied by the ROOT view — so the `next_swap` footer dims in lock-step —
// which is why these views take their liveness as plain inputs. Each non-active row is ALSO the quiet manual-switch
// affordance (issue #169), deliberately neutral against the footer's one accent action. Every string, glyph, and
// number comes from the unit-tested `StatusPanelFormat`; the whole row is one VoiceOver element.

import AppKit
import SwiftUI

// MARK: - Roster

/// The per-account roster. Its live-vs-retained dimming is applied by the parent (so the `next_swap`
/// footer dims in lock-step) — this view just lays the rows out and decides, per row, whether it is a
/// manual-switch target (issue #169).
struct RosterView: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let rows: [AccountRow]
    let now: Int64
    /// Whether rows offer the manual-switch affordance at all. `false` on a dropped connection, where a
    /// retained last-known row is not a live target.
    let switchable: Bool
    /// The snapshot's next-swap candidate (issue #572), threaded to the active blind row to compose the
    /// CORNERED verdict. Callers pass `nil` for a non-vouched (dropped/stale) roster — a retained
    /// `noViableTarget` must not raise a cornered alarm off stale data. Defaults to `nil` so existing
    /// call sites that never render a cornered row need no change.
    var nextSwap: NextSwap? = nil

    var body: some View {
        // Resolve every row's smart monogram ONCE over the whole roster (issue #445), so collision-escalation
        // sees all sibling labels — a same-local-part roster gets distinct 2-char monograms, not one letter.
        let monograms = StatusPanelFormat.accountMonograms(rows.map(\.label))
        // Resolve the expiry line's presence ONCE over the whole roster (issue #884), mirroring the CLI,
        // which materializes its `EXPIRY` column only when some row has an observed deadline. Deciding it
        // per row would let the line appear on one account and vanish on the next — a ragged roster where
        // the CLI shows a uniform column, and a `—` that reads as "nothing here" rather than the pointed
        // absence it is beside a sibling's real deadline.
        let showsExpiry = StatusPanelFormat.rosterShowsExpiry(rows.map(\.expiry), now: now)
        VStack(alignment: .leading, spacing: 2 * scale) {
            ForEach(rows) { row in
                // On a dropped connection every row is `notATarget` (non-interactive); otherwise the pure
                // `rowSwitchState` verdict decides (active → plain row, non-viable → disabled-with-reason,
                // else the switch affordance). The active-row-stays-plain and parked-still-switchable
                // rules live in that pure, unit-tested function — never re-decided here.
                let state: StatusPanelFormat.RowSwitchState = switchable
                    ? StatusPanelFormat.rowSwitchState(isActive: row.isActive,
                                                       isQuarantined: row.isQuarantined,
                                                       weeklyExhausted: row.weeklyExhausted,
                                                       isEnabled: row.isEnabled)
                    : .notATarget
                AccountRowView(row: row, monogram: monograms[row.label] ?? "?", now: now,
                               switchState: state, nextSwap: nextSwap,
                               // The panel is fixed-width, so this is a derived constant rather than a
                               // measurement — passed IN (#766) so the row's affordance-budget verdict has
                               // an injectable width rather than reaching for the global itself.
                               rowWidth: PanelMetrics.rowWidth,
                               showsExpiry: showsExpiry)
            }
        }
        // The design reference insets the roster (`.accts { padding: 6px 8px 2px }`): 8px horizontal so
        // the active row's accent card aligns with the swap-callout card below (also inset 8) instead of
        // bleeding edge-to-edge, plus 6px above / 2px below for breathing room under the divider.
        .padding(.horizontal, PanelMetrics.scaledRosterInset(scale)).padding(.top, 6 * scale).padding(.bottom, 2 * scale)
    }
}

/// The per-row manual-switch button style (issue #169): a QUIET, neutral affordance — deliberately NOT
/// the accent/primary treatment the footer **Swap** button wears. Von Restorff: the accent action is
/// what the daemon SUGGESTS; the quiet ones are the operator CHOOSING. A subtle wash on hover (a live
/// row only) and a slightly deeper one while pressed; a blocked row never washes, so it can never read
/// as pressable.
private struct RowSwitchButtonStyle: ButtonStyle {
    let hovering: Bool
    let live: Bool
    /// The panel's Dynamic Type multiplier (issue #756), passed IN rather than read from the environment:
    /// a `ButtonStyle` is not itself part of the view hierarchy, so `@Environment` is not dependable here.
    /// The caller is a panel view that already holds the injected `\.panelScale`.
    let scale: Double

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            // MIS-CLICK GUARD (issue #169 falsifier b) — resolved deliberately, not by accident. The
            // checklist item forbids "an INVISIBLE whole-row click"; the watch-out phrases it as "not a
            // whole-row HOT-ZONE". Both are honored by ARMING, not by shrinking the hit target:
            //   * At rest the row shows only a QUIET chip (#448) — no wash, no `pointingHand`: the chip
            //     hints the row is actionable, but WITHOUT the wash + cursor it does not yet read as an
            //     armed, pressable control, so the invisible-click hazard the checklist names cannot occur
            //     (the persistent chip aids DISCOVERY; the wash + cursor still gate ARMING).
            //   * The hit rect is the whole row (per the explicit "implement the row as a Button"
            //     instruction + Fitts's law — a glyph-only target would be a worse, error-prone
            //     mechanism), but it is ARMED only once hover has added the wash + cursor and brightened
            //     the chip, so the operator always SEES the row is live before a press can land.
            //   * Residual accidental-press risk is bounded by three things the daemon and model already
            //     enforce: the daemon re-validates every target (a stray press can't do something unsafe),
            //     a swap is reversible (undo = switch back), and a sibling swap is `.disabled()` mid-flight.
            // Net: cheaper than a confirm dialog, honest at rest. The real-popover press feel is a #380
            // manual-check item.
            .contentShape(RoundedRectangle(cornerRadius: 9 * scale))
            .background(
                RoundedRectangle(cornerRadius: 9 * scale)
                    // #388-EXEMPT: a COMPUTED hover/press interaction wash, not one of the mock's absolute
                    // chrome fills (the static mock has no hover state), so it keeps `Color.secondary.opacity(k)`
                    // rather than a `panelFill` token — `wash` is 0 at rest, a faint neutral only while live+hovered.
                    .fill(Color.secondary.opacity(wash(pressed: configuration.isPressed)))
            )
    }

    private func wash(pressed: Bool) -> Double {
        guard live, hovering else { return 0 }
        return pressed ? 0.16 : 0.08
    }
}

/// One account row, built to the design reference (`apps/menubar/design/menubar-preview.html`). BOTH
/// reset windows show — R-2 parity with the `status` CLI, which prints both — never collapsed to one.
/// The whole row is a single VoiceOver element.
///
/// A non-active row is ALSO the manual-switch affordance (issue #169): a `Button` whose trailing swap
/// chip is PERSISTENT — quiet at rest, brightening when the row is armed on hover (#448). The resting
/// row carries the quiet chip; arming still gates the wash + `pointingHand` cursor.
struct AccountRowView: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let row: AccountRow
    /// The roster-resolved 2-char monogram for this row's label (issue #445), computed once by `RosterView`.
    let monogram: String
    let now: Int64
    /// The row's manual-switch verdict (issue #169). `.notATarget` — the ACTIVE row, or any row on a
    /// dropped connection — stays a plain, non-interactive display row.
    let switchState: StatusPanelFormat.RowSwitchState
    /// The snapshot's next-swap candidate (issue #572) — read ONLY to compose the CORNERED blind verdict
    /// (a blind DEGRADED active row + `nextSwap == noViableTarget`). `nil` on a non-vouched roster
    /// (dropped/stale), so a retained `noViableTarget` never drives a cornered alarm off stale data
    /// (the honest-state discipline the glance already applies). Inert for every non-blind row.
    let nextSwap: NextSwap?

    /// The width available to THIS row, which `offersSwitch` measures the switch affordance against. A
    /// parameter rather than a read of the `PanelMetrics.rowWidth` global (issue #766) so the WIDTH axis of
    /// the mis-click guard is drivable: the whole point of `rowFitsSwitchAffordance` is that a sub-budget
    /// row goes NON-INTERACTIVE, and with the width hardcoded that branch was unreachable from a test, so
    /// only the pure predicate was ever exercised — never the row's actual interactive shape at a narrow
    /// width. `RosterView` passes the panel's real row width, so the shipped behaviour is unchanged.
    let rowWidth: Double
    /// Force the row into its ARMED presentation without a pointer (issue #766) — the render seam that makes
    /// the hover treatment measurable. `isHovering` is `@State` driven by `.onHover`, and an offscreen
    /// `ImageRenderer` / `NSHostingView` never receives a hover event, so the armed frame was un-renderable
    /// and the brighten could only be asserted as an enum value. This is a PRESENTATION-only override: it
    /// deliberately does NOT reach `syncCursor()`, so a forced-arm render never pushes a real `pointingHand`
    /// onto the operator's cursor stack. Always `false` in the app.
    let armed: Bool
    /// Whether the roster shows the refresh-token expiry line at all (issue #884) — a roster-wide verdict
    /// `RosterView` resolves ONCE and passes down (`StatusPanelFormat.rosterShowsExpiry` carries the why),
    /// never something a row decides for itself. The default is for the one call site that builds a row
    /// outside a roster (`PanelInteractionStateTests`); every rendered panel, goldens included, gets the
    /// resolved value.
    let showsExpiry: Bool

    @EnvironmentObject private var swap: AccountSwapModel
    /// The active row's accent-tint fill opacity is theme-aware (#388): the mock raises it in dark mode.
    @Environment(\.colorScheme) private var colorScheme
    @State private var isHovering = false
    /// Whether this row currently owns a pushed `pointingHand` cursor — tracked so a push is always
    /// balanced by exactly one pop, even when the row stops being live WHILE the pointer is inside it
    /// (a sibling swap starting mid-hover would otherwise strand the cursor).
    @State private var cursorPushed = false

    /// Explicit rather than memberwise: the `private` `@State` / `@Environment` members above would make a
    /// synthesized memberwise initializer private too, and `PanelInteractionStateTests` (#766) constructs
    /// this view directly.
    ///
    /// `armed` defaults to the shipped value so that every existing call site — and every committed panel
    /// golden — is untouched. `rowWidth` deliberately does NOT default: a default would be the same reach
    /// for the `PanelMetrics` global that giving the row an injectable width exists to remove, and it would
    /// let a future call site inherit a width it never stated while the affordance-budget verdict silently
    /// depends on it. Every caller states its width.
    init(row: AccountRow, monogram: String, now: Int64,
         switchState: StatusPanelFormat.RowSwitchState, nextSwap: NextSwap?,
         rowWidth: Double, armed: Bool = false, showsExpiry: Bool = false) {
        self.row = row
        self.monogram = monogram
        self.now = now
        self.switchState = switchState
        self.nextSwap = nextSwap
        self.rowWidth = rowWidth
        self.armed = armed
        self.showsExpiry = showsExpiry
    }

    /// Whether the row is ARMED — a real pointer over it, or the render seam above. The single source both
    /// arm treatments read (the `RowSwitchButtonStyle` wash and the chip's brighten), so a render can never
    /// arm one and not the other.
    private var isArmed: Bool { isHovering || armed }

    /// Each window's reset-in against the client's own clock — both shown, never collapsed to one pick.
    private var sessionReset: String {
        StatusPanelFormat.resetCell(row.sessionResetsAt, now: now)
    }
    private var weeklyReset: String {
        StatusPanelFormat.resetCell(row.weeklyResetsAt, now: now)
    }

    private var sessionSeverity: StatusPanelFormat.UsageSeverity? {
        StatusPanelFormat.sessionSeverity(row.sessionPct)
    }
    private var weeklySeverity: StatusPanelFormat.UsageSeverity? {
        StatusPanelFormat.weeklySeverity(weeklyPct: row.weeklyPct, weeklyExhausted: row.weeklyExhausted)
    }

    /// This row's blind severity (#485 OK/DEGRADED, #572 CORNERED) — `.ok` when not blind. Composes the
    /// row's `autoProtectionDegraded` with the snapshot `nextSwap` (only the active account is ever blind,
    /// and `nextSwap` is its swap candidate), mirroring the CLI's `cornered_state`. Drives the eye-slash
    /// tint (`authView`), the leading rule (`blindRuleTint`), and the `BlindMeter` verdict — one source.
    private var blindSeverity: StatusPanelFormat.BlindSeverity {
        guard let blind = row.blindActive else { return .ok }
        return StatusPanelFormat.blindSeverity(degraded: blind.autoProtectionDegraded, nextSwap: nextSwap)
    }

    /// The leading-rule tint for a blind active row, or `nil` for none: DEGRADED → orange, CORNERED → red
    /// (#572), OK / non-blind → no rule.
    private var blindRuleTint: StatusPanelFormat.HealthTint? {
        switch blindSeverity {
        case .degraded: return .orange
        case .cornered: return .red
        case .ok:       return nil
        }
    }

    // MARK: - Switch state (issue #169)

    /// The wire-visible reason this row cannot be switched to, if any.
    private var blockReason: StatusPanelFormat.SwitchBlock? {
        if case .blocked(let block) = switchState { return block }
        return nil
    }

    /// Whether the row is offered as a switch target AT ALL. `.notATarget` (active row / dropped
    /// connection) never is; otherwise it is, GATED on the row's available width — too narrow to host the
    /// affordance ⇒ not interactive, rather than an invisible whole-row hot-zone. The panel is
    /// fixed-width, so the width is a derived constant (`PanelMetrics`), not a measurement.
    ///
    /// Deliberately reads the UNSCALED width (issue #756). Under uniform scaling the row and the affordance
    /// it must host both grow by the same `k`, so `k·rowWidth ≥ k·minRowWidth` ⟺ `rowWidth ≥ minRowWidth`:
    /// the verdict is scale-INVARIANT and the unscaled pair is its honest statement. Feeding a scaled width
    /// to an unscaled threshold would be the actual bug — it would start offering the affordance at large
    /// text sizes where it fits no better in proportion than it did at the default.
    private var offersSwitch: Bool {
        switchState != .notATarget
            && StatusPanelFormat.rowFitsSwitchAffordance(rowWidth: rowWidth)
    }

    /// This row's own swap is in flight.
    private var isSwitching: Bool { swap.phase.pendingTarget == row.label }

    /// Whether a click on this row would actually do something.
    private var isLiveSwitch: Bool {
        offersSwitch && blockReason == nil && !swap.phase.isPending
    }

    var body: some View {
        Group {
            if offersSwitch {
                // ROW-ACTION CARDINALITY (issue #169 watch-out — decide the count BEFORE the mechanism):
                // a viable row carries exactly ONE action today (switch), so wrapping the whole row in a
                // `Button` is sound, and it earns the VoiceOver button trait, native `.disabled()`, and
                // hover styling for free. If a row ever gains a SECOND action (an enable-toggle, a
                // remove), this wrap MUST be undone — nested interactive children inside a `Button` do
                // not receive their own events. Hoist the secondary control into a trailing accessory or
                // a context menu and shrink the button to the identity region.
                Button(action: submit) { rowContent }
                    .buttonStyle(RowSwitchButtonStyle(hovering: isArmed, live: isLiveSwitch, scale: scale))
                    .disabled(blockReason != nil || swap.phase.isPending)
                    .help(hoverText)
                    // The button trait + `dimmed` come from `Button` + `.disabled()`; the label carries
                    // the row's facts and, when blocked, WHY it is dimmed (a trait alone never says why).
                    .accessibilityLabel(StatusPanelFormat.rowSwitchAccessibilityLabel(
                        base: accessibilityLabel, block: blockReason))
                    .accessibilityHint(blockReason == nil
                                       ? StatusPanelFormat.switchHelpText(label: row.label) : "")
            } else {
                rowContent
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(accessibilityLabel)
            }
        }
        .onHover { hovering in
            isHovering = hovering
            syncCursor()
        }
        // Resync the cursor whenever the row's live-ness can change WITHOUT a hover event, so a lingering
        // `pointingHand` never contradicts the row's real state: a sibling swap starting/finishing
        // (`swap.phase`), or a fresh snapshot flipping this row's viability or making it the new active
        // account (`switchState`) while the pointer rests on it.
        .onChange(of: swap.phase) { _ in syncCursor() }
        .onChange(of: switchState) { _ in syncCursor() }
        .onDisappear { setCursor(pushed: false) }
    }

    /// Submit a manual switch to THIS row's account. The clicked row's target goes on the wire verbatim;
    /// the daemon re-validates it (`swap_command_verdict`) and may still refuse — a `cooldown`, say, which
    /// never rides the wire and so cannot be pre-empted here. That refusal renders in `SwapStatusLine`.
    private func submit() {
        Task { await swap.swap(to: row.label) }
    }

    /// The hover tooltip: the block reason for a non-viable row, otherwise the switch invitation.
    private var hoverText: String {
        blockReason.map(StatusPanelFormat.switchBlockedText)
            ?? StatusPanelFormat.switchHelpText(label: row.label)
    }

    /// Push / pop the `pointingHand` cursor to match whether a click here would do anything.
    ///
    /// Reads `isHovering`, NOT `isArmed`: the cursor follows a REAL pointer only. The `armed` render seam
    /// (#766) exists to make the arm treatment visible offscreen, and a headless render pushing a cursor
    /// onto the operator's stack would be a side effect no render should have — and one nothing would pop.
    private func syncCursor() {
        setCursor(pushed: isHovering && isLiveSwitch)
    }

    private func setCursor(pushed: Bool) {
        guard pushed != cursorPushed else { return }
        if pushed {
            NSCursor.pointingHand.push()
        } else {
            NSCursor.pop()
        }
        cursorPushed = pushed
    }

    /// The row's visual content — identical whether or not it is wrapped in a `Button`, so the two
    /// branches cannot drift.
    private var rowContent: some View {
        VStack(alignment: .leading, spacing: 9 * scale) {
            HStack(spacing: StatusPanelFormat.rowInterElementSpacing * scale) {
                StatusDot(isActive: row.isActive)
                MonogramBadge(label: row.label, monogram: monogram)

                Text(row.label)
                    .font(.panel(style: .body, scale: scale))
                    .fontWeight(.semibold)
                    .lineLimit(1)
                    // MIDDLE-truncation (issue #445): a same-local-part label's distinguishing suffix /
                    // domain survives when it elides, where tail-truncation hid exactly that part.
                    .truncationMode(StatusPanelFormat.identityElision.truncationMode)

                // #699: the trailing identity slot carries NO "active" text. Active is encoded twice over
                // — the leading dot's FILL-vs-ring treatment (a SHAPE cue, so WCAG 1.4.1 / R-2 holds under
                // colour-blindness and macOS monochrome) plus the accent-tint row fill on top. A third,
                // textual cue said nothing the other two did not, and its ~56 pt (capsule + gap) came out
                // of the account label's width — pushing near-identical fleet emails into truncation. The
                // row's spoken label is unaffected: `rowAccessibilityLabel` derives ", active" from
                // `isActive` independently, and the capsule was `accessibilityHidden` anyway (#325).
                Spacer(minLength: StatusPanelFormat.rowSpacerMinLength * scale)

                authView
                switchSlot
            }

            if let blind = row.blindActive {
                // The active account's poll is blind — replace the two (now `n/a`) live meters with the
                // SEMANTIC held-state block: a held session bar, blind duration, and the auto-protection
                // verdict (#485; #572 adds the CORNERED verdict), the panel's render of the CLI's blind line.
                // A healthy row keeps its meters. `blindSeverity` composes this row's `autoProtectionDegraded`
                // with the snapshot `nextSwap` (cornered iff DEGRADED + no viable target).
                BlindMeter(blind: blind, severity: blindSeverity, nextSwap: nextSwap, now: now)
            } else {
                VStack(spacing: 6 * scale) {
                    UsageMeter(label: "Session", pct: row.sessionPct, severity: sessionSeverity,
                               reset: sessionReset)
                    UsageMeter(label: "Weekly", pct: row.weeklyPct, severity: weeklySeverity,
                               reset: weeklyReset)
                }
            }

            // The refresh-token expiry modifier (#884) — OUTSIDE the blind branch above, deliberately.
            // Usage-blindness and credential expiry are ORTHOGONAL per-account axes (a 429'd `/usage` poll
            // says nothing about the refresh token's deadline), so a blind row must not have its expiry
            // SUPPRESSED just because the held block took the meters' slot. The CLI keeps both for the same
            // reason — its `EXPIRY` column is untouched by the blind line — and hiding a real credential
            // signal is the #137 honest-state failure one axis over.
            if showsExpiry {
                ExpiryLine(expiry: row.expiry, now: now)
            }
        }
        .padding(.horizontal, StatusPanelFormat.rowHorizontalPadding * scale)
        .padding(.top, 9 * scale)
        .padding(.bottom, 10 * scale)
        // Active emphasis follows the design reference: an accent-tint fill ONLY. The accent ring was
        // dropped (#387 M5, ratified) to cut active over-signaling, and the text capsule with it (#699) —
        // active stays encoded by the filled leading dot (shape) + this tint, so color is never the SOLE
        // signal (WCAG 1.4.1 / R-2 state-parity holds). The mock's active-ring is dropped in lockstep
        // (menubar-preview.html `.acct.active` / `.stat.active`). The fill OPACITY is theme-aware (#388,
        // mock `--active-bg`): .08 light / .15 dark — the dark active row was ~1.5× too faint when hardcoded.
        .background(
            RoundedRectangle(cornerRadius: 9 * scale)
                .fill(row.isActive
                      ? Color.accentEmphasis(.activeRowFill, dark: colorScheme == .dark)
                      : Color.clear)
        )
        // #485/#572: a DEGRADED blind active row gets an at-risk ORANGE leading rule; a CORNERED one
        // (blind + degraded + no viable target) escalates it to RED — a non-color-redundant LOCALITY tell
        // (the fault is THIS row's; the header/footer stay fresh, the AC-2 distinction from #169's
        // whole-snapshot "stale"). Absent on a blind-OK row (calm) and every non-blind row.
        .overlay(alignment: .leading) {
            if let ruleTint = blindRuleTint {
                Capsule()
                    .fill(Color.panel(StatusPanelFormat.healthTint(ruleTint)))
                    .frame(width: 3 * scale)
                    .padding(.vertical, 7 * scale)
                    .accessibilityHidden(true)
            }
        }
    }

    /// The swap glyph the chip draws — a swap arrow, or a DISTINCT `nosign` on a wire-blocked target
    /// ("you cannot switch here" is a different fact from "switch here", and shape carries it without
    /// color). The tint is applied by `switchSlot` per emphasis level, so this stays tint-free.
    private var chipGlyph: some View {
        Image(systemName: blockReason == nil ? "arrow.left.arrow.right" : "nosign")
            .font(.panel(11, .semibold, scale: scale))
    }

    /// The trailing manual-switch chip (issue #169, made PERSISTENT by #448) — a quiet affordance shown at
    /// rest on every switch target, that BRIGHTENS when the row is armed (hover / focus). #169 revealed it
    /// only on hover, so on a transient popover a first-time operator never saw a row was actionable; the
    /// persistent-quiet chip makes the row discoverable without an always-loud control.
    ///
    /// The slot's WIDTH is laid out on every roster row, always — even where the chip is hidden (the active
    /// row) — so NEITHER the chip's resting presence NOR its hover-brighten can REFLOW the row: the label's
    /// available width is identical hidden / resting / armed, and so is its truncation (the issue's
    /// row-width watch-out). The auth column also stays aligned across active and non-active rows. The
    /// why-text never truncates: it is a native `.help` tooltip, not an inline label.
    ///
    /// The emphasis (hidden / resting / armed) is a pure `StatusPanelFormat.switchChipEmphasis` verdict, so
    /// the resting-visible-vs-armed-brighten distinction is unit-asserted; the view only maps it to a
    /// neutral system tint. ARMING (not the resting presence) is the mis-click guard — the full rationale
    /// lives on `RowSwitchButtonStyle`.
    @ViewBuilder
    private var switchSlot: some View {
        Group {
            if isSwitching {
                ProgressView().controlSize(PanelTypeScale.controlSize(for: scale))
            } else {
                switch StatusPanelFormat.switchChipEmphasis(offersSwitch: offersSwitch, armed: isArmed) {
                case .hidden:
                    Color.clear
                case .resting:
                    // Quiet at rest — `.tertiary` ≈ the mock's `--text-3` decorative token. Never `.tint`:
                    // the one accent action is the footer Swap (Von Restorff, one accent per panel).
                    chipGlyph.foregroundStyle(.tertiary)
                case .armed:
                    // Brightened once armed — `.secondary` ≈ the mock's `--text-2`. A SEMANTIC tint step,
                    // not a hardcoded opacity (#388 / #448).
                    chipGlyph.foregroundStyle(.secondary)
                }
            }
        }
        .frame(width: CGFloat(StatusPanelFormat.switchAffordanceSlotWidth) * scale, alignment: .trailing)
        .accessibilityHidden(true)
    }

    /// The auth glyph (modern path) or the legacy tag text (pre-#119), plus the DEAD/`disabled` cue.
    /// A blind active account (#485) shows the eye-slash blind glyph HERE instead — the credential may be
    /// fine; what's lost is usage visibility, so the health slot reports that, not a false auth verdict.
    ///
    /// The glyphs below carry an EXPLICIT `.font` (issue #756) where they previously carried none, which is
    /// an ADDITION rather than a substitution. An `Image(systemName:)` sizes off the ambient font, and the
    /// panel's ambient font is the unscaled system `.body` — so left alone these three would have held their
    /// default size while every label and cell around them grew, which is the scaled-text-beside-a-fixed-
    /// element mismatch AC-2 forbids. At the default factor `.panel(style: .body, …)` resolves to the same
    /// `.body` point size the ambient font was already giving them, which is why the goldens do not move.
    @ViewBuilder
    private var authView: some View {
        if row.blindActive != nil {
            // Usage visibility lost (#485): an eye-slash, a DISTINCT shape from every auth glyph. OK is
            // calm secondary; DEGRADED tints it at-risk orange (redundant with the row's rule + verdict).
            // If the credential is ITSELF in a warning state (stale/at-risk — orthogonal to usage-blindness),
            // its glyph rides ALONGSIDE the eye-slash so the warning isn't suppressed (the CLI keeps both;
            // #137 honest-state one axis over). Healthy/unknown → eye-slash alone (the common, ratified case).
            HStack(spacing: 4 * scale) {
                if let auth = row.auth, StatusPanelFormat.blindCoShowsAuthWarning(auth) {
                    let authSymbol = StatusPanelFormat.healthSymbol(auth)
                    Image(systemName: authSymbol.name)
                        .symbolRenderingMode(.hierarchical)
                        .font(.panel(style: .body, scale: scale))
                        .foregroundStyle(healthColor(authSymbol.tint))
                        .accessibilityHidden(true)
                }
                let symbol = StatusPanelFormat.blindSymbol(blindSeverity)
                Image(systemName: symbol.name)
                    .symbolRenderingMode(.hierarchical)
                    .font(.panel(style: .body, scale: scale))
                    .foregroundStyle(healthColor(symbol.tint))
                    .accessibilityHidden(true)
            }
        } else if let auth = row.auth {
            HStack(spacing: 4 * scale) {
                let symbol = StatusPanelFormat.healthSymbol(auth)
                Image(systemName: symbol.name)
                    .symbolRenderingMode(.hierarchical)
                    .font(.panel(style: .body, scale: scale))
                    .foregroundStyle(healthColor(symbol.tint))
                    .accessibilityHidden(true)
                if let cue = StatusPanelFormat.authCue(auth: auth,
                                                       recovering: row.isRecovering,
                                                       enabled: row.isEnabled) {
                    Text(cue)
                        .font(.panel(style: .caption1, scale: scale))
                        .foregroundStyle(cueColor(for: auth))
                }
            }
        } else {
            let legacy = StatusPanelFormat.legacyHealthTags(enabled: row.isEnabled,
                                                            quarantined: row.isQuarantined,
                                                            recovering: row.isRecovering)
            if !legacy.isEmpty {
                Text(legacy)
                    .font(.panel(style: .caption1, scale: scale))
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func cueColor(for auth: CredentialHealth) -> Color {
        // Each cue sits in the same row as its glyph, so it takes the SAME contrast-safe tint (#388) —
        // a system color beside the token-tinted glyph would read as two different shades. A healing
        // account's `recovering` cue stays neutral (it is holding, not acting). #427: the 🟠 degraded
        // cue is orange (`--ut-o`), the 🔴 dead cue red (`--ut-r`).
        guard !row.isRecovering else { return .secondary }
        switch auth {
        case .dead:     return .panel(StatusPanelFormat.healthTint(.red))
        case .degraded: return .panel(StatusPanelFormat.healthTint(.orange))
        default:        return .secondary
        }
    }

    /// Map the pure `HealthTint` role to its contrast-safe panel tint (#388) — never `accentColor` (the
    /// AUTH glyph is never app-tinted, #84); `.neutral` (unknown) stays `.secondary`, the #137 "no false
    /// green". The role→token table lives in `StatusPanelFormat.healthTint` (Foundation-only, unit-tested).
    private func healthColor(_ tint: StatusPanelFormat.HealthTint) -> Color {
        .panel(StatusPanelFormat.healthTint(tint))
    }

    private var accessibilityLabel: String {
        StatusPanelFormat.rowAccessibilityLabel(
            label: row.label,
            isActive: row.isActive,
            auth: row.auth,
            recovering: row.isRecovering,
            enabled: row.isEnabled,
            quarantined: row.isQuarantined,
            sessionPct: row.sessionPct,
            weeklyPct: row.weeklyPct,
            sessionReset: sessionReset,
            weeklyReset: weeklyReset,
            blind: row.blindActive,
            nextSwap: nextSwap,
            now: now,
            expiry: row.expiry,
            showsExpiry: showsExpiry)
    }
}

// MARK: - Row building blocks (per the design reference)

/// The row's refresh-token expiry line (issues #878/#882/#884) — the panel's render of the daemon's
/// `AccountExpiry`, and the R-2 STATE-parity twin of the `status` CLI's `EXPIRY` column.
///
/// **The design mock does not author this element**, and that is not a divergence to reconcile: the mock
/// (`apps/menubar/design/menubar-preview.html`) predates the credential-continuity work and carries no
/// expiry surface at all — the repo `CLAUDE.md` scopes it as the oracle "only for what it authors", and
/// silence is not authority. So this borrows the mock's ROW GRAMMAR, which it does author: the same
/// uppercase 10 pt semibold label in the same `meterLabelCellWidth` cell the two `UsageMeter` rows use,
/// so the three sub-lines share one left edge and the new one reads as a sibling rather than an intruder.
/// No bar — expiry is a CREDENTIAL fact, not a usage window, and a bar would imply a fraction of a
/// quantity that has none.
///
/// The STRING and the TINT are both `StatusPanelFormat` verdicts computed from the same inputs the CLI's
/// `expiry_cell` / `expiry_severity` consume, so the two surfaces cannot disagree about one snapshot.
/// This line NEVER escalates the menu-bar glance and has no banner — see `StatusPanelFormat`'s expiry
/// section for the both-or-neither rationale.
private struct ExpiryLine: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let expiry: AccountExpiry?
    let now: Int64

    var body: some View {
        HStack(spacing: 9 * scale) {
            Text(StatusPanelFormat.expiryRowLabel)
                .font(.panel(10, .semibold, scale: scale))
                .foregroundStyle(.secondary)
                .frame(width: StatusPanelFormat.meterLabelCellWidth * scale, alignment: .leading)

            Text(StatusPanelFormat.expiryCell(expiry, now: now))
                .font(.panel(11, scale: scale)).monospacedDigit()
                .foregroundStyle(valueColor)
                .lineLimit(1)

            Spacer(minLength: 0)
        }
        // One VoiceOver element already speaks this row (`rowAccessibilityLabel` carries the expiry
        // phrase), so the line's own text would be read twice.
        .accessibilityHidden(true)
    }

    /// The CLI's `Severity` mapped onto the panel's contrast-safe tokens (#388). An unobserved deadline
    /// (`nil`) and a `BEYOND` one (`.neutral`) both land on `.secondary` — matching the CLI, where
    /// uncoloured and `Dim` are likewise both de-emphasised, and honest either way: the TEXT (`—` vs a
    /// duration) is what distinguishes them, never the colour alone.
    private var valueColor: Color {
        guard let tint = StatusPanelFormat.expirySeverity(expiry, now: now) else { return .secondary }
        return .panel(StatusPanelFormat.healthTint(tint))
    }
}

/// One usage window's meter. Both percents render at a uniform weight — the design reference (and the
/// `status` CLI) carry severity in COLOR, not weight; the fixed column widths + monospaced digits keep
/// Session and Weekly aligned.
private struct UsageMeter: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let label: String
    let pct: UInt8?
    let severity: StatusPanelFormat.UsageSeverity?
    let reset: String

    var body: some View {
        HStack(spacing: 9 * scale) {
            Text(label.uppercased())
                .font(.panel(10, .semibold, scale: scale))
                .foregroundStyle(.secondary)
                // The three cell widths come from `StatusPanelFormat` (#750) rather than sitting inline, so
                // `PanelTextMetricsTests` measures text against the SAME number this lays out with.
                .frame(width: StatusPanelFormat.meterLabelCellWidth * scale, alignment: .leading)

            UsageBar(fraction: fraction, color: barColor)

            Text(StatusPanelFormat.pct(pct))
                .font(.panel(12, .semibold, scale: scale)).monospacedDigit()
                .foregroundStyle(pctColor)
                .frame(width: StatusPanelFormat.meterPercentCellWidth * scale, alignment: .trailing)

            Text(reset)
                .font(.panel(11, scale: scale)).monospacedDigit()
                .foregroundStyle(.secondary)
                .frame(width: StatusPanelFormat.meterResetCellWidth * scale, alignment: .trailing)
                .lineLimit(1)
        }
    }

    private var fraction: Double {
        pct.map { Double($0) / 100.0 } ?? 0
    }

    /// Bar fill = the green/amber/red usage band; a failed poll (`nil`) is muted, never a false green (#137).
    /// The FILL deliberately keeps the system-bright colors (≈ the mock's `--u-*` fill family) while the
    /// small `pctColor` TEXT took the darker `--ut-*` tokens in #388 — leaving it here is intentional, not
    /// an oversight, and `StatusPanelFormatTests` now asserts that split as a relation rather than trusting
    /// this comment for it (#759).
    ///
    /// CORRECTED (#759, measured): this used to claim the fill was fine because "a bar is a non-text fill,
    /// WCAG 3:1". The split is real but that justification is NOT — the fills do not clear 3:1 either. In
    /// light, against the `--track` they sit on: `.green` 1.61, `.orange` 1.67, `.red` 2.59 (the mock's own
    /// `--u-*` values fail identically, so this is a design property, not a drift). What actually makes the
    /// bar defensible is that it never carries its value alone: it is `.accessibilityHidden(true)` and the
    /// exact percent is printed beside it as AA-clearing text. Do NOT generalise "fills only need 3:1, and
    /// these clear it" to a fill with no adjacent text — that one would be a real violation. Issue #831.
    private var barColor: Color {
        switch severity {
        case .red:    return .red
        case .yellow: return .orange
        case .green:  return .green
        // #388-EXEMPT: reached only when `severity == nil` ⇒ `pct == nil` ⇒ `fraction == 0` ⇒ the `UsageBar`
        // fill has ZERO width (a failed poll shows a BARE track, matching the mock), so this muted color never
        // actually paints. No absolute mock fill exists for the failed-poll bar → keeps `secondary.opacity`.
        case .none:   return Color.secondary.opacity(0.45)
        }
    }

    /// The percent TEXT carries its severity band in color, matching the `status` CLI (which colors green
    /// percents green too — `Severity::Green => "32"`) and the design reference: green healthy, ≥75% amber,
    /// ≥90%/exhausted red. As small text it takes the contrast-safe `--ut-*` TEXT tints (#388) — a family
    /// apart from the bar's brighter `--u-*` fill (`barColor`, unchanged). A failed poll (`n/a`) stays
    /// neutral — no false green (#137).
    private var pctColor: Color {
        .panel(StatusPanelFormat.usageTextTint(severity))
    }
}

/// A capsule fill proportional to `fraction` (0…1), with a minimum sliver so a live-but-tiny percent
/// never reads as empty; a zero/failed reading shows a bare track. The number carries the real value.
private struct UsageBar: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let fraction: Double
    let color: Color
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                // Track = mock `--track` neutral fill (#388) — replaces a washed `Color.secondary.opacity(0.20)`.
                Capsule().fill(Color.panelFill(.track, dark: colorScheme == .dark))
                Capsule().fill(color)
                    // The clamp + minimum sliver live in `StatusPanelFormat.meterFillWidth` (#750) so they
                    // are assertable — nothing upstream clamps the percent (the wire decodes a bare
                    // `UInt8`), so this bound is what stops a 255 % reading painting 2.55× its own track.
                    // The rationale lives with the function.
                    .frame(width: StatusPanelFormat.meterFillWidth(fraction: fraction, full: geo.size.width))
            }
        }
        .frame(height: 6 * scale)
        .accessibilityHidden(true)
    }
}

/// The active account's blind-state block (issues #479/#485) — the panel's render of the daemon
/// `BlindActive`, REPLACING the two live meters a healthy row shows. A HELD session bar (dashed — a frozen
/// last-known value, never a live fill, #137) at the last-known %, the `blind {dur}` chip, the
/// LAST-KNOWN·RATE-LIMITED caption, and the auto-protection verdict — every fact from a unit-tested
/// `StatusPanelFormat` verdict, so this View stays a thin, un-screenshot-tested consumer. The held row
/// reuses `UsageMeter`'s SESSION-label (52) and percent (40) columns so THOSE align with sibling rows; its
/// trailing chip is wider (58 vs the reset column's 52) to fit `blind {dur}` un-clipped, so the held bar
/// itself sits ~6 pt narrower than a live sibling's — an accepted legibility trade, not a lined-up column.
private struct BlindMeter: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let blind: BlindActive
    /// The composed blind severity (#485 OK/DEGRADED, #572 CORNERED) — resolved once by `AccountRowView`
    /// so the held block, the eye-slash, and the leading rule all read one source.
    let severity: StatusPanelFormat.BlindSeverity
    /// The snapshot next-swap, read only to fold the reset into the CORNERED remedy sub-line.
    let nextSwap: NextSwap?
    let now: Int64
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        // The last-known session % carries the SAME severity band a live meter would (green/amber/red) —
        // the held bar shows "the last reading was at X%", while the blind OK/DEGRADED/CORNERED verdict rides
        // the eye glyph, the leading rule, and the shield line below (two orthogonal facts, two colour
        // channels). Named distinctly from the stored `severity` (the composed `BlindSeverity`) so the two
        // never shadow — the bar keys off the % BAND, the verdict off the auto-protection state.
        let lastKnownBand = StatusPanelFormat.utilSeverity(blind.lastKnownSessionPct)
        VStack(alignment: .leading, spacing: 4 * scale) {
            HStack(spacing: 9 * scale) {
                Text("SESSION")
                    .font(.panel(10, .semibold, scale: scale))
                    .foregroundStyle(.secondary)
                    // The SAME constants the live `UsageMeter` lays out with (#750), so the type comment
                    // above — "reuses UsageMeter's SESSION-label (52) and percent (40) columns" — is now
                    // structurally true rather than two literals that happen to agree today.
                    .frame(width: StatusPanelFormat.meterLabelCellWidth * scale, alignment: .leading)

                HeldUsageBar(fraction: Double(blind.lastKnownSessionPct) / 100.0, color: barColor(lastKnownBand))

                Text(StatusPanelFormat.pct(blind.lastKnownSessionPct))
                    .font(.panel(12, .semibold, scale: scale)).monospacedDigit()
                    .foregroundStyle(Color.panel(StatusPanelFormat.usageTextTint(lastKnownBand)))
                    .frame(width: StatusPanelFormat.meterPercentCellWidth * scale, alignment: .trailing)

                Text(StatusPanelFormat.blindDurationChip(blind.blindSecs))
                    .font(.panel(11, .medium, scale: scale)).monospacedDigit()
                    .foregroundStyle(.secondary)
                    .frame(width: 58 * scale, alignment: .trailing)
                    .lineLimit(1)
            }

            // WHY the bar is held — the value is last-known and the poll is rate-limited (the #137 tell,
            // so a held bar is never read as a live one).
            Text(StatusPanelFormat.blindLastKnownCaption)
                .font(.panel(9, .semibold, scale: scale))
                .tracking(0.3 * scale)
                .foregroundStyle(.tertiary)

            // The auto-protection verdict — OK calm / DEGRADED orange / CORNERED red — mirroring the CLI's
            // blind line (#485; #572 adds CORNERED "cannot act" + the remedy sub-line below).
            let verdict = StatusPanelFormat.blindVerdict(severity, nextSwap: nextSwap, now: now)
            HStack(spacing: 5 * scale) {
                Image(systemName: verdict.symbol)
                    .symbolRenderingMode(.hierarchical)
                    .font(.panel(11, scale: scale))
                    .foregroundStyle(Color.panel(StatusPanelFormat.healthTint(verdict.tint)))
                Text(verdict.text)
                    .font(.panel(style: .caption1, scale: scale))
                    .foregroundStyle(Color.panel(StatusPanelFormat.healthTint(verdict.tint)))
            }
            // The row's VoiceOver label already speaks the whole blind state — verdict AND cornered remedy
            // — as one element (#485/#572, `rowAccessibilityLabel`).
            .accessibilityHidden(true)
            // CORNERED only (#572): the operator remedy on its own sub-line — "Out of capacity … · add or
            // free an account" — indented under the verdict text (past the glyph + gap). Absent for
            // OK/DEGRADED (`remedy == nil`). Wraps rather than truncates: it is the one actionable line.
            if let remedy = verdict.remedy {
                Text(remedy)
                    .font(.panel(10.5, scale: scale))
                    .foregroundStyle(Color.panel(StatusPanelFormat.healthTint(verdict.tint)))
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 18 * scale)
                    .accessibilityHidden(true)
            }
        }
    }

    /// The held bar's fill hue — the SAME bright severity family `UsageMeter.barColor` uses, keyed off the
    /// last-known session band. It inherits that fill's measured contrast position and its compensating
    /// control (see `UsageMeter.barColor`; issue #831) — the held percent is likewise printed beside it.
    private func barColor(_ severity: StatusPanelFormat.UsageSeverity) -> Color {
        switch severity {
        case .red:    return .red
        case .yellow: return .orange
        case .green:  return .green
        }
    }
}

/// A HELD usage bar (#485) — the last-known fill under a DASHED capsule outline. The dash is the "held /
/// estimate, not live" tell that reads at the 6 px bar height where diagonal hatching would not, so a
/// frozen last-known value is never mistaken for a live meter (#137). The fill itself keeps the severity
/// hue (muted) so the band is still legible.
private struct HeldUsageBar: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let fraction: Double
    let color: Color
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.panelFill(.track, dark: colorScheme == .dark))
                // Muted fill (0.5α) — a held value reads dimmer than a live meter, never a bright false-now.
                // Shares `UsageMeter`'s bound (#750): the held bar reads a LAST-KNOWN `UInt8` off the same
                // unclamped wire field, so it needs the same clamp for the same reason.
                Capsule().fill(color.opacity(0.5))
                    .frame(width: StatusPanelFormat.meterFillWidth(fraction: fraction, full: geo.size.width))
                // Dashed outline over the whole track — the legible-at-6px "held" signal.
                Capsule().strokeBorder(color.opacity(0.9),
                                       style: StrokeStyle(lineWidth: 1 * scale, dash: [2.5 * scale, 2 * scale]))
            }
        }
        .frame(height: 6 * scale)
        .accessibilityHidden(true)
    }
}
