// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The panel's swap-on-click model (issue #169): the thin `@MainActor` shell that owns the ONE-SHOT
// `swap` exchange over the #358 control-command transport and exposes its idle → pending → done →
// failed phase to the SwiftUI panel. It is the swap SIBLING of `AccountCaptureModel` (#360) — the same
// phase-machine shape over the same transport, only the verb differs — and, like it, the write-path
// counterpart of the read-only `WatchStatusStore` (#324): the store PROJECTS the daemon's `watch`
// stream; this model SENDS one command and renders its redacted ack. It never mutates the roster: the
// new active account arrives on its own via the next `watch` snapshot (command → daemon → `watch` → UI).
//
// ONE command, BOTH paths (issue #169): the footer **Swap** button sends the DISPLAYED `next_swap`
// target (WYSIWYG — never a client re-pick), and a per-row manual switch sends the clicked row's
// target. Both call `swap(to:)`; the daemon's `swap` verb already takes an ARBITRARY target and
// re-validates it from its OWN live state (`swap_command_verdict`, `src/daemon.rs`) — the client never
// sends a viability hint, and never re-derives the daemon's selection. There is no second, targetless
// command.
//
// AppKit-free BY DESIGN (Foundation + Combine + os only) so it compiles into the headless `MenubarTests`
// bundle and its phase transitions are driven hermetically against a fake connector — no socket, no live
// daemon (the same testability split `AccountCaptureModel` and the store use).
//
// NO credential handling of any kind (C-001 / issue #15): it serializes a verb + a non-secret roster
// LABEL and receives a redacted ack carrying only a machine tag plus labels — the whole channel carries
// no token, email, or oauth blob.

import Combine
import Foundation
import os

private let swapLog = Logger(subsystem: "org.sessiometer.menubar", category: "swap")

/// The `swap` control-command request (issue #167 wire): `{"cmd":"swap","target":"<label>","force":…}`.
///
/// `force` is ALWAYS `false` from the panel, deliberately. It is a POLICY-only bypass
/// (`swap_command_verdict`, `src/daemon.rs`): it skips the quarantined / weekly-exhausted / cooldown
/// gates that exist to protect the operator. An armed-on-hover row click is a low-ceremony gesture — far
/// too low to carry a silent policy override — so forcing stays where it is explicit and deliberate:
/// the CLI's `use --force`. The field is sent explicitly (not omitted) because the daemon's
/// `ControlRequest.force` is a plain `bool`.
struct SwapCommand: Encodable, Sendable {
    let cmd = "swap"
    /// The target's non-secret roster handle — an operator label, never an email (issue #15).
    let target: String
    let force = false
}

/// Why a swap did not succeed — a non-secret verdict the panel maps to human copy
/// (`StatusPanelFormat.swapErrorText`). The swap counterpart of `CaptureFailure`: it unifies the
/// daemon's redacted refusal, the shared error ack, the transport's bounded failures, an undecodable
/// ack (wire drift), and the no-client degraded case.
enum SwapFailure: Equatable {
    /// The daemon REFUSED with a known redacted machine reason (#167) — ZERO writes happened.
    case rejected(SwapRejection)
    /// The shared redacted `{"error":…}` ack (e.g. `unauthorized`) — the same-user local peer should
    /// never see it, but it is surfaced honestly rather than swallowed.
    case daemonError(String)
    /// A bounded transport failure (#358 `ControlCommandError`): no daemon (refused), a wedged daemon
    /// (timed out / closed before ack), or an I/O / encode fault.
    case transport(ControlCommandError)
    /// The ack line did not match the swap wire contract (a buggy / drifted daemon) — degrade loudly.
    case undecodable
    /// No control client — the daemon control-socket path would not resolve (sandboxed / home
    /// unresolved), so switching is unavailable from this app instance.
    case unavailable
}

/// A swap that SUCCEEDED — the two success shapes of `SwapAck`, narrowed so a `done` phase can never
/// carry a rejection. Both are label-only (redaction by construction, issue #15). The panel renders each
/// via `StatusPanelFormat.swapDoneText`, which pattern-matches the cases directly.
enum SwapSuccess: Equatable {
    /// The active credential was rerouted OFF `from` ONTO `to`.
    case swapped(from: String, to: String)
    /// A no-op success: `to` was ALREADY the active account, so nothing was written.
    case alreadyActive(to: String)
}

