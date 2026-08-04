// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Hermetic tests for the swap-on-click affordance (issue #169): the `swap` request serialization
// (`SwapCommand`), the affordance model's idle → pending → done → failed transitions
// (`AccountSwapModel`), the pure viability + copy + layout-budget layer (`StatusPanelFormat`).
//
// The model is driven by the SAME in-process fake connection the transport suite uses
// (`CommandFakeConnection` / `CommandFakeConnector` in `ControlCommandTransportTests`) — NO real socket,
// NO live daemon — so every phase transition and every redacted ack variant is exercised
// deterministically, and a test run can NEVER perform a real credential swap against the operator's
// active account. A gated fake proves the in-flight (`.pending` / `isBusy`) window the panel's
// sibling-disable and panel-retain gates depend on.
//
// The `SwapAck` decoder itself is covered in `ControlCommandTransportTests`; what is asserted here is how
// the MODEL routes each decoded verdict into a phase, and what the panel is allowed to SAY about it.

import Foundation
import os
import XCTest

final class AccountSwapTests: XCTestCase {

    // MARK: - SwapCommand: the wire request

    // AC (one command, both paths): the clicked/displayed target rides the wire verbatim, as the `swap`
    // verb the daemon already speaks. Keys in the client's deterministic sorted order.
    func testSwapCommandSerializesTheTargetVerbatim() throws {
        XCTAssertEqual(try encode(SwapCommand(target: "Scratch")),
                       #"{"cmd":"swap","force":false,"target":"Scratch"}"#)
    }

    // The panel NEVER forces. `force` is a POLICY bypass (it skips the quarantined / weekly-exhausted /
    // cooldown gates that protect the operator); an armed-on-hover row click is far too low-ceremony to
    // carry a silent override. Forcing stays explicit, in the CLI's `use --force`.
    func testSwapCommandNeverForces() throws {
        for target in ["Work", "Personal", "a1b2c3"] {
            XCTAssertTrue(try encode(SwapCommand(target: target)).contains(#""force":false"#),
                          "the panel must never send force:true for \(target)")
        }
    }

    // The command bytes carry a verb + a non-secret roster LABEL and nothing else (redaction, issue #15).
    func testSwapCommandBytesCarryNoSecret() throws {
        let line = try encode(SwapCommand(target: "Work"))
        XCTAssertFalse(line.contains("@"), "no email in the command bytes")
        XCTAssertFalse(line.lowercased().contains("token"), "no token in the command bytes")
        XCTAssertFalse(line.lowercased().contains("oauth"), "no oauth blob in the command bytes")
    }

    // MARK: - AccountSwapModel: the settled success phases

    @MainActor
    func testAcceptedAckLandsInDoneSwapped() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"accepted","from":"Work","to":"Scratch"}"#)
        let model = AccountSwapModel(client: client(conn))
        await model.swap(to: "Scratch")
        XCTAssertEqual(model.phase, .done(.swapped(from: "Work", to: "Scratch")))
    }