/// The swap affordance's interaction phase. `pending` carries the TARGET so the panel can show the
/// spinner on the clicked row (or the footer button) while `.disabled()`-ing every sibling target;
/// `done` / `failed` carry the structured facts the panel renders via `StatusPanelFormat`, so the view
/// never invents its own strings.
///
/// A `pending` that never resolves is IMPOSSIBLE: the transport bounds BOTH the connect+write and the
/// ack read (`ControlCommandClient`), so a lost ack lands in `.failed(.transport(.timedOut))` — the
/// issue's "pending times out so a lost ack can't stick a spinner", earned by the transport rather than
/// by a second timer here.
enum SwapPhase: Equatable {
    case idle
    case pending(target: String)
    case done(SwapSuccess)
    case failed(SwapFailure)

    /// The label of the target whose swap is in flight, or `nil` when none is.
    var pendingTarget: String? {
        if case .pending(let target) = self { return target }
        return nil
    }

    /// Whether ANY swap is in flight — the sibling-disable predicate, and one half of the panel-retain
    /// gate (`isBusy`).
    var isPending: Bool { pendingTarget != nil }
}

@MainActor
final class AccountSwapModel: ObservableObject {
    /// The short-lived control-command client, or `nil` when the socket path would not resolve — in which
    /// case a swap attempt short-circuits to `.failed(.unavailable)` (honest, never a dead button).
    private let client: ControlCommandClient?

    /// The current interaction phase the panel observes.
    @Published private(set) var phase: SwapPhase = .idle

    init(client: ControlCommandClient?) {
        self.client = client
    }

    /// The panel-retain predicate: an outside click must NOT dismiss while a swap is in flight, so the
    /// operator sees the outcome of a write they just authorized (the capture affordance's `isBusy`
    /// gate, extended to the swap's write path).
    var isBusy: Bool { phase.isPending }

    /// Switch the active account to `target` — the ONE verb behind both the footer **Swap** button (which
    /// passes the displayed `next_swap` target) and a per-row manual switch (which passes the clicked
    /// row's target). Renders pending → done / failed.
    ///
    /// The daemon re-validates the target from its own live state and may still refuse (`cooldown` is not
    /// even on the wire, so a row the panel shows as viable can legitimately come back rejected) — that
    /// refusal is rendered inline, never pre-empted by a client-side guess. Never mutates the roster: the
    /// new active row arrives via the `watch` snapshot; on `done` the affordance auto-resets to idle after
    /// a short confirmation beat. A double-submit while a swap is already in flight is ignored.
    func swap(to target: String) async {
        guard !phase.isPending else { return }

        guard let client else {
            phase = .failed(.unavailable)
            return
        }

        phase = .pending(target: target)
        let result = await client.send(SwapCommand(target: target))

        switch result {
        case .failure(let error):
            swapLog.error("swap: transport failure — \(String(describing: error), privacy: .public)")
            phase = .failed(.transport(error))
        case .success(let line):
            do {
                switch try SwapAck.decode(line) {
                case .accepted(let from, let to):
                    settle(.swapped(from: from, to: to))
                case .alreadyActive(let to):
                    settle(.alreadyActive(to: to))
                case .rejected(let reason):
                    swapLog.error("swap: rejected — \(reason.rawValue, privacy: .public)")
                    phase = .failed(.rejected(reason))
                case .error(let reason):
                    swapLog.error("swap: daemon error — \(reason, privacy: .public)")
                    phase = .failed(.daemonError(reason))
                }
            } catch {
                swapLog.error("swap: undecodable ack — \(String(describing: error), privacy: .public)")
                phase = .failed(.undecodable)
            }
        }
    }

    /// Land a successful swap and schedule its confirmation beat.
    private func settle(_ success: SwapSuccess) {
        phase = .done(success)
        scheduleIdleReset(for: success)
    }

    /// Reset a `done` phase back to idle after a short confirmation beat — but only if the phase is STILL
    /// that same `done` (a new swap supersedes it), so a racing transition is never clobbered. The window
    /// mirrors the capture affordance's confirmation beat. A `failed` phase deliberately does NOT
    /// auto-clear: an error the operator has not acted on must not vanish on its own; the next swap
    /// attempt replaces it.
    private func scheduleIdleReset(for success: SwapSuccess) {
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(1.8))
            guard let self else { return }
            if self.phase == .done(success) { self.phase = .idle }
        }
    }
}