    // A no-op success is reported AS a no-op — never dressed up as a switch that did not happen.
    @MainActor
    func testAlreadyActiveAckLandsInDoneAlreadyActive() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"already_active","to":"Work"}"#)
        let model = AccountSwapModel(client: client(conn))
        await model.swap(to: "Work")
        XCTAssertEqual(model.phase, .done(.alreadyActive(to: "Work")))
    }

    // AC (WYSIWYG): the model sends the target it was GIVEN — it never re-picks one. The footer button
    // passes the displayed `next_swap` target; a row passes the clicked row's. Same verb, same path.
    @MainActor
    func testTheGivenTargetIsTheTargetOnTheWire() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"accepted","from":"Work","to":"Personal"}"#)
        let model = AccountSwapModel(client: client(conn))
        await model.swap(to: "Personal")
        XCTAssertEqual(conn.sentStrings, [#"{"cmd":"swap","force":false,"target":"Personal"}"# + "\n"])
    }

    // MARK: - AccountSwapModel: the daemon's redacted refusals

    // Every redacted rejection the daemon can send routes to a `.failed(.rejected(_))` phase — including
    // `cooldown`, which the client CANNOT pre-empt (post-swap cooldown is in-memory daemon state and never
    // rides the wire). This is the honest split: the panel disables only what the wire proves, and renders
    // everything else as the daemon's own refusal.
    @MainActor
    func testEveryRejectionReasonLandsInFailedRejected() async {
        let cases: [(String, SwapRejection)] = [
            ("unknown-target", .unknownTarget),
            ("ambiguous-target", .ambiguousTarget),
            ("quarantined", .quarantined),
            ("weekly-exhausted", .weeklyExhausted),
            ("cooldown", .cooldown),
            ("no-active-account", .noActiveAccount),
            ("keychain-locked", .keychainLocked),
            ("swap-lock-busy", .swapLockBusy),
            ("failed", .failed),
        ]
        for (code, expected) in cases {
            let conn = CommandFakeConnection(ackOnSend: #"{"result":"rejected","reason":"\#(code)"}"#)
            let model = AccountSwapModel(client: client(conn))
            await model.swap(to: "Scratch")
            XCTAssertEqual(model.phase, .failed(.rejected(expected)), "reason \(code)")
        }
    }

    // The shared redacted error ack — the same-user local peer should never be unauthorized, but it is
    // surfaced honestly rather than swallowed.
    @MainActor
    func testDaemonErrorAckLandsInFailedDaemonError() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"error":"unauthorized"}"#)
        let model = AccountSwapModel(client: client(conn))
        await model.swap(to: "Scratch")
        XCTAssertEqual(model.phase, .failed(.daemonError("unauthorized")))
    }

    // A drifted / buggy daemon degrades LOUDLY rather than being mis-read as a success.
    @MainActor
    func testUndecodableAckLandsInFailedUndecodable() async {
        let conn = CommandFakeConnection(ackOnSend: #"{"result":"teleported"}"#)
        let model = AccountSwapModel(client: client(conn))
        await model.swap(to: "Scratch")
        XCTAssertEqual(model.phase, .failed(.undecodable))
    }

    // MARK: - AccountSwapModel: the bounded transport failures

    // No control client (sandboxed / home unresolved): an honest "unreachable", never a dead button.
    @MainActor
    func testNoClientLandsInFailedUnavailable() async {
        let model = AccountSwapModel(client: nil)
        await model.swap(to: "Scratch")
        XCTAssertEqual(model.phase, .failed(.unavailable))
    }

    @MainActor
    func testConnectionRefusedLandsInFailedTransport() async {
        let model = AccountSwapModel(
            client: ControlCommandClient(connector: CommandFakeConnector(.fail("ECONNREFUSED")),
                                         timeout: .seconds(5)))
        await model.swap(to: "Scratch")
        guard case .failed(.transport(.connectionRefused)) = model.phase else {
            return XCTFail("expected .failed(.transport(.connectionRefused)), got \(model.phase)")
        }
    }

    // AC (issue #169): "pending TIMES OUT so a lost ack can't stick a spinner." A daemon that accepts the
    // command and never answers resolves the phase to a bounded failure — `pending` is never terminal.
    @MainActor
    func testASilentDaemonResolvesPendingRatherThanStickingTheSpinner() async {
        let conn = CommandFakeConnection(ackOnSend: nil)  // accepts, never answers
        let model = AccountSwapModel(
            client: ControlCommandClient(connector: CommandFakeConnector(.succeed(conn)),
                                         timeout: .milliseconds(150)))
        await model.swap(to: "Scratch")
        XCTAssertEqual(model.phase, .failed(.transport(.timedOut)))
        XCTAssertFalse(model.phase.isPending, "a lost ack must never leave the affordance pending")
    }

    @MainActor
    func testEOFBeforeAckResolvesPending() async {
        let conn = CommandFakeConnection(ackOnSend: nil, eofOnSend: true)
        let model = AccountSwapModel(client: client(conn))
        await model.swap(to: "Scratch")
        XCTAssertEqual(model.phase, .failed(.transport(.closedBeforeAck)))
    }

    // MARK: - AccountSwapModel: the in-flight window (sibling-disable + panel-retain depend on it)

    @MainActor
    func testPendingNamesTheTargetAndIsBusyThenClearsOnDone() async throws {
        let conn = GatedSwapConnection(ack: #"{"result":"accepted","from":"Work","to":"Scratch"}"#)
        let model = AccountSwapModel(
            client: ControlCommandClient(connector: SwapOneShotConnector(connection: conn),
                                         timeout: .seconds(10)))

        let task = Task { await model.swap(to: "Scratch") }
        try await waitUntil({ model.phase.isPending }, "pending")

        // The pending TARGET is what the panel keys the row spinner on, and what tells every SIBLING row
        // to `.disabled()` itself for the swap's duration.
        XCTAssertEqual(model.phase.pendingTarget, "Scratch")
        XCTAssertTrue(model.isBusy, "an in-flight swap keeps the panel retained until its outcome shows")

        conn.release()
        await task.value
        XCTAssertEqual(model.phase, .done(.swapped(from: "Work", to: "Scratch")))
        XCTAssertFalse(model.isBusy, "a settled swap is dismissible again")
    }

    // A second click while a swap is in flight is ignored — the daemon holds a single-writer lock behind
    // this verb, so a second command would only queue up to contend with the first.
    @MainActor
    func testDoubleSubmitWhilePendingIsIgnored() async throws {
        let conn = GatedSwapConnection(ack: #"{"result":"accepted","from":"Work","to":"Scratch"}"#)
        let model = AccountSwapModel(
            client: ControlCommandClient(connector: SwapOneShotConnector(connection: conn),
                                         timeout: .seconds(10)))

        let task = Task { await model.swap(to: "Scratch") }
        try await waitUntil({ model.phase.isPending }, "pending")

        await model.swap(to: "Personal")   // a sibling click that the panel would have disabled anyway
        XCTAssertEqual(model.phase.pendingTarget, "Scratch", "the in-flight swap is not superseded")

        conn.release()
        await task.value
        XCTAssertEqual(conn.sendCount, 1, "exactly one swap command reached the daemon")
    }

    // A failure does NOT wedge the model and does NOT vanish on its own — an error the operator has not
    // read must persist until a fresh attempt replaces it. Uses ONE model across two swaps (a fresh
    // connection per send, as production opens a new socket each time), so it tests persistence AND reuse
    // on the same instance — not two throwaway models.
    @MainActor
    func testAFailureStaysAndTheModelStaysReusable() async {
        let connector = FreshCommandConnector(ack: #"{"result":"rejected","reason":"cooldown"}"#)
        let model = AccountSwapModel(client: ControlCommandClient(connector: connector, timeout: .seconds(5)))

        await model.swap(to: "Scratch")
        XCTAssertEqual(model.phase, .failed(.rejected(.cooldown)))

        // The failure does not clear itself: `settle`/`scheduleIdleReset` fire only for `.done`, never
        // `.failed`, so after many scheduler turns it is still the same rejection.
        for _ in 0..<50 { await Task.yield() }
        XCTAssertEqual(model.phase, .failed(.rejected(.cooldown)), "a failure never auto-clears")

        // The SAME model accepts a second swap — `.failed` is not pending, so the re-entrancy guard lets a
        // fresh attempt through, and it lands its own terminal (proving the error was never sticky).
        await model.swap(to: "Scratch")
        XCTAssertEqual(model.phase, .failed(.rejected(.cooldown)))
        XCTAssertFalse(model.phase.isPending, "a reattempt ran — the failed phase never blocked the model")
    }

    // MARK: - StatusPanelFormat: row viability (the CLIENT-VISIBLE subset of `swap_command_verdict`)

    func testViableRowHasNoSwitchBlock() {
        XCTAssertNil(StatusPanelFormat.switchBlock(quarantined: false, weeklyExhausted: false))
    }

    func testQuarantinedAndWeeklyExhaustedEachBlock() {
        XCTAssertEqual(StatusPanelFormat.switchBlock(quarantined: true, weeklyExhausted: false), .quarantined)
        XCTAssertEqual(StatusPanelFormat.switchBlock(quarantined: false, weeklyExhausted: true), .weeklyExhausted)
    }

    // Gate ORDER mirrors the daemon's own (`swap_command_verdict` checks quarantined BEFORE weekly), so the
    // reason the panel shows is the reason the daemon would give.
    func testBlockOrderMirrorsTheDaemonsGateOrder() {
        XCTAssertEqual(StatusPanelFormat.switchBlock(quarantined: true, weeklyExhausted: true), .quarantined)
    }

    // MARK: - StatusPanelFormat: the full per-row verdict (`rowSwitchState`)

    // The ACTIVE row is never a switch target — it stays a plain display row (a disabled button reads as
    // "broken"). True regardless of its other flags.
    func testActiveRowIsNotATarget() {
        XCTAssertEqual(StatusPanelFormat.rowSwitchState(
            isActive: true, isQuarantined: false, weeklyExhausted: false, isEnabled: true), .notATarget)
        // even a quarantined active row is still just "not a target", never "blocked"
        XCTAssertEqual(StatusPanelFormat.rowSwitchState(
            isActive: true, isQuarantined: true, weeklyExhausted: true, isEnabled: false), .notATarget)
    }

    func testViableNonActiveRowIsAvailable() {
        XCTAssertEqual(StatusPanelFormat.rowSwitchState(
            isActive: false, isQuarantined: false, weeklyExhausted: false, isEnabled: true), .available)
    }

    func testNonViableRowIsBlockedWithTheDaemonsReasonOrder() {
        XCTAssertEqual(StatusPanelFormat.rowSwitchState(
            isActive: false, isQuarantined: true, weeklyExhausted: false, isEnabled: true), .blocked(.quarantined))
        XCTAssertEqual(StatusPanelFormat.rowSwitchState(
            isActive: false, isQuarantined: false, weeklyExhausted: true, isEnabled: true), .blocked(.weeklyExhausted))
        // both gates fire → quarantined wins, mirroring `swap_command_verdict`'s order
        XCTAssertEqual(StatusPanelFormat.rowSwitchState(
            isActive: false, isQuarantined: true, weeklyExhausted: true, isEnabled: true), .blocked(.quarantined))
    }

    // The load-bearing daemon-parity invariant, now genuinely exercised (not a tautology on `switchBlock`):
    // a PARKED account (`isEnabled: false`, issue #36 — out of the AUTO rotation) that is otherwise viable is
    // STILL `.available`. `swap_command_verdict` (`src/daemon.rs`) takes no `enabled` input, so the CLI's
    // `use <account>` reaches a parked account and the panel must too. If a future edit ever gates
    // `rowSwitchState` on `isEnabled`, this flips to `.blocked`/`.notATarget` and fails loudly.
    func testAParkedButViableAccountIsStillSwitchable() {
        XCTAssertEqual(
            StatusPanelFormat.rowSwitchState(
                isActive: false, isQuarantined: false, weeklyExhausted: false, isEnabled: false),
            .available,
            "parked-ness never blocks a manual switch — the daemon's verdict does not read `enabled`")
        // and a parked row that IS non-viable is blocked for the non-viability, never for being parked
        XCTAssertEqual(
            StatusPanelFormat.rowSwitchState(
                isActive: false, isQuarantined: true, weeklyExhausted: false, isEnabled: false),
            .blocked(.quarantined))
    }

    // MARK: - StatusPanelFormat: the switch-affordance layout budget (never truncate to something uninformative)

    func testARowAtOrAboveTheBudgetHostsTheAffordance() {
        XCTAssertTrue(StatusPanelFormat.rowFitsSwitchAffordance(
            rowWidth: StatusPanelFormat.switchAffordanceMinRowWidth))
        XCTAssertTrue(StatusPanelFormat.rowFitsSwitchAffordance(rowWidth: 1_000))
    }

    // Below the budget the affordance is not merely hidden — the caller makes the row NON-INTERACTIVE, so a
    // too-narrow row can never degrade into an invisible whole-row hot-zone (the mis-click hazard the
    // arm-on-hover guard exists to prevent).
    func testARowBelowTheBudgetDoesNotHostTheAffordance() {
        XCTAssertFalse(StatusPanelFormat.rowFitsSwitchAffordance(
            rowWidth: StatusPanelFormat.switchAffordanceMinRowWidth - 1))
        XCTAssertFalse(StatusPanelFormat.rowFitsSwitchAffordance(rowWidth: 0))
    }

    // The SHIPPED layout clears the budget — asserted against the REAL constants (`defaultRowWidth` is the
    // source of truth the panel's `.frame(width:)` pins), so if the panel width ever shrinks below the
    // budget this fails rather than passing on a hardcoded number. Below the budget the affordance turns
    // itself off rather than truncating the label.
    func testTheShippedPanelWidthHostsTheAffordance() {
        XCTAssertTrue(StatusPanelFormat.rowFitsSwitchAffordance(rowWidth: StatusPanelFormat.defaultRowWidth))
        XCTAssertGreaterThanOrEqual(StatusPanelFormat.defaultRowWidth,
                                    StatusPanelFormat.switchAffordanceMinRowWidth)
    }

    // MARK: - StatusPanelFormat: the persistent swap chip (issue #448 — visible at rest, brightens when armed)

    // The chip is HIDDEN wherever the row does not offer a switch: `offersSwitch: false` → `.hidden`,
    // whatever the hover state and whatever the block.
    //
    // The block half is not a throwaway case. `offersSwitch` is `switchState != .notATarget &&
    // rowFitsSwitchAffordance(rowWidth:)`, so it is false for TWO different rows — the active row / a
    // dropped connection, AND a genuinely blocked row that sits below the width budget. The second really
    // does carry a block, and this pins that the width gate short-circuits AHEAD of it: such a row is
    // `.hidden` for the width reason, and would still be `.hidden` if the block were lifted.
    func testSwitchChipIsHiddenOnANonSwitchTargetRow() {
        for armed in [false, true] {
            XCTAssertEqual(
                StatusPanelFormat.switchChipEmphasis(offersSwitch: false, block: nil, armed: armed), .hidden)
            XCTAssertEqual(
                StatusPanelFormat.switchChipEmphasis(offersSwitch: false, block: .weeklyExhausted,
                                                     armed: armed), .hidden)
        }
    }

    // The load-bearing #448 change: a VIABLE switch target's chip is PERSISTENT — VISIBLE at rest
    // (`.resting`, no longer `.hidden` as #169's hover-reveal left it), and it BRIGHTENS to `.armed` when the
    // row is armed (hover / focus). The resting-visible state is what makes a row discoverable as actionable
    // on a transient popover — the exact gap #448 closes. #959 narrowed this to VIABLE targets only (below);
    // it did not touch the persistence that #448 bought.
    func testSwitchChipIsPersistentAtRestAndBrightensWhenArmed() {
        XCTAssertEqual(StatusPanelFormat.switchChipEmphasis(offersSwitch: true, block: nil, armed: false),
                       .resting,
                       "a viable switch target's chip must be VISIBLE (not hidden) at rest — the #448 fix")
        XCTAssertEqual(StatusPanelFormat.switchChipEmphasis(offersSwitch: true, block: nil, armed: true),
                       .armed,
                       "the chip brightens (arms) when the row is hovered/focused")
        // The resting state is explicitly NOT hidden — the whole point of #448 (a regression back to
        // hover-only would flip this to `.hidden` and fail loudly).
        XCTAssertNotEqual(StatusPanelFormat.switchChipEmphasis(offersSwitch: true, block: nil, armed: false),
                          .hidden)
    }

    // Issue #959: a WIRE-BLOCKED target renders NO chip. Before this, the affordance and its own negation
    // shared one slot at one size in one token, discriminated by glyph SHAPE alone — and measured on a live
    // 1:1 capture `arrow.left.arrow.right` and `nosign` are at ink-mass parity (18.2 over 70 px vs 19.5 over
    // 82 px, the negation marginally the QUIETER), both strokes horizontal along the row's dominant axis.
    // The two were not tellable apart at rest without ~9× magnification.
    //
    // The routing lives HERE, in the pure verdict, rather than as an `if` at the view's render site: that is
    // what makes it assertable at all, and it is how the rest of this file is built. A regression that moved
    // the decision back into SwiftUI would leave this test green while the panel changed — which is why the
    // render suite carries the pixel half (`PanelInteractionStateTests`).
    func testAWireBlockedTargetRendersNoChipAtAll() {
        for block in [StatusPanelFormat.SwitchBlock.quarantined, .weeklyExhausted] {
            for armed in [false, true] {
                XCTAssertEqual(
                    StatusPanelFormat.switchChipEmphasis(offersSwitch: true, block: block, armed: armed),
                    .hidden, """
                    a \(block) row still renders a chip (armed: \(armed)) — #959 removed it because the chip \
                    and its own negation were interchangeable at rest. Blocking is carried by #955's \
                    persistent reason line, `.disabled()` dimming and the spoken label, never by a glyph
                    """)
            }
        }
    }

    // The one real risk #959 introduces, pinned rather than assumed: the ACTIVE row and a BLOCKED row now
    // agree on the chip axis (both `.hidden`), so that axis can no longer tell them apart. THIS test's job
    // is that collapse — asserting it explicitly, so the compensating channels below are documented against
    // a stated premise rather than an implied one. Those channels are covered where each can be:
    //
    //   • the blocked row has a persistent REASON LINE and the active row none — copy covered in depth by
    //     `testEveryBlockedVariantHasAPersistentCueAndAViableRowHasNone` below (both variants, and that
    //     they read distinctly), so it is not re-asserted here;
    //   • the active row carries a FILLED leading dot against a ring on every non-active row — a SHAPE cue,
    //     so it survives monochrome and colour-vision deficiency — plus the accent-tint row fill
    //     (`StatusDot`, and the row background in `StatusPanelRoster`). `PanelInteractionStateTests`
    //     renders the two rows and measures their separation; note there that the FILL carries essentially
    //     all of that number and the dot is ≤0.0057 of the frame, so the shape cue is structural rather
    //     than pixel-gated;
    //   • the blocked row is `.disabled()` and speaks its reason (`testABlockedRowSpeaksItsReason`, below).
    //
    // If this ever fails, the answer is NOT to add a new blocked-row marker — #959 rejected that explicitly
    // (five of six rows carry a mark, so an added element pays its cost five times). It is to re-open which
    // of the channels above regressed.
    func testTheActiveAndBlockedRowsAgreeOnTheChipAxisSoTheOthersMustCarryIt() {
        let activeRowChip = StatusPanelFormat.switchChipEmphasis(offersSwitch: false, block: nil,
                                                                 armed: false)
        let blockedRowChip = StatusPanelFormat.switchChipEmphasis(offersSwitch: true,
                                                                  block: .weeklyExhausted, armed: false)
        XCTAssertEqual(activeRowChip, .hidden)
        XCTAssertEqual(blockedRowChip, .hidden,
                       "post-#959 a blocked row is as chip-free as the active row — the collapse this "
                       + "test exists to state, and the premise the other channels compensate for")
    }

    // MARK: - Which element owns the hover tooltip (issue #953)

    // THE ONE INVARIANT WORTH PINNING, and it is a platform fact rather than a style rule: a `.help()` on
    // the row-wrapping `Button` WINS over a `.help()` on a child inside it. Measured on macOS 26.5.2 across
    // eight runs — with both attached, the ROW's tooltip answered even over the chip (239 pt wide, against
    // the chip's own 38 pt); see `docs/findings/0953-help-nesting-inside-a-row-button.md` § The answer.
    //
    // So "keep a row-level fallback AND scope one to the chip" is not a safer belt-and-braces version of
    // #953 — it is the #953 defect, rebuilt. Returning both non-nil means the chip's tooltip is DEAD and
    // the row is answering everywhere again, which is exactly the state the issue exists to leave.
    //
    // This fails LOUDLY where the real regression would otherwise be SILENT: a tooltip is a hover
    // affordance, so nothing crashes, no golden moves (they render at `.idle`), and no other test notices.
    func testTheChipAndTheRowNeverBothClaimTheTooltip() {
        let blocks: [StatusPanelFormat.SwitchBlock?] = [nil, .quarantined, .weeklyExhausted]
        for offersSwitch in [true, false] {
            for block in blocks {
                for armed in [true, false] {
                    for switching in [true, false] {
                        let emphasis = StatusPanelFormat.switchChipEmphasis(offersSwitch: offersSwitch,
                                                                            block: block, armed: armed)
                        let chip = StatusPanelFormat.switchChipHelp(emphasis: emphasis,
                                                                    switching: switching, label: "work")
                        let row = StatusPanelFormat.switchRowHelp(block: block)
                        XCTAssertFalse(chip != nil && row != nil,
                                       "offersSwitch=\(offersSwitch) block=\(String(describing: block)) "
                                       + "armed=\(armed) switching=\(switching): both claimed the tooltip, "
                                       + "so the row's would win and the chip's is dead — the #953 defect")
                    }
                }
            }
        }
    }

    // A viable row: the invitation is the CHIP's, and the row stays silent so the chip can be reached at
    // all. The chip's text is `switchHelpText` verbatim rather than a second copy of the sentence, so the
    // tooltip and the spoken `.accessibilityHint` cannot drift apart.
    func testAViableRowPutsTheInvitationOnTheChipAndNotOnTheRow() {
        for armed in [true, false] {
            let emphasis = StatusPanelFormat.switchChipEmphasis(offersSwitch: true, block: nil, armed: armed)
            XCTAssertEqual(StatusPanelFormat.switchChipHelp(emphasis: emphasis, switching: false,
                                                            label: "work"),
                           StatusPanelFormat.switchHelpText(label: "work"),
                           "the chip carries the switch invitation, armed or at rest")
        }
        XCTAssertNil(StatusPanelFormat.switchRowHelp(block: nil),
                     "a viable row must NOT also answer — a row-level help wins and would kill the chip's")
    }

    // A blocked row keeps its tooltip at ROW level, because since #959 it renders no chip to hang one on
    // (`switchChipEmphasis` → `.hidden`). The remedy sentence is the part that exists ONLY here — the
    // reason's first sentence is already on-screen at rest via `switchBlockedCue` (#955).
    func testABlockedRowKeepsItsTooltipOnTheRowBecauseItHasNoChip() {
        for block in [StatusPanelFormat.SwitchBlock.quarantined, .weeklyExhausted] {
            let emphasis = StatusPanelFormat.switchChipEmphasis(offersSwitch: true, block: block,
                                                                armed: false)
            XCTAssertEqual(emphasis, .hidden, "premise: #959 leaves a blocked row chip-free")
            XCTAssertNil(StatusPanelFormat.switchChipHelp(emphasis: emphasis, switching: false,
                                                          label: "work"),
                         "there is no chip drawn, so nothing may claim to be one")
            XCTAssertEqual(StatusPanelFormat.switchRowHelp(block: block),
                           StatusPanelFormat.switchBlockedText(block),
                           "the row carries the reason IN FULL, remedy included")
        }
    }

    // An in-flight swap replaces the chip with a `ProgressView`. Nothing is drawn to hover, so nothing
    // answers — the row is `.disabled()` and the spinner is the explanation.
    func testAnInFlightSwapLeavesNoChipTooltip() {
        let emphasis = StatusPanelFormat.switchChipEmphasis(offersSwitch: true, block: nil, armed: false)
        XCTAssertNotNil(StatusPanelFormat.switchChipHelp(emphasis: emphasis, switching: false,
                                                         label: "work"),
                        "premise: this row would carry a chip tooltip when not switching")
        XCTAssertNil(StatusPanelFormat.switchChipHelp(emphasis: emphasis, switching: true, label: "work"),
                     "the chip is a spinner mid-swap — a stale invitation would describe a press that "
                     + "cannot happen")
    }

    // The slot widened 18 → 28 (#448) so the now-persistent chip sits comfortably; the shipped panel still
    // clears the layout budget with the wider slot — guarded against the REAL constants, so a future width
    // regression fails loudly rather than silently truncating the label.
    func testSwitchChipSlotWidenedAndStillFitsTheShippedPanel() {
        XCTAssertEqual(StatusPanelFormat.switchAffordanceSlotWidth, 28)
        XCTAssertGreaterThan(StatusPanelFormat.switchAffordanceSlotWidth, 18,
                             "the persistent chip earns more room than #169's hover-revealed 18 pt slot")
        XCTAssertTrue(StatusPanelFormat.rowFitsSwitchAffordance(rowWidth: StatusPanelFormat.defaultRowWidth))
    }

    // MARK: - StatusPanelFormat: the copy

    func testBlockedTextIsDistinctAndActionable() {
        let quarantined = StatusPanelFormat.switchBlockedText(.quarantined)
        let weekly = StatusPanelFormat.switchBlockedText(.weeklyExhausted)
        XCTAssertNotEqual(quarantined, weekly)
        XCTAssertTrue(quarantined.contains("sessiometer poke"),
                      "a quarantined credential names its refresh remedy (issue #427)")
        XCTAssertFalse(quarantined.contains("claude /login"),
                       "a refreshable quarantine must NOT tell the operator to re-login (issue #427)")
        for text in [quarantined, weekly] {
            XCTAssertFalse(text.isEmpty)
            XCTAssertFalse(text.contains("weekly-exhausted"), "never the raw machine tag: \(text)")
        }
    }

    // Issue #955: a blocked row's reason is PERSISTENT on-screen text, not hover-only. Both blocked
    // variants produce a cue; a viable row produces none — the `nil` case is what keeps the line from
    // appearing under every healthy row, and it is asserted rather than assumed because the view renders
    // straight off this optional.
    func testEveryBlockedVariantHasAPersistentCueAndAViableRowHasNone() throws {
        XCTAssertNil(StatusPanelFormat.switchBlockedCue(nil),
                     "a row that is not blocked shows no cue line at all")
        for block in [StatusPanelFormat.SwitchBlock.quarantined, .weeklyExhausted] {
            let cue = try XCTUnwrap(StatusPanelFormat.switchBlockedCue(block),
                                    "\(block) is a load-bearing reason and must show at rest")
            XCTAssertFalse(cue.isEmpty)
            XCTAssertFalse(cue.contains("weekly-exhausted"), "never the raw machine tag: \(cue)")
        }
        XCTAssertNotEqual(StatusPanelFormat.switchBlockedCue(.quarantined),
                          StatusPanelFormat.switchBlockedCue(.weeklyExhausted),
                          "the two blocks read distinctly at rest, not just in the tooltip")
    }

    // The three channels must not drift: the resting cue is the OPENING of the hover/spoken text, so the
    // only thing hover and VoiceOver add is the remedy. Composed rather than copied in the source — this
    // pins that composition, so re-wording one channel cannot silently desynchronize the others.
    func testTheRestingCueIsThePrefixOfTheHoverAndSpokenText() throws {
        for block in [StatusPanelFormat.SwitchBlock.quarantined, .weeklyExhausted] {
            let cue = try XCTUnwrap(StatusPanelFormat.switchBlockedCue(block))
            let full = StatusPanelFormat.switchBlockedText(block)
            XCTAssertTrue(full.hasPrefix(cue),
                          "the tooltip must open with what the row already says at rest: \(full)")
            let spoken = StatusPanelFormat.rowSwitchAccessibilityLabel(base: "Scratch", block: block)
            XCTAssertTrue(spoken.contains(cue), "VoiceOver speaks the same reason: \(spoken)")
        }
        // The remedy is the ONLY hover/spoken-only part, and only where a remedy exists at all: a weekly
        // window is not something the operator fixes, so its cue and its tooltip are the same sentence.
        let quarantinedCue = try XCTUnwrap(StatusPanelFormat.switchBlockedCue(.quarantined))
        XCTAssertGreaterThan(StatusPanelFormat.switchBlockedText(.quarantined).count,
                             quarantinedCue.count,
                             "the quarantine tooltip adds its refresh remedy")
        XCTAssertEqual(StatusPanelFormat.switchBlockedText(.weeklyExhausted),
                       StatusPanelFormat.switchBlockedCue(.weeklyExhausted),
                       "a weekly exhaustion has no operator remedy, so nothing is held back for hover")
    }

    func testDoneTextNamesWhatActuallyHappened() {
        XCTAssertEqual(StatusPanelFormat.swapDoneText(.swapped(from: "Work", to: "Scratch")),
                       "Switched Work → Scratch")
        // A no-op says so — it never claims a switch that did not occur.
        let noop = StatusPanelFormat.swapDoneText(.alreadyActive(to: "Work"))
        XCTAssertEqual(noop, "Work is already active")
        XCTAssertFalse(noop.contains("Switched"))
    }

    // Every redacted verdict maps to exactly one operator-facing sentence — never the kebab tag, never
    // transport jargon. (Only the HYPHENATED rawValues are checked for leakage: `quarantined`, `cooldown`
    // and `failed` are ordinary English words that legitimately appear in their own copy.)
    func testErrorTextIsHumanForEveryRejection() {
        let rejections: [SwapRejection] = [
            .unknownTarget, .ambiguousTarget, .quarantined, .weeklyExhausted,
            .cooldown, .noActiveAccount, .keychainLocked, .swapLockBusy, .failed,
        ]
        var seen = Set<String>()
        for reason in rejections {
            let text = StatusPanelFormat.swapErrorText(.rejected(reason))
            XCTAssertFalse(text.isEmpty, "\(reason) has copy")
            if reason.rawValue.contains("-") {
                XCTAssertFalse(text.contains(reason.rawValue), "\(reason) leaks its machine tag: \(text)")
            }
            seen.insert(text)
        }
        XCTAssertEqual(seen.count, rejections.count, "each rejection reads distinctly")
    }

    // The two AMBIGUOUS transport outcomes must NOT claim the switch failed: the daemon writes the ack only
    // AFTER the swap runs, so a lost ack means the swap may well have COMMITTED. Claiming failure there is a
    // false negative; the copy points the operator at the roster, which the next `watch` snapshot settles.
    func testALostAckDoesNotClaimTheSwitchFailed() {
        for failure in [SwapFailure.transport(.timedOut), .transport(.closedBeforeAck)] {
            let text = StatusPanelFormat.swapErrorText(failure)
            XCTAssertTrue(text.contains("check the roster"),
                          "an ambiguous outcome sends the operator to the roster: \(text)")
            XCTAssertFalse(text.lowercased().contains("switch failed"),
                           "an ambiguous outcome never asserts failure: \(text)")
        }
    }

    // An absent daemon and an unreachable socket read differently — the operator's next move differs.
    func testAbsentDaemonAndUnreachableSocketReadDifferently() {
        XCTAssertNotEqual(StatusPanelFormat.swapErrorText(.transport(.connectionRefused(reason: "x"))),
                          StatusPanelFormat.swapErrorText(.unavailable))
    }

    // MARK: - StatusPanelFormat: accessibility

    // A `dimmed` trait alone never tells a VoiceOver user WHY a row cannot be switched to, so a blocked
    // row's label carries the reason. A viable row's label is untouched.
    func testABlockedRowSpeaksItsReason() {
        let base = "Scratch, session 12%, weekly 40%"
        XCTAssertEqual(StatusPanelFormat.rowSwitchAccessibilityLabel(base: base, block: nil), base)

        let blocked = StatusPanelFormat.rowSwitchAccessibilityLabel(base: base, block: .weeklyExhausted)
        XCTAssertTrue(blocked.hasPrefix(base))
        XCTAssertTrue(blocked.contains(StatusPanelFormat.switchBlockedText(.weeklyExhausted)))
    }

    func testSwitchHelpTextNamesTheTarget() {
        XCTAssertEqual(StatusPanelFormat.switchHelpText(label: "Scratch"), "Switch to Scratch")
    }

    // MARK: - Helpers

    private func encode(_ command: SwapCommand) throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        return String(decoding: try encoder.encode(command), as: UTF8.self)
    }

    @MainActor
    private func client(_ connection: CommandFakeConnection) -> ControlCommandClient {
        ControlCommandClient(connector: CommandFakeConnector(.succeed(connection)), timeout: .seconds(5))
    }

    private func waitUntil(_ predicate: () -> Bool, _ label: String) async throws {
        for _ in 0..<10_000 {
            if predicate() { return }
            await Task.yield()
        }
        XCTFail("timed out waiting for \(label)")
    }
}

// MARK: - Test doubles (a gated one-shot connection for the in-flight window)

/// Returns one pre-built `WatchConnection` — the any-`WatchConnection` analogue of the transport suite's
/// `CommandFakeConnector` (which is typed to `CommandFakeConnection`).
private struct SwapOneShotConnector: WatchConnector {
    let connection: WatchConnection
    func connect() throws -> WatchConnection { connection }
}

/// Hands out a FRESH `CommandFakeConnection` on every `connect()` — models the production client, which
/// opens a new one-shot socket per `send`. Lets ONE `AccountSwapModel` run several swaps in a test
/// (`CommandFakeConnection`'s stream finishes on close, so a single instance is single-use).
private struct FreshCommandConnector: WatchConnector {
    let ack: String
    func connect() throws -> WatchConnection { CommandFakeConnection(ackOnSend: ack) }
}

/// A one-shot control-command connection that HOLDS its ack until `release()` — so a test can observe the
/// model's `.pending` / `isBusy` window before the ack resolves it. `sendCount` proves a second click while
/// pending never reaches the daemon.
private final class GatedSwapConnection: WatchConnection, @unchecked Sendable {
    let lines: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private let ack: String
    private let sends = OSAllocatedUnfairLock(initialState: 0)

    init(ack: String) {
        self.ack = ack
        (lines, continuation) = AsyncStream<String>.makeStream()
    }

    func send(_ bytes: [UInt8]) throws { sends.withLock { $0 += 1 } }  // hold — do not ack until release()
    func release() { continuation.yield(ack); continuation.finish() }
    func close() { continuation.finish() }

    var sendCount: Int { sends.withLock { $0 } }
}
